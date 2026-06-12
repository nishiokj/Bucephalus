import { spawn } from "node:child_process";
import { mkdir, readFile, readdir, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import * as tar from "tar";
import { authOwnerKey, type AuthContext } from "../auth";
import { loadConfig } from "../config";
import { HttpError, jsonResponse, optionalString, readJsonObject, requireString } from "../http";
import { extractAuthoringContextArchive, safeAuthoringContextPath } from "../imports/authoringContext";
import { ImportRepository, type UploadRecord } from "../imports/repository";
import { redactSensitiveText } from "../jsonRedaction";
import { importJobToWire, importSealedPackageUpload } from "./imports";
import { materializeStoredObject, putUploadObject } from "../objectStorage";
import { optionalJsonObject, PackageRepository, RunRepository, type PackageArtifactRecord, type RunRequirements } from "../packages/repository";
import { RunnerRepository } from "../runners/repository";
import type { CloudSecretRepository } from "../secrets/repository";
import { diagnoseCloudRunRequest, packageSecretRequirements, requireSchedulableRun, runRequirementsForArtifact } from "./runs";
import { sha256Digest, type JsonObject } from "../primitives";

const DEFAULT_HOSTED_AUTHORING_BUILD_TIMEOUT_MS = 10 * 60 * 1000;

export async function handleExperimentRoute(
  request: Request,
  url: URL,
  imports: ImportRepository,
  packages: PackageRepository,
  _runs: RunRepository,
  runners: RunnerRepository,
  auth?: AuthContext | null,
  secrets?: CloudSecretRepository,
): Promise<Response | null> {
  const ownerKey = authOwnerKey(auth);

  if (request.method === "POST" && url.pathname === "/v1/experiments/builds") {
    const config = loadConfig();
    const body = await readJsonObject(request);
    const inputKind = optionalString(body.input_kind, "/input_kind") ?? "sealed_package";
    if (inputKind === "authoring_context") {
      const build = await hostedAuthoringBuildUpload(body, imports, packages, runners, ownerKey, config);
      return jsonResponse(build, { status: 201 });
    }
    if (inputKind !== "sealed_package") {
      throw new HttpError(400, "unsupported_build_input_kind", `Unsupported hosted build input_kind '${inputKind}'`);
    }
    const runtimeOptions = optionalJsonObject(body.runtime_options as JsonObject | undefined, "/runtime_options");
    const uploadId = requireString(body.upload_id, "/upload_id");
    const sourceUpload = await imports.getUpload(uploadId, ownerKey);
    if (!sourceUpload) {
      throw new HttpError(404, "upload_not_found", "Upload not found");
    }
    if (sourceUpload.status !== "completed" || !sourceUpload.storage_path) {
      throw new HttpError(409, "upload_not_completed", "Upload must be completed before hosted build");
    }
    const source = hostedBuildSource("sealed_package", sourceUpload);
    const buildEnvironment = hostedBuildEnvironment({
      inputKind: "sealed_package",
      runtimeOptions,
      evidencePolicy: config.buildEvidencePolicy,
      source,
    });
    const job = await importSealedPackageUpload(body, imports, packages, ownerKey, {
      packageProvenance: packageProvenanceFromBuildEnvironment(buildEnvironment),
    });
    const cloudReadiness = await hostedCloudReadiness({
      jobPackageDigest: job.package_digest,
      importStatus: job.status,
      packages,
      runners,
      ownerKey,
      runtimeOptions,
      buildEnvironment,
    });
    return jsonResponse({
      build_id: job.import_id,
      build_kind: "sealed_package_import",
      build_environment: buildEnvironment,
      authoring_build: {
        status: "unavailable",
        message: "Sealed package input was imported directly; hosted authoring build only runs for input_kind=authoring_context.",
      },
      status: cloudReadiness.status === "unavailable" ? job.status : cloudReadiness.status,
      label: optionalString(body.label, "/label"),
      package_digest: job.package_digest,
      cloud_readiness: cloudReadiness,
      import: importJobToWire(job),
    }, { status: 201 });
  }

  if (request.method === "POST" && url.pathname === "/v1/experiments/doctor") {
    const body = await readJsonObject(request);
    const diagnosis = await diagnoseCloudRunRequest({
      body,
      packages,
      runners,
      ownerKey,
      secrets,
      requireHostedSecretRefs: true,
    });
    return jsonResponse({
      ok: true,
      status: "runnable",
      package_digest: diagnosis.artifact.package_digest,
      package_status: diagnosis.artifact.status,
      name: packageName(diagnosis.artifact.resolved_experiment_json),
      image_refs: diagnosis.artifact.image_refs,
      package_provenance: diagnosis.artifact.package_provenance,
      secret_requirements: packageSecretRequirements(diagnosis.artifact),
      supplied_secret_ids: Object.keys(diagnosis.secretRefs).sort(),
      runtime_options: diagnosis.runtimeOptions,
      run_requirements: diagnosis.runRequirements,
    });
  }

  return null;
}

function packageName(resolvedExperiment: Record<string, unknown>): string | null {
  const experiment = resolvedExperiment.experiment;
  if (!isRecord(experiment)) {
    return null;
  }
  const name = experiment.name;
  return typeof name === "string" && name.trim().length > 0 ? name.trim() : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function hostedAuthoringBuildUpload(
  body: Record<string, unknown>,
  imports: ImportRepository,
  packages: PackageRepository,
  runners: RunnerRepository,
  ownerKey?: string,
  config = loadConfig(),
): Promise<Record<string, unknown>> {
  const sourceUploadId = requireString(body.upload_id, "/upload_id");
  const entrypoint = safeAuthoringContextPath(requireString(body.entrypoint, "/entrypoint"), "/entrypoint");
  const sourceUpload = await imports.getUpload(sourceUploadId, ownerKey);
  if (!sourceUpload) {
    throw new HttpError(404, "upload_not_found", "Upload not found");
  }
  if (sourceUpload.status !== "completed" || !sourceUpload.storage_path) {
    throw new HttpError(409, "upload_not_completed", "Upload must be completed before hosted authoring build");
  }

  const runtimeOptions = optionalJsonObject(body.runtime_options as JsonObject | undefined, "/runtime_options");
  const source = hostedBuildSource("authoring_context", sourceUpload, entrypoint);
  const buildEnvironment = hostedBuildEnvironment({
    inputKind: "authoring_context",
    runtimeOptions,
    evidencePolicy: config.buildEvidencePolicy,
    source,
  });
  const buildId = crypto.randomUUID();
  const buildRoot = join(config.dataDir, "authoring-builds", buildId);
  const contextDir = join(buildRoot, "context");
  const outputDir = join(buildRoot, "package");
  const homeDir = join(buildRoot, "home");
  const tmpDir = join(homeDir, "tmp");

  let authoringBuild: Record<string, unknown>;
  let job = null;
  let cloudReadiness: HostedCloudReadiness | null = null;
  try {
    validateHostedAuthoringArchiveUpload(sourceUpload);
    const archivePath = await materializeStoredObject(sourceUpload.storage_path, join(buildRoot, "source-archive"), "context.tgz");
    await verifyMaterializedHostedAuthoringSource(archivePath, sourceUpload);
    const contextInspection = await extractAuthoringContextArchive({
      archivePath,
      workDir: contextDir,
      entrypoint,
    });
    const coreResult = await runCoreAuthoringBuild({
      coreCli: coreCliPath(),
      cwd: contextDir,
      entrypoint: contextInspection.entrypoint,
      outputDir,
      homeDir,
      tmpDir,
    });
    const packageArchivePath = join(buildRoot, "package.tgz");
    await createArchiveFromDirectory(outputDir, packageArchivePath);
    const packageBytes = await readFile(packageArchivePath);
    const packageUpload = await imports.createUpload({
      filename: "package.tgz",
      mediaType: "application/gzip",
      expectedDigest: sha256Digest(packageBytes),
      byteSize: packageBytes.byteLength,
      ownerKey,
    });
    const storagePath = await putUploadObject(packageUpload.upload_id, packageBytes, "application/gzip", config);
    await imports.markUploaded({
      uploadId: packageUpload.upload_id,
      contentDigest: sha256Digest(packageBytes),
      byteSize: packageBytes.byteLength,
      storagePath,
      ownerKey,
    });
    await imports.completeUpload(packageUpload.upload_id, ownerKey);
    job = await importSealedPackageUpload(
      {
        upload_id: packageUpload.upload_id,
        label: optionalString(body.label, "/label"),
      },
      imports,
      packages,
      ownerKey,
      {
        packageProvenance: packageProvenanceFromBuildEnvironment(buildEnvironment),
      },
    );
    cloudReadiness = await hostedCloudReadiness({
      jobPackageDigest: job.package_digest,
      importStatus: job.status,
      packages,
      runners,
      ownerKey,
      runtimeOptions,
      buildEnvironment,
    });
    authoringBuild = {
      status: "succeeded",
      source_upload_id: sourceUploadId,
      entrypoint: contextInspection.entrypoint,
      context_entries: contextInspection.entries,
      context_expanded_bytes: contextInspection.expandedBytes,
      core: coreResult,
    };
  } catch (error) {
    authoringBuild = {
      status: "failed",
      source_upload_id: sourceUploadId,
      entrypoint,
      error: error instanceof Error ? error.message : String(error),
      code: error instanceof HttpError ? error.code : "hosted_authoring_build_failed",
      detail: error instanceof HttpError ? error.detail ?? {} : {},
    };
    cloudReadiness = withBuildEnvironmentEvidence({
      status: "unavailable",
      target: { kind: "hosted_cloud", name: "default" },
      runtime_options: runtimeOptions,
      package_digest: null,
      package_provenance: packageProvenanceFromBuildEnvironment(buildEnvironment),
      package_status: null,
      run_requirements: null,
      secret_requirements: [],
      required_actions: [],
      checks: [{
        name: "authoring_build",
        status: "blocked",
        code: error instanceof HttpError ? error.code : "hosted_authoring_build_failed",
        message: error instanceof Error ? error.message : String(error),
      }],
    }, buildEnvironment);
  }

  return {
    build_id: job?.import_id ?? buildId,
    build_kind: "hosted_authoring_build",
    build_environment: buildEnvironment,
    authoring_build: authoringBuild,
    status: authoringBuild.status === "failed"
      ? "failed"
      : cloudReadiness.status === "unavailable" ? job?.status ?? "failed" : cloudReadiness.status,
    label: optionalString(body.label, "/label"),
    package_digest: job?.package_digest ?? null,
    cloud_readiness: cloudReadiness,
    import: job ? importJobToWire(job) : null,
  };
}

function validateHostedAuthoringArchiveUpload(upload: UploadRecord): void {
  const mediaType = upload.media_type.toLowerCase().split(";")[0]?.trim() ?? "";
  const filename = upload.filename.toLowerCase();
  const mediaTypeSupported = [
    "application/gzip",
    "application/x-gzip",
    "application/tar",
    "application/x-tar",
  ].includes(mediaType);
  const filenameSupported = filename.endsWith(".tgz")
    || filename.endsWith(".tar.gz")
    || filename.endsWith(".tar");
  if (!mediaTypeSupported || !filenameSupported) {
    throw new HttpError(400, "invalid_authoring_context_upload", "Hosted authoring builds require an uploaded tar archive for the authoring context", {
      upload_id: upload.upload_id,
      filename: upload.filename,
      media_type: upload.media_type,
      supported_media_types: [
        "application/gzip",
        "application/x-gzip",
        "application/tar",
        "application/x-tar",
      ],
      supported_filename_suffixes: [".tgz", ".tar.gz", ".tar"],
    });
  }
}

async function verifyMaterializedHostedAuthoringSource(archivePath: string, upload: UploadRecord): Promise<void> {
  const expectedDigest = upload.content_digest!;
  const expectedBytes = uploadByteSizeForSourceVerification(upload.byte_size)!;
  const bytes = await readFile(archivePath);
  const actualDigest = sha256Digest(bytes);
  if (actualDigest !== expectedDigest) {
    throw new HttpError(409, "authoring_context_source_digest_mismatch", "Materialized authoring context source does not match the completed upload digest", {
      upload_id: upload.upload_id,
      expected_digest: expectedDigest,
      content_digest: actualDigest,
    });
  }
  if (bytes.byteLength !== expectedBytes) {
    throw new HttpError(409, "authoring_context_source_size_mismatch", "Materialized authoring context source does not match the completed upload byte_size", {
      upload_id: upload.upload_id,
      expected_byte_size: expectedBytes,
      byte_size: bytes.byteLength,
    });
  }
}

function uploadByteSizeForSourceVerification(value: UploadRecord["byte_size"]): number | null {
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) {
    return value;
  }
  if (typeof value === "string" && /^[0-9]+$/.test(value)) {
    const parsed = Number.parseInt(value, 10);
    return Number.isSafeInteger(parsed) ? parsed : null;
  }
  return null;
}

function packageProvenanceFromBuildEnvironment(environment: HostedBuildEnvironment): JsonObject {
  const provenance = environment.package_contract.authoring_provenance;
  return {
    schema_version: "cloud_package_provenance_v1",
    status: provenance.status,
    source: provenance.source,
    message: provenance.message,
    build_target: environment.target,
    input_kind: environment.source.input_kind,
    source_upload_id: environment.source.upload_id,
    source_content_digest: environment.source.content_digest,
    builder: environment.builder,
    core: environment.core,
  };
}

async function runCoreAuthoringBuild(input: {
  coreCli: string;
  cwd: string;
  entrypoint: string;
  outputDir: string;
  homeDir: string;
  tmpDir: string;
}): Promise<Record<string, unknown>> {
  await rm(input.outputDir, { recursive: true, force: true });
  await mkdir(input.homeDir, { recursive: true });
  await mkdir(input.tmpDir, { recursive: true });
  const timeoutMs = hostedAuthoringBuildTimeoutMs();
  const result = await runHostedCoreProcess({
    executable: input.coreCli,
    args: [
      "build",
      input.entrypoint,
      "--out",
      input.outputDir,
      "--json",
    ],
    cwd: input.cwd,
    env: hostedCoreBuildEnv(input.homeDir, input.tmpDir),
    timeoutMs,
  });
  const { stdout, stderr, exitCode } = result;
  if (exitCode !== 0) {
    throw new HttpError(400, "authoring_build_failed", "Hosted Core build failed", {
      exit_code: exitCode,
      stdout_tail: tailText(stdout),
      stderr_tail: tailText(stderr),
    });
  }
  return {
    command: "bucephalus build",
    exit_code: exitCode,
    timeout_ms: timeoutMs,
    stdout_tail: tailText(stdout),
    stderr_tail: tailText(stderr),
  };
}

async function runHostedCoreProcess(input: {
  executable: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
  timeoutMs: number;
}): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return await new Promise((resolve, reject) => {
    const child = spawn(input.executable, input.args, {
      cwd: input.cwd,
      env: input.env,
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let settled = false;
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    const timeout = globalThis.setTimeout(() => {
      if (settled) {
        return;
      }
      settled = true;
      signalProcessGroup(child, "SIGKILL");
      reject(new HttpError(408, "authoring_build_timed_out", "Hosted Core build timed out", {
        timeout_ms: input.timeoutMs,
        stdout_tail: tailText(Buffer.concat(stdout).toString("utf8")),
        stderr_tail: tailText(Buffer.concat(stderr).toString("utf8")),
      }));
    }, input.timeoutMs);
    timeout.unref?.();
    child.stdout?.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.on("error", (error) => {
      if (settled) {
        return;
      }
      settled = true;
      globalThis.clearTimeout(timeout);
      reject(error);
    });
    child.on("close", (code) => {
      if (settled) {
        return;
      }
      settled = true;
      globalThis.clearTimeout(timeout);
      resolve({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
  });
}

function signalProcessGroup(child: ReturnType<typeof spawn>, signal: NodeJS.Signals): void {
  if (!child.pid) {
    return;
  }
  try {
    process.kill(-child.pid, signal);
  } catch {
    child.kill(signal);
  }
}

function hostedCoreBuildEnv(homeDir: string, tmpDir: string): Record<string, string> {
  const env: Record<string, string> = {
    BUCEPHALUS_HOME: homeDir,
    BUCEPHALUS_NO_SETUP: "1",
    HOME: homeDir,
    TMPDIR: tmpDir,
    TMP: tmpDir,
    TEMP: tmpDir,
    USER: process.env.USER?.trim() || "bucephalus-cloud-builder",
    USERNAME: process.env.USERNAME?.trim() || process.env.USER?.trim() || "bucephalus-cloud-builder",
  };
  for (const name of [
    "PATH",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NO_COLOR",
    "RUST_BACKTRACE",
  ]) {
    const value = process.env[name];
    if (value && value.trim().length > 0) {
      env[name] = value;
    }
  }
  return env;
}

async function createArchiveFromDirectory(directory: string, archivePath: string): Promise<void> {
  const directoryStat = await stat(directory).catch((error) => {
    if (isNodeError(error) && error.code === "ENOENT") {
      return null;
    }
    throw error;
  });
  if (!directoryStat) {
    throw new HttpError(500, "authoring_build_missing_package", "Hosted Core build did not create a package output directory");
  }
  if (!directoryStat.isDirectory()) {
    throw new HttpError(500, "authoring_build_invalid_package", "Hosted Core build package output path is not a directory");
  }
  const entries = (await readdir(directory)).sort();
  if (entries.length === 0) {
    throw new HttpError(500, "authoring_build_empty_package", "Hosted Core build produced an empty package directory");
  }
  await tar.c({
    gzip: true,
    cwd: directory,
    file: archivePath,
  }, entries);
}

function isNodeError(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && "code" in error;
}

function coreCliPath(): string {
  return process.env.BUCEPHALUS_CLOUD_CORE_CLI?.trim()
    || join(process.cwd(), "bin", "bucephalus");
}

function hostedAuthoringBuildTimeoutMs(): number {
  return positiveIntegerEnv("BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS", DEFAULT_HOSTED_AUTHORING_BUILD_TIMEOUT_MS);
}

function hostedBuildEnvironment(input: {
  inputKind: "sealed_package" | "authoring_context";
  runtimeOptions: JsonObject;
  evidencePolicy: HostedBuildEvidencePolicy;
  source: HostedBuildEnvironment["source"];
}): HostedBuildEnvironment {
  const releaseVersion = firstNonEmptyEnv([
    "BUCEPHALUS_CLOUD_RELEASE_VERSION",
    "BUCEPHALUS_RELEASE_VERSION",
  ]);
  const builder = {
    kind: input.inputKind === "authoring_context"
      ? "hosted_authoring_builder" as const
      : "sealed_package_importer" as const,
    image_digest: firstNonEmptyEnv([
      "BUCEPHALUS_CLOUD_BUILDER_IMAGE_DIGEST",
      "BUCEPHALUS_CLOUD_API_IMAGE_DIGEST",
    ]),
    release_version: releaseVersion,
    git_sha: firstNonEmptyEnv([
      "BUCEPHALUS_CLOUD_GIT_SHA",
      "BUCEPHALUS_GIT_SHA",
      "BUCEPHALUS_RELEASE_GIT_SHA",
      "GITHUB_SHA",
    ]),
    os: process.platform,
    arch: process.arch,
  };
  const core = input.inputKind === "authoring_context"
    ? {
      executed: true as const,
      command: "bucephalus build" as const,
      path: coreCliPath(),
      version: firstNonEmptyEnv([
        "BUCEPHALUS_CLOUD_CORE_VERSION",
        "BUCEPHALUS_CORE_VERSION",
      ]) ?? releaseVersion,
      timeout_ms: hostedAuthoringBuildTimeoutMs(),
    }
    : {
      executed: false as const,
      command: null,
      path: null,
      version: null,
      timeout_ms: null,
      reason: "Sealed package input was imported directly; Cloud did not run hosted Core authoring.",
    };
  return {
    schema_version: "hosted_build_environment_v1",
    target: {
      kind: "hosted_cloud",
      name: "default",
    },
    source: input.source,
    runtime_options: input.runtimeOptions,
    builder,
    core,
    package_contract: {
      input_kind: input.inputKind,
      authoring_compiler: input.inputKind === "authoring_context" ? "core_universal_v1" : null,
      authoring_provenance: packageAuthoringProvenance(input.inputKind),
      sealed_schema_version: "sealed_run_package_v2",
      readiness_schema_version: "hosted_cloud_readiness_v1",
      cloud_readiness_required: true,
    },
    evidence: buildEnvironmentEvidence(builder, core, input.evidencePolicy),
  };
}

function packageAuthoringProvenance(
  inputKind: "sealed_package" | "authoring_context",
): HostedBuildEnvironment["package_contract"]["authoring_provenance"] {
  return inputKind === "authoring_context"
    ? {
      status: "hosted_attested",
      source: "hosted_core",
      message: "Cloud ran hosted Core authoring for this package and recorded the builder/core environment.",
    }
    : {
      status: "external_unattested",
      source: "sealed_package_manifest",
      message: "Cloud verified sealed package integrity and hosted readiness, but sealed_run_package_v2 does not attest the package's original authoring environment.",
    };
}

function hostedBuildSource(
  inputKind: "sealed_package" | "authoring_context",
  upload: UploadRecord,
  entrypoint?: string,
): HostedBuildEnvironment["source"] {
  const evidence = requireHostedBuildSourceEvidence(upload);
  return {
    input_kind: inputKind,
    upload_id: upload.upload_id,
    filename: upload.filename,
    media_type: upload.media_type,
    content_digest: evidence.contentDigest,
    byte_size: evidence.byteSize,
    ...(entrypoint ? { entrypoint } : {}),
  };
}

function requireHostedBuildSourceEvidence(upload: UploadRecord): { contentDigest: string; byteSize: number } {
  if (!upload.content_digest) {
    throw new HttpError(409, "invalid_build_source_upload", "Hosted build source upload is missing content_digest", {
      upload_id: upload.upload_id,
      filename: upload.filename,
      media_type: upload.media_type,
    });
  }
  const byteSize = uploadByteSizeForSourceVerification(upload.byte_size);
  if (byteSize === null) {
    throw new HttpError(409, "invalid_build_source_upload", "Hosted build source upload is missing byte_size", {
      upload_id: upload.upload_id,
      filename: upload.filename,
      media_type: upload.media_type,
    });
  }
  return {
    contentDigest: upload.content_digest,
    byteSize,
  };
}

function buildEnvironmentEvidence(
  builder: HostedBuildEnvironment["builder"],
  core: HostedBuildEnvironment["core"],
  policy: HostedBuildEvidencePolicy,
): HostedBuildEnvironment["evidence"] {
  const checks: HostedBuildEnvironment["evidence"]["checks"] = [
    builder.image_digest
      ? {
        name: "builder_image_digest",
        status: "passed",
        code: "builder_image_digest_recorded",
        message: "Build environment records the immutable hosted builder/API image digest.",
      }
      : {
        name: "builder_image_digest",
        status: "warning",
        code: "builder_image_digest_missing",
        message: "Build environment does not include the hosted builder/API image digest; production deployments must inject BUCEPHALUS_CLOUD_API_IMAGE_DIGEST.",
      },
    builder.release_version
      ? {
        name: "builder_release_version",
        status: "passed",
        code: "builder_release_version_recorded",
        message: "Build environment records the hosted release version.",
      }
      : {
        name: "builder_release_version",
        status: "warning",
        code: "builder_release_version_missing",
        message: "Build environment does not include the hosted release version.",
      },
    builder.git_sha
      ? {
        name: "builder_git_sha",
        status: "passed",
        code: "builder_git_sha_recorded",
        message: "Build environment records the hosted release git SHA.",
      }
      : {
        name: "builder_git_sha",
        status: "warning",
        code: "builder_git_sha_missing",
        message: "Build environment does not include a hosted release git SHA.",
      },
  ];
  if (core.executed) {
    checks.push(
      core.version
        ? {
          name: "core_version",
          status: "passed",
          code: "core_version_recorded",
          message: "Build environment records the hosted Core version.",
        }
        : {
          name: "core_version",
          status: "warning",
          code: "core_version_missing",
          message: "Build environment does not include the hosted Core version.",
        },
    );
  } else {
    checks.push({
      name: "hosted_core_authoring",
      status: "passed",
      code: "hosted_core_not_run_for_sealed_package",
      message: "Sealed package input was imported directly; Cloud readiness was checked without claiming hosted Core authored the package.",
    });
  }
  const missing = checks
    .filter((check) => check.status === "warning")
    .map((check) => check.name);
  return {
    policy,
    status: missing.length === 0 ? "complete" : "partial",
    missing,
    checks,
  };
}

function firstNonEmptyEnv(names: string[]): string | null {
  for (const name of names) {
    const value = process.env[name]?.trim();
    if (value) {
      return value;
    }
  }
  return null;
}

function tailText(value: string, maxChars = 4000): string {
  const redacted = redactSensitiveText(value);
  return redacted.length <= maxChars ? redacted : redacted.slice(redacted.length - maxChars);
}

function positiveIntegerEnv(name: string, fallback: number): number {
  const parsed = Number.parseInt(process.env[name] ?? "", 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

interface HostedCloudReadiness {
  status: "cloud_runnable" | "cloud_blocked" | "unavailable";
  target: {
    kind: "hosted_cloud";
    name: "default";
  };
  runtime_options: JsonObject;
  package_digest: string | null;
  package_provenance: JsonObject;
  package_status: string | null;
  run_requirements: RunRequirements | null;
  secret_requirements: ReturnType<typeof packageSecretRequirements>;
  required_actions: HostedCloudAction[];
  checks: HostedCloudCheck[];
}

interface HostedBuildEnvironment {
  schema_version: "hosted_build_environment_v1";
  target: {
    kind: "hosted_cloud";
    name: "default";
  };
  source: {
    input_kind: "sealed_package" | "authoring_context";
    upload_id: string;
    filename: string;
    media_type: string;
    content_digest: string;
    byte_size: number;
    entrypoint?: string;
  };
  runtime_options: JsonObject;
  builder: {
    kind: "hosted_authoring_builder" | "sealed_package_importer";
    image_digest: string | null;
    release_version: string | null;
    git_sha: string | null;
    os: NodeJS.Platform;
    arch: string;
  };
  core:
    | {
      executed: true;
      command: "bucephalus build";
      path: string;
      version: string | null;
      timeout_ms: number;
    }
    | {
      executed: false;
      command: null;
      path: null;
      version: null;
      timeout_ms: null;
      reason: string;
    };
  package_contract: {
    input_kind: "sealed_package" | "authoring_context";
    authoring_compiler: "core_universal_v1" | null;
    authoring_provenance:
      | {
        status: "hosted_attested";
        source: "hosted_core";
        message: string;
      }
      | {
        status: "external_unattested";
        source: "sealed_package_manifest";
        message: string;
      };
    sealed_schema_version: "sealed_run_package_v2";
    readiness_schema_version: "hosted_cloud_readiness_v1";
    cloud_readiness_required: boolean;
  };
  evidence: {
    policy: HostedBuildEvidencePolicy;
    status: "complete" | "partial";
    missing: string[];
    checks: Array<{
      name: string;
      status: "passed" | "warning";
      code: string;
      message: string;
    }>;
  };
}

type HostedBuildEvidencePolicy = "warn" | "enforce";

interface HostedCloudCheck {
  name: string;
  status: "passed" | "blocked" | "warning" | "unavailable";
  code: string;
  message: string;
  detail?: Record<string, unknown>;
}

interface HostedCloudAction {
  action: string;
  stage: "before_run" | "before_rebuild" | "operator";
  description: string;
  command?: string;
  requirement_id?: string;
  blocking?: boolean;
}

async function hostedCloudReadiness(input: {
  jobPackageDigest: string | null;
  importStatus: string;
  packages: PackageRepository;
  runners: RunnerRepository;
  ownerKey?: string | undefined;
  runtimeOptions: JsonObject;
  buildEnvironment: HostedBuildEnvironment;
}): Promise<HostedCloudReadiness> {
  const base = {
    target: {
      kind: "hosted_cloud" as const,
      name: "default" as const,
    },
    runtime_options: input.runtimeOptions,
    package_digest: input.jobPackageDigest,
    package_provenance: packageProvenanceFromBuildEnvironment(input.buildEnvironment),
  };

  if (input.importStatus !== "accepted") {
    return withBuildEnvironmentEvidence({
      ...base,
      status: "unavailable",
      package_status: null,
      run_requirements: null,
      secret_requirements: [],
      required_actions: [{
        action: "fix_package_import",
        stage: "before_rebuild",
        description: "Fix the sealed package import diagnostics, then rerun buc build.",
        command: "buc build <same-input>",
        blocking: true,
      }],
      checks: [{
        name: "package_import",
        status: "unavailable",
        code: "package_import_not_accepted",
        message: "Hosted Cloud readiness is unavailable until the sealed package import is accepted.",
      }],
    }, input.buildEnvironment);
  }

  if (!input.jobPackageDigest) {
    return withBuildEnvironmentEvidence({
      ...base,
      status: "cloud_blocked",
      package_status: null,
      run_requirements: null,
      secret_requirements: [],
      required_actions: [{
        action: "contact_support",
        stage: "operator",
        description: "Cloud accepted the import but did not persist a package digest for readiness checks.",
        blocking: true,
      }],
      checks: [{
        name: "package_import",
        status: "blocked",
        code: "package_digest_missing_after_import",
        message: "The sealed package import was accepted, but no package_digest was recorded; hosted Cloud cannot prove this build is runnable.",
      }],
    }, input.buildEnvironment);
  }

  const artifact = await input.packages.getArtifact(input.jobPackageDigest, input.ownerKey);
  if (!artifact) {
    return withBuildEnvironmentEvidence({
      ...base,
      status: "cloud_blocked",
      package_status: null,
      run_requirements: null,
      secret_requirements: [],
      required_actions: [{
        action: "contact_support",
        stage: "operator",
        description: "Cloud accepted the import but could not load the package artifact for readiness checks.",
        blocking: true,
      }],
      checks: [{
        name: "package_import",
        status: "blocked",
        code: "package_artifact_missing_after_import",
        message: "The sealed package import was accepted, but the package artifact is not available for hosted Cloud readiness checks.",
      }],
    }, input.buildEnvironment);
  }

  const readiness = await hostedCloudReadinessForArtifact({
    artifact,
    runners: input.runners,
    runtimeOptions: input.runtimeOptions,
  });
  return withBuildEnvironmentEvidence(readiness, input.buildEnvironment);
}

async function hostedCloudReadinessForArtifact(input: {
  artifact: PackageArtifactRecord;
  runners: RunnerRepository;
  runtimeOptions: JsonObject;
}): Promise<HostedCloudReadiness> {
  const secretRequirements = packageSecretRequirements(input.artifact);
  const secretActions = secretRequirements.map(secretRequirementAction);
  const secretRefsForRequirementPlanning = Object.fromEntries(
    secretRequirements.map((requirement) => [requirement.id, "bucephalus://build-placeholder"]),
  );
  const checks: HostedCloudCheck[] = [{
    name: "package_import",
    status: "passed",
    code: "package_import_accepted",
    message: "Sealed package import accepted.",
  }];

  let runRequirements: RunRequirements;
  try {
    runRequirements = runRequirementsForArtifact(
      input.artifact,
      input.runtimeOptions,
      secretRefsForRequirementPlanning,
    );
    checks.push({
      name: "runtime_contract",
      status: "passed",
      code: "hosted_runtime_contract_supported",
      message: "Package runtime, images, network, resource requests, and isolation map to the hosted Cloud contract.",
    });
  } catch (error) {
    checks.push(checkFromError("runtime_contract", error));
    return {
      status: "cloud_blocked",
      target: { kind: "hosted_cloud", name: "default" },
      runtime_options: input.runtimeOptions,
      package_digest: input.artifact.package_digest,
      package_provenance: input.artifact.package_provenance,
      package_status: input.artifact.status,
      run_requirements: null,
      secret_requirements: secretRequirements,
      required_actions: [
        ...secretActions,
        cloudActionFromError(error, "before_rebuild"),
      ],
      checks,
    };
  }

  if (secretRequirements.length > 0) {
    checks.push({
      name: "secrets",
      status: "warning",
      code: "secrets_required_at_run_time",
      message: "Package declares runtime secrets. Build can pass, but run creation must provide matching hosted/provider secret refs.",
      detail: {
        required_secret_ids: secretRequirements.map((requirement) => requirement.id),
      },
    });
  } else {
    checks.push({
      name: "secrets",
      status: "passed",
      code: "no_runtime_secrets_required",
      message: "Package does not declare runtime secrets.",
    });
  }

  try {
    await requireSchedulableRun(input.runners, runRequirements);
    checks.push({
      name: "runner_capacity",
      status: "passed",
      code: "runner_pool_available",
      message: "At least one active hosted runner pool can satisfy this package.",
    });
  } catch (error) {
    checks.push(checkFromError("runner_capacity", error));
    return {
      status: "cloud_blocked",
      target: { kind: "hosted_cloud", name: "default" },
      runtime_options: input.runtimeOptions,
      package_digest: input.artifact.package_digest,
      package_provenance: input.artifact.package_provenance,
      package_status: input.artifact.status,
      run_requirements: runRequirements,
      secret_requirements: secretRequirements,
      required_actions: [
        ...secretActions,
        cloudActionFromError(error, "operator"),
      ],
      checks,
    };
  }

  return {
    status: "cloud_runnable",
    target: { kind: "hosted_cloud", name: "default" },
    runtime_options: input.runtimeOptions,
    package_digest: input.artifact.package_digest,
    package_provenance: input.artifact.package_provenance,
    package_status: input.artifact.status,
    run_requirements: runRequirements,
    secret_requirements: secretRequirements,
    required_actions: secretActions,
    checks,
  };
}

function secretRequirementAction(requirement: ReturnType<typeof packageSecretRequirements>[number]): HostedCloudAction {
  return {
    action: "upload_hosted_secret",
    stage: "before_run",
    requirement_id: requirement.id,
    description: `Upload hosted secret '${requirement.id}' before creating a run, then pass --secret-ref ${requirement.id}=bucephalus://${requirement.id}.`,
    command: `buc secrets put ${requirement.id} --from-env ${requirement.id}`,
    blocking: false,
  };
}

function cloudActionFromError(error: unknown, stage: HostedCloudAction["stage"]): HostedCloudAction {
  if (error instanceof HttpError) {
    return {
      action: error.code,
      stage,
      description: error.message,
      blocking: true,
    };
  }
  return {
    action: "hosted_cloud_check_failed",
    stage,
    description: error instanceof Error ? error.message : String(error),
    blocking: true,
  };
}

function checkFromError(name: string, error: unknown): HostedCloudCheck {
  if (error instanceof HttpError) {
    const check: HostedCloudCheck = {
      name,
      status: "blocked",
      code: error.code,
      message: error.message,
    };
    if (error.detail) {
      check.detail = error.detail;
    }
    return check;
  }
  return {
    name,
    status: "blocked",
    code: "hosted_cloud_check_failed",
    message: error instanceof Error ? error.message : String(error),
  };
}

function withBuildEnvironmentEvidence(
  readiness: HostedCloudReadiness,
  environment: HostedBuildEnvironment,
): HostedCloudReadiness {
  const evidenceEnforced = environment.evidence.policy === "enforce";
  const evidencePartial = environment.evidence.status === "partial";
  const evidenceChecks = environment.evidence.checks.map((check) => ({
    name: "build_environment",
    status: evidenceEnforced && check.status === "warning" ? "blocked" as const : check.status,
    code: check.code,
    message: evidenceEnforced && check.status === "warning"
      ? `${check.message} Build evidence policy is enforce, so this build is not considered runnable in hosted Cloud.`
      : check.message,
    detail: {
      evidence: check.name,
      evidence_policy: environment.evidence.policy,
      evidence_status: environment.evidence.status,
      missing_evidence: environment.evidence.missing,
    },
  }));
  const evidenceAction: HostedCloudAction[] = evidenceEnforced && evidencePartial && readiness.status !== "unavailable"
    ? [{
      action: "complete_build_environment_evidence",
      stage: "operator",
      description: `Complete hosted build environment evidence before declaring this package runnable: ${environment.evidence.missing.join(", ")}.`,
      blocking: true,
    }]
    : [];
  return {
    ...readiness,
    status: evidenceEnforced && evidencePartial && readiness.status === "cloud_runnable"
      ? "cloud_blocked"
      : readiness.status,
    required_actions: [
      ...evidenceAction,
      ...readiness.required_actions,
    ],
    checks: [
      ...evidenceChecks,
      ...readiness.checks,
    ],
  };
}

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { chmod, mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { AuthContext } from "../src/auth";
import { createSql, type Sql } from "../src/db/client";
import { runMigrations } from "../src/db/migrate";
import { errorResponse, jsonResponse } from "../src/http";
import { ImportRepository } from "../src/imports/repository";
import { LatchSubmissionRepository } from "../src/latch/repository";
import { PackageRepository, RunRepository } from "../src/packages/repository";
import { canonicalJsonStringify, sha256Digest, type JsonObject } from "../src/primitives";
import { RegistryRepository } from "../src/registry/repository";
import { RuntimeRepository } from "../src/runtime/repository";
import { handleDraftRoute } from "../src/routes/drafts";
import { handleExperimentRoute } from "../src/routes/experiments";
import { handleImportRoute } from "../src/routes/imports";
import { handleLatchRoute } from "../src/routes/latch";
import { handleRegistryRoute } from "../src/routes/registry";
import { handleRunRoute } from "../src/routes/runs";
import { handleSecretRoute } from "../src/routes/secrets";
import { RunnerRepository } from "../src/runners/repository";
import { CloudSecretRepository } from "../src/secrets/repository";
import { FilesystemSecretStoreBackend, SecretStore } from "../src/secrets/store";

const runSmoke = process.env.BUCEPHALUS_CLOUD_HTTP_WORKFLOW_SMOKE === "1";
const describeSmoke = runSmoke ? describe : describe.skip;
const smokeToken = "smoke-user-token";
const workerToken = "smoke-worker-token";
const repoRoot = resolve(import.meta.dir, "../..");

let previousEnv: Record<string, string | undefined> = {};

beforeEach(() => {
  previousEnv = {
    BUCEPHALUS_CLOUD_DATA_DIR: process.env.BUCEPHALUS_CLOUD_DATA_DIR,
    BUCEPHALUS_CLOUD_CORE_CLI: process.env.BUCEPHALUS_CLOUD_CORE_CLI,
    BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS: process.env.BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS,
    BUC_SMOKE_SECRET: process.env.BUC_SMOKE_SECRET,
    DATABASE_URL: process.env.DATABASE_URL,
  };
});

afterEach(() => {
  for (const [key, value] of Object.entries(previousEnv)) {
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
});

describeSmoke("buc hosted workflow smoke", () => {
  test("drives build, doctor, run, and readback through HTTP API routes", async () => {
    const baseDatabaseUrl = requireDatabaseUrl();
    const root = await mkdtemp(join(tmpdir(), "buc-hosted-http-smoke-"));
    const scratch = await createScratchDatabase(baseDatabaseUrl);
    let sql: Sql | null = null;
    let server: ReturnType<typeof Bun.serve> | null = null;

    try {
      process.env.DATABASE_URL = scratch.databaseUrl;
      process.env.BUCEPHALUS_CLOUD_DATA_DIR = join(root, "cloud-data");
      process.env.BUC_SMOKE_SECRET = "smoke-secret-value";
      await runMigrations({ databaseUrl: scratch.databaseUrl, runtimeRoleName: null });
      sql = createSql(scratch.databaseUrl);
      const repositories = await createRepositories(sql);
      const runnerInstanceId = await seedRunnerPool(repositories.runners);

      server = serveHarness(repositories);
      const apiUrl = `http://${server.hostname}:${server.port}`;
      const { packageDir, packageDigest } = await writeCloudRunnablePackage(root);
      const { experimentYaml } = await writeHostedAuthoringContext(root);
      process.env.BUCEPHALUS_CLOUD_CORE_CLI = await writeFakeHostedCoreBuilder(root, packageDir);
      process.env.BUCEPHALUS_CLOUD_AUTHORING_BUILD_TIMEOUT_MS = "60000";
      const buc = resolve(process.env.BUC_BINARY ?? join(repoRoot, "target", "debug", "buc"));

      const build = await runBucJson(buc, apiUrl, [
        "build",
        experimentYaml,
        "--json",
      ]);
      expect(build.package_digest).toBe(packageDigest);
      expect(build.status).toBe("cloud_runnable");
      expect(build.build_kind).toBe("hosted_authoring_build");
      expect(pointer(build, "/authoring_build/status")).toBe("succeeded");
      expect(pointer(build, "/authoring_build/entrypoint")).toBe("experiments/peter/experiment.yaml");
      expect(pointer(build, "/authoring_build/source_upload_id")).toBe(pointer(build, "/build_environment/source/upload_id"));
      expect(pointer(build, "/build_environment/source/input_kind")).toBe("authoring_context");
      expect(pointer(build, "/build_environment/source/entrypoint")).toBe("experiments/peter/experiment.yaml");
      expect(pointer(build, "/build_environment/package_contract/authoring_compiler")).toBe("core_universal_v1");
      expect(pointer(build, "/build_environment/package_contract/authoring_provenance/status")).toBe("hosted_attested");
      expect(pointer(build, "/build_environment/package_contract/authoring_provenance/source")).toBe("hosted_core");
      expect(pointer(build, "/cloud_readiness/status")).toBe("cloud_runnable");
      expect(pointer(build, "/build_environment/package_contract/cloud_readiness_required")).toBe(true);
      expect(pointer(build, "/cloud_readiness/run_requirements/requires")).toEqual([
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "secret_resolver",
      ]);
      expect(pointer(build, "/cloud_readiness/secret_requirements/0/id")).toBe("GEMINI_API_KEY");

      const putSecret = await runBucJson(buc, apiUrl, [
        "secrets",
        "put",
        "GEMINI_API_KEY",
        "--from-env",
        "BUC_SMOKE_SECRET",
        "--json",
      ]);
      expect(putSecret.name).toBe("GEMINI_API_KEY");
      expect(JSON.stringify(putSecret)).not.toContain("smoke-secret-value");

      const listedSecrets = await runBucJson(buc, apiUrl, ["secrets", "list", "--json"]);
      expect(pointer(listedSecrets, "/secrets/0/name")).toBe("GEMINI_API_KEY");
      expect(JSON.stringify(listedSecrets)).not.toContain("smoke-secret-value");

      const secretArgs = ["--secret-ref", "GEMINI_API_KEY=bucephalus://GEMINI_API_KEY"];
      const doctor = await runBucJson(buc, apiUrl, ["doctor", packageDigest, ...secretArgs, "--json"]);
      expect(doctor.status).toBe("runnable");
      expect(pointer(doctor, "/run_requirements/executor")).toBe("runner-docker");
      expect(pointer(doctor, "/package_provenance/status")).toBe("hosted_attested");
      expect(pointer(doctor, "/package_provenance/source")).toBe("hosted_core");
      expect(pointer(doctor, "/supplied_secret_ids")).toEqual(["GEMINI_API_KEY"]);

      const run = await runBucJson(buc, apiUrl, ["run", packageDigest, ...secretArgs, "--label", "smoke", "--json"]);
      expect(typeof run.run_id).toBe("string");
      expect(run.package_digest).toBe(packageDigest);
      expect(run.run_label).toBe("smoke");
      expect(pointer(run, "/package_provenance/status")).toBe("hosted_attested");
      expect(pointer(run, "/package_provenance/source")).toBe("hosted_core");
      expect(pointer(run, "/run_requirements/requires")).toEqual([
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "secret_resolver",
      ]);
      expect(pointer(run, "/secret_ids")).toEqual(["GEMINI_API_KEY"]);
      expect(JSON.stringify(run)).not.toContain("smoke-secret-value");

      const claim = await workerJson(apiUrl, "/v1/worker/runs/claim", workerToken, {
        runner_instance_id: runnerInstanceId,
        lease_seconds: 60,
      });
      expect(claim.claimed).toBe(true);
      expect(pointer(claim, "/run/run_id")).toBe(run.run_id);
      expect(pointer(claim, "/run/package_digest")).toBe(packageDigest);
      expect(String(pointer(claim, "/run/secret_refs/GEMINI_API_KEY"))).toMatch(/^file:/);
      expect(JSON.stringify(claim)).not.toContain("smoke-secret-value");
      const attemptId = requireJsonString(pointer(claim, "/attempt/attempt_id"), "/attempt/attempt_id");
      const attemptToken = requireJsonString(pointer(claim, "/attempt/attempt_token"), "/attempt/attempt_token");

      const content = await workerPackageContent(apiUrl, packageDigest, attemptId, attemptToken);
      expect(content.byteLength).toBeGreaterThan(0);

      const coreRunId = "run_buc_hosted_workflow_smoke";
      const lifecycle = await workerJson(
        apiUrl,
        `/v1/worker/run-attempts/${encodeURIComponent(attemptId)}/events`,
        attemptToken,
        {
          runner_instance_id: runnerInstanceId,
          event_type: "worker.core.started",
          payload: {
            core_run_ids: [coreRunId],
            run_root_dir: "/workspace/.bucephalus/runs",
          },
        },
      );
      expect(pointer(lifecycle, "/event/event_type")).toBe("worker.core.started");

      const ingested = await workerJson(
        apiUrl,
        `/v1/worker/run-attempts/${encodeURIComponent(attemptId)}/runtime/event-rows`,
        attemptToken,
        {
          runner_instance_id: runnerInstanceId,
          rows: [{
            core_run_id: coreRunId,
            trial_id: "trial-smoke-1",
            schedule_idx: 0,
            attempt: 0,
            row_seq: 1,
            slot_commit_id: "slot-smoke-1",
            variant_id: "baseline",
            task_id: "case-smoke-1",
            repl_idx: 0,
            seq: 1,
            event_type: "metric.observed",
            ts: "2026-06-12T00:00:00.000Z",
            payload: {
              metric_name: "ok",
              metric_value: 1,
              source: "buc-hosted-workflow-smoke",
            },
          }],
        },
      );
      expect(ingested.received).toBe(1);
      expect(ingested.upserted).toBe(1);

      const completed = await workerJson(
        apiUrl,
        `/v1/worker/run-attempts/${encodeURIComponent(attemptId)}/complete`,
        attemptToken,
        { runner_instance_id: runnerInstanceId },
      );
      expect(pointer(completed, "/run/status")).toBe("completed");
      expect(pointer(completed, "/attempt/status")).toBe("completed");

      const fetchedRun = await runBucJson(buc, apiUrl, ["runs", "get", String(run.run_id), "--json"]);
      expect(fetchedRun.run_id).toBe(run.run_id);
      expect(fetchedRun.package_digest).toBe(packageDigest);
      expect(fetchedRun.status).toBe("completed");
      expect(pointer(fetchedRun, "/secret_refs")).toBeUndefined();
      expect(pointer(fetchedRun, "/secret_ids")).toEqual(["GEMINI_API_KEY"]);
      const attempts = requireJsonArray(pointer(fetchedRun, "/attempts"), "/attempts");
      expect(pointer(attempts[0], "/status")).toBe("completed");

      const events = await runBucJson(buc, apiUrl, ["runs", "events", String(run.run_id), "--limit", "20", "--json"]);
      const eventStream = requireJsonArray(pointer(events, "/events"), "/events");
      expect(eventStream.some((event) =>
        pointer(event, "/source") === "worker"
          && pointer(event, "/event_type") === "worker.core.started"
      )).toBe(true);
      expect(eventStream.some((event) =>
        pointer(event, "/source") === "trial"
          && pointer(event, "/core_run_id") === coreRunId
          && pointer(event, "/event_type") === "metric.observed"
          && pointer(event, "/payload/metric_value") === 1
      )).toBe(true);

      const runtime = await runBucJson(buc, apiUrl, ["runs", "runtime", String(run.run_id), "--json"]);
      expect(requireJsonArray(pointer(runtime, "/core_run_ids"), "/core_run_ids")).toContain(coreRunId);
      expect(requireJsonArray(pointer(runtime, "/recent_events"), "/recent_events").some((event) =>
        pointer(event, "/event_type") === "metric.observed"
      )).toBe(true);

      const results = await runBucJson(buc, apiUrl, ["runs", "results", String(run.run_id), "--json"]);
      expect(requireJsonArray(pointer(results, "/core_run_ids"), "/core_run_ids")).toContain(coreRunId);

      const inspectedPackage = await runBucJson(buc, apiUrl, ["packages", "inspect", packageDigest, "--json"]);
      expect(inspectedPackage.package_digest).toBe(packageDigest);
      expect(inspectedPackage.name).toBe("Hosted Workflow Smoke");
      expect(pointer(inspectedPackage, "/package_provenance/status")).toBe("hosted_attested");
      expect(pointer(inspectedPackage, "/package_provenance/source")).toBe("hosted_core");
    } finally {
      server?.stop(true);
      if (sql) {
        await sql.end();
      }
      await dropScratchDatabase(scratch.adminSql, scratch.databaseName);
      await scratch.adminSql.end();
      await rm(root, { recursive: true, force: true });
    }
  }, 120_000);
});

async function createRepositories(sql: Sql) {
  const registry = new RegistryRepository(sql);
  const imports = new ImportRepository(sql);
  const latchSubmissions = new LatchSubmissionRepository(sql);
  const packages = new PackageRepository(sql);
  const runs = new RunRepository(sql);
  const runtime = new RuntimeRepository(sql);
  const runners = new RunnerRepository(sql);
  const secrets = new CloudSecretRepository(sql);
  return { registry, imports, latchSubmissions, packages, runs, runtime, runners, secrets };
}

function serveHarness(repositories: Awaited<ReturnType<typeof createRepositories>>): ReturnType<typeof Bun.serve> {
  const secretStore = new SecretStore(
    new FilesystemSecretStoreBackend(process.env.BUCEPHALUS_CLOUD_DATA_DIR ?? "."),
    "buc-smoke",
  );
  return Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      const url = new URL(request.url);
      try {
        if (request.method === "GET" && url.pathname === "/healthz") {
          return jsonResponse({ ok: true });
        }
        if (request.method === "GET" && url.pathname === "/readyz") {
          return jsonResponse({ ok: true, database: "ok" });
        }

        const auth = requiresSmokeUserAuth(url.pathname) ? authenticatedSmokeUser(request) : null;
        const registryResponse = await handleRegistryRoute(request, url, repositories.registry);
        if (registryResponse) {
          return registryResponse;
        }
        const draftResponse = await handleDraftRoute(request, url, repositories.registry);
        if (draftResponse) {
          return draftResponse;
        }
        const experimentResponse = await handleExperimentRoute(
          request,
          url,
          repositories.imports,
          repositories.packages,
          repositories.runs,
          repositories.runners,
          auth,
          repositories.secrets,
        );
        if (experimentResponse) {
          return experimentResponse;
        }
        const importResponse = await handleImportRoute(
          request,
          url,
          repositories.imports,
          repositories.packages,
          auth,
        );
        if (importResponse) {
          return importResponse;
        }
        const latchResponse = await handleLatchRoute(
          request,
          url,
          repositories.registry,
          repositories.latchSubmissions,
          auth,
        );
        if (latchResponse) {
          return latchResponse;
        }
        const secretResponse = await handleSecretRoute(
          request,
          url,
          repositories.secrets,
          secretStore,
          auth,
        );
        if (secretResponse) {
          return secretResponse;
        }
        const runResponse = await handleRunRoute(
          request,
          url,
          repositories.packages,
          repositories.runs,
          repositories.runtime,
          repositories.runners,
          workerToken,
          auth,
          repositories.secrets,
        );
        if (runResponse) {
          return runResponse;
        }
        return jsonResponse({ code: "not_found", message: "Route not found" }, { status: 404 });
      } catch (error) {
        return errorResponse(error);
      }
    },
  });
}

function requiresSmokeUserAuth(pathname: string): boolean {
  if (pathname.startsWith("/v1/worker/") || pathname.startsWith("/v1/runner-")) {
    return false;
  }
  if (pathname.startsWith("/v1/packages/") && pathname.endsWith("/content")) {
    return false;
  }
  return pathname.startsWith("/v1/");
}

function authenticatedSmokeUser(request: Request): AuthContext {
  if (request.headers.get("authorization") !== `Bearer ${smokeToken}`) {
    throw new Error("Bucephalus Cloud smoke requires the fixed smoke bearer token");
  }
  return {
    subject: "smoke-user",
    issuer: "smoke",
    audience: "buc-hosted-workflow-smoke",
    claims: { sub: "smoke-user" },
  };
}

async function seedRunnerPool(runners: RunnerRepository): Promise<string> {
  const capabilities = {
    executors: ["runner-docker"],
    resources: ["core_runner", "docker_daemon", "registry_pull", "secret_resolver"],
    arch: "x86_64",
    cpu_count: 4,
    memory_mb: 8192,
    disk_mb: 65536,
    isolation: ["reusable_vm"],
  };
  const pool = await runners.createPool({
    name: "smoke-docker",
    capabilities,
    metadata: { source: "buc-hosted-workflow-smoke" },
  });
  await runners.promoteWorkerImage({
    poolId: pool.runner_pool_id,
    imageRef: "us-central1-docker.pkg.dev/acme/buc/worker@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    registryHost: "us-central1-docker.pkg.dev",
    repository: "acme/buc/worker",
    digest: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    releaseVersion: "smoke",
    releaseGitSha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    boundaryVerifiedAt: new Date().toISOString(),
    metadata: { source: "buc-hosted-workflow-smoke" },
  });
  const instance = await runners.registerInstance({
    runnerPoolId: pool.runner_pool_id,
    instanceName: "smoke-runner-1",
    capabilities,
    metadata: { source: "buc-hosted-workflow-smoke" },
  });
  return instance.runner_instance_id;
}

async function runBucJson(buc: string, apiUrl: string, args: string[]): Promise<JsonObject> {
  const result = await runCommand(buc, [
    "--api-url",
    apiUrl,
    "--user-token",
    smokeToken,
    ...args,
  ]);
  try {
    return JSON.parse(result.stdout) as JsonObject;
  } catch (error) {
    throw new Error(`buc ${args.join(" ")} did not return JSON: ${error}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  }
}

async function runCommand(command: string, args: string[]): Promise<{ stdout: string; stderr: string }> {
  return await new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      env: {
        ...process.env,
        BUCEPHALUS_CLOUD_API_URL: "",
        BUCEPHALUS_CLOUD_USER_TOKEN: "",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      const output = {
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      };
      if (code === 0) {
        resolvePromise(output);
      } else {
        reject(new Error(`command failed (${code}): ${command} ${args.join(" ")}\nstdout:\n${output.stdout}\nstderr:\n${output.stderr}`));
      }
    });
  });
}

async function workerJson(apiUrl: string, path: string, token: string, body: JsonObject): Promise<JsonObject> {
  const response = await fetch(`${apiUrl}${path}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`worker request failed (${response.status}): ${path}\n${text}`);
  }
  try {
    return JSON.parse(text) as JsonObject;
  } catch (error) {
    throw new Error(`worker request did not return JSON: ${path}: ${error}\n${text}`);
  }
}

async function workerPackageContent(
  apiUrl: string,
  packageDigest: string,
  attemptId: string,
  attemptToken: string,
): Promise<Uint8Array> {
  const response = await fetch(`${apiUrl}/v1/packages/${encodeURIComponent(packageDigest)}/content`, {
    headers: {
      authorization: `Bearer ${attemptToken}`,
      "x-bucephalus-attempt-id": attemptId,
    },
  });
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (!response.ok) {
    throw new Error(`package content download failed (${response.status}): ${new TextDecoder().decode(bytes)}`);
  }
  expect(response.headers.get("x-bucephalus-package-digest")).toBe(packageDigest);
  return bytes;
}

async function writeCloudRunnablePackage(root: string): Promise<{ packageDir: string; packageDigest: string }> {
  const packageDir = join(root, "package");
  await mkdir(packageDir, { recursive: true });
  const resolvedExperiment = {
    experiment: {
      id: "hosted_workflow_smoke",
      name: "Hosted Workflow Smoke",
    },
    runtime: {
      compute: { backend: "local-docker" },
      secrets: [{ name: "GEMINI_API_KEY", from: "env" }],
    },
    trial_runtime: {
      agent: {
        image: "us-central1-docker.pkg.dev/acme/buc/agent@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      },
    },
    matrix: {
      variants: [{ id: "baseline", baseline: true }],
      cases: { count: 1 },
      repeats: 1,
      seeds: [1],
    },
    metrics: [{ id: "ok", direction: "maximize", primary: true }],
  };
  await writeFile(join(packageDir, "resolved_experiment.json"), JSON.stringify(resolvedExperiment));
  await writeFile(join(packageDir, "staging_manifest.json"), JSON.stringify({
    schema_version: "runtime_path_staging_manifest_v1",
    variants: { baseline: [] },
  }));
  const checksums = await checksumsForPackage(packageDir);
  const packageDigest = sha256Digest(canonicalJsonStringify(checksums.files));
  await writeFile(join(packageDir, "checksums.json"), JSON.stringify(checksums));
  await writeFile(join(packageDir, "package.lock"), JSON.stringify({
    schema_version: "sealed_package_lock_v1",
    package_digest: packageDigest,
  }));
  await writeFile(join(packageDir, "package_checks.json"), JSON.stringify({
    schema_version: "package_checks_v1",
    package_digest: packageDigest,
    passed: true,
    checks: [],
    summary: { checks: 0, failed: 0, warnings: 0 },
  }));
  await writeFile(join(packageDir, "manifest.json"), JSON.stringify({
    schema_version: "sealed_run_package_v2",
    created_at: "2026-06-12T00:00:00Z",
    resolved_experiment: resolvedExperiment,
    checksums_ref: "checksums.json",
    package_checks_ref: "package_checks.json",
    package_digest: packageDigest,
  }));
  return { packageDir, packageDigest };
}

async function writeHostedAuthoringContext(root: string): Promise<{ contextRoot: string; experimentYaml: string }> {
  const contextRoot = join(root, "authoring-context");
  const experimentDir = join(contextRoot, "experiments", "peter");
  const sharedDir = join(contextRoot, "shared");
  await mkdir(experimentDir, { recursive: true });
  await mkdir(sharedDir, { recursive: true });
  const experimentYaml = join(experimentDir, "experiment.yaml");
  await writeFile(experimentYaml, [
    "experiment:",
    "  id: hosted_workflow_smoke",
    "  name: Hosted Workflow Smoke",
    "runtime:",
    "  compute:",
    "    backend: local-docker",
    "matrix:",
    "  variants:",
    "    - id: baseline",
    "      baseline: true",
    "  cases:",
    "    count: 1",
    "  repeats: 1",
    "  seeds: [1]",
    "metrics:",
    "  - id: ok",
    "    direction: maximize",
    "    primary: true",
    "",
  ].join("\n"));
  await writeFile(join(sharedDir, "cases.jsonl"), "{\"id\":\"case-smoke-1\"}\n");
  await writeFile(join(contextRoot, "bucephalus.project.yaml"), [
    "schema_version: bucephalus_project_v1",
    "project:",
    "  id: hosted_workflow_smoke",
    "package_sources:",
    "  default:",
    "    root: .",
    "    entrypoints:",
    "      - experiments/peter/experiment.yaml",
    "    include:",
    "      - experiments/peter/**",
    "      - shared/**",
    "targets:",
    "  hosted_cloud: {}",
    "",
  ].join("\n"));
  await writeFile(join(contextRoot, ".env"), "SHOULD_NOT_UPLOAD=1\n");
  await writeFile(join(contextRoot, ".npmrc"), "//registry.example/:_authToken=SHOULD_NOT_UPLOAD\n");
  await mkdir(join(contextRoot, ".ssh"), { recursive: true });
  await writeFile(join(contextRoot, ".ssh/id_ed25519"), "SHOULD_NOT_UPLOAD\n");
  await mkdir(join(contextRoot, ".aws"), { recursive: true });
  await writeFile(join(contextRoot, ".aws/credentials"), "aws_secret_access_key=SHOULD_NOT_UPLOAD\n");
  await mkdir(join(contextRoot, "node_modules/pkg"), { recursive: true });
  await writeFile(join(contextRoot, "node_modules/pkg/index.js"), "SHOULD_NOT_UPLOAD\n");
  await mkdir(join(contextRoot, "target/debug"), { recursive: true });
  await writeFile(join(contextRoot, "target/debug/blob"), "SHOULD_NOT_UPLOAD\n");
  return { contextRoot, experimentYaml };
}

async function writeFakeHostedCoreBuilder(root: string, packageDir: string): Promise<string> {
  const corePath = join(root, "fake-hosted-core.sh");
  await writeFile(corePath, [
    "#!/bin/sh",
    "set -eu",
    "entrypoint=\"\"",
    "out=\"\"",
    "while [ \"$#\" -gt 0 ]; do",
    "  case \"$1\" in",
    "    build) shift ;;",
    "    --out) out=\"$2\"; shift 2 ;;",
    "    --json) shift ;;",
    "    *) if [ -z \"$entrypoint\" ]; then entrypoint=\"$1\"; fi; shift ;;",
    "  esac",
    "done",
    "if [ \"$entrypoint\" != \"experiments/peter/experiment.yaml\" ]; then echo \"wrong entrypoint: $entrypoint\" >&2; exit 64; fi",
    "if [ ! -f \"$entrypoint\" ]; then echo \"entrypoint missing in hosted context\" >&2; exit 65; fi",
    "if [ ! -f \"bucephalus.project.yaml\" ]; then echo \"project manifest missing in hosted context\" >&2; exit 76; fi",
    "if [ ! -f \"shared/cases.jsonl\" ]; then echo \"shared context file missing\" >&2; exit 66; fi",
    "if [ -f \".env\" ]; then echo \".env leaked into hosted context\" >&2; exit 67; fi",
    "if [ -f \".npmrc\" ]; then echo \".npmrc leaked into hosted context\" >&2; exit 68; fi",
    "if [ -d \".ssh\" ]; then echo \".ssh leaked into hosted context\" >&2; exit 69; fi",
    "if [ -d \".aws\" ]; then echo \".aws leaked into hosted context\" >&2; exit 70; fi",
    "if [ -d \"node_modules\" ]; then echo \"node_modules leaked into hosted context\" >&2; exit 71; fi",
    "if [ -d \"target\" ]; then echo \"target leaked into hosted context\" >&2; exit 72; fi",
    "if [ -n \"${DATABASE_URL:-}\" ]; then echo \"DATABASE_URL leaked into hosted Core\" >&2; exit 73; fi",
    "if [ -n \"${BUCEPHALUS_CLOUD_WORKER_TOKEN:-}\" ]; then echo \"worker token leaked into hosted Core\" >&2; exit 74; fi",
    "if [ -z \"$out\" ]; then echo \"missing --out\" >&2; exit 75; fi",
    "mkdir -p \"$out\"",
    `cp -R ${shellSingleQuote(`${packageDir}/.`)} "$out/"`,
    "echo '{\"ok\":true}'",
    "",
  ].join("\n"));
  await chmod(corePath, 0o755);
  return corePath;
}

function shellSingleQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

async function checksumsForPackage(packageDir: string): Promise<{ schema_version: string; files: Record<string, string> }> {
  const files: Record<string, string> = {};
  async function visit(relDir: string): Promise<void> {
    const dir = relDir ? join(packageDir, relDir) : packageDir;
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const rel = relDir ? `${relDir}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        await visit(rel);
      } else if (entry.isFile() && !["manifest.json", "checksums.json", "package.lock"].includes(rel)) {
        files[rel] = sha256Digest(await readFile(join(packageDir, rel)));
      }
    }
  }
  await visit("");
  return {
    schema_version: "sealed_package_checksums_v2",
    files: Object.fromEntries(Object.entries(files).sort(([left], [right]) => left.localeCompare(right))),
  };
}

function pointer(root: unknown, pointerPath: string): unknown {
  return pointerPath
    .split("/")
    .slice(1)
    .map((token) => token.replaceAll("~1", "/").replaceAll("~0", "~"))
    .reduce<unknown>((value, token) => {
      if (Array.isArray(value)) {
        return /^\d+$/.test(token) ? value[Number.parseInt(token, 10)] : undefined;
      }
      if (typeof value !== "object" || value === null) {
        return undefined;
      }
      return (value as Record<string, unknown>)[token];
    }, root);
}

function requireJsonString(value: unknown, pointerPath: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${pointerPath} must be a non-empty string`);
  }
  return value;
}

function requireJsonArray(value: unknown, pointerPath: string): unknown[] {
  if (!Array.isArray(value)) {
    throw new Error(`${pointerPath} must be an array`);
  }
  return value;
}

function requireDatabaseUrl(): string {
  const databaseUrl = process.env.BUCEPHALUS_CLOUD_HTTP_WORKFLOW_DATABASE_URL
    ?? process.env.DATABASE_URL
    ?? null;
  if (!databaseUrl) {
    throw new Error("BUCEPHALUS_CLOUD_HTTP_WORKFLOW_SMOKE requires DATABASE_URL or BUCEPHALUS_CLOUD_HTTP_WORKFLOW_DATABASE_URL");
  }
  requireSafeDatabaseServer(databaseUrl);
  return databaseUrl;
}

function requireSafeDatabaseServer(databaseUrl: string): void {
  const parsed = new URL(databaseUrl);
  if (parsed.protocol !== "postgres:" && parsed.protocol !== "postgresql:") {
    throw new Error(`Hosted workflow smoke requires a postgres URL, got: ${parsed.protocol}`);
  }
  const host = parsed.hostname.toLowerCase();
  const isLocal = host === "localhost" || host === "127.0.0.1" || host === "::1";
  if (!isLocal && process.env.BUCEPHALUS_ALLOW_REMOTE_MIGRATION_TESTS !== "true") {
    throw new Error("Refusing to create a hosted workflow smoke database on a non-local host.");
  }
}

function databaseUrlFor(databaseUrl: string, databaseName: string): string {
  const parsed = new URL(databaseUrl);
  parsed.pathname = `/${databaseName}`;
  return parsed.toString();
}

async function createScratchDatabase(baseUrl: string): Promise<{ databaseName: string; databaseUrl: string; adminSql: Sql }> {
  const adminSql = createSql(databaseUrlFor(baseUrl, "postgres"));
  const databaseName = `bucephalus_workflow_smoke_${Date.now()}_${Math.random().toString(16).slice(2)}`;
  try {
    await adminSql.unsafe(`create database ${quoteIdentifier(databaseName)}`);
    return {
      databaseName,
      databaseUrl: databaseUrlFor(baseUrl, databaseName),
      adminSql,
    };
  } catch (error) {
    await adminSql.end();
    throw error;
  }
}

async function dropScratchDatabase(adminSql: Sql, databaseName: string): Promise<void> {
  await adminSql.unsafe(`drop database if exists ${quoteIdentifier(databaseName)} with (force)`);
}

function quoteIdentifier(value: string): string {
  if (!/^[a-z_][a-z0-9_]*$/.test(value)) {
    throw new Error(`Unsafe database identifier: ${value}`);
  }
  return `"${value}"`;
}

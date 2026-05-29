#!/usr/bin/env bun
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";
import { createHash } from "node:crypto";
import YAML from "yaml";

type JsonObject = Record<string, unknown>;

interface CliContext {
  apiUrl: string;
  workerToken: string | null;
  args: string[];
}

class CliError extends Error {
  constructor(
    message: string,
    public readonly exitCode = 1,
  ) {
    super(message);
    this.name = "CliError";
  }
}

async function main(argv: string[]): Promise<void> {
  const context = parseGlobalArgs(argv);
  const [group, command, ...rest] = context.args;
  if (!group || group === "help" || group === "--help" || group === "-h") {
    printHelp();
    return;
  }

  if (group === "health") {
    printJson(await cloudFetch(context, "/readyz"));
    return;
  }

  if (group === "registry" && command === "search") {
    await registrySearch({ ...context, args: rest });
    return;
  }

  if (group === "draft" && command === "validate") {
    await draftValidate({ ...context, args: rest });
    return;
  }

  if (group === "draft" && command === "preview") {
    await draftPreview({ ...context, args: rest });
    return;
  }

  if (group === "draft" && command === "export") {
    await draftExport({ ...context, args: rest });
    return;
  }

  if (group === "import" && command === "sealed-package") {
    await importSealedPackage({ ...context, args: rest });
    return;
  }

  if (group === "import" && command === "inspect") {
    await importInspect({ ...context, args: rest });
    return;
  }

  if (group === "package" && command === "get") {
    await packageGet({ ...context, args: rest });
    return;
  }

  if (group === "run" && command === "create") {
    await runCreate({ ...context, args: rest });
    return;
  }

  if (group === "run" && command === "get") {
    await runGet({ ...context, args: rest });
    return;
  }

  if (group === "runner-pool" && command === "create") {
    await runnerPoolCreate({ ...context, args: rest });
    return;
  }

  if (group === "runner-pool" && command === "list") {
    await runnerPoolList({ ...context, args: rest });
    return;
  }

  if (group === "runner-instance" && command === "drain") {
    await runnerInstanceDrain({ ...context, args: rest });
    return;
  }

  throw new CliError(`unknown command: ${[group, command].filter(Boolean).join(" ")}`);
}

function parseGlobalArgs(argv: string[]): CliContext {
  const args = [...argv];
  let apiUrl = process.env.BUCEPHALUS_CLOUD_API_URL ?? "http://localhost:8099";
  let workerToken = process.env.BUCEPHALUS_CLOUD_WORKER_TOKEN ?? null;
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] !== "--api-url" && args[index] !== "--worker-token") {
      continue;
    }
    const option = args[index];
    const value = args[index + 1];
    if (!value) {
      throw new CliError(`${option} requires a value`);
    }
    if (option === "--api-url") {
      apiUrl = value;
    } else {
      workerToken = value;
    }
    args.splice(index, 2);
    index -= 1;
  }
  return { apiUrl: apiUrl.replace(/\/+$/, ""), workerToken: workerToken?.trim() || null, args };
}

async function registrySearch(context: CliContext): Promise<void> {
  const kind = optionValue(context.args, "--kind");
  const query = optionValue(context.args, "--query") ?? optionValue(context.args, "-q") ?? context.args[0];
  if (!query) {
    throw new CliError("registry search requires --query <text>");
  }
  const params = new URLSearchParams({ q: query });
  if (kind) {
    params.set("kind", kind);
  }
  printJson(await cloudFetch(context, `/v1/registry/search?${params.toString()}`));
}

async function draftValidate(context: CliContext): Promise<void> {
  const draft = await readDraftFromOptions(context.args);
  printJson(await cloudFetch(context, "/v1/drafts/validate", { method: "POST", body: { draft } }));
}

async function draftPreview(context: CliContext): Promise<void> {
  const draft = await readDraftFromOptions(context.args);
  printJson(await cloudFetch(context, "/v1/drafts/preview-schedule", { method: "POST", body: { draft } }));
}

async function draftExport(context: CliContext): Promise<void> {
  const draftPath = requiredOption(context.args, "--file");
  const outDir = requiredOption(context.args, "--out");
  const format = optionValue(context.args, "--format") ?? "yaml";
  const draft = await readDraftFile(draftPath);
  const response = await cloudFetch(context, "/v1/drafts/export", {
    method: "POST",
    body: { draft, format },
  });
  if (!isObject(response) || typeof response.body !== "string") {
    throw new CliError("draft export response did not include a string body");
  }

  await mkdir(outDir, { recursive: true });
  const filename = format === "resolved_json" ? "resolved_experiment.json" : "experiment.yaml";
  const target = join(outDir, filename);
  await writeFile(target, response.body);
  printJson({
    exported: target,
    source: basename(draftPath),
    format: response.format ?? format,
    issues: response.issues ?? [],
  });
}

async function importSealedPackage(context: CliContext): Promise<void> {
  const path = context.args[0] ?? requiredOption(context.args, "--file");
  const label = optionValue(context.args, "--label");
  const bytes = new Uint8Array(await readFile(path));
  const expectedDigest = sha256Digest(bytes);

  const upload = await cloudFetch(context, "/v1/uploads", {
    method: "POST",
    body: {
      filename: basename(path),
      media_type: mediaTypeForPath(path),
      expected_digest: expectedDigest,
      byte_size: bytes.byteLength,
    },
  });
  if (!isObject(upload) || typeof upload.upload_id !== "string") {
    throw new CliError("upload response did not include upload_id");
  }

  const uploadId = upload.upload_id;
  await cloudFetch(context, `/v1/uploads/${uploadId}/content`, {
    method: "PUT",
    rawBody: bytes,
    contentType: "application/octet-stream",
  });
  await cloudFetch(context, `/v1/uploads/${uploadId}/complete`, {
    method: "POST",
    body: {},
  });
  const imported = await cloudFetch(context, "/v1/imports/sealed-package", {
    method: "POST",
    body: {
      upload_id: uploadId,
      label: label ?? null,
    },
  });
  printJson(imported);
}

async function importInspect(context: CliContext): Promise<void> {
  const importId = positionalArg(context.args) ?? requiredOption(context.args, "--import-id");
  const job = await cloudFetch(context, `/v1/imports/${importId}`);
  if (context.args.includes("--json")) {
    printJson(job);
    return;
  }
  printImportSummary(job);
}

async function packageGet(context: CliContext): Promise<void> {
  const digest = positionalArg(context.args) ?? requiredOption(context.args, "--package-digest");
  printJson(await cloudFetch(context, `/v1/packages/${encodeURIComponent(digest)}`));
}

async function runCreate(context: CliContext): Promise<void> {
  const packageDigest = requiredOption(context.args, "--package-digest");
  const runLabel = optionValue(context.args, "--label");
  const backend = optionValue(context.args, "--backend");
  const materialize = optionValue(context.args, "--materialize");
  printJson(
    await cloudFetch(context, "/v1/runs", {
      method: "POST",
      body: {
        package_digest: packageDigest,
        run_label: runLabel ?? null,
        env: keyValueOptions(context.args, "--env"),
        secret_refs: keyValueOptions(context.args, "--secret-ref"),
        runtime_options: {
          ...(backend ? { backend } : {}),
          ...(materialize ? { materialize } : {}),
          ...(context.args.includes("--smoke-test") ? { smoke_test: true } : {}),
        },
      },
    }),
  );
}

async function runGet(context: CliContext): Promise<void> {
  const runId = positionalArg(context.args) ?? requiredOption(context.args, "--run-id");
  printJson(await cloudFetch(context, `/v1/runs/${encodeURIComponent(runId)}`));
}

async function runnerPoolCreate(context: CliContext): Promise<void> {
  printJson(await cloudFetch(context, "/v1/runner-pools", {
    method: "POST",
    body: {
      name: requiredOption(context.args, "--name"),
      capabilities: {
        executors: csvOption(context.args, "--executors", ["runner-docker"]),
        resources: csvOption(context.args, "--resources", ["core_runner", "docker_daemon", "registry_pull"]),
      },
      metadata: {},
    },
  }));
}

async function runnerPoolList(context: CliContext): Promise<void> {
  printJson(await cloudFetch(context, "/v1/runner-pools"));
}

async function runnerInstanceDrain(context: CliContext): Promise<void> {
  const runnerInstanceId = positionalArg(context.args) ?? requiredOption(context.args, "--runner-instance-id");
  printJson(await cloudFetch(context, `/v1/runner-instances/${encodeURIComponent(runnerInstanceId)}/drain`, {
    method: "POST",
    body: {},
  }));
}

async function readDraftFromOptions(args: string[]): Promise<JsonObject> {
  return readDraftFile(requiredOption(args, "--file"));
}

async function readDraftFile(path: string): Promise<JsonObject> {
  const raw = await readFile(path, "utf8");
  const parsed = extname(path).toLowerCase() === ".json" ? JSON.parse(raw) : YAML.parse(raw);
  if (!isObject(parsed)) {
    throw new CliError(`draft file must parse to an object: ${path}`);
  }
  return parsed;
}

async function cloudFetch(
  context: CliContext,
  path: string,
  options: { method?: string; body?: unknown; rawBody?: BodyInit; contentType?: string } = {},
): Promise<unknown> {
  const init: RequestInit = { method: options.method ?? "GET" };
  const headers: Record<string, string> = {};
  if (context.workerToken) {
    headers.authorization = `Bearer ${context.workerToken}`;
  }
  if (options.rawBody) {
    init.headers = { ...headers, "content-type": options.contentType ?? "application/octet-stream" };
    init.body = options.rawBody;
  } else if (options.body) {
    init.headers = { ...headers, "content-type": options.contentType ?? "application/json" };
    init.body = JSON.stringify(options.body);
  } else {
    init.headers = headers;
  }
  const response = await fetch(`${context.apiUrl}${path}`, init);
  const text = await response.text();
  const payload = text.trim().length > 0 ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new CliError(
      isObject(payload) && typeof payload.message === "string"
        ? payload.message
        : `Cloud API request failed: ${response.status}`,
    );
  }
  return payload;
}

function requiredOption(args: string[], name: string): string {
  const value = optionValue(args, name);
  if (!value) {
    throw new CliError(`${name} is required`);
  }
  return value;
}

function optionValue(args: string[], name: string): string | null {
  const index = args.indexOf(name);
  if (index === -1) {
    return null;
  }
  const value = args[index + 1];
  if (!value) {
    throw new CliError(`${name} requires a value`);
  }
  return value;
}

function keyValueOptions(args: string[], name: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] !== name) {
      continue;
    }
    const value = args[index + 1];
    if (!value) {
      throw new CliError(`${name} requires KEY=VALUE`);
    }
    const separator = value.indexOf("=");
    if (separator <= 0) {
      throw new CliError(`${name} requires KEY=VALUE`);
    }
    out[value.slice(0, separator)] = value.slice(separator + 1);
    index += 1;
  }
  return out;
}

function csvOption(args: string[], name: string, fallback: string[]): string[] {
  const value = optionValue(args, name);
  if (!value) {
    return fallback;
  }
  return value.split(",").map((item) => item.trim()).filter((item) => item.length > 0);
}

function printJson(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

function printImportSummary(value: unknown): void {
  if (!isObject(value)) {
    throw new CliError("import inspect response was not an object");
  }
  const diagnostics = Array.isArray(value.diagnostics) ? value.diagnostics : [];
  const lines = [
    `Import: ${stringField(value, "import_id") ?? "(unknown)"}`,
    `Status: ${stringField(value, "status") ?? "(unknown)"}`,
  ];
  const label = stringField(value, "label");
  if (label) {
    lines.push(`Label: ${label}`);
  }
  const packageDigest = stringField(value, "package_digest");
  if (packageDigest) {
    lines.push(`Package: ${packageDigest}`);
  }
  const errorMessage = stringField(value, "error_message");
  if (errorMessage) {
    lines.push(`Error: ${errorMessage}`);
  }

  if (diagnostics.length > 0) {
    lines.push("", "Diagnostics:");
    for (const diagnostic of diagnostics) {
      if (!isObject(diagnostic)) {
        continue;
      }
      lines.push(
        `  - [${stringField(diagnostic, "severity") ?? "unknown"}] ${stringField(diagnostic, "code") ?? "diagnostic"} ${stringField(diagnostic, "pointer") ?? "/"}: ${stringField(diagnostic, "message") ?? ""}`,
      );
    }
  }

  process.stdout.write(`${lines.join("\n")}\n`);
}

function printHelp(): void {
  process.stdout.write(`Bucephalus Cloud CLI

Usage:
  bucephalus-cloud [--api-url URL] health
  bucephalus-cloud [--api-url URL] registry search --kind variant --query codex
  bucephalus-cloud [--api-url URL] draft validate --file experiment.yaml
  bucephalus-cloud [--api-url URL] draft preview --file experiment.yaml
  bucephalus-cloud [--api-url URL] draft export --file experiment.yaml --out ./exported
  bucephalus-cloud [--api-url URL] import sealed-package ./package.tgz
  bucephalus-cloud [--api-url URL] import inspect <import-id> [--json]
  bucephalus-cloud [--api-url URL] package get <package-digest>
  bucephalus-cloud [--api-url URL] run create --package-digest sha256:... [--backend runner-docker|modal] [--label name] [--env KEY=VALUE] [--secret-ref KEY=SECRET_NAME]
  bucephalus-cloud [--api-url URL] run get <run-id>
  bucephalus-cloud [--api-url URL] runner-pool create --name local --executors runner-docker --resources core_runner,docker_daemon,registry_pull
  bucephalus-cloud [--api-url URL] runner-pool list
  bucephalus-cloud [--api-url URL] runner-instance drain <runner-instance-id>

Environment:
  BUCEPHALUS_CLOUD_API_URL       Defaults to http://localhost:8099
  BUCEPHALUS_CLOUD_WORKER_TOKEN  Required for runner pool and worker management commands
`);
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function positionalArg(args: string[]): string | null {
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (!arg) {
      continue;
    }
    if (arg.startsWith("--")) {
      if (
        arg === "--import-id" ||
        arg === "--action-file" ||
        arg === "--aliases" ||
        arg === "--label" ||
        arg === "--file" ||
        arg === "--package-digest" ||
        arg === "--run-id" ||
        arg === "--runner-instance-id" ||
        arg === "--name" ||
        arg === "--executors" ||
        arg === "--resources" ||
        arg === "--env" ||
        arg === "--secret-ref"
      ) {
        index += 1;
      }
      continue;
    }
    return arg;
  }
  return null;
}

function stringField(value: JsonObject, field: string): string | null {
  return typeof value[field] === "string" ? value[field] : null;
}

function sha256Digest(bytes: Uint8Array): string {
  const hash = createHash("sha256");
  hash.update(bytes);
  return `sha256:${hash.digest("hex")}`;
}

function mediaTypeForPath(path: string): string {
  const lower = path.toLowerCase();
  if (lower.endsWith(".tar.gz") || lower.endsWith(".tgz")) {
    return "application/gzip";
  }
  if (lower.endsWith(".tar")) {
    return "application/x-tar";
  }
  return "application/octet-stream";
}

main(process.argv.slice(2)).catch((error) => {
  const message = error instanceof Error ? error.message : String(error);
  console.error(message);
  process.exit(error instanceof CliError ? error.exitCode : 1);
});

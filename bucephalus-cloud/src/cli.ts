#!/usr/bin/env bun
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";
import { createHash } from "node:crypto";
import YAML from "yaml";

type JsonObject = Record<string, unknown>;

interface CliContext {
  apiUrl: string;
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

  if (group === "import" && command === "apply") {
    await importApply({ ...context, args: rest });
    return;
  }

  throw new CliError(`unknown command: ${[group, command].filter(Boolean).join(" ")}`);
}

function parseGlobalArgs(argv: string[]): CliContext {
  const args = [...argv];
  let apiUrl = process.env.BUCEPHALUS_CLOUD_API_URL ?? "http://localhost:8099";
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] !== "--api-url") {
      continue;
    }
    const value = args[index + 1];
    if (!value) {
      throw new CliError("--api-url requires a value");
    }
    apiUrl = value;
    args.splice(index, 2);
    index -= 1;
  }
  return { apiUrl: apiUrl.replace(/\/+$/, ""), args };
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

async function importApply(context: CliContext): Promise<void> {
  const importId = context.args[0] ?? requiredOption(context.args, "--import-id");
  const actionFile = optionValue(context.args, "--action-file");
  const allRegister = context.args.includes("--all-register");
  const aliasesMode = optionValue(context.args, "--aliases") ?? "none";
  const replaceAliases = context.args.includes("--replace-aliases");

  let actions: unknown;
  if (actionFile) {
    const raw = await readFile(actionFile, "utf8");
    const parsed = JSON.parse(raw);
    actions = isObject(parsed) && Array.isArray(parsed.actions) ? parsed.actions : parsed;
  } else if (allRegister) {
    const inspected = await cloudFetch(context, `/v1/imports/${importId}`);
    if (!isObject(inspected) || !Array.isArray(inspected.proposed_entities)) {
      throw new CliError("import inspect response did not include proposed_entities");
    }
    actions = inspected.proposed_entities.map((proposal) => {
      if (!isObject(proposal) || typeof proposal.proposal_id !== "string") {
        throw new CliError("proposal is missing proposal_id");
      }
      const suggestedAliases = Array.isArray(proposal.suggested_aliases)
        ? proposal.suggested_aliases.filter((value): value is string => typeof value === "string")
        : [];
      const alias = suggestedAliases[0] ?? null;
      if (aliasesMode === "suggested" && alias) {
        return {
          proposal_id: proposal.proposal_id,
          action: replaceAliases ? "replace_alias" : "create_alias",
          alias,
        };
      }
      return {
        proposal_id: proposal.proposal_id,
        action: "register_new",
      };
    });
  } else {
    throw new CliError("import apply requires --action-file <path> or --all-register");
  }

  printJson(
    await cloudFetch(context, `/v1/imports/${importId}/actions`, {
      method: "POST",
      body: { actions },
    }),
  );
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
  if (options.rawBody) {
    init.headers = { "content-type": options.contentType ?? "application/octet-stream" };
    init.body = options.rawBody;
  } else if (options.body) {
    init.headers = { "content-type": options.contentType ?? "application/json" };
    init.body = JSON.stringify(options.body);
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

function printJson(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

function printImportSummary(value: unknown): void {
  if (!isObject(value)) {
    throw new CliError("import inspect response was not an object");
  }
  const proposals = Array.isArray(value.proposed_entities) ? value.proposed_entities : [];
  const diagnostics = Array.isArray(value.diagnostics) ? value.diagnostics : [];
  const counts = countByStatus(proposals);
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
  lines.push(
    `Proposals: ${proposals.length} total, ${counts.proposed ?? 0} proposed, ${counts.registered ?? 0} registered, ${counts.skipped ?? 0} skipped`,
  );

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

  if (proposals.length > 0) {
    lines.push("", "Proposed Entities:");
    for (const proposal of proposals) {
      if (!isObject(proposal)) {
        continue;
      }
      const aliases = Array.isArray(proposal.suggested_aliases)
        ? proposal.suggested_aliases.filter((alias): alias is string => typeof alias === "string")
        : [];
      lines.push(
        `  - ${stringField(proposal, "kind") ?? "unknown"} ${stringField(proposal, "source_pointer") ?? "/"} ${stringField(proposal, "status") ?? "unknown"}`,
      );
      lines.push(`    digest: ${stringField(proposal, "content_digest") ?? "(missing)"}`);
      if (aliases.length > 0) {
        lines.push(`    aliases: ${aliases.join(", ")}`);
      }
      const proposalId = stringField(proposal, "proposal_id");
      if (proposalId) {
        lines.push(`    proposal: ${proposalId}`);
      }
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
  bucephalus-cloud [--api-url URL] import apply <import-id> --all-register
  bucephalus-cloud [--api-url URL] import apply <import-id> --all-register --aliases suggested
  bucephalus-cloud [--api-url URL] import apply <import-id> --all-register --aliases suggested --replace-aliases
  bucephalus-cloud [--api-url URL] import apply <import-id> --action-file actions.json

Environment:
  BUCEPHALUS_CLOUD_API_URL   Defaults to http://localhost:8099
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
      if (arg === "--import-id" || arg === "--action-file" || arg === "--aliases" || arg === "--label" || arg === "--file") {
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

function countByStatus(values: unknown[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const value of values) {
    if (!isObject(value)) {
      continue;
    }
    const status = stringField(value, "status");
    if (status) {
      counts[status] = (counts[status] ?? 0) + 1;
    }
  }
  return counts;
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

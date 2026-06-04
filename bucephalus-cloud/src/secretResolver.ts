#!/usr/bin/env bun
import { chmod, mkdir, open } from "node:fs/promises";
import { spawn } from "node:child_process";
import { resolve } from "node:path";

type JsonObject = Record<string, unknown>;

interface SecretResolverInput {
  attempt_id: string;
  run_id: string;
  output_dir: string;
  secrets: SecretRequest[];
}

interface SecretRequest {
  id: string;
  ref: string;
}

interface SecretResolverOptions {
  env?: NodeJS.ProcessEnv;
  runCommand?: CommandRunner;
}

type CommandRunner = (executable: string, args: string[]) => Promise<{ stdout: string; stderr: string }>;

class SecretResolverError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SecretResolverError";
  }
}

export async function resolveSecrets(
  input: unknown,
  options: SecretResolverOptions = {},
): Promise<{ files: Record<string, string> }> {
  const request = parseResolverInput(input);
  const env = options.env ?? process.env;
  const runCommand = options.runCommand ?? runProviderCommand;
  await mkdir(request.output_dir, { recursive: true, mode: 0o700 });

  const files: Record<string, string> = {};
  for (const secret of request.secrets) {
    const value = await fetchSecretValue(secret.ref, { env, runCommand });
    const relativePath = `${secret.id}.secret`;
    const outputPath = resolvedOutputPath(request.output_dir, relativePath);
    await writeSecretFile(outputPath, value);
    files[secret.id] = relativePath;
  }
  return { files };
}

export async function fetchSecretValue(
  ref: string,
  options: Required<Pick<SecretResolverOptions, "env" | "runCommand">>,
): Promise<string> {
  const plan = secretFetchPlan(ref, options.env);
  if (plan.kind === "env") {
    const value = options.env[plan.name];
    if (value === undefined) {
      throw new SecretResolverError(`Environment secret '${plan.name}' is not set`);
    }
    return value;
  }
  const result = await options.runCommand(plan.executable, plan.args);
  return stripOneTrailingNewline(result.stdout);
}

export function secretFetchPlan(ref: string, env: NodeJS.ProcessEnv = process.env):
  | { kind: "env"; name: string }
  | { kind: "command"; executable: string; args: string[] } {
  if (ref.startsWith("env:")) {
    if (!truthy(env.BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV)) {
      throw new SecretResolverError(
        "env: secret refs are disabled. Set BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV=true only for local development.",
      );
    }
    const name = ref.slice("env:".length);
    assertEnvName(name);
    return { kind: "env", name };
  }
  const gcp = parseGcpSecretRef(ref);
  if (gcp) {
    return {
      kind: "command",
      executable: env.BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD || "gcloud",
      args: [
        "secrets",
        "versions",
        "access",
        gcp.version,
        "--secret",
        gcp.secret,
        "--project",
        gcp.project,
      ],
    };
  }
  const aws = parseAwsSecretRef(ref);
  if (aws) {
    return {
      kind: "command",
      executable: env.BUCEPHALUS_SECRET_RESOLVER_AWS_CMD || "aws",
      args: [
        "secretsmanager",
        "get-secret-value",
        "--secret-id",
        aws.secretId,
        "--query",
        "SecretString",
        "--output",
        "text",
      ],
    };
  }
  throw new SecretResolverError(
    "Unsupported secret ref. Use gcp-secret-manager://projects/<project>/secrets/<secret>/versions/<version>, "
      + "aws-secrets-manager://<secret-id>, or env:<NAME> for explicitly enabled local development.",
  );
}

function parseResolverInput(input: unknown): SecretResolverInput {
  if (!isRecord(input)) {
    throw new SecretResolverError("Resolver input must be a JSON object");
  }
  const attemptId = requiredString(input.attempt_id, "/attempt_id");
  const runId = requiredString(input.run_id, "/run_id");
  const outputDir = requiredString(input.output_dir, "/output_dir");
  if (!Array.isArray(input.secrets)) {
    throw new SecretResolverError("/secrets must be an array");
  }
  const seen = new Set<string>();
  const secrets = input.secrets.map((item, index) => {
    if (!isRecord(item)) {
      throw new SecretResolverError(`/secrets/${index} must be an object`);
    }
    const id = requiredString(item.id, `/secrets/${index}/id`);
    const ref = requiredString(item.ref, `/secrets/${index}/ref`);
    assertSecretId(id);
    if (seen.has(id)) {
      throw new SecretResolverError(`Duplicate secret id '${id}'`);
    }
    seen.add(id);
    return { id, ref };
  });
  return {
    attempt_id: attemptId,
    run_id: runId,
    output_dir: outputDir,
    secrets,
  };
}

function parseGcpSecretRef(ref: string): { project: string; secret: string; version: string } | null {
  const path = ref.startsWith("gcp-secret-manager://")
    ? ref.slice("gcp-secret-manager://".length)
    : ref.startsWith("gcp://")
      ? ref.slice("gcp://".length)
      : null;
  if (path === null) {
    return null;
  }
  const match = /^projects\/([^/]+)\/secrets\/([^/]+)\/versions\/([^/]+)$/.exec(path);
  if (!match) {
    throw new SecretResolverError("Invalid GCP Secret Manager ref");
  }
  const project = match[1];
  const secret = match[2];
  const version = match[3];
  if (!project || !secret || !version) {
    throw new SecretResolverError("Invalid GCP Secret Manager ref");
  }
  assertGcpProject(project);
  assertGcpSecret(secret);
  assertSecretVersion(version);
  return { project, secret, version };
}

function parseAwsSecretRef(ref: string): { secretId: string } | null {
  const prefix = "aws-secrets-manager://";
  if (!ref.startsWith(prefix)) {
    return null;
  }
  const secretId = ref.slice(prefix.length);
  if (secretId.trim().length === 0 || secretId.includes("\n") || secretId.includes("\r")) {
    throw new SecretResolverError("Invalid AWS Secrets Manager ref");
  }
  return { secretId };
}

function resolvedOutputPath(outputDir: string, relativePath: string): string {
  const outputRoot = resolve(outputDir);
  const outputPath = resolve(outputRoot, relativePath);
  if (outputPath !== outputRoot && !outputPath.startsWith(`${outputRoot}/`)) {
    throw new SecretResolverError("Secret output path escaped output_dir");
  }
  return outputPath;
}

async function writeSecretFile(outputPath: string, value: string): Promise<void> {
  let file;
  try {
    file = await open(outputPath, "wx", 0o600);
  } catch (error) {
    if (isRecord(error) && error.code === "EEXIST") {
      throw new SecretResolverError(`Secret output file already exists: ${outputPath}`);
    }
    throw error;
  }
  try {
    await file.writeFile(value);
  } finally {
    await file.close();
  }
  await chmod(outputPath, 0o600);
}

async function runProviderCommand(executable: string, args: string[]): Promise<{ stdout: string; stderr: string }> {
  const result = await new Promise<{ exitCode: number; stdout: string; stderr: string }>((resolvePromise, reject) => {
    const child = spawn(executable, args, {
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      resolvePromise({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
  });
  if (result.exitCode !== 0) {
    throw new SecretResolverError(`${executable} exited ${result.exitCode}: ${tail(result.stderr, 1000)}`);
  }
  return {
    stdout: result.stdout,
    stderr: result.stderr,
  };
}

function assertSecretId(id: string): void {
  if (!/^[A-Za-z0-9_.-]+$/.test(id)) {
    throw new SecretResolverError(`Invalid secret id '${id}'`);
  }
}

function assertEnvName(name: string): void {
  if (!/^[A-Z_][A-Z0-9_]*$/.test(name)) {
    throw new SecretResolverError(`Invalid env secret ref '${name}'`);
  }
}

function assertGcpProject(project: string): void {
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(project)) {
    throw new SecretResolverError(`Invalid GCP project '${project}'`);
  }
}

function assertGcpSecret(secret: string): void {
  if (!/^[A-Za-z0-9_-]+$/.test(secret)) {
    throw new SecretResolverError(`Invalid GCP secret '${secret}'`);
  }
}

function assertSecretVersion(version: string): void {
  if (!/^(latest|[0-9]+)$/.test(version)) {
    throw new SecretResolverError(`Invalid secret version '${version}'`);
  }
}

function requiredString(value: unknown, pointer: string): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new SecretResolverError(`${pointer} must be a non-empty string`);
  }
  if (value.includes("\n") || value.includes("\r")) {
    throw new SecretResolverError(`${pointer} must not contain newlines`);
  }
  return value;
}

function stripOneTrailingNewline(value: string): string {
  return value.endsWith("\r\n")
    ? value.slice(0, -2)
    : value.endsWith("\n")
      ? value.slice(0, -1)
      : value;
}

function tail(value: string, maxBytes: number): string {
  const buffer = Buffer.from(value, "utf8");
  if (buffer.byteLength <= maxBytes) {
    return value;
  }
  return buffer.subarray(buffer.byteLength - maxBytes).toString("utf8");
}

function truthy(value: string | undefined): boolean {
  return value !== undefined && ["1", "true", "yes", "on"].includes(value.trim().toLowerCase());
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function readStdin(): Promise<unknown> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  const text = Buffer.concat(chunks).toString("utf8");
  return JSON.parse(text);
}

if (import.meta.main) {
  readStdin()
    .then((input) => resolveSecrets(input))
    .then((output) => {
      process.stdout.write(`${JSON.stringify(output)}\n`);
    })
    .catch((error) => {
      process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
      process.exit(1);
    });
}

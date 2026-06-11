import { createSql } from "./client";
import { RunnerRepository } from "../runners/repository";
import type { JsonObject } from "../primitives";

interface PromotionInput {
  poolId: string;
  imageRef: string;
  releaseVersion: string | null;
  releaseGitSha: string | null;
  promotionEvidenceUri: string | null;
  promotionEvidenceSha256: string | null;
  modalLauncherSha256: string | null;
  workerRunnerSha256: string | null;
  boundaryVerifiedAt: string | null;
  metadata: JsonObject;
}

const digestAddressedImage = /^([^/]+)\/(.+)@(sha256:[a-f0-9]{64})$/;
const sha256Value = /^sha256:[a-f0-9]{64}$/;
const bareSha256Value = /^[a-f0-9]{64}$/;
const gitSha = /^[a-f0-9]{40}$/;
const knownArgs = new Set([
  "pool-id",
  "image",
  "release-version",
  "release-git-sha",
  "promotion-evidence-uri",
  "promotion-evidence-sha256",
  "modal-launcher-sha256",
  "worker-runner-sha256",
  "boundary-verified-at",
  "metadata-json",
]);

export async function main(argv = process.argv.slice(2)): Promise<void> {
  const args = parseArgs(argv);
  const image = parseImageRef(args.imageRef);
  const sql = createSql();
  try {
    const runners = new RunnerRepository(sql);
    const result = await runners.promoteWorkerImage({
      poolId: args.poolId,
      imageRef: args.imageRef,
      registryHost: image.registryHost,
      repository: image.repository,
      digest: image.digest,
      releaseVersion: args.releaseVersion,
      releaseGitSha: args.releaseGitSha,
      promotionEvidenceUri: args.promotionEvidenceUri,
      promotionEvidenceSha256: args.promotionEvidenceSha256,
      modalLauncherSha256: args.modalLauncherSha256,
      workerRunnerSha256: args.workerRunnerSha256,
      boundaryVerifiedAt: args.boundaryVerifiedAt,
      metadata: args.metadata,
    });
    console.log(JSON.stringify({
      runner_pool_id: result.pool.runner_pool_id,
      active_worker_image_id: result.pool.active_worker_image_id,
      worker_image: result.workerImage,
    }, null, 2));
  } finally {
    await sql.end({ timeout: 5 });
  }
}

export function parseArgs(argv: string[]): PromotionInput {
  if (argv.length === 0 && process.env.BUCEPHALUS_PROMOTE_WORKER_IMAGE) {
    return parseEnv();
  }
  const values = new Map<string, string>();
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index] ?? "";
    if (arg === "-h" || arg === "--help") {
      usage(0);
    }
    if (!arg.startsWith("--")) {
      fail(`unknown positional argument: ${arg}`);
    }
    const key = arg.slice(2);
    if (!knownArgs.has(key)) {
      fail(`unknown argument: --${key}`);
    }
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) {
      fail(`missing value for --${key}`);
    }
    values.set(key, value);
    index += 1;
  }

  const releaseGitSha = optional(values, "release-git-sha");
  if (releaseGitSha && !gitSha.test(releaseGitSha)) {
    fail("--release-git-sha must be a 40-character lowercase git SHA");
  }
  const metadata = parseMetadata(optional(values, "metadata-json"));
  metadata.promoted_by = metadata.promoted_by ?? "bucephalus-cloud-worker-image-promotion";
  metadata.promoted_at = metadata.promoted_at ?? new Date().toISOString();

  return {
    poolId: required(values, "pool-id"),
    imageRef: required(values, "image"),
    releaseVersion: optional(values, "release-version"),
    releaseGitSha,
    promotionEvidenceUri: optional(values, "promotion-evidence-uri"),
    promotionEvidenceSha256: normalizeSha256(optional(values, "promotion-evidence-sha256"), "--promotion-evidence-sha256"),
    modalLauncherSha256: normalizeSha256(optional(values, "modal-launcher-sha256"), "--modal-launcher-sha256"),
    workerRunnerSha256: normalizeSha256(optional(values, "worker-runner-sha256"), "--worker-runner-sha256"),
    boundaryVerifiedAt: optional(values, "boundary-verified-at"),
    metadata,
  };
}

function parseEnv(): PromotionInput {
  const releaseGitSha = envOptional("BUCEPHALUS_PROMOTE_WORKER_RELEASE_GIT_SHA");
  if (releaseGitSha && !gitSha.test(releaseGitSha)) {
    fail("BUCEPHALUS_PROMOTE_WORKER_RELEASE_GIT_SHA must be a 40-character lowercase git SHA");
  }
  const metadata = parseMetadata(envOptional("BUCEPHALUS_PROMOTE_WORKER_METADATA_JSON"));
  metadata.promoted_by = metadata.promoted_by ?? "bucephalus-cloud-worker-image-promotion";
  metadata.promoted_at = metadata.promoted_at ?? new Date().toISOString();

  return {
    poolId: envRequired("BUCEPHALUS_PROMOTE_WORKER_POOL_ID"),
    imageRef: envRequired("BUCEPHALUS_PROMOTE_WORKER_IMAGE"),
    releaseVersion: envOptional("BUCEPHALUS_PROMOTE_WORKER_RELEASE_VERSION"),
    releaseGitSha,
    promotionEvidenceUri: envOptional("BUCEPHALUS_PROMOTE_WORKER_EVIDENCE_URI"),
    promotionEvidenceSha256: normalizeSha256(
      envOptional("BUCEPHALUS_PROMOTE_WORKER_EVIDENCE_SHA256"),
      "BUCEPHALUS_PROMOTE_WORKER_EVIDENCE_SHA256",
    ),
    modalLauncherSha256: normalizeSha256(
      envOptional("BUCEPHALUS_PROMOTE_WORKER_MODAL_LAUNCHER_SHA256"),
      "BUCEPHALUS_PROMOTE_WORKER_MODAL_LAUNCHER_SHA256",
    ),
    workerRunnerSha256: normalizeSha256(
      envOptional("BUCEPHALUS_PROMOTE_WORKER_RUNNER_SHA256"),
      "BUCEPHALUS_PROMOTE_WORKER_RUNNER_SHA256",
    ),
    boundaryVerifiedAt: envOptional("BUCEPHALUS_PROMOTE_WORKER_BOUNDARY_VERIFIED_AT"),
    metadata,
  };
}

function parseImageRef(imageRef: string): { registryHost: string; repository: string; digest: string } {
  const match = digestAddressedImage.exec(imageRef);
  if (!match || imageRef.includes(":latest")) {
    fail("--image must be an immutable digest-addressed image ref, e.g. host/project/repo/worker@sha256:<64 hex>");
  }
  const [, registryHost, repositoryPath, digest] = match;
  return {
    registryHost: registryHost!,
    repository: `${registryHost}/${repositoryPath}`,
    digest: digest!,
  };
}

function normalizeSha256(value: string | null, label: string): string | null {
  if (!value) {
    return null;
  }
  const normalized = bareSha256Value.test(value) ? `sha256:${value}` : value;
  if (!sha256Value.test(normalized)) {
    fail(`${label} must be sha256:<64 lowercase hex>`);
  }
  return normalized;
}

function parseMetadata(value: string | null): JsonObject {
  if (!value) {
    return {};
  }
  try {
    const parsed = JSON.parse(value);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      fail("--metadata-json must be a JSON object");
    }
    return parsed as JsonObject;
  } catch (error) {
    fail(`--metadata-json must be valid JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function required(values: Map<string, string>, key: string): string {
  const value = optional(values, key);
  if (!value) {
    fail(`--${key} is required`);
  }
  return value;
}

function optional(values: Map<string, string>, key: string): string | null {
  const value = values.get(key);
  return value && value.trim() !== "" ? value.trim() : null;
}

function envRequired(name: string): string {
  const value = envOptional(name);
  if (!value) {
    fail(`${name} is required`);
  }
  return value;
}

function envOptional(name: string): string | null {
  const value = process.env[name];
  return value && value.trim() !== "" ? value.trim() : null;
}

function fail(message: string): never {
  console.error(message);
  usage(2);
}

function usage(exitCode: number): never {
  console.error(`Usage: bun run src/db/promoteWorkerImage.ts --pool-id <runner-pool-id> --image <image@sha256:digest> [options]

Options:
  --release-version <version>
  --release-git-sha <40-char-sha>
  --promotion-evidence-uri <uri>
  --promotion-evidence-sha256 <sha256>
  --modal-launcher-sha256 <sha256>
  --worker-runner-sha256 <sha256>
  --boundary-verified-at <iso timestamp>
  --metadata-json <json object>
`);
  process.exit(exitCode);
}

if (import.meta.main) {
  await main();
}

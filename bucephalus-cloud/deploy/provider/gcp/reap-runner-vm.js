#!/usr/bin/env bun
import { spawnSync } from "node:child_process";

async function main() {
  const input = parseJson(await Bun.stdin.text(), "reap input");
  const env = process.env;
  const project = requiredEnv(env.BUCEPHALUS_GCP_PROJECT, "BUCEPHALUS_GCP_PROJECT");
  const zone = env.BUCEPHALUS_GCP_ZONE || "us-central1-a";
  const name = String(input.provider_instance_id || input.instance_name || "").trim();
  if (!name) {
    writeJson({ metadata: { provider: "gcp", terminated: true, skipped: true, reason: "missing provider_instance_id" } });
    return;
  }

  if (truthy(env.BUCEPHALUS_GCP_DRY_RUN)) {
    writeJson({
      metadata: {
        provider: "gcp",
        dry_run: true,
        terminated: true,
        project,
        zone,
        command: ["gcloud", "compute", "instances", "delete", name, `--project=${project}`, `--zone=${zone}`, "--quiet"],
      },
    });
    return;
  }

  const describe = spawnSync("gcloud", [
    "compute",
    "instances",
    "describe",
    name,
    `--project=${project}`,
    `--zone=${zone}`,
    "--format=value(name)",
  ], { encoding: "utf8" });
  if (describe.status !== 0) {
    writeJson({ metadata: { provider: "gcp", terminated: true, already_absent: true, project, zone } });
    return;
  }

  const deleted = spawnSync("gcloud", [
    "compute",
    "instances",
    "delete",
    name,
    `--project=${project}`,
    `--zone=${zone}`,
    "--quiet",
  ], { encoding: "utf8" });
  if (deleted.status !== 0) {
    throw new Error(`gcloud delete failed: ${tail(deleted.stderr || deleted.stdout, 4000)}`);
  }
  writeJson({ metadata: { provider: "gcp", terminated: true, project, zone } });
}

function parseJson(raw, name) {
  try {
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed;
    }
    throw new Error(`${name} must be a JSON object`);
  } catch (error) {
    throw new Error(`invalid ${name}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function requiredEnv(value, name) {
  if (!value || value.trim().length === 0) {
    throw new Error(`${name} is required`);
  }
  return value.trim();
}

function truthy(value) {
  return ["1", "true", "yes", "on"].includes(String(value || "").toLowerCase());
}

function writeJson(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function tail(value, maxBytes) {
  const buffer = Buffer.from(String(value || ""), "utf8");
  if (buffer.byteLength <= maxBytes) {
    return buffer.toString("utf8");
  }
  return buffer.subarray(buffer.byteLength - maxBytes).toString("utf8");
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});

#!/usr/bin/env bun
import {
  ProviderError,
  googleJson,
  optionalEnv,
  parseProviderInstanceId,
  readJsonStdin,
  requiredEnv,
  waitForZoneOperation,
} from "./gce-provider-common.js";

async function main() {
  const input = await readJsonStdin();
  const config = loadConfig();
  const parsed = parseProviderInstanceId(input.provider_instance_id);
  const projectId = parsed?.projectId ?? config.projectId;
  const zone = parsed?.zone ?? config.zone;
  const instanceName = parsed?.instanceName ?? stringOrNull(input.instance_name);
  if (!instanceName) {
    throw new ProviderError("reap input requires provider_instance_id or instance_name");
  }

  const url = `https://compute.googleapis.com/compute/v1/projects/${encodeURIComponent(projectId)}/zones/${encodeURIComponent(zone)}/instances/${encodeURIComponent(instanceName)}`;
  const getResponse = await fetchWithStatus(url);
  if (getResponse.status === 404) {
    console.log(JSON.stringify({
      metadata: {
        provider: "gcp-gce-per-run-v1",
        instance_name: instanceName,
        already_absent: true,
      },
    }));
    return;
  }
  if (!getResponse.ok) {
    throw new ProviderError(getResponse.message);
  }

  const operation = await googleJson(url, { method: "DELETE" });
  await waitForZoneOperation(projectId, zone, operation.name, config.operationTimeoutMs);
  console.log(JSON.stringify({
    metadata: {
      provider: "gcp-gce-per-run-v1",
      instance_name: instanceName,
      deleted: true,
    },
  }));
}

function loadConfig() {
  return {
    projectId: requiredEnv("BUCEPHALUS_GCP_PROJECT_ID"),
    zone: requiredEnv("BUCEPHALUS_GCP_ZONE"),
    operationTimeoutMs: Number(optionalEnv("BUCEPHALUS_GCP_OPERATION_TIMEOUT_MS", "600")) * 1000,
  };
}

async function fetchWithStatus(url) {
  try {
    await googleJson(url);
    return { ok: true, status: 200 };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (/not found/i.test(message)) {
      return { ok: false, status: 404, message };
    }
    return { ok: false, status: 500, message };
  }
}

function stringOrNull(value) {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : null;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});

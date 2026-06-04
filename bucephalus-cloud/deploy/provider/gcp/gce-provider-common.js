const metadataTokenUrl = "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

export class ProviderError extends Error {
  constructor(message) {
    super(message);
    this.name = "ProviderError";
  }
}

export async function readJsonStdin() {
  const text = await new Response(Bun.stdin.stream()).text();
  if (text.trim().length === 0) {
    throw new ProviderError("provider command requires JSON on stdin");
  }
  const parsed = JSON.parse(text);
  if (!isRecord(parsed)) {
    throw new ProviderError("provider command input must be a JSON object");
  }
  return parsed;
}

export function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function requiredEnv(name) {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new ProviderError(`${name} is required`);
  }
  return value;
}

export function optionalEnv(name, fallback = null) {
  const value = process.env[name]?.trim();
  return value && value.length > 0 ? value : fallback;
}

export function integerEnv(name, fallback) {
  const raw = optionalEnv(name, String(fallback));
  if (!/^[1-9][0-9]*$/.test(raw)) {
    throw new ProviderError(`${name} must be a positive integer`);
  }
  return Number(raw);
}

export function assertDigestRef(value, name) {
  if (typeof value !== "string" || !/^.+@sha256:[a-f0-9]{64}$/.test(value) || /@sha256:0{64}$/.test(value)) {
    throw new ProviderError(`${name} must be a real digest-addressed image ref`);
  }
}

export function assertGcpName(value, name) {
  if (typeof value !== "string" || !/^[a-z]([-a-z0-9]{0,61}[a-z0-9])?$/.test(value)) {
    throw new ProviderError(`${name} must be a valid GCP resource name`);
  }
}

export function assertSimpleToken(value, name) {
  if (typeof value !== "string" || !/^[A-Za-z0-9._:@/+,-]+$/.test(value)) {
    throw new ProviderError(`${name} contains unsupported characters`);
  }
}

export function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\"'\"'")}'`;
}

export function labelValue(value) {
  return String(value)
    .toLowerCase()
    .replaceAll(/[^a-z0-9_-]/g, "-")
    .replaceAll(/^-+|-+$/g, "")
    .slice(0, 63) || "unknown";
}

export function shortId(value) {
  return String(value).toLowerCase().replaceAll(/[^a-z0-9]/g, "").slice(0, 12);
}

export function registryHost(imageRef) {
  const [host] = imageRef.split("/");
  if (!host || !/^[a-z0-9-]+-docker\.pkg\.dev$/.test(host)) {
    throw new ProviderError("runner image must be hosted in GCP Artifact Registry");
  }
  return host;
}

export async function accessToken() {
  const explicit = optionalEnv("BUCEPHALUS_GCP_ACCESS_TOKEN");
  if (explicit) {
    return explicit;
  }
  const command = optionalEnv("BUCEPHALUS_GCP_ACCESS_TOKEN_CMD_JSON");
  if (command) {
    const parsed = JSON.parse(command);
    if (!Array.isArray(parsed) || parsed.length === 0 || parsed.some((item) => typeof item !== "string" || item.length === 0)) {
      throw new ProviderError("BUCEPHALUS_GCP_ACCESS_TOKEN_CMD_JSON must be a JSON string array");
    }
    const [executable, ...args] = parsed;
    const result = Bun.spawnSync([executable, ...args], { stdout: "pipe", stderr: "pipe" });
    if (result.exitCode !== 0) {
      throw new ProviderError(`access token command failed: ${new TextDecoder().decode(result.stderr).slice(0, 1000)}`);
    }
    return new TextDecoder().decode(result.stdout).trim();
  }
  try {
    const response = await fetch(metadataTokenUrl, {
      headers: { "Metadata-Flavor": "Google" },
    });
    if (response.ok) {
      const payload = await response.json();
      if (typeof payload.access_token === "string" && payload.access_token.length > 0) {
        return payload.access_token;
      }
    }
  } catch {
    // Fall through to the local gcloud fallback.
  }
  const result = Bun.spawnSync(["gcloud", "auth", "print-access-token"], { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    throw new ProviderError("could not obtain GCP access token from metadata server or local gcloud");
  }
  return new TextDecoder().decode(result.stdout).trim();
}

export async function googleJson(url, init = {}) {
  const token = await accessToken();
  const response = await fetch(url, {
    ...init,
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
      ...(init.headers ?? {}),
    },
  });
  const text = await response.text();
  const body = text.trim().length > 0 ? JSON.parse(text) : null;
  if (!response.ok) {
    const message = isRecord(body?.error) && typeof body.error.message === "string"
      ? body.error.message
      : `GCP request failed: ${response.status}`;
    throw new ProviderError(message);
  }
  return body;
}

export async function waitForZoneOperation(projectId, zone, operationName, timeoutMs = 600000) {
  const startedAt = Date.now();
  const url = `https://compute.googleapis.com/compute/v1/projects/${encodeURIComponent(projectId)}/zones/${encodeURIComponent(zone)}/operations/${encodeURIComponent(operationName)}`;
  while (Date.now() - startedAt < timeoutMs) {
    const operation = await googleJson(url);
    if (operation.status === "DONE") {
      if (operation.error?.errors?.length > 0) {
        throw new ProviderError(operation.error.errors.map((error) => error.message).join("; "));
      }
      return operation;
    }
    await Bun.sleep(2000);
  }
  throw new ProviderError(`timed out waiting for GCE operation ${operationName}`);
}

export function providerInstanceId(projectId, zone, instanceName) {
  return `gce://projects/${projectId}/zones/${zone}/instances/${instanceName}`;
}

export function parseProviderInstanceId(value) {
  if (typeof value !== "string") {
    return null;
  }
  const match = /^gce:\/\/projects\/([^/]+)\/zones\/([^/]+)\/instances\/([^/]+)$/.exec(value);
  if (!match) {
    return null;
  }
  return {
    projectId: match[1],
    zone: match[2],
    instanceName: match[3],
  };
}

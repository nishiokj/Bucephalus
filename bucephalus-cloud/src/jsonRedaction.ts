type JsonObject = Record<string, unknown>;

const REDACTED_VALUE = "[redacted]";

/**
 * Strips credential-shaped keys and values from JSON evidence before it
 * leaves the worker. Trial trajectories regularly echo environment access,
 * so every payload crossing to the control plane goes through this.
 */
export function redactSensitiveJsonObject(value: JsonObject): JsonObject {
  return redactSensitiveJsonValue(value, null) as JsonObject;
}

export function redactSensitiveText(value: string): string {
  return value
    .replace(/\b(?:AKIA|ASIA)[0-9A-Z]{16}\b/g, REDACTED_VALUE)
    .replace(/\bsk-[A-Za-z0-9_-]{20,}\b/g, REDACTED_VALUE)
    .replace(/\bya29\.[A-Za-z0-9_-]{20,}\b/g, REDACTED_VALUE)
    .replace(/\bgcp-secret-manager:\/\/[^\s"'`]+/g, REDACTED_VALUE)
    .replace(/\baws-secrets-manager:\/\/[^\s"'`]+/g, REDACTED_VALUE)
    .replace(/((?:secret|password|passwd|token|api[_-]?key|credential|authorization|bearer)[A-Za-z0-9_.-]*\s*[:=]\s*)[^\s"'`]+/gi, `$1${REDACTED_VALUE}`);
}

function redactSensitiveJsonValue(value: unknown, key: string | null): unknown {
  if (key !== null && sensitiveJsonKey(key)) {
    return REDACTED_VALUE;
  }
  if (typeof value === "string") {
    return sensitiveStringValue(value) ? REDACTED_VALUE : value;
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactSensitiveJsonValue(item, null));
  }
  if (isRecord(value)) {
    const out: JsonObject = {};
    for (const [childKey, childValue] of Object.entries(value)) {
      out[childKey] = redactSensitiveJsonValue(childValue, childKey);
    }
    return out;
  }
  return value;
}

function sensitiveJsonKey(key: string): boolean {
  const normalized = key.toLowerCase().replaceAll(/[^a-z0-9]/g, "");
  return normalized.includes("secret")
    || normalized.includes("password")
    || normalized.includes("passwd")
    || normalized.includes("token")
    || normalized.includes("apikey")
    || normalized.includes("credential")
    || normalized.includes("authorization")
    || normalized.includes("bearer");
}

function sensitiveStringValue(value: string): boolean {
  return value.includes("gcp-secret-manager://")
    || value.includes("aws-secrets-manager://")
    || /\bAKIA[0-9A-Z]{16}\b/.test(value)
    || /\bASIA[0-9A-Z]{16}\b/.test(value)
    || /\bsk-[A-Za-z0-9_-]{20,}\b/.test(value)
    || /\bya29\.[A-Za-z0-9_-]{20,}\b/.test(value);
}

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

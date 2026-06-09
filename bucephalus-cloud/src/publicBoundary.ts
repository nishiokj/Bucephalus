import type { JsonObject, JsonValue } from "./primitives";
import type { ImportDiagnostic } from "./imports/sealedPackage";

const REDACTED_LOCAL_PATH = "[redacted-local-path]";
const REDACTED_SECRET = "[redacted-secret]";
const REDACTED_VALUE = "[redacted]";
const PATH_TOKEN_BOUNDARY = "(?:(?!\\s+[A-Za-z0-9_.-]+=)[^\"'`<>{}\\[\\](),;\\r\\n])";
const POSIX_LOCAL_PATH_PATTERN = new RegExp(
  `(?:/Users|/home|/private|/tmp|/var/folders|/Volumes|/mnt/[A-Za-z]/Users)/${PATH_TOKEN_BOUNDARY}*`,
  "g",
);
const WINDOWS_DRIVE_PATH_PATTERN = new RegExp(`\\b[A-Za-z]:[\\\\/]${PATH_TOKEN_BOUNDARY}*`, "g");
const WINDOWS_PROFILE_ENV_PATH_PATTERN = new RegExp(
  `%[A-Z_]*(?:USERPROFILE|HOME|TEMP|TMP|APPDATA|LOCALAPPDATA)[A-Z_]*%[\\\\/]${PATH_TOKEN_BOUNDARY}*`,
  "gi",
);
const HOME_RELATIVE_PATH_PATTERN = new RegExp(
  `(^|[\\s="'\`<>{}\\[\\]()])~[\\\\/]${PATH_TOKEN_BOUNDARY}*`,
  "g",
);

export function publicBoundaryText(value: string): string {
  return value
    .replace(/file:\/\/[^\s"'`<>{}\[\]()]+/gi, `file://${REDACTED_LOCAL_PATH}`)
    .replace(/https?:\/\/[^\s"'`<>{}\[\]()]+/gi, (url) => publicBoundaryUrl(url))
    .replace(/\b(?:gcp-secret-manager|aws-secrets-manager):\/\/[^\s"'`<>{}\[\]()]+/gi, REDACTED_SECRET)
    .replace(POSIX_LOCAL_PATH_PATTERN, REDACTED_LOCAL_PATH)
    .replace(WINDOWS_DRIVE_PATH_PATTERN, REDACTED_LOCAL_PATH)
    .replace(WINDOWS_PROFILE_ENV_PATH_PATTERN, REDACTED_LOCAL_PATH)
    .replace(HOME_RELATIVE_PATH_PATTERN, (_match, prefix) => `${prefix}${REDACTED_LOCAL_PATH}`)
    .replace(
      /\b([A-Za-z0-9_.-]*(?:token|secret|password|passwd|api[_-]?key|credential|authorization|bearer)[A-Za-z0-9_.-]*)=([^\s"'`<>{}\[\]()]+)/gi,
      (_match, key) => `${key}=${REDACTED_SECRET}`,
    )
    .replace(/\bsk-[A-Za-z0-9_-]{20,}\b/g, REDACTED_SECRET)
    .replace(/\bya29\.[A-Za-z0-9_-]{20,}\b/g, REDACTED_SECRET)
    .replace(/\b(?:AKIA|ASIA)[0-9A-Z]{16}\b/g, REDACTED_SECRET);
}

export function publicBoundaryJsonValue(value: JsonValue): JsonValue {
  return publicBoundaryValue(value) as JsonValue;
}

export function publicBoundaryValue(value: unknown): unknown {
  if (typeof value === "string") {
    return publicBoundaryText(value);
  }
  if (Array.isArray(value)) {
    return value.map((item) => publicBoundaryValue(item));
  }
  if (isJsonObject(value)) {
    const out: Record<string, unknown> = {};
    for (const [key, child] of Object.entries(value)) {
      out[key] = sensitiveJsonKey(key) ? REDACTED_VALUE : publicBoundaryValue(child);
    }
    return out;
  }
  return value;
}

export function publicBoundaryJsonObject(value: JsonObject | null): JsonObject | null {
  return value === null ? null : publicBoundaryJsonValue(value) as JsonObject;
}

export function publicBoundaryImportDiagnostic(diagnostic: ImportDiagnostic): ImportDiagnostic {
  return {
    severity: diagnostic.severity,
    code: publicBoundaryText(diagnostic.code),
    pointer: publicBoundaryText(diagnostic.pointer),
    message: publicBoundaryText(diagnostic.message),
  };
}

function publicBoundaryUrl(rawUrl: string): string {
  try {
    const parsed = new URL(rawUrl);
    if (parsed.protocol === "file:") {
      return `file://${REDACTED_LOCAL_PATH}`;
    }
    const redacted = parsed.username !== ""
      || parsed.password !== ""
      || parsed.search !== ""
      || parsed.hash !== "";
    parsed.username = "";
    parsed.password = "";
    parsed.search = "";
    parsed.hash = "";
    return redacted
      ? `${parsed.toString()} [redacted URL credentials/query]`
      : parsed.toString();
  } catch {
    return "[redacted-url]";
  }
}

function sensitiveJsonKey(key: string): boolean {
  const normalized = key.toLowerCase().replaceAll(/[^a-z0-9]/g, "");
  return normalized.includes("password")
    || normalized.includes("passwd")
    || normalized.includes("secret")
    || normalized.includes("token")
    || normalized.includes("apikey")
    || normalized.includes("credential")
    || normalized.includes("authorization")
    || normalized.includes("bearer")
    || normalized.includes("privatekey");
}

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

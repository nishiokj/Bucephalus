import { createHash } from "node:crypto";

export const CANONICAL_PROTOCOL = "bucephalus-canonical-json-v1" as const;

export const ENTITY_KINDS = [
  "agent_app",
  "case",
  "dataset",
  "experiment_package",
  "grader",
  "metric",
  "runtime_profile",
  "task_boundary",
  "trial_contract",
  "variant",
] as const;

export type EntityKind = (typeof ENTITY_KINDS)[number];

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export type JsonObject = { [key: string]: JsonValue };

export interface CanonicalizeEntityInput {
  kind: EntityKind;
  schemaVersion: string;
  object: JsonObject;
}

export interface CanonicalEntity {
  protocol: typeof CANONICAL_PROTOCOL;
  kind: EntityKind;
  schemaVersion: string;
  contentDigest: string;
  canonicalJson: JsonObject;
  canonicalEnvelope: JsonObject;
  canonicalBytes: Uint8Array;
  canonicalSizeBytes: number;
}

export class CanonicalizationError extends Error {
  constructor(
    message: string,
    public readonly pointer: string,
  ) {
    super(message);
    this.name = "CanonicalizationError";
  }
}

export function canonicalizeEntity(input: CanonicalizeEntityInput): CanonicalEntity {
  assertEntityKind(input.kind);
  assertNonemptyString(input.schemaVersion, "/schemaVersion");
  assertPlainJsonObject(input.object, "/object");

  const canonicalJson = canonicalizeJsonValue(input.object, "/object") as JsonObject;
  const canonicalEnvelope: JsonObject = {
    canonical_json: canonicalJson,
    kind: input.kind,
    protocol: CANONICAL_PROTOCOL,
    schema_version: input.schemaVersion,
  };
  const canonicalText = canonicalJsonStringify(canonicalEnvelope);
  const canonicalBytes = new TextEncoder().encode(canonicalText);
  const contentDigest = sha256Digest(canonicalBytes);

  return {
    protocol: CANONICAL_PROTOCOL,
    kind: input.kind,
    schemaVersion: input.schemaVersion,
    contentDigest,
    canonicalJson,
    canonicalEnvelope,
    canonicalBytes,
    canonicalSizeBytes: canonicalBytes.byteLength,
  };
}

export function canonicalJsonStringify(value: JsonValue): string {
  return stringifyCanonicalJson(canonicalizeJsonValue(value, ""));
}

export function sha256Digest(bytes: Uint8Array | string): string {
  const hash = createHash("sha256");
  hash.update(bytes);
  return `sha256:${hash.digest("hex")}`;
}

function assertEntityKind(kind: string): asserts kind is EntityKind {
  if (!ENTITY_KINDS.includes(kind as EntityKind)) {
    throw new CanonicalizationError(`unsupported entity kind '${kind}'`, "/kind");
  }
}

function assertNonemptyString(value: string, pointer: string): void {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new CanonicalizationError("expected a non-empty string", pointer);
  }
}

function assertPlainJsonObject(value: unknown, pointer: string): asserts value is JsonObject {
  if (!isPlainObject(value)) {
    throw new CanonicalizationError("expected a JSON object", pointer);
  }
}

function canonicalizeJsonValue(value: unknown, pointer: string): JsonValue {
  if (value === null) {
    return null;
  }
  if (typeof value === "string" || typeof value === "boolean") {
    return value;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw new CanonicalizationError("JSON number must be finite", pointer);
    }
    if (Object.is(value, -0)) {
      throw new CanonicalizationError("negative zero is not allowed in canonical JSON", pointer);
    }
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((item, index) => canonicalizeJsonValue(item, `${pointer}/${index}`));
  }
  if (isPlainObject(value)) {
    const out: JsonObject = {};
    for (const key of Object.keys(value).sort(compareJsonObjectKeys)) {
      out[key] = canonicalizeJsonValue(value[key], `${pointer}/${escapeJsonPointer(key)}`);
    }
    return out;
  }
  throw new CanonicalizationError(
    `unsupported JSON value type '${typeof value}'`,
    pointer || "/",
  );
}

function stringifyCanonicalJson(value: JsonValue): string {
  if (value === null) {
    return "null";
  }
  if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => stringifyCanonicalJson(item)).join(",")}]`;
  }
  const entries = Object.entries(value);
  return `{${entries
    .map(([key, item]) => `${JSON.stringify(key)}:${stringifyCanonicalJson(item)}`)
    .join(",")}}`;
}

function compareJsonObjectKeys(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}

function escapeJsonPointer(segment: string): string {
  return segment.replaceAll("~", "~0").replaceAll("/", "~1");
}


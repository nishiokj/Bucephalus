import { HttpError, jsonResponse, optionalString, readJsonObject, requireRecord, requireString } from "../http";
import {
  canonicalizeDraft,
  diffDraftJson,
  DRAFT_VALIDATION_LEVELS,
  exportDraftYaml,
  previewSchedule,
  resolveDraftRefs,
  validateDraft,
  type DraftValidationLevel,
} from "../drafts/primitives";
import { ENTITY_KINDS, type EntityKind, type JsonObject, type JsonValue } from "../primitives";
import { RegistryRepository, type RegistrySearchHit } from "../registry/repository";

export async function handleDraftRoute(
  request: Request,
  url: URL,
  repository: RegistryRepository,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/drafts/canonicalize") {
    const draft = await draftFromRequest(request);
    const result = await canonicalizeDraft(draft);
    return jsonResponse({
      canonical_draft: result.canonicalDraft,
      draft_digest: result.draftDigest,
      digest_map: result.digestMap.map(bindingToWire),
      issues: issuesToWire(result.issues),
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/resolve") {
    const draft = await draftFromRequest(request);
    const result = await resolveDraftRefs(draft, {
      hasDigest: (kind, digest) => repository.hasDigest(kind, digest),
      resolveAlias: (kind, alias, scope) => repository.resolveAlias(kind, alias, scope),
    });
    return jsonResponse({
      resolved_draft: result.resolvedDraft,
      bindings: result.bindings.map(bindingToWire),
      unresolved: result.unresolved,
      issues: issuesToWire(result.issues),
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/validate") {
    const body = await readJsonObject(request);
    const draft = requireRecord(body.draft, "/draft") as JsonObject;
    const validationLevel = draftValidationLevel(body.validation_level);
    const result = await validateDraft(draft, {
      hasDigest: (kind, digest) => repository.hasDigest(kind, digest),
      resolveAlias: (kind, alias, scope) => repository.resolveAlias(kind, alias, scope),
    }, { validationLevel });
    return jsonResponse({
      valid: result.valid,
      validation_level: result.validationLevel,
      issues: issuesToWire(result.issues),
      resolved_refs: result.resolvedRefs.map(bindingToWire),
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/preview-schedule") {
    const draft = await draftFromRequest(request);
    return jsonResponse(previewSchedule(draft));
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/suggest") {
    const body = await readJsonObject(request);
    const draft = requireRecord(body.draft, "/draft") as JsonObject;
    const target = requireString(body.target, "/target");
    const q = optionalString(body.q, "/q") ?? "";
    const limit = boundedLimit(body.limit);
    const suggestions = [];
    const entityKind = targetToEntityKind(target);
    if (entityKind) {
      const hits = await repository.search({ kind: entityKind, q, limit });
      suggestions.push(...hits.map((hit) => registryHitSuggestion(hit)));
    } else if (target === "template") {
      suggestions.push({
        suggestion_type: "template",
        title: "No hosted templates are currently registered",
        detail: "Build from an experiment.yaml authoring context, or register reusable templates before requesting template suggestions.",
        score: 0,
        registry_hit: null,
        patch: null,
      });
    }
    const validation = await validateDraft(draft, {
      hasDigest: (kind, digest) => repository.hasDigest(kind, digest),
      resolveAlias: (kind, alias, scope) => repository.resolveAlias(kind, alias, scope),
    });
    suggestions.push(
      ...validation.issues
        .filter((issue) => issue.severity !== "info")
        .slice(0, Math.max(0, limit - suggestions.length))
        .map((issue) => ({
          suggestion_type: "warning",
          title: issue.code,
          detail: issue.pointer ? `${issue.pointer}: ${issue.message}` : issue.message,
          score: issue.severity === "error" ? 0.8 : 0.4,
          registry_hit: null,
          patch: null,
        })),
    );
    return jsonResponse({ suggestions: suggestions.slice(0, limit) });
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/diff") {
    const body = await readJsonObject(request);
    const leftSide = requireRecord(body.left, "/left");
    const rightSide = requireRecord(body.right, "/right");
    const left = await resolveDraftSide(leftSide, "/left", repository);
    const right = await resolveDraftSide(rightSide, "/right", repository);
    return jsonResponse({
      left: left.ref,
      right: right.ref,
      changes: diffDraftJson(left.value, right.value),
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/export") {
    const body = await readJsonObject(request);
    const draft = requireRecord(body.draft, "/draft") as JsonObject;
    const format = typeof body.format === "string" ? body.format : "yaml";
    const result = await validateDraft(draft, {
      hasDigest: (kind, digest) => repository.hasDigest(kind, digest),
      resolveAlias: (kind, alias, scope) => repository.resolveAlias(kind, alias, scope),
    });
    if (format === "resolved_json") {
      return jsonResponse({
        format,
        body: JSON.stringify(draft, null, 2),
        issues: issuesToWire(result.issues),
      });
    }
    return jsonResponse({
      format: "yaml",
      body: exportDraftYaml(draft),
      issues: issuesToWire(result.issues),
    });
  }

  return null;
}

function boundedLimit(value: unknown): number {
  if (value === undefined || value === null) {
    return 10;
  }
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1) {
    throw new HttpError(400, "invalid_request", "/limit must be an integer >= 1");
  }
  if (value > 100) {
    throw new HttpError(400, "invalid_request", "/limit must be <= 100");
  }
  return value;
}

function registryHitSuggestion(hit: RegistrySearchHit) {
  return {
    suggestion_type: "registry_entity",
    title: hit.display_name,
    detail: hit.aliases.length > 0 ? `aliases: ${hit.aliases.join(", ")}` : hit.content_digest,
    score: hit.score,
    registry_hit: hit,
    patch: null,
  };
}

function targetToEntityKind(target: string): EntityKind | null {
  if (target === "agent_app" || target === "variant" || target === "benchmark" || target === "dataset" || target === "case" || target === "grader" || target === "metric" || target === "runtime_profile") {
    return target;
  }
  if (target === "template") {
    return null;
  }
  throw new HttpError(400, "invalid_request", "/target is not a supported suggestion target");
}

function draftValidationLevel(value: unknown): DraftValidationLevel {
  if (value === undefined || value === null) {
    return "authoring";
  }
  if (typeof value === "string" && DRAFT_VALIDATION_LEVELS.includes(value as DraftValidationLevel)) {
    return value as DraftValidationLevel;
  }
  throw new HttpError(
    400,
    "invalid_validation_level",
    "/validation_level must be one of authoring, package, launch_hint",
  );
}

async function resolveDraftSide(
  side: Record<string, unknown>,
  pointer: string,
  repository: RegistryRepository,
): Promise<{ ref: Record<string, unknown>; value: JsonValue }> {
  if ("draft" in side) {
    const draft = requireRecord(side.draft, `${pointer}/draft`) as JsonObject;
    return {
      ref: {
        kind: "experiment_package",
        inline: draft,
      },
      value: draft,
    };
  }
  if ("ref" in side) {
    const ref = requireRecord(side.ref, `${pointer}/ref`);
    const kind = requireEntityKind(ref.kind, `${pointer}/ref/kind`);
    const digest = requireString(ref.digest, `${pointer}/ref/digest`);
    const object = await repository.getContentObject(digest);
    if (!object || object.kind !== kind) {
      throw new HttpError(404, "draft_diff_ref_not_found", "Draft diff ref does not resolve to a registered object", {
        pointer,
        kind,
        digest,
      });
    }
    const canonicalJson = object.canonical_json;
    if (!isJsonValue(canonicalJson)) {
      throw new HttpError(409, "draft_diff_ref_invalid", "Registered object does not contain canonical JSON");
    }
    return {
      ref: {
        kind,
        digest,
      },
      value: canonicalJson,
    };
  }
  throw new HttpError(400, "invalid_request", `${pointer} must contain draft or ref`);
}

function requireEntityKind(value: unknown, pointer: string): EntityKind {
  if (typeof value !== "string" || !ENTITY_KINDS.includes(value as EntityKind)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be a valid entity kind`);
  }
  return value as EntityKind;
}

function isJsonValue(value: unknown): value is JsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") {
    return true;
  }
  if (typeof value === "number") {
    return Number.isFinite(value);
  }
  if (Array.isArray(value)) {
    return value.every(isJsonValue);
  }
  if (typeof value === "object" && value !== null) {
    return Object.values(value).every(isJsonValue);
  }
  return false;
}

async function draftFromRequest(request: Request): Promise<JsonObject> {
  const body = await readJsonObject(request);
  return requireRecord(body.draft, "/draft") as JsonObject;
}

function bindingToWire(binding: {
  pointer: string;
  kind: string;
  contentDigest: string;
  resolution: string;
  alias?: string | null;
  displayName?: string | null;
}) {
  return {
    pointer: binding.pointer,
    kind: binding.kind,
    content_digest: binding.contentDigest,
    resolution: binding.resolution,
    alias: binding.alias ?? null,
    display_name: binding.displayName ?? null,
  };
}

function issuesToWire(
  issues: Array<{
    severity: string;
    code: string;
    message: string;
    pointer?: string | null;
    relatedDigest?: string | null;
  }>,
) {
  return issues.map((issue) => ({
    severity: issue.severity,
    code: issue.code,
    message: issue.message,
    pointer: issue.pointer ?? null,
    related_digest: issue.relatedDigest ?? null,
  }));
}

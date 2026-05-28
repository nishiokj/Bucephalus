import { jsonResponse, readJsonObject, requireRecord } from "../http";
import {
  canonicalizeDraft,
  exportDraftYaml,
  previewSchedule,
  resolveDraftRefs,
  validateDraft,
} from "../drafts/primitives";
import type { JsonObject } from "../primitives";
import { RegistryRepository } from "../registry/repository";

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
    const draft = await draftFromRequest(request);
    const result = await validateDraft(draft, {
      hasDigest: (kind, digest) => repository.hasDigest(kind, digest),
      resolveAlias: (kind, alias, scope) => repository.resolveAlias(kind, alias, scope),
    });
    return jsonResponse({
      valid: result.valid,
      issues: issuesToWire(result.issues),
      resolved_refs: result.resolvedRefs.map(bindingToWire),
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/preview-schedule") {
    const draft = await draftFromRequest(request);
    return jsonResponse(previewSchedule(draft));
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


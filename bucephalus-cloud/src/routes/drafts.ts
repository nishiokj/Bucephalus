import { HttpError, jsonResponse, optionalString, readJsonObject, requireRecord } from "../http";
import {
  canonicalizeDraft,
  exportDraftYaml,
  previewSchedule,
  resolveDraftRefs,
  validateDraft,
} from "../drafts/primitives";
import type { JsonObject } from "../primitives";
import { publicBoundaryText } from "../publicBoundary";
import { RegistryRepository } from "../registry/repository";

interface DraftRouteOptions {
  exportDraftYaml?: (draft: JsonObject) => string;
}

export async function handleDraftRoute(
  request: Request,
  url: URL,
  repository: RegistryRepository,
  options: DraftRouteOptions = {},
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
      unresolved: result.unresolved.map(unresolvedToWire),
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
    const preview = previewSchedule(draft);
    return jsonResponse({
      ...preview,
      warnings: issuesToWire(preview.warnings),
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/drafts/export") {
    const body = await readJsonObject(request);
    const draft = requireRecord(body.draft, "/draft") as JsonObject;
    const format = draftExportFormat(body.format);
    const result = await validateDraft(draft, {
      hasDigest: (kind, digest) => repository.hasDigest(kind, digest),
      resolveAlias: (kind, alias, scope) => repository.resolveAlias(kind, alias, scope),
    });
    if (format === "resolved_json") {
      return jsonResponse({
        format,
        body: JSON.stringify(result.resolvedDraft, null, 2),
        issues: issuesToWire(result.issues),
      });
    }
    return jsonResponse({
      format: "yaml",
      body: exportDraftYamlForResponse(draft, options.exportDraftYaml ?? exportDraftYaml),
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
    alias: binding.alias === undefined || binding.alias === null ? null : publicBoundaryText(binding.alias),
    display_name: binding.displayName === undefined || binding.displayName === null ? null : publicBoundaryText(binding.displayName),
  };
}

function unresolvedToWire(unresolved: {
  pointer: string;
  kind: string;
  reason: string;
}) {
  return {
    pointer: publicBoundaryText(unresolved.pointer),
    kind: unresolved.kind,
    reason: publicBoundaryText(unresolved.reason),
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
    severity: publicBoundaryText(issue.severity),
    code: publicBoundaryText(issue.code),
    message: publicBoundaryText(issue.message),
    pointer: issue.pointer === undefined || issue.pointer === null ? null : publicBoundaryText(issue.pointer),
    related_digest: issue.relatedDigest === undefined || issue.relatedDigest === null ? null : publicBoundaryText(issue.relatedDigest),
  }));
}

function draftExportFormat(value: unknown): "yaml" | "resolved_json" {
  const format = optionalString(value, "/format") ?? "yaml";
  if (format === "yaml" || format === "resolved_json") {
    return format;
  }
  throw new HttpError(400, "unsupported_draft_export_format", "/format must be one of: yaml, resolved_json", {
    allowed: ["yaml", "resolved_json"],
  });
}

function exportDraftYamlForResponse(draft: JsonObject, exporter: (draft: JsonObject) => string): string {
  try {
    return exporter(draft);
  } catch {
    throw new HttpError(
      503,
      "draft_export_unavailable",
      "Draft YAML export is unavailable; retry with format=resolved_json or try again later",
      {
        format: "yaml",
        fallback_format: "resolved_json",
      },
    );
  }
}

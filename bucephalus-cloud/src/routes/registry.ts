import {
  HttpError,
  jsonResponse,
  optionalString,
  queryInteger,
  readJsonObject,
  requireRecord,
  requireString,
} from "../http";
import {
  ENTITY_KINDS,
  canonicalizeEntity,
  normalizationHints,
  resolveRegistryRef,
  type EntityKind,
  type JsonObject,
} from "../primitives";
import { RegistryRepository, type AliasReview, type RegistrySearchHit } from "../registry/repository";

export async function handleRegistryRoute(
  request: Request,
  url: URL,
  repository: RegistryRepository,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/registry/canonicalize") {
    return canonicalize(request);
  }

  if (request.method === "POST" && url.pathname === "/v1/registry/review") {
    return reviewObject(request, repository);
  }

  if (request.method === "POST" && url.pathname === "/v1/registry/objects") {
    return registerObject(request, repository);
  }

  if (request.method === "GET" && url.pathname.startsWith("/v1/registry/objects/")) {
    const digest = decodeURIComponent(url.pathname.slice("/v1/registry/objects/".length));
    const object = await repository.getContentObject(digest);
    if (!object) {
      throw new HttpError(404, "not_found", "Content object not found");
    }
    return jsonResponse({
      object,
      aliases: await repository.aliasesForDigest(digest),
    });
  }

  if (request.method === "GET" && url.pathname === "/v1/registry/search") {
    const q = url.searchParams.get("q")?.trim() ?? "";
    const kind = optionalEntityKind(url.searchParams.get("kind"), "/kind");
    const limit = queryInteger(url, "limit", { defaultValue: 50, min: 1, max: 200 });
    const searchOptions = {
      q,
      limit,
    };
    const hits = await repository.search(
      kind ? { ...searchOptions, kind } : searchOptions,
    );
    return jsonResponse({
      hits,
      page: { has_more: false, next_cursor: null },
    });
  }

  if (request.method === "POST" && url.pathname === "/v1/registry/resolve") {
    return resolveRef(request, repository);
  }

  if (request.method === "POST" && url.pathname === "/v1/registry/aliases") {
    return createAlias(request, repository);
  }

  return null;
}

async function canonicalize(request: Request): Promise<Response> {
  const body = await readJsonObject(request);
  const kind = requireString(body.kind, "/kind") as EntityKind;
  const schemaVersion = optionalString(body.schema_version, "/schema_version") ?? "v1";
  const object = requireRecord(body.object, "/object") as JsonObject;
  const canonical = canonicalizeEntity({ kind, schemaVersion, object });
  const hints = normalizationHints({ kind, object });

  return jsonResponse({
    content_digest: canonical.contentDigest,
    canonical_json: canonical.canonicalJson,
    canonical_size_bytes: canonical.canonicalSizeBytes,
    protocol: canonical.protocol,
    suggestions: hints.suggestions,
  });
}

async function reviewObject(
  request: Request,
  repository: RegistryRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const kind = requireString(body.kind, "/kind") as EntityKind;
  const schemaVersion = optionalString(body.schema_version, "/schema_version") ?? "v1";
  const aliases = parseAliasReviews(body.aliases);
  const providedDigest = optionalString(body.content_digest, "/content_digest");
  const inlineObject = body.object === undefined
    ? null
    : (requireRecord(body.object, "/object") as JsonObject);

  if (!providedDigest && !inlineObject) {
    throw new HttpError(400, "review_input_required", "Review requires content_digest or object");
  }

  const canonical = inlineObject
    ? canonicalizeEntity({ kind, schemaVersion, object: inlineObject })
    : null;
  const contentDigest = canonical?.contentDigest ?? providedDigest;
  if (!contentDigest) {
    throw new HttpError(400, "review_input_required", "Review requires content_digest or object");
  }
  if (providedDigest && canonical && providedDigest !== canonical.contentDigest) {
    throw new HttpError(409, "digest_mismatch", "Provided content_digest does not match canonical object", {
      provided_digest: providedDigest,
      computed_digest: canonical.contentDigest,
    });
  }

  const existing = await repository.getContentObject(contentDigest);
  const exactMatch = existing && existing.kind === kind
    ? {
        exists: true,
        object: existing,
        aliases: await repository.aliasesForDigest(contentDigest),
      }
    : {
        exists: false,
        object: null,
        aliases: [],
      };
  const aliasReviews = await repository.reviewAliases({
    kind,
    contentDigest,
    aliases,
  });
  const similarInput = Object.assign(
    {
      kind,
      aliases: aliases.map((alias) => alias.alias),
      excludeDigest: contentDigest,
    },
    canonical?.canonicalJson
      ? { object: canonical.canonicalJson }
      : isJsonObject(existing?.canonical_json)
        ? { object: existing.canonical_json }
        : {},
  );
  const similar = await similarCandidates(repository, similarInput);
  const hints = inlineObject
    ? normalizationHints({
        kind,
        object: inlineObject,
        similarDigests: similar.map((hit) => ({
          digest: hit.content_digest,
          score: hit.score,
          displayName: hit.display_name,
        })),
      })
    : null;

  return jsonResponse({
    canonical: {
      kind,
      schema_version: canonical?.schemaVersion ?? (typeof existing?.schema_version === "string" ? existing.schema_version : schemaVersion),
      content_digest: contentDigest,
      canonical_json: canonical?.canonicalJson ?? existing?.canonical_json ?? null,
      canonical_size_bytes: canonical?.canonicalSizeBytes ?? existing?.canonical_size_bytes ?? null,
      protocol: canonical?.protocol ?? "bucephalus-canonical-json-v1",
    },
    exact_match: exactMatch,
    alias_reviews: aliasReviews,
    similar,
    suggestions: hints?.suggestions ?? [],
    suggested_actions: suggestedReviewActions({
      exactExists: exactMatch.exists,
      aliasReviews,
      similar,
    }),
  });
}

async function registerObject(
  request: Request,
  repository: RegistryRepository,
): Promise<Response> {
  const body = await readJsonObject(request);
  const kind = requireString(body.kind, "/kind") as EntityKind;
  const schemaVersion = requireString(body.schema_version, "/schema_version");
  const object = requireRecord(body.canonical_json, "/canonical_json") as JsonObject;
  const expectedDigest = optionalString(body.expected_digest, "/expected_digest");
  const sourceUri = optionalString(body.source_uri, "/source_uri");
  const canonical = canonicalizeEntity({ kind, schemaVersion, object });

  if (expectedDigest && expectedDigest !== canonical.contentDigest) {
    throw new HttpError(409, "digest_mismatch", "Expected digest does not match canonical object", {
      expected_digest: expectedDigest,
      computed_digest: canonical.contentDigest,
    });
  }

  const created = await repository.register(canonical, sourceUri);
  const aliases = Array.isArray(body.aliases) ? body.aliases : [];
  for (const rawAlias of aliases) {
    const aliasObject = requireRecord(rawAlias, "/aliases[]");
    await repository.createAlias({
      kind,
      alias: requireString(aliasObject.alias, "/aliases[]/alias"),
      contentDigest: canonical.contentDigest,
      scopeType: optionalString(aliasObject.scope_type, "/aliases[]/scope_type") ?? "global",
      scopeId: optionalString(aliasObject.scope_id, "/aliases[]/scope_id"),
      replaceExisting: false,
    });
  }

  const objectRow = await repository.getContentObject(canonical.contentDigest);
  return jsonResponse(
    {
      created,
      object: objectRow,
      aliases: await repository.aliasesForDigest(canonical.contentDigest),
    },
    { status: created ? 201 : 200 },
  );
}

function parseAliasReviews(value: unknown): Array<{ alias: string; scopeType: string; scopeId?: string | null }> {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.map((rawAlias, index) => {
    if (typeof rawAlias === "string") {
      return { alias: rawAlias, scopeType: "global", scopeId: null };
    }
    const aliasObject = requireRecord(rawAlias, `/aliases/${index}`);
    return {
      alias: requireString(aliasObject.alias, `/aliases/${index}/alias`),
      scopeType: optionalString(aliasObject.scope_type, `/aliases/${index}/scope_type`) ?? "global",
      scopeId: optionalString(aliasObject.scope_id, `/aliases/${index}/scope_id`),
    };
  });
}

async function similarCandidates(
  repository: RegistryRepository,
  input: {
    kind: EntityKind;
    object?: JsonObject;
    aliases: string[];
    excludeDigest: string;
  },
): Promise<RegistrySearchHit[]> {
  const queryTerms = [
    ...input.aliases,
    stringValue(input.object?.display_name),
    stringValue(input.object?.name),
    stringValue(input.object?.id),
  ].filter(isNonemptyString);
  const hitsByDigest = new Map<string, RegistrySearchHit>();
  for (const query of queryTerms.slice(0, 5)) {
    const hits = await repository.search({ kind: input.kind, q: query, limit: 10 });
    for (const hit of hits) {
      if (hit.content_digest !== input.excludeDigest && !hitsByDigest.has(hit.content_digest)) {
        hitsByDigest.set(hit.content_digest, hit);
      }
    }
  }
  return [...hitsByDigest.values()]
    .sort((left, right) => right.score - left.score || left.display_name.localeCompare(right.display_name))
    .slice(0, 10);
}

function suggestedReviewActions(input: {
  exactExists: boolean;
  aliasReviews: AliasReview[];
  similar: RegistrySearchHit[];
}): Array<Record<string, unknown>> {
  const actions: Array<Record<string, unknown>> = [];
  if (input.exactExists) {
    actions.push({
      action: "use_existing",
      reason: "An object with this exact content digest already exists.",
    });
  } else {
    actions.push({
      action: "register_new",
      reason: "No object with this exact content digest exists.",
    });
  }
  for (const alias of input.aliasReviews) {
    if (alias.status === "available") {
      actions.push({
        action: "create_alias",
        alias: alias.alias,
        scope_type: alias.scope_type,
        scope_id: alias.scope_id,
        reason: "Alias is available in this scope.",
      });
    } else if (alias.status === "already_points_here") {
      actions.push({
        action: "keep_alias",
        alias: alias.alias,
        scope_type: alias.scope_type,
        scope_id: alias.scope_id,
        reason: "Alias already points at this content digest.",
      });
    } else {
      actions.push({
        action: "replace_alias",
        alias: alias.alias,
        scope_type: alias.scope_type,
        scope_id: alias.scope_id,
        existing_digest: alias.existing_digest,
        reason: "Alias points at another content digest; replacement must be explicit.",
      });
    }
  }
  if (!input.exactExists && input.similar.length > 0) {
    actions.push({
      action: "inspect_similar",
      reason: "Similar registry objects exist, but no equivalence decision was applied.",
    });
  }
  return actions;
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function optionalEntityKind(value: string | null, pointer: string): EntityKind | undefined {
  if (value === null || value.trim() === "") {
    return undefined;
  }
  if (!ENTITY_KINDS.includes(value as EntityKind)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be a valid entity kind`);
  }
  return value as EntityKind;
}

function isNonemptyString(value: string | null): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function resolveRef(request: Request, repository: RegistryRepository): Promise<Response> {
  const body = await readJsonObject(request);
  const rawRef = requireRecord(body.ref, "/ref");
  const kind = requireString(rawRef.kind, "/ref/kind") as EntityKind;
  const inline =
    rawRef.inline === undefined
      ? undefined
      : (requireRecord(rawRef.inline, "/ref/inline") as JsonObject);
  const digest = optionalString(rawRef.digest, "/ref/digest");
  const alias = optionalString(rawRef.alias, "/ref/alias");
  const schemaVersion =
    optionalString(rawRef.schema_version, "/ref/schema_version") ??
    optionalString(body.default_schema_version, "/default_schema_version") ??
    "v1";
  const resolved = await resolveRegistryRef(
    Object.assign(
      {
        kind,
        schemaVersion,
        scopeType: optionalString(rawRef.scope_type, "/ref/scope_type") ?? "global",
        scopeId: optionalString(rawRef.scope_id, "/ref/scope_id"),
      },
      digest ? { digest } : {},
      alias ? { alias } : {},
      inline ? { inline } : {},
    ),
    {
      hasDigest: (refKind, digestValue) => repository.hasDigest(refKind, digestValue),
      resolveAlias: (refKind, aliasValue, scope) =>
        repository.resolveAlias(refKind, aliasValue, scope),
      register: async (entity) => {
        await repository.register(entity);
      },
    },
    {
      registerInlineIfMissing: body.register_if_missing === true,
      defaultSchemaVersion: "v1",
      scopeType: "global",
    },
  ).catch((error) => {
    const message = error instanceof Error ? error.message : String(error);
    if (message.startsWith("digest_not_found") || message.startsWith("alias_not_found")) {
      throw new HttpError(404, "not_found", message);
    }
    throw error;
  });

  return jsonResponse({
    kind: resolved.kind,
    content_digest: resolved.contentDigest,
    resolution: resolved.resolution,
    canonical_json: resolved.canonical?.canonicalJson,
  });
}

async function createAlias(request: Request, repository: RegistryRepository): Promise<Response> {
  const body = await readJsonObject(request);
  const alias = await repository.createAlias({
    kind: requireString(body.kind, "/kind") as EntityKind,
    alias: requireString(body.alias, "/alias"),
    contentDigest: requireString(body.content_digest, "/content_digest"),
    scopeType: optionalString(body.scope_type, "/scope_type") ?? "global",
    scopeId: optionalString(body.scope_id, "/scope_id"),
    replaceExisting: body.replace_existing === true,
  });
  return jsonResponse(alias);
}

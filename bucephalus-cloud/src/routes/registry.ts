import {
  HttpError,
  jsonResponse,
  optionalString,
  readJsonObject,
  requireRecord,
  requireString,
} from "../http";
import {
  canonicalizeEntity,
  normalizationHints,
  resolveRegistryRef,
  type EntityKind,
  type JsonObject,
} from "../primitives";
import { RegistryRepository } from "../registry/repository";

export async function handleRegistryRoute(
  request: Request,
  url: URL,
  repository: RegistryRepository,
): Promise<Response | null> {
  if (request.method === "POST" && url.pathname === "/v1/registry/canonicalize") {
    return canonicalize(request);
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
    return jsonResponse(object);
  }

  if (request.method === "GET" && url.pathname === "/v1/registry/search") {
    const q = requireString(url.searchParams.get("q"), "q");
    const rawKind = url.searchParams.get("kind");
    const limit = Number.parseInt(url.searchParams.get("limit") ?? "50", 10);
    const searchOptions = {
      q,
      limit: Number.isFinite(limit) ? limit : 50,
    };
    const hits = await repository.search(
      rawKind ? { ...searchOptions, kind: rawKind as EntityKind } : searchOptions,
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

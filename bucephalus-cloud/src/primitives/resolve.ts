import {
  canonicalizeEntity,
  type CanonicalEntity,
  type EntityKind,
  type JsonObject,
} from "./canonicalization";

export interface RegistryRef {
  kind: EntityKind;
  digest?: string;
  alias?: string;
  inline?: JsonObject;
  schemaVersion?: string;
  scopeType?: string;
  scopeId?: string | null;
}

export type ResolutionKind =
  | "digest"
  | "alias"
  | "inline_existing"
  | "inline_unregistered"
  | "inline_registered";

export interface ResolvedRegistryRef {
  kind: EntityKind;
  contentDigest: string;
  resolution: ResolutionKind;
  canonical?: CanonicalEntity;
}

export interface RegistryResolverRepository {
  hasDigest(kind: EntityKind, digest: string): Promise<boolean> | boolean;
  resolveAlias(
    kind: EntityKind,
    alias: string,
    scope: { scopeType: string; scopeId?: string | null },
  ): Promise<string | null> | string | null;
  register?(entity: CanonicalEntity): Promise<void> | void;
}

export interface ResolveOptions {
  registerInlineIfMissing?: boolean;
  defaultSchemaVersion?: string;
  scopeType?: string;
  scopeId?: string | null;
}

export async function resolveRegistryRef(
  ref: RegistryRef,
  repository: RegistryResolverRepository,
  options: ResolveOptions = {},
): Promise<ResolvedRegistryRef> {
  if (ref.digest) {
    const exists = await repository.hasDigest(ref.kind, ref.digest);
    if (!exists) {
      throw new Error(`digest_not_found:${ref.kind}:${ref.digest}`);
    }
    return {
      kind: ref.kind,
      contentDigest: ref.digest,
      resolution: "digest",
    };
  }

  if (ref.alias) {
    const contentDigest = await repository.resolveAlias(ref.kind, ref.alias, {
      scopeType: ref.scopeType ?? options.scopeType ?? "global",
      scopeId: ref.scopeId ?? options.scopeId ?? null,
    });
    if (!contentDigest) {
      throw new Error(`alias_not_found:${ref.kind}:${ref.alias}`);
    }
    return {
      kind: ref.kind,
      contentDigest,
      resolution: "alias",
    };
  }

  if (ref.inline) {
    const canonical = canonicalizeEntity({
      kind: ref.kind,
      schemaVersion: ref.schemaVersion ?? options.defaultSchemaVersion ?? "v1",
      object: ref.inline,
    });
    const exists = await repository.hasDigest(ref.kind, canonical.contentDigest);
    if (exists) {
      return {
        kind: ref.kind,
        contentDigest: canonical.contentDigest,
        resolution: "inline_existing",
        canonical,
      };
    }
    if (options.registerInlineIfMissing) {
      if (!repository.register) {
        throw new Error("register_not_supported");
      }
      await repository.register(canonical);
      return {
        kind: ref.kind,
        contentDigest: canonical.contentDigest,
        resolution: "inline_registered",
        canonical,
      };
    }
    return {
      kind: ref.kind,
      contentDigest: canonical.contentDigest,
      resolution: "inline_unregistered",
      canonical,
    };
  }

  throw new Error("registry_ref_empty");
}


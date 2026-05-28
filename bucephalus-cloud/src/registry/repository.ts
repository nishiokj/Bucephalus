import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { CanonicalEntity, EntityKind } from "../primitives";

export interface RegistrySearchOptions {
  kind?: EntityKind;
  q: string;
  limit: number;
}

export interface RegistrySearchHit {
  kind: EntityKind;
  content_digest: string;
  display_name: string;
  aliases: string[];
  score: number;
  metadata: Record<string, unknown>;
}

export class RegistryRepository {
  constructor(private readonly sql: Sql) {}

  async hasDigest(kind: EntityKind, digest: string): Promise<boolean> {
    const rows = await this.sql`
      select 1
      from registry.content_objects
      where kind = ${kind}
        and content_digest = ${digest}
      limit 1
    `;
    return rows.length > 0;
  }

  async getContentObject(digest: string): Promise<Record<string, unknown> | null> {
    const rows = await this.sql`
      select
        content_digest,
        kind,
        schema_version,
        canonical_json,
        canonical_size_bytes,
        created_at,
        created_by,
        source_uri
      from registry.content_objects
      where content_digest = ${digest}
      limit 1
    `;
    return (rows[0] as Record<string, unknown> | undefined) ?? null;
  }

  async register(entity: CanonicalEntity, sourceUri?: string | null): Promise<boolean> {
    const rows = await this.sql`
      insert into registry.content_objects (
        content_digest,
        kind,
        schema_version,
        canonical_json,
        canonical_size_bytes,
        source_uri
      )
      values (
        ${entity.contentDigest},
        ${entity.kind},
        ${entity.schemaVersion},
        ${this.sql.json(entity.canonicalJson)},
        ${entity.canonicalSizeBytes},
        ${sourceUri ?? null}
      )
      on conflict (content_digest) do nothing
      returning content_digest
    `;
    return rows.length > 0;
  }

  async resolveAlias(
    kind: EntityKind,
    alias: string,
    scope: { scopeType: string; scopeId?: string | null },
  ): Promise<string | null> {
    const rows = await this.sql`
      select content_digest
      from registry.entity_aliases
      where kind = ${kind}
        and alias = ${alias}
        and scope_type = ${scope.scopeType}
        and coalesce(scope_id, '') = coalesce(${scope.scopeId ?? null}, '')
        and retired_at is null
      order by created_at desc
      limit 1
    `;
    const row = rows[0] as { content_digest?: string } | undefined;
    return row?.content_digest ?? null;
  }

  async createAlias(input: {
    kind: EntityKind;
    alias: string;
    contentDigest: string;
    scopeType: string;
    scopeId?: string | null;
    replaceExisting: boolean;
  }): Promise<Record<string, unknown>> {
    const exists = await this.hasDigest(input.kind, input.contentDigest);
    if (!exists) {
      throw new HttpError(404, "digest_not_found", "Cannot alias an unknown content digest");
    }

    return await this.sql.begin(async (tx) => {
      if (input.replaceExisting) {
        await tx`
          update registry.entity_aliases
          set retired_at = now()
          where kind = ${input.kind}
            and alias = ${input.alias}
            and scope_type = ${input.scopeType}
            and coalesce(scope_id, '') = coalesce(${input.scopeId ?? null}, '')
            and retired_at is null
        `;
      }

      try {
        const rows = await tx`
          insert into registry.entity_aliases (
            scope_type,
            scope_id,
            kind,
            alias,
            content_digest
          )
          values (
            ${input.scopeType},
            ${input.scopeId ?? null},
            ${input.kind},
            ${input.alias},
            ${input.contentDigest}
          )
          returning
            alias_id,
            scope_type,
            scope_id,
            kind,
            alias,
            content_digest,
            created_at,
            retired_at
        `;
        return rows[0] as Record<string, unknown>;
      } catch (error) {
        if (isUniqueViolation(error)) {
          throw new HttpError(
            409,
            "alias_conflict",
            "Alias already exists. Set replace_existing=true to move it explicitly.",
          );
        }
        throw error;
      }
    });
  }

  async aliasesForDigest(digest: string): Promise<Record<string, unknown>[]> {
    const rows = await this.sql`
      select
        alias_id,
        scope_type,
        scope_id,
        kind,
        alias,
        content_digest,
        created_at,
        retired_at
      from registry.entity_aliases
      where content_digest = ${digest}
        and retired_at is null
      order by created_at desc
    `;
    return rows as Record<string, unknown>[];
  }

  async search(options: RegistrySearchOptions): Promise<RegistrySearchHit[]> {
    const limit = Math.min(Math.max(options.limit, 1), 200);
    const pattern = `%${options.q}%`;
    const rows = options.kind
      ? await this.sql`
          select
            o.kind,
            o.content_digest,
            coalesce(
              o.canonical_json->>'display_name',
              o.canonical_json->>'name',
              o.canonical_json->>'id',
              min(a.alias),
              o.content_digest
            ) as display_name,
            coalesce(array_remove(array_agg(distinct a.alias), null), array[]::text[]) as aliases,
            max(case
              when a.alias ilike ${pattern} then 1.0
              when o.canonical_json->>'display_name' ilike ${pattern} then 0.9
              when o.canonical_json->>'name' ilike ${pattern} then 0.8
              when o.canonical_json->>'id' ilike ${pattern} then 0.7
              else 0.2
            end) as score,
            '{}'::jsonb as metadata
          from registry.content_objects o
          left join registry.entity_aliases a
            on a.content_digest = o.content_digest
           and a.retired_at is null
          where o.kind = ${options.kind}
            and (
              o.content_digest = ${options.q}
              or a.alias ilike ${pattern}
              or o.canonical_json->>'display_name' ilike ${pattern}
              or o.canonical_json->>'name' ilike ${pattern}
              or o.canonical_json->>'id' ilike ${pattern}
            )
          group by o.kind, o.content_digest, o.canonical_json
          order by score desc, display_name asc
          limit ${limit}
        `
      : await this.sql`
          select
            o.kind,
            o.content_digest,
            coalesce(
              o.canonical_json->>'display_name',
              o.canonical_json->>'name',
              o.canonical_json->>'id',
              min(a.alias),
              o.content_digest
            ) as display_name,
            coalesce(array_remove(array_agg(distinct a.alias), null), array[]::text[]) as aliases,
            max(case
              when a.alias ilike ${pattern} then 1.0
              when o.canonical_json->>'display_name' ilike ${pattern} then 0.9
              when o.canonical_json->>'name' ilike ${pattern} then 0.8
              when o.canonical_json->>'id' ilike ${pattern} then 0.7
              else 0.2
            end) as score,
            '{}'::jsonb as metadata
          from registry.content_objects o
          left join registry.entity_aliases a
            on a.content_digest = o.content_digest
           and a.retired_at is null
          where o.content_digest = ${options.q}
             or a.alias ilike ${pattern}
             or o.canonical_json->>'display_name' ilike ${pattern}
             or o.canonical_json->>'name' ilike ${pattern}
             or o.canonical_json->>'id' ilike ${pattern}
          group by o.kind, o.content_digest, o.canonical_json
          order by score desc, display_name asc
          limit ${limit}
        `;
    return rows as unknown as RegistrySearchHit[];
  }
}

function isUniqueViolation(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    (error as { code?: unknown }).code === "23505"
  );
}


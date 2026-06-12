import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { CanonicalEntity, EntityKind } from "../primitives";

export interface RegistrySearchOptions {
  kind?: EntityKind;
  q: string;
  limit: number;
  ownerKey?: string | undefined;
}

export interface RegistrySearchHit {
  kind: EntityKind;
  content_digest: string;
  display_name: string;
  aliases: string[];
  score: number;
  metadata: Record<string, unknown>;
  used_by_runs: number;
  last_used_at: string | null;
}

export interface RegistryUsage {
  used_by_runs: number;
  last_used_at: string | null;
}

export interface AliasReview {
  alias: string;
  scope_type: string;
  scope_id: string | null;
  status: "available" | "already_points_here" | "conflicts";
  existing_digest: string | null;
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

  async usageForDigest(digest: string, ownerKey?: string): Promise<RegistryUsage> {
    const usage = await this.usageForDigests([digest], ownerKey);
    return usage.get(digest) ?? { used_by_runs: 0, last_used_at: null };
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

  async reviewAliases(input: {
    kind: EntityKind;
    contentDigest: string;
    aliases: Array<{ alias: string; scopeType: string; scopeId?: string | null }>;
  }): Promise<AliasReview[]> {
    const reviews: AliasReview[] = [];
    for (const alias of input.aliases) {
      const scope = Object.assign(
        { scopeType: alias.scopeType },
        alias.scopeId === undefined ? {} : { scopeId: alias.scopeId },
      );
      const existingDigest = await this.resolveAlias(input.kind, alias.alias, scope);
      reviews.push({
        alias: alias.alias,
        scope_type: alias.scopeType,
        scope_id: alias.scopeId ?? null,
        existing_digest: existingDigest,
        status: existingDigest === null
          ? "available"
          : existingDigest === input.contentDigest
            ? "already_points_here"
            : "conflicts",
      });
    }
    return reviews;
  }

  async search(options: RegistrySearchOptions): Promise<RegistrySearchHit[]> {
    const limit = Math.min(Math.max(options.limit, 1), 200);
    const query = options.q.trim();
    if (!query) {
      const rows = options.kind
        ? await this.sql`
            select
              o.kind,
              o.content_digest,
              o.schema_version,
              o.canonical_size_bytes,
              o.created_at,
              o.created_by,
              coalesce(
                o.canonical_json->>'display_name',
                o.canonical_json->>'name',
                o.canonical_json->>'id',
                min(a.alias),
                o.content_digest
              ) as display_name,
              coalesce(array_remove(array_agg(distinct a.alias), null), array[]::text[]) as aliases,
              0.0 as score,
              '{}'::jsonb as metadata
            from registry.content_objects o
            left join registry.entity_aliases a
              on a.content_digest = o.content_digest
             and a.retired_at is null
            where o.kind = ${options.kind}
            group by o.kind, o.content_digest, o.schema_version, o.canonical_size_bytes, o.created_at, o.created_by, o.canonical_json
            order by o.created_at desc, display_name asc
            limit ${limit}
          `
        : await this.sql`
            select
              o.kind,
              o.content_digest,
              o.schema_version,
              o.canonical_size_bytes,
              o.created_at,
              o.created_by,
              coalesce(
                o.canonical_json->>'display_name',
                o.canonical_json->>'name',
                o.canonical_json->>'id',
                min(a.alias),
                o.content_digest
              ) as display_name,
              coalesce(array_remove(array_agg(distinct a.alias), null), array[]::text[]) as aliases,
              0.0 as score,
              '{}'::jsonb as metadata
            from registry.content_objects o
            left join registry.entity_aliases a
              on a.content_digest = o.content_digest
             and a.retired_at is null
            group by o.kind, o.content_digest, o.schema_version, o.canonical_size_bytes, o.created_at, o.created_by, o.canonical_json
            order by o.created_at desc, display_name asc
            limit ${limit}
          `;
      return await this.withUsage(rows as unknown as RegistrySearchHit[], options.ownerKey);
    }
    const pattern = `%${query}%`;
    const rows = options.kind
      ? await this.sql`
          select
            o.kind,
            o.content_digest,
            o.schema_version,
            o.canonical_size_bytes,
            o.created_at,
            o.created_by,
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
              o.content_digest = ${query}
              or a.alias ilike ${pattern}
              or o.canonical_json->>'display_name' ilike ${pattern}
              or o.canonical_json->>'name' ilike ${pattern}
              or o.canonical_json->>'id' ilike ${pattern}
            )
          group by o.kind, o.content_digest, o.schema_version, o.canonical_size_bytes, o.created_at, o.created_by, o.canonical_json
          order by score desc, display_name asc
          limit ${limit}
        `
      : await this.sql`
          select
            o.kind,
            o.content_digest,
            o.schema_version,
            o.canonical_size_bytes,
            o.created_at,
            o.created_by,
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
          where o.content_digest = ${query}
             or a.alias ilike ${pattern}
             or o.canonical_json->>'display_name' ilike ${pattern}
             or o.canonical_json->>'name' ilike ${pattern}
             or o.canonical_json->>'id' ilike ${pattern}
          group by o.kind, o.content_digest, o.schema_version, o.canonical_size_bytes, o.created_at, o.created_by, o.canonical_json
          order by score desc, display_name asc
          limit ${limit}
        `;
    return await this.withUsage(rows as unknown as RegistrySearchHit[], options.ownerKey);
  }

  private async withUsage(rows: RegistrySearchHit[], ownerKey?: string): Promise<RegistrySearchHit[]> {
    const usage = await this.usageForDigests(rows.map((row) => row.content_digest), ownerKey);
    return rows.map((row) => ({
      ...row,
      ...(usage.get(row.content_digest) ?? { used_by_runs: 0, last_used_at: null }),
    }));
  }

  private async usageForDigests(digests: string[], ownerKey?: string): Promise<Map<string, RegistryUsage>> {
    const uniqueDigests = [...new Set(digests)];
    if (uniqueDigests.length === 0) {
      return new Map();
    }
    const rows = await this.sql`
      with requested(content_digest) as (
        select unnest(${uniqueDigests}::text[])
      ),
      package_registry_digests as (
        select
          artifact.package_digest,
          artifact.package_digest as content_digest
        from cloud.package_artifacts artifact
        where artifact.package_digest = any(${uniqueDigests}::text[])
        union
        select
          artifact.package_digest,
          trim(both '"' from digest_value::text) as content_digest
        from cloud.package_artifacts artifact
        cross join lateral jsonb_path_query(
          artifact.resolved_experiment_json,
          'lax $.**.__cloud.digest'
        ) as digest_value
        where trim(both '"' from digest_value::text) = any(${uniqueDigests}::text[])
      ),
      usage as (
        select
          package_registry_digests.content_digest,
          count(distinct run.run_id)::int as used_by_runs,
          max(run.created_at) as last_used_at
        from package_registry_digests
        join cloud.runs run
          on run.package_digest = package_registry_digests.package_digest
        where (${ownerKey ?? null}::text is null or run.owner_key = ${ownerKey ?? null})
        group by package_registry_digests.content_digest
      )
      select
        requested.content_digest,
        coalesce(usage.used_by_runs, 0)::int as used_by_runs,
        usage.last_used_at
      from requested
      left join usage
        on usage.content_digest = requested.content_digest
    `;
    return new Map(rows.map((row) => {
      const usage = row as { content_digest: string; used_by_runs: number | string; last_used_at: string | null };
      return [
        usage.content_digest,
        {
          used_by_runs: typeof usage.used_by_runs === "number"
            ? usage.used_by_runs
            : Number.parseInt(usage.used_by_runs, 10),
          last_used_at: usage.last_used_at,
        },
      ];
    }));
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

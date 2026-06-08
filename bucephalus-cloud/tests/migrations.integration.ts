import { describe, expect, test } from "bun:test";
import { createSql } from "../src/db/client";
import type { Sql } from "../src/db/client";
import { migrationFiles, runMigrations } from "../src/db/migrate";

const defaultDatabaseUrl = "postgres://bucephalus:bucephalus_dev@localhost:55432/bucephalus_cloud";

function migrationTestBaseUrl(): string {
  return process.env.BUCEPHALUS_MIGRATION_TEST_DATABASE_URL
    ?? process.env.DATABASE_URL
    ?? defaultDatabaseUrl;
}

function requireSafeDatabaseServer(databaseUrl: string): void {
  const parsed = new URL(databaseUrl);
  if (parsed.protocol !== "postgres:" && parsed.protocol !== "postgresql:") {
    throw new Error(`Migration tests require a postgres URL, got: ${parsed.protocol}`);
  }

  const host = parsed.hostname.toLowerCase();
  const isLocal = host === "localhost" || host === "127.0.0.1" || host === "::1";
  if (!isLocal && process.env.BUCEPHALUS_ALLOW_REMOTE_MIGRATION_TESTS !== "true") {
    throw new Error(
      "Refusing to create a scratch migration-test database on a non-local host. "
        + "Set BUCEPHALUS_MIGRATION_TEST_DATABASE_URL to a local/CI Postgres URL, "
        + "or set BUCEPHALUS_ALLOW_REMOTE_MIGRATION_TESTS=true for an intentional staging rehearsal.",
    );
  }
}

function databaseUrlFor(databaseUrl: string, databaseName: string): string {
  const parsed = new URL(databaseUrl);
  parsed.pathname = `/${databaseName}`;
  return parsed.toString();
}

function adminDatabaseUrlFor(databaseUrl: string): string {
  return databaseUrlFor(databaseUrl, "postgres");
}

function quoteIdentifier(value: string): string {
  if (!/^[a-z_][a-z0-9_]*$/.test(value)) {
    throw new Error(`Unsafe test database identifier: ${value}`);
  }
  return `"${value}"`;
}

async function createScratchDatabase(baseUrl: string): Promise<{ databaseName: string; databaseUrl: string; adminSql: Sql }> {
  requireSafeDatabaseServer(baseUrl);
  const adminSql = createSql(adminDatabaseUrlFor(baseUrl));
  const databaseName = `bucephalus_migration_test_${Date.now()}_${Math.random().toString(16).slice(2)}`;
  const quotedDatabaseName = quoteIdentifier(databaseName);
  try {
    await adminSql.unsafe(`create database ${quotedDatabaseName}`);
    return {
      databaseName,
      databaseUrl: databaseUrlFor(baseUrl, databaseName),
      adminSql,
    };
  } catch (error) {
    await adminSql.end();
    throw error;
  }
}

async function dropScratchDatabase(adminSql: Sql, databaseName: string): Promise<void> {
  await adminSql.unsafe(`drop database if exists ${quoteIdentifier(databaseName)} with (force)`);
}

async function expectRegclass(sql: Sql, name: string): Promise<void> {
  const [row] = await sql<{ regclass: string | null }[]>`
    select to_regclass(${name})::text as regclass
  `;
  expect(row?.regclass).toBe(name);
}

describe("cloud SQL migrations", () => {
  test("apply from empty Postgres and are idempotent", async () => {
    const baseUrl = migrationTestBaseUrl();
    const scratch = await createScratchDatabase(baseUrl);

    try {
      const expectedMigrations = await migrationFiles();
      await runMigrations({ databaseUrl: scratch.databaseUrl, runtimeRoleName: null });

      const sql = createSql(scratch.databaseUrl);
      try {
        const migrationRows = await sql<{ migration_name: string }[]>`
          select migration_name
          from cloud_schema_migrations
          order by migration_name
        `;
        expect(migrationRows.map((row) => row.migration_name)).toEqual(expectedMigrations);

        const extensionRows = await sql<{ extname: string }[]>`
          select extname from pg_extension where extname = 'pgcrypto'
        `;
        expect(extensionRows).toHaveLength(1);

        const requiredSchemas = ["registry", "fact", "ingest", "cloud", "bucephalus_runtime"];
        const schemaRows = await sql<{ schema_name: string }[]>`
          select schema_name
          from information_schema.schemata
          where schema_name in ${sql(requiredSchemas)}
          order by schema_name
        `;
        expect(schemaRows.map((row) => row.schema_name)).toEqual([...requiredSchemas].sort());

        for (const tableName of [
          "registry.content_objects",
          "ingest.uploads",
          "ingest.import_jobs",
          "cloud.package_artifacts",
          "cloud.runs",
          "cloud.run_attempts",
          "cloud.runner_pools",
          "cloud.runner_instances",
          "cloud.runner_provision_requests",
          "cloud.latch_submissions",
          "bucephalus_runtime.runs",
          "bucephalus_runtime.trial_rows",
          "bucephalus_runtime.trial_conclusion_rows",
        ]) {
          await expectRegclass(sql, tableName);
        }

        const contentKindRows = await sql<{ enumlabel: string }[]>`
          select e.enumlabel
          from pg_type t
          join pg_namespace n on n.oid = t.typnamespace
          join pg_enum e on e.enumtypid = t.oid
          where n.nspname = 'registry'
            and t.typname = 'content_kind'
          order by e.enumsortorder
        `;
        expect(contentKindRows.map((row) => row.enumlabel)).toContain("benchmark");

        const packageDigest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        await sql`
          insert into cloud.package_artifacts (
            package_digest,
            manifest_json,
            resolved_experiment_json
          )
          values (
            ${packageDigest},
            ${sql.json({ schema_version: "manifest_v1" })},
            ${sql.json({ schema_version: "resolved_experiment_v1" })}
          )
        `;
        await sql`
          insert into cloud.runs (package_digest, run_label)
          values (${packageDigest}, 'migration-gate')
        `;

        await runMigrations({ databaseUrl: scratch.databaseUrl, runtimeRoleName: null });

        const [runCount] = await sql<{ row_count: number }[]>`
          select count(*)::int as row_count
          from cloud.runs
          where package_digest = ${packageDigest}
        `;
        expect(runCount?.row_count).toBe(1);

        const migrationRowsAfterSecondRun = await sql<{ migration_name: string }[]>`
          select migration_name
          from cloud_schema_migrations
          order by migration_name
        `;
        expect(migrationRowsAfterSecondRun.map((row) => row.migration_name)).toEqual(expectedMigrations);
      } finally {
        await sql.end();
      }
    } finally {
      await dropScratchDatabase(scratch.adminSql, scratch.databaseName);
      await scratch.adminSql.end();
    }
  }, 60_000);
});

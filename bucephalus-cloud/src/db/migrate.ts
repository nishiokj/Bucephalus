import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { createSql } from "./client";
import type { Sql } from "./client";

const migrationsDir = new URL("../../db/migrations", import.meta.url).pathname;
const runtimeRoleName = process.env.BUCEPHALUS_RUNTIME_DATABASE_ROLE?.trim();
const runtimeSchemas = ["registry", "fact", "ingest", "cloud", "bucephalus_runtime"];

function quoteIdentifier(value: string): string {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(value)) {
    throw new Error(`Invalid database identifier: ${value}`);
  }
  return `"${value.replaceAll('"', '""')}"`;
}

async function grantRuntimeRole(sql: Sql, roleName: string): Promise<void> {
  const role = quoteIdentifier(roleName);
  const [databaseRow] = await sql<{ database_name: string }[]>`
    select current_database() as database_name
  `;
  if (!databaseRow) {
    throw new Error("Could not determine current database for runtime grants.");
  }
  const databaseName = databaseRow.database_name;
  const database = quoteIdentifier(databaseName);

  await sql.unsafe(`grant connect on database ${database} to ${role}`);

  const schemas = await sql<{ schema_name: string }[]>`
    select schema_name
    from information_schema.schemata
    where schema_name in ${sql(runtimeSchemas)}
    order by schema_name
  `;

  for (const { schema_name: schemaName } of schemas) {
    const schema = quoteIdentifier(schemaName);
    await sql.unsafe(`grant usage on schema ${schema} to ${role}`);
    await sql.unsafe(`grant select, insert, update, delete on all tables in schema ${schema} to ${role}`);
    await sql.unsafe(`grant usage, select, update on all sequences in schema ${schema} to ${role}`);
    await sql.unsafe(`alter default privileges in schema ${schema} grant select, insert, update, delete on tables to ${role}`);
    await sql.unsafe(`alter default privileges in schema ${schema} grant usage, select, update on sequences to ${role}`);
  }
}

export async function runMigrations(): Promise<void> {
  const sql = createSql();
  try {
    await sql`
      create table if not exists cloud_schema_migrations (
        migration_name text primary key,
        applied_at timestamptz not null default now()
      )
    `;

    const files = (await readdir(migrationsDir))
      .filter((file) => file.endsWith(".sql"))
      .sort();

    for (const file of files) {
      const existing = await sql`
        select migration_name
        from cloud_schema_migrations
        where migration_name = ${file}
      `;
      if (existing.length > 0) {
        console.log(`migration already applied: ${file}`);
        continue;
      }

      const body = await readFile(join(migrationsDir, file), "utf8");
      await sql.begin(async (tx) => {
        await tx.unsafe(body);
        await tx`
          insert into cloud_schema_migrations (migration_name)
          values (${file})
        `;
      });
      console.log(`migration applied: ${file}`);
    }

    if (runtimeRoleName) {
      await grantRuntimeRole(sql, runtimeRoleName);
      console.log(`runtime database grants applied for role: ${runtimeRoleName}`);
    }
  } finally {
    await sql.end();
  }
}

if (import.meta.main) {
  runMigrations().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}

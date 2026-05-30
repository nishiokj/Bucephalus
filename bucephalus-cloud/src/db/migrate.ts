import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { createSql } from "./client";

const migrationsDir = new URL("../../db/migrations", import.meta.url).pathname;

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

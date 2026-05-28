import postgres from "postgres";
import { loadConfig } from "../config";

export type Sql = ReturnType<typeof postgres>;

export function createSql(databaseUrl = loadConfig().databaseUrl): Sql {
  return postgres(databaseUrl, {
    max: 10,
    idle_timeout: 20,
    connect_timeout: 5,
  });
}

export async function checkDatabase(sql: Sql): Promise<void> {
  await sql`select 1 as ok`;
}


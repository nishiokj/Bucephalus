import { describe, expect, test } from "bun:test";
import { createSql, type Sql } from "../src/db/client";
import { runMigrations } from "../src/db/migrate";
import { RuntimeRepository, type RuntimeEventRowInsert } from "../src/runtime/repository";

const defaultDatabaseUrl = "postgres://bucephalus:bucephalus_dev@127.0.0.1:55432/bucephalus_cloud";
const CORE_RUN_ID = "run_20260611_000001_000001_000001";
const PACKAGE_DIGEST = `sha256:${"a".repeat(64)}`;

function baseUrl(): string {
  return process.env.BUCEPHALUS_MIGRATION_TEST_DATABASE_URL
    ?? process.env.DATABASE_URL
    ?? defaultDatabaseUrl;
}

function databaseUrlFor(base: string, databaseName: string): string {
  const parsed = new URL(base);
  parsed.pathname = `/${databaseName}`;
  return parsed.toString();
}

function quoteIdentifier(value: string): string {
  if (!/^[a-z_][a-z0-9_]*$/.test(value)) {
    throw new Error(`Unsafe test database identifier: ${value}`);
  }
  return `"${value}"`;
}

async function withScratchDatabase(run: (sql: Sql) => Promise<void>): Promise<void> {
  const base = baseUrl();
  const host = new URL(base).hostname.toLowerCase();
  if (host !== "localhost" && host !== "127.0.0.1" && host !== "::1") {
    throw new Error("Runtime ingest integration tests require a local Postgres");
  }
  const adminSql = createSql(databaseUrlFor(base, "postgres"));
  const databaseName = `bucephalus_ingest_test_${Date.now()}_${Math.random().toString(16).slice(2)}`;
  await adminSql.unsafe(`create database ${quoteIdentifier(databaseName)}`);
  try {
    const databaseUrl = databaseUrlFor(base, databaseName);
    await runMigrations({ databaseUrl, runtimeRoleName: null });
    const sql = createSql(databaseUrl);
    try {
      await run(sql);
    } finally {
      await sql.end();
    }
  } finally {
    await adminSql.unsafe(`drop database if exists ${quoteIdentifier(databaseName)} with (force)`);
    await adminSql.end();
  }
}

async function seedRunWithAnnouncement(sql: Sql): Promise<string> {
  await sql`
    insert into cloud.package_artifacts (package_digest, manifest_json, resolved_experiment_json)
    values (${PACKAGE_DIGEST}, ${sql.json({})}, ${sql.json({})})
  `;
  const [run] = await sql`
    insert into cloud.runs (package_digest, run_label, status)
    values (${PACKAGE_DIGEST}, ${"ingest-test"}, ${"running"})
    returning run_id
  `;
  const cloudRunId = String(run!.run_id);
  await sql`
    insert into cloud.run_events (run_id, seq, event_type, payload)
    values (${cloudRunId}, 1, ${"worker.runtime.core_runs_discovered"}, ${sql.json({ core_run_ids: [CORE_RUN_ID] })})
  `;
  return cloudRunId;
}

function row(rowSeq: number, overrides: Partial<RuntimeEventRowInsert> = {}): RuntimeEventRowInsert {
  return {
    core_run_id: CORE_RUN_ID,
    trial_id: "trial-001",
    schedule_idx: 0,
    attempt: 0,
    row_seq: rowSeq,
    slot_commit_id: "",
    variant_id: "unknown",
    task_id: "unknown",
    repl_idx: 0,
    seq: rowSeq,
    event_type: "agent.step",
    ts: `2026-06-11T00:00:0${rowSeq % 10}Z`,
    payload: { event_type: "agent.step", seq: rowSeq },
    row: { source: "worker_live_ingest" },
    ...overrides,
  };
}

describe("runtime ingest against Postgres", () => {
  test("live rows land idempotently, enrich in place, and read back once announced", async () => {
    await withScratchDatabase(async (sql) => {
      const runtime = new RuntimeRepository(sql);
      const cloudRunId = await seedRunWithAnnouncement(sql);

      // First batch lands.
      await runtime.upsertEventRows([row(0), row(1), row(2)]);
      let events = await runtime.eventRows(cloudRunId, { limit: 100 });
      expect(events.map((event) => event.row_seq)).toEqual([0, 1, 2]);

      // A verbatim replay (worker retry after a network failure) changes nothing.
      await runtime.upsertEventRows([row(0), row(1), row(2)]);
      events = await runtime.eventRows(cloudRunId, { limit: 100 });
      expect(events).toHaveLength(3);

      // The enrichment pass refreshes attribution on the same keys.
      await runtime.upsertEventRows([
        row(0, { variant_id: "sonnet-4-6", task_id: "task-9" }),
        row(1, { variant_id: "sonnet-4-6", task_id: "task-9" }),
        row(2, { variant_id: "sonnet-4-6", task_id: "task-9" }),
      ]);
      events = await runtime.eventRows(cloudRunId, { limit: 100 });
      expect(events).toHaveLength(3);
      expect(events.every((event) => event.variant_id === "sonnet-4-6")).toBe(true);

      // after_row_seq pagination still works over live rows.
      const tail = await runtime.eventRows(cloudRunId, { limit: 100, afterRowSeq: 1 });
      expect(tail.map((event) => event.row_seq)).toEqual([2]);
    });
  }, 30_000);

  test("worker lifecycle events expose markers but suppress snapshot payload bodies", async () => {
    await withScratchDatabase(async (sql) => {
      const runtime = new RuntimeRepository(sql);
      const cloudRunId = await seedRunWithAnnouncement(sql);
      await sql`
        insert into cloud.run_events (run_id, seq, event_type, payload)
        values
          (${cloudRunId}, 2, ${"worker.core.starting"}, ${sql.json({ run_root_dir: "/data/run" })}),
          (${cloudRunId}, 3, ${"worker.runtime.snapshot"}, ${sql.json({ core_run_id: CORE_RUN_ID, trial_summaries: [{ huge: "payload" }] })})
      `;

      const events = await runtime.workerLifecycleEvents(cloudRunId);
      expect(events.map((event) => event.event_type)).toEqual([
        "worker.runtime.core_runs_discovered",
        "worker.core.starting",
        "worker.runtime.snapshot",
      ]);
      const snapshot = events.at(-1)!;
      expect(snapshot.payload).toEqual({ note: "runtime snapshot payload omitted from event stream" });
      expect(events[1]!.payload).toEqual({ run_root_dir: "/data/run" });
    });
  }, 30_000);

  test("rows for an unannounced core run stay invisible until discovery", async () => {
    await withScratchDatabase(async (sql) => {
      const runtime = new RuntimeRepository(sql);
      await sql`
        insert into cloud.package_artifacts (package_digest, manifest_json, resolved_experiment_json)
        values (${PACKAGE_DIGEST}, ${sql.json({})}, ${sql.json({})})
      `;
      const [run] = await sql`
        insert into cloud.runs (package_digest, status) values (${PACKAGE_DIGEST}, ${"running"})
        returning run_id
      `;
      const cloudRunId = String(run!.run_id);

      await runtime.upsertEventRows([row(0)]);
      expect(await runtime.eventRows(cloudRunId, { limit: 10 })).toHaveLength(0);

      await sql`
        insert into cloud.run_events (run_id, seq, event_type, payload)
        values (${cloudRunId}, 1, ${"worker.runtime.core_runs_discovered"}, ${sql.json({ core_run_ids: [CORE_RUN_ID] })})
      `;
      expect(await runtime.eventRows(cloudRunId, { limit: 10 })).toHaveLength(1);
    });
  }, 30_000);
});

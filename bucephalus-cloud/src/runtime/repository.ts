import type { Sql } from "../db/client";
import type { JsonObject } from "../primitives";

export interface RuntimeSummary {
  cloud_run_id: string;
  core_run_ids: string[];
  run_controls: JsonObject[];
  schedule_progress: JsonObject[];
  active_slots: RuntimeSlotRecord[];
  recent_events: RuntimeEventRecord[];
}

export interface RuntimeSlotRecord {
  core_run_id: string;
  schedule_idx: number;
  state: string;
  trial_id: string | null;
  attempt: number;
  worker_id: string | null;
  owner_id: string | null;
  lease_expires_at: string | null;
  slot_commit_id: string | null;
  slot_status: string | null;
  slot: JsonObject;
}

export interface RuntimeEventRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  row_seq: number;
  slot_commit_id: string;
  variant_id: string;
  task_id: string;
  repl_idx: number;
  seq: number;
  event_type: string;
  ts: string | null;
  payload: JsonObject;
  row: JsonObject;
}

export class RuntimeRepository {
  private readonly schema: string;

  constructor(private readonly sql: Sql, schema = process.env.BUCEPHALUS_RUN_STORE_SCHEMA ?? "bucephalus_runtime") {
    this.schema = validateIdentifier(schema.trim() || "bucephalus_runtime");
  }

  async getSummary(cloudRunId: string): Promise<RuntimeSummary> {
    const coreRunIds = await this.coreRunIdsForCloudRun(cloudRunId);
    const [runControls, scheduleProgress, activeSlots, recentEvents] = await Promise.all([
      this.runtimeValues(coreRunIds, "run_control_v2"),
      this.runtimeValues(coreRunIds, "schedule_progress_v2"),
      this.activeSlots(coreRunIds),
      this.eventRows(cloudRunId, { limit: 50 }),
    ]);
    return {
      cloud_run_id: cloudRunId,
      core_run_ids: coreRunIds,
      run_controls: runControls,
      schedule_progress: scheduleProgress,
      active_slots: activeSlots,
      recent_events: recentEvents,
    };
  }

  async runtimeValue(cloudRunId: string, key: string): Promise<JsonObject[]> {
    const coreRunIds = await this.coreRunIdsForCloudRun(cloudRunId);
    return this.runtimeValues(coreRunIds, key);
  }

  async eventRows(cloudRunId: string, input?: { limit?: number; afterRowSeq?: number | undefined }): Promise<RuntimeEventRecord[]> {
    const coreRunIds = await this.coreRunIdsForCloudRun(cloudRunId);
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        row_seq,
        slot_commit_id,
        variant_id,
        task_id,
        repl_idx,
        seq,
        event_type,
        ts,
        payload_json,
        row_json
      from ${this.table("event_rows")}
      where run_id = any(${coreRunIds})
        and row_seq > ${input?.afterRowSeq ?? -1}
      order by run_id, schedule_idx, attempt, row_seq
      limit ${boundedLimit(input?.limit, 200)}
    `;
    return rows.map((row) => ({
      core_run_id: String(row.run_id),
      trial_id: String(row.trial_id),
      schedule_idx: Number(row.schedule_idx),
      attempt: Number(row.attempt),
      row_seq: Number(row.row_seq),
      slot_commit_id: String(row.slot_commit_id),
      variant_id: String(row.variant_id),
      task_id: String(row.task_id),
      repl_idx: Number(row.repl_idx),
      seq: Number(row.seq),
      event_type: String(row.event_type),
      ts: row.ts === null ? null : String(row.ts),
      payload: parseObject(row.payload_json),
      row: parseObject(row.row_json),
    }));
  }

  private async coreRunIdsForCloudRun(cloudRunId: string): Promise<string[]> {
    const rows = await this.sql`
      with run_roots as (
        select distinct payload->>'run_root_dir' as run_root_dir
        from cloud.run_events
        where run_id = ${cloudRunId}
          and payload ? 'run_root_dir'
          and nullif(payload->>'run_root_dir', '') is not null
      ),
      cleanup_ids as (
        select distinct core_ids.core_run_id
        from cloud.run_events events
        cross join lateral jsonb_array_elements_text(events.payload->'core_run_ids') as core_ids(core_run_id)
        where events.run_id = ${cloudRunId}
          and jsonb_typeof(events.payload->'core_run_ids') = 'array'
      ),
      discovered_ids as (
        select distinct runtime_runs.run_id as core_run_id
        from ${this.table("runs")} runtime_runs
        join run_roots on runtime_runs.run_dir like run_roots.run_root_dir || '/%'
      )
      select core_run_id from discovered_ids
      union
      select core_run_id from cleanup_ids
      order by core_run_id
    `;
    return rows.map((row) => String(row.core_run_id));
  }

  private async runtimeValues(coreRunIds: string[], key: string): Promise<JsonObject[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select value_json
      from ${this.table("runtime_kv")}
      where run_id = any(${coreRunIds})
        and key = ${key}
      order by updated_at_ms desc
    `;
    return rows.map((row) => parseObject(row.value_json));
  }

  private async activeSlots(coreRunIds: string[]): Promise<RuntimeSlotRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        schedule_idx,
        state,
        trial_id,
        attempt,
        worker_id,
        owner_id,
        lease_expires_at,
        slot_commit_id,
        slot_status,
        slot_json
      from ${this.table("schedule_slots")}
      where run_id = any(${coreRunIds})
        and state in ('active', 'committed')
      order by run_id, schedule_idx
      limit 200
    `;
    return rows.map((row) => ({
      core_run_id: String(row.run_id),
      schedule_idx: Number(row.schedule_idx),
      state: String(row.state),
      trial_id: row.trial_id === null ? null : String(row.trial_id),
      attempt: Number(row.attempt),
      worker_id: row.worker_id === null ? null : String(row.worker_id),
      owner_id: row.owner_id === null ? null : String(row.owner_id),
      lease_expires_at: row.lease_expires_at === null ? null : String(row.lease_expires_at),
      slot_commit_id: row.slot_commit_id === null ? null : String(row.slot_commit_id),
      slot_status: row.slot_status === null ? null : String(row.slot_status),
      slot: parseObject(row.slot_json),
    }));
  }

  private table(name: string): ReturnType<Sql["unsafe"]> {
    return this.sql.unsafe(`${quoteIdentifier(this.schema)}.${quoteIdentifier(name)}`);
  }
}

function boundedLimit(value: number | undefined, fallback: number): number {
  if (!Number.isFinite(value) || value === undefined) {
    return fallback;
  }
  return Math.max(1, Math.min(Math.trunc(value), 1000));
}

function parseObject(value: unknown): JsonObject {
  const parsed = typeof value === "string" ? JSON.parse(value) : value;
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return {};
  }
  return parsed as JsonObject;
}

function validateIdentifier(value: string): string {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(value)) {
    throw new Error(`Invalid Postgres identifier '${value}'`);
  }
  return value;
}

function quoteIdentifier(value: string): string {
  return `"${validateIdentifier(value).replaceAll('"', '""')}"`;
}

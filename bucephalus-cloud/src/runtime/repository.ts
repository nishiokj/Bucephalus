import type { Sql } from "../db/client";
import type { JsonObject, JsonValue } from "../primitives";

export interface RuntimeSummary {
  cloud_run_id: string;
  core_run_ids: string[];
  run_controls: JsonObject[];
  schedule_progress: JsonObject[];
  active_slots: RuntimeSlotRecord[];
  recent_events: RuntimeEventRecord[];
}

export interface RuntimeResults {
  cloud_run_id: string;
  core_run_ids: string[];
  trial_results: RuntimeTrialResultRecord[];
  metric_observations: RuntimeMetricObservationRecord[];
  contract_stages: RuntimeContractStageRecord[];
  attempt_objects: RuntimeAttemptObjectRecord[];
}

export interface RuntimeTrialResultRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  row_seq: number;
  variant_id: string;
  task_id: string;
  repl_idx: number;
  outcome: string;
  primary_metric_name: string;
  primary_metric_value: JsonValue;
  metrics: JsonValue;
  bindings: JsonValue;
  events_total: number;
  has_events: boolean;
  row: JsonObject;
}

export interface RuntimeMetricObservationRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  row_seq: number;
  variant_id: string;
  task_id: string;
  repl_idx: number;
  outcome: string;
  metric_name: string;
  metric_value: JsonValue;
  metric_source: string | null;
  row: JsonObject;
}

export interface RuntimeContractStageRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  row_seq: number;
  variant_id: string;
  task_id: string;
  repl_idx: number;
  stage: string;
  status: string;
  recorded_at: string;
  detail: JsonValue;
  row: JsonObject;
}

export interface RuntimeAttemptObjectRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  role: string;
  object_ref: string;
  metadata: JsonValue | null;
  recorded_at_ms: number;
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

  async results(cloudRunId: string, input?: { limit?: number }): Promise<RuntimeResults> {
    const coreRunIds = await this.coreRunIdsForCloudRun(cloudRunId);
    if (coreRunIds.length === 0) {
      return {
        cloud_run_id: cloudRunId,
        core_run_ids: [],
        trial_results: [],
        metric_observations: [],
        contract_stages: [],
        attempt_objects: [],
      };
    }
    const limit = boundedLimit(input?.limit, 500);
    const [trialRows, metricRows, contractStageRows, attemptObjectRows] = await Promise.all([
      this.sql`
        select
          run_id,
          trial_id,
          schedule_idx,
          attempt,
          row_seq,
          variant_id,
          task_id,
          repl_idx,
          outcome,
          primary_metric_name,
          primary_metric_value_json,
          metrics_json,
          bindings_json,
          events_total,
          has_events,
          row_json
        from ${this.table("trial_rows")}
        where run_id = any(${coreRunIds})
        order by run_id, schedule_idx, attempt, row_seq
        limit ${limit}
      `,
      this.sql`
        select
          run_id,
          trial_id,
          schedule_idx,
          attempt,
          row_seq,
          variant_id,
          task_id,
          repl_idx,
          outcome,
          metric_name,
          metric_value_json,
          metric_source,
          row_json
        from ${this.table("metric_rows")}
        where run_id = any(${coreRunIds})
        order by run_id, schedule_idx, attempt, row_seq
        limit ${limit * 10}
      `,
      this.sql`
        select
          run_id,
          trial_id,
          schedule_idx,
          attempt,
          row_seq,
          variant_id,
          task_id,
          repl_idx,
          stage,
          status,
          recorded_at,
          detail_json,
          row_json
        from ${this.table("contract_stage_rows")}
        where run_id = any(${coreRunIds})
        order by run_id, schedule_idx, attempt, row_seq
        limit ${limit * 10}
      `,
      this.sql`
        select
          run_id,
          trial_id,
          schedule_idx,
          attempt,
          role,
          object_ref,
          metadata_json,
          recorded_at_ms
        from ${this.table("attempt_objects")}
        where run_id = any(${coreRunIds})
        order by run_id, schedule_idx, attempt, role
        limit ${limit * 10}
      `,
    ]);
    return {
      cloud_run_id: cloudRunId,
      core_run_ids: coreRunIds,
      trial_results: trialRows.map((row) => ({
        core_run_id: String(row.run_id),
        trial_id: String(row.trial_id),
        schedule_idx: Number(row.schedule_idx),
        attempt: Number(row.attempt),
        row_seq: Number(row.row_seq),
        variant_id: String(row.variant_id),
        task_id: String(row.task_id),
        repl_idx: Number(row.repl_idx),
        outcome: String(row.outcome),
        primary_metric_name: String(row.primary_metric_name),
        primary_metric_value: parseJson(row.primary_metric_value_json),
        metrics: parseJson(row.metrics_json),
        bindings: parseJson(row.bindings_json),
        events_total: Number(row.events_total),
        has_events: Boolean(Number(row.has_events)),
        row: parseObject(row.row_json),
      })),
      metric_observations: metricRows.map((row) => ({
        core_run_id: String(row.run_id),
        trial_id: String(row.trial_id),
        schedule_idx: Number(row.schedule_idx),
        attempt: Number(row.attempt),
        row_seq: Number(row.row_seq),
        variant_id: String(row.variant_id),
        task_id: String(row.task_id),
        repl_idx: Number(row.repl_idx),
        outcome: String(row.outcome),
        metric_name: String(row.metric_name),
        metric_value: parseJson(row.metric_value_json),
        metric_source: row.metric_source === null ? null : String(row.metric_source),
        row: parseObject(row.row_json),
      })),
      contract_stages: contractStageRows.map((row) => ({
        core_run_id: String(row.run_id),
        trial_id: String(row.trial_id),
        schedule_idx: Number(row.schedule_idx),
        attempt: Number(row.attempt),
        row_seq: Number(row.row_seq),
        variant_id: String(row.variant_id),
        task_id: String(row.task_id),
        repl_idx: Number(row.repl_idx),
        stage: String(row.stage),
        status: String(row.status),
        recorded_at: String(row.recorded_at),
        detail: parseJson(row.detail_json),
        row: parseObject(row.row_json),
      })),
      attempt_objects: attemptObjectRows.map((row) => ({
        core_run_id: String(row.run_id),
        trial_id: String(row.trial_id),
        schedule_idx: Number(row.schedule_idx),
        attempt: Number(row.attempt),
        role: String(row.role),
        object_ref: String(row.object_ref),
        metadata: row.metadata_json === null ? null : parseJson(row.metadata_json),
        recorded_at_ms: Number(row.recorded_at_ms),
      })),
    };
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
  const parsed = parseJson(value);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return {};
  }
  return parsed as JsonObject;
}

function parseJson(value: unknown): JsonValue {
  return (typeof value === "string" ? JSON.parse(value) : value) as JsonValue;
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

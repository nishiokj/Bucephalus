import type { Sql } from "../db/client";
import type { JsonObject, JsonValue } from "../primitives";

export interface RuntimeSummary {
  cloud_run_id: string;
  core_run_ids: string[];
  runtime_snapshots: RuntimeSnapshotRecord[];
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

/** Trial event row as accepted from the worker live-evidence pump. */
export interface RuntimeEventRowInsert {
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

export interface WorkerLifecycleEventRecord {
  event_id: string;
  seq: number;
  event_type: string;
  payload: JsonObject;
  created_at: string;
}

export interface RuntimeSnapshotRecord {
  core_run_id: string;
  run_dir_name: string;
  runtime_values: Record<string, JsonObject>;
  trial_summaries: RuntimeTrialSummaryRecord[];
  evidence_records: JsonObject[];
  omitted: string[];
  seq?: number;
  created_at?: string;
}

export interface RuntimeTrialSummaryRecord {
  trial_id: string;
  summary: JsonObject;
  contract_trace?: JsonObject;
  trial_events?: JsonObject[];
}

// Live-ingested rows need a deterministic account id so retried batches hit
// the same primary key. Runner-direct writes derive theirs from the runner
// host; worker-pumped rows all land under this marker.
const WORKER_INGEST_ACCOUNT_ID = "cloud-worker";
const RUNTIME_SNAPSHOT_EVENT_TYPE = "worker.runtime.snapshot";

export class RuntimeRepository {
  private readonly schema: string;

  constructor(private readonly sql: Sql, schema = process.env.BUCEPHALUS_RUN_STORE_SCHEMA ?? "bucephalus_runtime") {
    this.schema = validateIdentifier(schema.trim() || "bucephalus_runtime");
  }

  async getSummary(cloudRunId: string): Promise<RuntimeSummary> {
    const [coreRunIds, runtimeSnapshots] = await Promise.all([
      this.coreRunIdsForCloudRun(cloudRunId),
      this.workerRuntimeSnapshots(cloudRunId),
    ]);
    const [runControls, scheduleProgress, activeSlots, recentEvents] = await Promise.all([
      this.runtimeValues(coreRunIds, "run_control_v2"),
      this.runtimeValues(coreRunIds, "schedule_progress_v2"),
      this.activeSlots(coreRunIds),
      this.eventRows(cloudRunId, { limit: 50 }),
    ]);
    return {
      cloud_run_id: cloudRunId,
      core_run_ids: coreRunIds,
      runtime_snapshots: runtimeSnapshots,
      run_controls: [
        ...runControls,
        ...runtimeValuesFromSnapshots(runtimeSnapshots, "run_control_v2"),
      ],
      schedule_progress: [
        ...scheduleProgress,
        ...runtimeValuesFromSnapshots(runtimeSnapshots, "schedule_progress_v2"),
      ],
      active_slots: activeSlots,
      recent_events: recentEvents,
    };
  }

  async runtimeValue(cloudRunId: string, key: string): Promise<JsonObject[]> {
    const [coreRunIds, runtimeSnapshots] = await Promise.all([
      this.coreRunIdsForCloudRun(cloudRunId),
      this.workerRuntimeSnapshots(cloudRunId),
    ]);
    const storedValues = await this.runtimeValues(coreRunIds, key);
    return [
      ...storedValues,
      ...runtimeValuesFromSnapshots(runtimeSnapshots, key),
    ];
  }

  async results(cloudRunId: string, input?: { limit?: number }): Promise<RuntimeResults> {
    const [coreRunIds, runtimeSnapshots] = await Promise.all([
      this.coreRunIdsForCloudRun(cloudRunId),
      this.workerRuntimeSnapshots(cloudRunId),
    ]);
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
    ]).catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [[], [], [], []];
      }
      throw error;
    });
    const legacyTrialResults = trialRows.map((row) => ({
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
    }));
    const legacyMetricObservations = metricRows.map((row) => ({
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
    }));
    const snapshotTrialResults = runtimeTrialResultsFromSnapshots(runtimeSnapshots)
      .filter((row) => !legacyTrialResults.some((legacy) => runtimeTrialResultKey(legacy) === runtimeTrialResultKey(row)))
      .slice(0, Math.max(0, limit - legacyTrialResults.length));
    const snapshotMetricObservations = runtimeMetricObservationsFromTrialResults(snapshotTrialResults);
    const legacyContractStages = contractStageRows.map((row) => ({
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
    }));
    const snapshotContractStages = runtimeContractStagesFromSnapshots(runtimeSnapshots)
      .filter((row) => !legacyContractStages.some((legacy) => runtimeContractStageKey(legacy) === runtimeContractStageKey(row)))
      .slice(0, Math.max(0, limit * 10 - legacyContractStages.length));
    const legacyAttemptObjects = attemptObjectRows.map((row) => ({
      core_run_id: String(row.run_id),
      trial_id: String(row.trial_id),
      schedule_idx: Number(row.schedule_idx),
      attempt: Number(row.attempt),
      role: String(row.role),
      object_ref: String(row.object_ref),
      metadata: row.metadata_json === null ? null : parseJson(row.metadata_json),
      recorded_at_ms: Number(row.recorded_at_ms),
    }));
    const snapshotAttemptObjects = runtimeAttemptObjectsFromSnapshots(runtimeSnapshots)
      .filter((row) => !legacyAttemptObjects.some((legacy) => runtimeAttemptObjectKey(legacy) === runtimeAttemptObjectKey(row)))
      .slice(0, Math.max(0, limit * 10 - legacyAttemptObjects.length));

    return {
      cloud_run_id: cloudRunId,
      core_run_ids: coreRunIds,
      trial_results: [
        ...legacyTrialResults,
        ...snapshotTrialResults,
      ],
      metric_observations: [
        ...legacyMetricObservations,
        ...snapshotMetricObservations,
      ],
      contract_stages: [
        ...legacyContractStages,
        ...snapshotContractStages,
      ],
      attempt_objects: [
        ...legacyAttemptObjects,
        ...snapshotAttemptObjects,
      ],
    };
  }

  async eventRows(cloudRunId: string, input?: { limit?: number; afterRowSeq?: number | undefined }): Promise<RuntimeEventRecord[]> {
    const [coreRunIds, runtimeSnapshots] = await Promise.all([
      this.coreRunIdsForCloudRun(cloudRunId),
      this.workerRuntimeSnapshots(cloudRunId),
    ]);
    if (coreRunIds.length === 0) {
      return [];
    }
    const limit = boundedLimit(input?.limit, 200);
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
      limit ${limit}
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    const legacyEvents = rows.map((row) => ({
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
    const snapshotEvents = runtimeEventRowsFromSnapshots(runtimeSnapshots)
      .filter((row) => row.row_seq > (input?.afterRowSeq ?? -1));
    return mergeRuntimeEventRecords(legacyEvents, snapshotEvents).slice(0, limit);
  }

  /**
   * Idempotently lands live-ingested trial event rows. Conflicting rows are
   * refreshed rather than dropped so a late identity-enrichment pass (once a
   * trial's contract trace exists) can upgrade attribution fields in place.
   */
  async upsertEventRows(rows: RuntimeEventRowInsert[]): Promise<number> {
    if (rows.length === 0) {
      return 0;
    }
    const values = rows.map((row) => ({
      account_id: WORKER_INGEST_ACCOUNT_ID,
      run_id: row.core_run_id,
      trial_id: row.trial_id,
      schedule_idx: row.schedule_idx,
      attempt: row.attempt,
      row_seq: row.row_seq,
      slot_commit_id: row.slot_commit_id,
      variant_id: row.variant_id,
      task_id: row.task_id,
      repl_idx: row.repl_idx,
      seq: row.seq,
      event_type: row.event_type,
      ts: row.ts,
      payload_json: JSON.stringify(row.payload),
      row_json: JSON.stringify(row.row),
    }));
    const result = await this.sql`
      insert into ${this.table("event_rows")} ${this.sql(values)}
      on conflict (account_id, run_id, trial_id, schedule_idx, attempt, row_seq)
      do update set
        slot_commit_id = excluded.slot_commit_id,
        variant_id = excluded.variant_id,
        task_id = excluded.task_id,
        repl_idx = excluded.repl_idx,
        seq = excluded.seq,
        event_type = excluded.event_type,
        ts = excluded.ts,
        payload_json = excluded.payload_json,
        row_json = excluded.row_json
    `;
    return result.count;
  }

  /**
   * Worker harness lifecycle events for a cloud run (materializing, core
   * starting, …). Runtime snapshot payloads are megabytes of repair data, so
   * they surface as a marker without the payload body.
   */
  async workerLifecycleEvents(cloudRunId: string, input?: { limit?: number }): Promise<WorkerLifecycleEventRecord[]> {
    const limit = boundedLimit(input?.limit, 200);
    const rows = await this.sql`
      select event_id, seq, event_type, payload, created_at
      from cloud.run_events
      where run_id = ${cloudRunId}
      order by seq
      limit ${limit}
    `;
    return rows.map((row) => {
      const eventType = String(row.event_type);
      return {
        event_id: String(row.event_id),
        seq: Number(row.seq),
        event_type: eventType,
        payload: eventType === RUNTIME_SNAPSHOT_EVENT_TYPE
          ? { note: "runtime snapshot payload omitted from event stream" }
          : parseObject(row.payload),
        created_at: String(row.created_at),
      };
    });
  }

  private async coreRunIdsForCloudRun(cloudRunId: string): Promise<string[]> {
    const [cloudEventIds, legacyDiscoveredIds] = await Promise.all([
      this.cloudEventCoreRunIds(cloudRunId),
      this.legacyDiscoveredCoreRunIds(cloudRunId),
    ]);
    return [...new Set([...cloudEventIds, ...legacyDiscoveredIds])].sort();
  }

  private async cloudEventCoreRunIds(cloudRunId: string): Promise<string[]> {
    const rows = await this.sql`
      with
      cleanup_ids as (
        select distinct core_ids.core_run_id
        from cloud.run_events events
        cross join lateral jsonb_array_elements_text(events.payload->'core_run_ids') as core_ids(core_run_id)
        where events.run_id = ${cloudRunId}
          and jsonb_typeof(events.payload->'core_run_ids') = 'array'
      ),
      snapshot_ids as (
        select distinct payload->>'core_run_id' as core_run_id
        from cloud.run_events
        where run_id = ${cloudRunId}
          and event_type = 'worker.runtime.snapshot'
          and payload ? 'core_run_id'
          and nullif(payload->>'core_run_id', '') is not null
      )
      select core_run_id from cleanup_ids
      union
      select core_run_id from snapshot_ids
      order by core_run_id
    `;
    return rows.map((row) => String(row.core_run_id));
  }

  private async legacyDiscoveredCoreRunIds(cloudRunId: string): Promise<string[]> {
    const rows = await this.sql`
      with run_roots as (
        select distinct payload->>'run_root_dir' as run_root_dir
        from cloud.run_events
        where run_id = ${cloudRunId}
          and payload ? 'run_root_dir'
          and nullif(payload->>'run_root_dir', '') is not null
      ),
      discovered_ids as (
        select distinct runtime_runs.run_id as core_run_id
        from ${this.table("runs")} runtime_runs
        join run_roots on runtime_runs.run_dir like run_roots.run_root_dir || '/%'
      )
      select core_run_id from discovered_ids
      order by core_run_id
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map((row) => String(row.core_run_id));
  }

  private async workerRuntimeSnapshots(cloudRunId: string): Promise<RuntimeSnapshotRecord[]> {
    const rows = await this.sql`
      select seq, payload, created_at
      from cloud.run_events
      where run_id = ${cloudRunId}
        and event_type = 'worker.runtime.snapshot'
      order by seq
    `;
    return rows.flatMap((row) => {
      const snapshot = runtimeSnapshotFromWorkerEventPayload(parseObject(row.payload));
      if (!snapshot) {
        return [];
      }
      return [{
        ...snapshot,
        seq: Number(row.seq),
        created_at: String(row.created_at),
      }];
    });
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
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
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
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
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

function isMissingRuntimeStore(error: unknown): boolean {
  return isRecord(error)
    && (error.code === "42P01" || error.code === "3F000");
}

export function runtimeSnapshotFromWorkerEventPayload(payload: JsonObject): RuntimeSnapshotRecord | null {
  const coreRunId = stringField(payload.core_run_id);
  if (!coreRunId) {
    return null;
  }
  const runtimeValues = recordOfJsonObjects(payload.runtime_values);
  const trialSummaries = Array.isArray(payload.trial_summaries)
    ? payload.trial_summaries.flatMap((item) => {
      if (!isRecord(item)) {
        return [];
      }
      const trialId = stringField(item.trial_id);
      const summary = isRecord(item.summary) ? item.summary as JsonObject : null;
      const contractTrace = isRecord(item.contract_trace) ? item.contract_trace as JsonObject : undefined;
      const trialEvents = Array.isArray(item.trial_events)
        ? item.trial_events.filter((event): event is JsonObject => isRecord(event))
        : undefined;
      return trialId && summary ? [{
        trial_id: trialId,
        summary,
        ...(contractTrace ? { contract_trace: contractTrace } : {}),
        ...(trialEvents ? { trial_events: trialEvents } : {}),
      }] : [];
    })
    : [];
  return {
    core_run_id: coreRunId,
    run_dir_name: stringField(payload.run_dir_name) ?? coreRunId,
    runtime_values: runtimeValues,
    trial_summaries: trialSummaries,
    evidence_records: Array.isArray(payload.evidence_records)
      ? payload.evidence_records.filter((item): item is JsonObject => isRecord(item))
      : [],
    omitted: Array.isArray(payload.omitted)
      ? payload.omitted.filter((item): item is string => typeof item === "string")
      : [],
  };
}

export function runtimeValuesFromSnapshots(snapshots: RuntimeSnapshotRecord[], key: string): JsonObject[] {
  return snapshots
    .map((snapshot) => snapshot.runtime_values[key])
    .filter((value): value is JsonObject => isRecord(value));
}

export function runtimeTrialResultsFromSnapshots(snapshots: RuntimeSnapshotRecord[]): RuntimeTrialResultRecord[] {
  const rows: RuntimeTrialResultRecord[] = [];
  for (const snapshot of snapshots) {
    snapshot.trial_summaries.forEach((item, index) => {
      const summary = item.summary;
      const ids = isRecord(summary.ids) ? summary.ids : {};
      const contractIds = isRecord(item.contract_trace?.ids) ? item.contract_trace.ids : {};
      const primaryMetric = isRecord(summary.primary_metric) ? summary.primary_metric : {};
      const metrics = isRecord(summary.metrics) ? summary.metrics as JsonObject : {};
      const trialId = stringField(ids.trial_id) ?? stringField(contractIds.trial_id) ?? item.trial_id;
      rows.push({
        core_run_id: snapshot.core_run_id,
        trial_id: trialId,
        schedule_idx: numberField(ids.schedule_idx) ?? numberField(contractIds.schedule_idx) ?? index,
        attempt: numberField(ids.attempt) ?? numberField(contractIds.attempt) ?? 0,
        row_seq: index,
        variant_id: stringField(ids.variant_id) ?? stringField(contractIds.variant_id) ?? "unknown",
        task_id: stringField(ids.task_id) ?? stringField(contractIds.task_id) ?? "unknown",
        repl_idx: numberField(ids.repl_idx) ?? numberField(contractIds.repl_idx) ?? 0,
        outcome: outcomeString(summary.outcome),
        primary_metric_name: stringField(primaryMetric.name) ?? "primary",
        primary_metric_value: jsonValueOrNull(primaryMetric.value),
        metrics,
        bindings: isRecord(summary.bindings) ? summary.bindings as JsonObject : {},
        events_total: numberField(summary.events_total) ?? 0,
        has_events: Boolean(summary.has_events),
        row: {
          source: "worker_runtime_snapshot",
          core_run_id: snapshot.core_run_id,
          trial_id: trialId,
          summary,
        },
      });
    });
  }
  return rows;
}

export function runtimeMetricObservationsFromTrialResults(
  trialResults: RuntimeTrialResultRecord[],
): RuntimeMetricObservationRecord[] {
  return trialResults.flatMap((trial) => {
    if (!isRecord(trial.metrics)) {
      return [];
    }
    return Object.entries(trial.metrics).map(([metricName, metricValue], index) => ({
      core_run_id: trial.core_run_id,
      trial_id: trial.trial_id,
      schedule_idx: trial.schedule_idx,
      attempt: trial.attempt,
      row_seq: trial.row_seq + index,
      variant_id: trial.variant_id,
      task_id: trial.task_id,
      repl_idx: trial.repl_idx,
      outcome: trial.outcome,
      metric_name: metricName,
      metric_value: jsonValueOrNull(metricValue),
      metric_source: "worker_runtime_snapshot",
      row: trial.row,
    }));
  });
}

export function runtimeContractStagesFromSnapshots(snapshots: RuntimeSnapshotRecord[]): RuntimeContractStageRecord[] {
  const stageOrder = [
    "task_mapping",
    "agent_execution",
    "artifact_extraction",
    "grader_execution",
    "grade_mapping",
  ];
  const rows: RuntimeContractStageRecord[] = [];
  for (const snapshot of snapshots) {
    for (const item of snapshot.trial_summaries) {
      const contractTrace = item.contract_trace;
      if (!contractTrace) {
        continue;
      }
      const stages = isRecord(contractTrace.stages) ? contractTrace.stages : {};
      const ids = isRecord(contractTrace.ids)
        ? contractTrace.ids
        : isRecord(item.summary.ids)
          ? item.summary.ids
          : {};
      const trialId = stringField(ids.trial_id) ?? item.trial_id;
      const overallStatus = jsonValueOrNull(contractTrace.overall_status);
      const scoreTrust = jsonValueOrNull(contractTrace.score_trust);
      const score = jsonValueOrNull(contractTrace.score);
      let rowSeq = 0;
      for (const stage of stageOrder) {
        const rawDetail = stages[stage];
        if (!isRecord(rawDetail)) {
          continue;
        }
        const detail: JsonObject = { ...rawDetail };
        if (stage === "grade_mapping") {
          detail.overall_status = overallStatus;
          detail.score_trust = scoreTrust;
          detail.score = score;
        }
        rows.push({
          core_run_id: snapshot.core_run_id,
          trial_id: trialId,
          schedule_idx: numberField(ids.schedule_idx) ?? 0,
          attempt: numberField(ids.attempt) ?? 0,
          row_seq: rowSeq,
          variant_id: stringField(ids.variant_id) ?? "unknown",
          task_id: stringField(ids.task_id) ?? "unknown",
          repl_idx: numberField(ids.repl_idx) ?? 0,
          stage,
          status: stringField(detail.status) ?? "unknown",
          recorded_at: snapshot.created_at ?? "",
          detail,
          row: {
            source: "worker_runtime_snapshot",
            core_run_id: snapshot.core_run_id,
            trial_id: trialId,
            contract_trace: contractTrace,
          },
        });
        rowSeq += 1;
      }
    }
  }
  return rows;
}

export function runtimeEventRowsFromSnapshots(snapshots: RuntimeSnapshotRecord[]): RuntimeEventRecord[] {
  const rows: RuntimeEventRecord[] = [];
  for (const snapshot of snapshots) {
    for (const item of snapshot.trial_summaries) {
      const events = item.trial_events ?? [];
      if (events.length === 0) {
        continue;
      }
      const ids = isRecord(item.contract_trace?.ids)
        ? item.contract_trace.ids
        : isRecord(item.summary.ids)
          ? item.summary.ids
          : {};
      const trialId = stringField(ids.trial_id) ?? item.trial_id;
      events.forEach((payload, index) => {
        rows.push({
          core_run_id: snapshot.core_run_id,
          trial_id: trialId,
          schedule_idx: numberField(ids.schedule_idx) ?? 0,
          attempt: numberField(ids.attempt) ?? 0,
          row_seq: index,
          slot_commit_id: "",
          variant_id: stringField(ids.variant_id) ?? "unknown",
          task_id: stringField(ids.task_id) ?? "unknown",
          repl_idx: numberField(ids.repl_idx) ?? 0,
          seq: numberField(payload.seq) ?? index,
          event_type: stringField(payload.event_type) ?? stringField(payload.type) ?? "unknown",
          ts: stringField(payload.ts) ?? stringField(payload.timestamp),
          payload,
          row: {
            source: "worker_runtime_snapshot",
            core_run_id: snapshot.core_run_id,
            trial_id: trialId,
          },
        });
      });
    }
  }
  return rows;
}

export function runtimeAttemptObjectsFromSnapshots(snapshots: RuntimeSnapshotRecord[]): RuntimeAttemptObjectRecord[] {
  const roles = [
    "trial_input_ref",
    "trial_output_ref",
    "events_ref",
    "stdout_ref",
    "stderr_ref",
    "workspace_pre_ref",
    "workspace_post_ref",
    "diff_incremental_ref",
    "diff_cumulative_ref",
    "patch_incremental_ref",
    "patch_cumulative_ref",
    "workspace_bundle_ref",
  ];
  const rows: RuntimeAttemptObjectRecord[] = [];
  for (const snapshot of snapshots) {
    for (const record of snapshot.evidence_records) {
      const evidence = isRecord(record.evidence) ? record.evidence : {};
      const ids = isRecord(record.ids) ? record.ids : {};
      const trialId = stringField(ids.trial_id);
      if (!trialId) {
        continue;
      }
      const scheduleIdx = numberField(record.schedule_idx);
      const attempt = numberField(record.attempt);
      if (scheduleIdx === null || attempt === null) {
        continue;
      }
      for (const roleRef of roles) {
        const objectRef = stringField(evidence[roleRef]);
        if (!objectRef) {
          continue;
        }
        rows.push({
          core_run_id: snapshot.core_run_id,
          trial_id: trialId,
          schedule_idx: scheduleIdx,
          attempt,
          role: roleRef.endsWith("_ref") ? roleRef.slice(0, -"_ref".length) : roleRef,
          object_ref: objectRef,
          metadata: record,
          recorded_at_ms: numberField(record.recorded_at_ms) ?? 0,
        });
      }
    }
  }
  return rows;
}

function runtimeTrialResultKey(row: Pick<RuntimeTrialResultRecord, "core_run_id" | "trial_id" | "attempt">): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}`;
}

function runtimeContractStageKey(
  row: Pick<RuntimeContractStageRecord, "core_run_id" | "trial_id" | "attempt" | "stage">,
): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}\0${row.stage}`;
}

function runtimeEventKey(row: Pick<RuntimeEventRecord, "core_run_id" | "trial_id" | "attempt" | "row_seq">): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}\0${row.row_seq}`;
}

/**
 * Store rows win over snapshot-derived rows, and duplicate logical rows
 * within a source (e.g. the same row landed by both a direct runtime store
 * writer and the worker pump under different account ids) collapse to the
 * first occurrence.
 */
export function mergeRuntimeEventRecords(
  storeRows: RuntimeEventRecord[],
  snapshotRows: RuntimeEventRecord[],
): RuntimeEventRecord[] {
  const seen = new Set<string>();
  const merged: RuntimeEventRecord[] = [];
  for (const row of [...storeRows, ...snapshotRows]) {
    const key = runtimeEventKey(row);
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    merged.push(row);
  }
  return merged.sort((a, b) => {
    const core = a.core_run_id.localeCompare(b.core_run_id);
    if (core !== 0) {
      return core;
    }
    return a.schedule_idx - b.schedule_idx
      || a.attempt - b.attempt
      || a.row_seq - b.row_seq;
  });
}

function runtimeAttemptObjectKey(
  row: Pick<RuntimeAttemptObjectRecord, "core_run_id" | "trial_id" | "attempt" | "role">,
): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}\0${row.role}`;
}

function recordOfJsonObjects(value: unknown): Record<string, JsonObject> {
  if (!isRecord(value)) {
    return {};
  }
  const out: Record<string, JsonObject> = {};
  for (const [key, item] of Object.entries(value)) {
    if (isRecord(item)) {
      out[key] = item as JsonObject;
    }
  }
  return out;
}

function stringField(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function numberField(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function outcomeString(value: unknown): string {
  if (typeof value === "string" && value.trim().length > 0) {
    return value;
  }
  if (isRecord(value)) {
    return stringField(value.status) ?? "unknown";
  }
  return "unknown";
}

function jsonValueOrNull(value: unknown): JsonValue {
  return isJsonValue(value) ? value : null;
}

function isJsonValue(value: unknown): value is JsonValue {
  if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return Number.isFinite(value as number) || typeof value !== "number";
  }
  if (Array.isArray(value)) {
    return value.every(isJsonValue);
  }
  if (isRecord(value)) {
    return Object.values(value).every(isJsonValue);
  }
  return false;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
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

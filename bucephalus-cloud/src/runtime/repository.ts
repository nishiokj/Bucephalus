import { readFile, stat } from "node:fs/promises";
import { resolve, sep } from "node:path";
import type { Sql } from "../db/client";
import { HttpError } from "../http";
import type { CloudRunRecord, RunAttemptRecord, WorkerCapabilities } from "../packages/repository";
import { canonicalJsonStringify, sha256Digest, type JsonObject, type JsonValue } from "../primitives";

const RUNTIME_API_VERSION = "bucephalus.dev/v1alpha1";
const DEFAULT_MAX_RUNTIME_ARTIFACT_BYTES = 5 * 1024 * 1024;
const RUNTIME_EVENT_RESOURCE_LIMIT = 250;
const RUNTIME_ACCESS_TARGET_KINDS = new Set(["Run", "RunnerInstance", "RunnerAttempt", "Trial", "ScheduleSlot", "TrialContainer"]);
const RUNTIME_REACHABLE_RUNNER_STATUSES = new Set(["online", "cordoned", "draining"]);
const WORKER_INGEST_ACCOUNT_ID = "cloud-worker";
const RUNTIME_SNAPSHOT_EVENT_TYPE = "worker.runtime.snapshot";
const RUNTIME_RESOURCE_FIELD_SELECTORS = [
  "kind",
  "metadata.name",
  "metadata.uid",
  "metadata.generation",
  "metadata.resourceVersion",
  "metadata.creationTimestamp",
  "metadata.deletionTimestamp",
  "metadata.labels.<key>",
  "metadata.ownerReferences",
  "metadata.ownerReferences.apiVersion",
  "metadata.ownerReferences.kind",
  "metadata.ownerReferences.name",
  "metadata.ownerReferences.uid",
  "spec.<path>",
  "status.access.reachable",
  "status.access.reason",
  "status.access.port_forward",
  "status.access.exec",
  "status.access.runner_instance_id",
  "status.access.runner_instance_status",
  "status.access.attempt_id",
  "status.access.worker_id",
  "status.provider",
  "status.provider_instance_id",
  "status.instance_name",
  "status.project_id",
  "status.zone",
  "status.conditions.<type>",
  "status.conditions.<type>.status",
  "status.conditions.<type>.reason",
  "status.conditions.<type>.message",
  "status.<path>",
  "audit.<path>",
];
const RUNTIME_RESOURCE_FIELD_SELECTOR_EXTRAS: Record<string, string[]> = {
  Event: [
    "spec.event_type",
    "spec.source",
    "spec.row_seq",
    "spec.involved_object.kind",
    "spec.involved_object.name",
    "spec.involved_object.uid",
    "status.involved",
    "status.involved_kind",
    "status.involved_name",
    "status.involved_uid",
    "status.involved_count",
  ],
};
const RUNTIME_RESOURCE_BASE_LABEL_SELECTORS = ["bucephalus.dev/run-id"];
const RUNTIME_RESOURCE_LABEL_SELECTOR_EXTRAS: Record<string, string[]> = {
  Run: ["bucephalus.dev/package-digest"],
  CoreRun: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/experiment-id",
    "bucephalus.dev/runtime-status",
  ],
  RunnerPool: ["bucephalus.dev/runner-pool-id"],
  RunnerInstance: [
    "bucephalus.dev/runner-instance-id",
    "bucephalus.dev/runner-pool-id",
    "bucephalus.dev/current-attempt-id",
    "bucephalus.dev/provider",
    "topology.kubernetes.io/zone",
  ],
  RunnerAttempt: [
    "bucephalus.dev/runner-instance-id",
    "bucephalus.dev/runner-pool-id",
    "bucephalus.dev/worker-id",
  ],
  RunnerProvisionRequest: [
    "bucephalus.dev/runner-pool-id",
    "bucephalus.dev/runner-instance-id",
  ],
  RunManifest: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/experiment-id",
    "bucephalus.dev/runtime-status",
    "bucephalus.dev/workload-type",
    "bucephalus.dev/baseline-id",
  ],
  MetricDefinition: [
    "bucephalus.dev/experiment-id",
    "bucephalus.dev/metric-id",
    "bucephalus.dev/semantic-key",
    "bucephalus.dev/source-type",
    "bucephalus.dev/direction",
    "bucephalus.dev/required",
    "bucephalus.dev/primary-metric",
  ],
  ScheduleSlot: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/schedule-idx",
    "bucephalus.dev/worker-id",
    "bucephalus.dev/attempt-id",
    "bucephalus.dev/runner-instance-id",
    "bucephalus.dev/runner-pool-id",
  ],
  SlotCommit: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/schedule-idx",
    "bucephalus.dev/attempt",
    "bucephalus.dev/record-type",
    "bucephalus.dev/slot-commit-id",
    "bucephalus.dev/slot-status",
  ],
  PendingTrialCompletion: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/schedule-idx",
    "bucephalus.dev/slot-status",
  ],
  VariantSnapshot: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/schedule-idx",
    "bucephalus.dev/attempt",
    "bucephalus.dev/row-seq",
    "bucephalus.dev/slot-commit-id",
    "bucephalus.dev/variant-id",
    "bucephalus.dev/baseline-id",
    "bucephalus.dev/task-id",
    "bucephalus.dev/binding-name",
  ],
  EvidenceRecord: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/schedule-idx",
    "bucephalus.dev/attempt",
    "bucephalus.dev/row-seq",
    "bucephalus.dev/slot-commit-id",
    "bucephalus.dev/schema-version",
    "bucephalus.dev/record-kind",
  ],
  ChainState: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/schedule-idx",
    "bucephalus.dev/attempt",
    "bucephalus.dev/row-seq",
    "bucephalus.dev/slot-commit-id",
    "bucephalus.dev/schema-version",
    "bucephalus.dev/record-kind",
  ],
  TrialConclusion: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/schedule-idx",
    "bucephalus.dev/attempt",
    "bucephalus.dev/row-seq",
    "bucephalus.dev/slot-commit-id",
    "bucephalus.dev/schema-version",
    "bucephalus.dev/outcome",
    "bucephalus.dev/status",
  ],
  LineageVersion: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/chain-key",
    "bucephalus.dev/version-id",
    "bucephalus.dev/parent-version-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/step-index",
  ],
  LineageHead: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/chain-key",
    "bucephalus.dev/latest-version-id",
    "bucephalus.dev/step-index",
  ],
  Trial: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/variant-id",
    "bucephalus.dev/task-id",
    "bucephalus.dev/worker-id",
    "bucephalus.dev/attempt-id",
    "bucephalus.dev/runner-instance-id",
    "bucephalus.dev/runner-pool-id",
  ],
  TrialContainer: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/container-role",
    "bucephalus.dev/worker-id",
    "bucephalus.dev/attempt-id",
    "bucephalus.dev/runner-instance-id",
    "bucephalus.dev/runner-pool-id",
  ],
  TrialStage: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/stage",
    "bucephalus.dev/attempt",
    "bucephalus.dev/variant-id",
    "bucephalus.dev/task-id",
  ],
  RuntimeValue: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/runtime-key",
    "bucephalus.dev/value-source",
  ],
  MetricObservation: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/metric-name",
    "bucephalus.dev/metric-source",
    "bucephalus.dev/attempt",
    "bucephalus.dev/variant-id",
    "bucephalus.dev/task-id",
  ],
  PerformanceSample: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/sample-kind",
    "bucephalus.dev/stage",
    "bucephalus.dev/attempt",
  ],
  RuntimeOperation: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/op-kind",
    "bucephalus.dev/op-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/parent-trial-id",
  ],
  TrialArtifact: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/artifact-role",
    "bucephalus.dev/attempt",
    "bucephalus.dev/sha256",
  ],
  RuntimeSnapshot: ["bucephalus.dev/core-run-id"],
  Event: [
    "bucephalus.dev/core-run-id",
    "bucephalus.dev/trial-id",
    "bucephalus.dev/task-id",
    "bucephalus.dev/event-type",
    "bucephalus.dev/event-source",
    "bucephalus.dev/resource-kind",
    "bucephalus.dev/resource-name",
  ],
  PortForward: [
    "bucephalus.dev/runner-instance-id",
    "bucephalus.dev/attempt-id",
    "bucephalus.dev/worker-id",
    "bucephalus.dev/resource-kind",
    "bucephalus.dev/resource-name",
  ],
  Exec: [
    "bucephalus.dev/runner-instance-id",
    "bucephalus.dev/attempt-id",
    "bucephalus.dev/worker-id",
    "bucephalus.dev/resource-kind",
    "bucephalus.dev/resource-name",
  ],
};

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

export interface RuntimeTrialProgress {
  cloud_run_id: string;
  trials_completed: number | null;
  trials_total: number | null;
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

export interface RuntimePerformanceSampleRecord {
  core_run_id: string;
  sample_id: string;
  trial_id: string | null;
  schedule_idx: number | null;
  attempt: number | null;
  sample_seq: number;
  sample_kind: string;
  stage: string;
  duration_ms: number | null;
  process_rss_kb: number | null;
  payload: JsonObject;
  recorded_at_ms: number;
}

export interface RuntimeOperationRecord {
  core_run_id: string;
  op_kind: string;
  op_id: string;
  payload: JsonObject;
  updated_at_ms: number;
}

export interface RuntimeValueRecord {
  core_run_id: string;
  key: string;
  value: JsonObject;
  source: string;
  updated_at_ms: number | null;
  observed_at: string | null;
  snapshot_seq?: number | undefined;
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
  content_available: boolean;
  media_type: string;
  byte_size: number | null;
  sha256: string;
  relative_path: string;
}

export interface RuntimeAttemptObjectContentInsert {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  role: string;
  object_ref: string;
  storage_path: string;
  media_type: string;
  byte_size: number;
  sha256: string;
  relative_path?: string | null;
  metadata?: JsonObject | null;
  recorded_at_ms: number;
}

export interface RuntimeAttemptObjectContentRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  role: string;
  object_ref: string;
  storage_path: string;
  media_type: string;
  byte_size: number;
  sha256: string;
  relative_path: string | null;
  metadata: JsonValue | null;
  recorded_at_ms: number;
}

export interface RuntimeArtifactContent {
  object: RuntimeAttemptObjectRecord;
  media_type: string;
  bytes: Uint8Array;
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

export interface RuntimeSlotCommitRecord {
  core_run_id: string;
  schedule_idx: number;
  attempt: number;
  record_type: string;
  slot_commit_id: string;
  record: JsonObject;
  recorded_at_ms: number;
}

export interface RuntimePendingTrialCompletionRecord {
  core_run_id: string;
  schedule_idx: number;
  trial_result: JsonObject;
  updated_at_ms: number;
}

export interface RuntimeCoreRunRecord {
  core_run_id: string;
  experiment_id: string | null;
  project_root: string | null;
  run_dir: string;
  artifact_root: string;
  runtime_status: string;
  manifest: JsonObject;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface RuntimeRunManifestRecord {
  core_run_id: string;
  experiment_id: string | null;
  project_root: string | null;
  run_dir: string | null;
  artifact_root: string | null;
  runtime_status: string | null;
  manifest: JsonObject;
  updated_at_ms: number;
}

export interface RuntimeMetricDefinitionRecord {
  experiment_id: string;
  metric_id: string;
  semantic_key: string | null;
  label: string | null;
  value_type: string | null;
  unit: string | null;
  direction: string | null;
  source_type: string;
  source_pointer: string | null;
  required: boolean;
  primary_metric: boolean;
  definition: JsonObject;
  updated_at_ms: number;
}

export interface RuntimeVariantSnapshotRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  row_seq: number;
  slot_commit_id: string;
  variant_id: string;
  baseline_id: string;
  task_id: string;
  repl_idx: number;
  binding_name: string;
  binding_value: JsonValue;
  binding_value_text: string;
  row: JsonObject;
}

export interface RuntimeProvenanceRowRecord {
  core_run_id: string;
  schedule_idx: number;
  attempt: number;
  row_seq: number;
  slot_commit_id: string;
  row: JsonObject;
}

type RuntimeProvenanceResourceKind = "EvidenceRecord" | "ChainState" | "TrialConclusion";

export interface RuntimeLineageVersionRecord {
  core_run_id: string;
  version_id: string;
  chain_key: string;
  step_index: number;
  trial_id: string;
  parent_version_id: string | null;
  pre_snapshot_ref: string | null;
  post_snapshot_ref: string | null;
  diff_incremental_ref: string | null;
  diff_cumulative_ref: string | null;
  patch_incremental_ref: string | null;
  patch_cumulative_ref: string | null;
  workspace_ref: string | null;
  checkpoint_labels: JsonObject;
}

export interface RuntimeLineageHeadRecord {
  core_run_id: string;
  chain_key: string;
  latest_version_id: string;
  step_index: number;
  latest_workspace_ref: string | null;
}

interface RuntimeTrialAttemptRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  phase: string;
  paused_from_phase: string | null;
  variant_id: string;
  task_id: string;
  repl_idx: number;
  state: JsonObject;
  updated_at_ms: number;
}

interface RuntimeTrialRecord extends RuntimeTrialAttemptRecord {
  source: string;
  row_seq?: number | undefined;
  outcome?: string | undefined;
  primary_metric_name?: string | undefined;
  primary_metric_value?: JsonValue | undefined;
  metrics?: JsonValue | undefined;
  bindings?: JsonValue | undefined;
  events_total?: number | undefined;
  has_events?: boolean | undefined;
}

export interface RuntimeEventRecord {
  source: string;
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
  resource_refs: RuntimeResourceWatchRef[];
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

export interface RuntimeResourceList {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceList";
  metadata: RuntimeResourceListMeta;
  cloud_run_id: string;
  core_run_ids: string[];
  resources: RuntimeResourceRecord[];
}

export interface RuntimeResourceListMeta {
  resourceVersion: string;
  continue: string | null;
  remainingItemCount: number;
  total: number;
  returned: number;
}

export interface RuntimeResourceWatchList {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceWatchList";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  resource_versions: Record<string, string>;
  events: RuntimeResourceWatchEvent[];
  resource_inventory: RuntimeResourceList;
}

export interface RuntimeResourceWatchEvent {
  type: "ADDED" | "MODIFIED" | "DELETED" | "BOOKMARK";
  resource_ref: RuntimeResourceWatchRef;
  resource_version?: string | undefined;
  previous_resource_version?: string | undefined;
  resource?: RuntimeResourceRecord | undefined;
}

export interface RuntimeResourceWatchRef {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: string;
  name: string;
  uid?: string | undefined;
}

export interface RuntimeApiResourceList {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeApiResourceList";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  resources: RuntimeApiResourceRecord[];
}

export interface RuntimeApiResourceRecord {
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  group: "bucephalus.dev";
  version: "v1alpha1";
  name: string;
  singularName: string;
  namespaced: false;
  scope: "run";
  kind: string;
  shortNames: string[];
  categories: string[];
  verbs: string[];
  subresources: string[];
  actions: string[];
  access: string[];
  supports: RuntimeApiResourceSupports;
  pathTemplates: RuntimeApiResourcePathTemplates;
  exampleCommands: RuntimeApiResourceExampleCommand[];
  printerColumns: RuntimeApiResourcePrinterColumn[];
  fieldSelectors: string[];
  labelSelectors: string[];
  labelSelector: true;
  count: number;
  description: string;
}

export interface RuntimeApiResourcesInput {
  requester?: string | null | undefined;
}

export interface RuntimeApiResourceExampleCommand {
  purpose: string;
  command: string;
}

export interface RuntimeApiResourcePrinterColumn {
  name: string;
  type: "string" | "integer" | "number" | "boolean" | "date";
  jsonPath: string;
  description: string;
  priority: number;
}

export interface RuntimeApiResourceSupports {
  list: boolean;
  get: boolean;
  watch: boolean;
  describe: boolean;
  create: boolean;
  delete: boolean;
  actions: boolean;
  access: boolean;
  labelSelector: true;
  fieldSelector: true;
}

export interface RuntimeApiResourcePathTemplates {
  collection: string;
  resource: string;
  describe: string;
  operationReview: string;
  watch: string;
  create?: string;
  delete?: string;
  subresources: Record<string, string>;
}

export interface RuntimeInspectBundle {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeInspectBundle";
  cloud_run_id: string;
  generated_at: string;
  resource_filter: RuntimeInspectBundleFilter;
  api_resources: RuntimeApiResourceList;
  resource_inventory: RuntimeResourceList;
  resource_health: RuntimeResourceHealth;
  resource_metrics: RuntimeResourceMetricsList;
  event_list: RuntimeEventList;
  log_refs: RuntimeInspectLogRef[];
}

export interface RuntimeInspectBundleFilter {
  kinds: string[];
  categories: string[];
  label_selector: string | null;
  field_selector: string | null;
}

export interface RuntimeInspectBundleInput extends RuntimeResourceFilter {
  eventLimit?: number | undefined;
  requester?: string | null | undefined;
}

export interface RuntimeInspectLogRef {
  resource: JsonObject;
  streams: Array<"stdout" | "stderr">;
  urls: {
    stdout: string;
    stderr: string;
  };
}

export interface RuntimeResourceFilter {
  kinds?: string[] | undefined;
  categories?: string[] | undefined;
  labelSelector?: string | null | undefined;
  fieldSelector?: string | null | undefined;
}

export interface RuntimeResourceListInput extends RuntimeResourceFilter {
  limit?: number | undefined;
  continueToken?: string | null | undefined;
  requester?: string | null | undefined;
}

export interface RuntimeResourceWatchInput {
  filter?: RuntimeResourceFilter | undefined;
  resourceVersion?: string | null | undefined;
  knownResourceVersions?: Map<string, string> | undefined;
  allowBookmarks?: boolean | undefined;
  requester?: string | null | undefined;
}

export interface RuntimeEventFilter {
  eventTypes?: string[] | undefined;
  sources?: string[] | undefined;
  resourceKind?: string | null | undefined;
  resourceName?: string | null | undefined;
  trialId?: string | null | undefined;
  taskId?: string | null | undefined;
}

export interface RuntimeEventRowsInput extends RuntimeEventFilter {
  limit?: number | undefined;
  afterRowSeq?: number | undefined;
  continueToken?: string | null | undefined;
}

export interface RuntimeEventList {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeEventList";
  cloud_run_id: string;
  generated_at: string;
  event_filter: RuntimeEventListFilter;
  metadata: RuntimeEventListMetadata;
  events: RuntimeEventRecord[];
}

export interface RuntimeEventListFilter {
  event_types: string[];
  sources: string[];
  resource_kind: string | null;
  resource_name: string | null;
  trial_id: string | null;
  task_id: string | null;
}

export interface RuntimeEventListMetadata {
  resourceVersion: string;
  continue: string | null;
  remainingItemCount: number | null;
  limit: number;
  returned: number;
  after_row_seq: number | null;
  next_after_row_seq: number | null;
}

export interface RuntimeResourceDescribe {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceDescribe";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  resource: RuntimeResourceRecord;
  operations: RuntimeResourceOperation[];
  related_resources: RuntimeRelatedResourceRecord[];
  event_list: RuntimeResourceEventList;
}

export interface RuntimeResourceOperation {
  purpose: string;
  command: string;
  supported: boolean;
  reason: string | null;
  message: string | null;
  verb: string | null;
  subresource: string | null;
  action: string | null;
  requires_running_run: boolean;
}

export interface RuntimeResourceOperationReview {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceOperationReview";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  resource_ref: RuntimeResourceWatchRef;
  resource_version: string;
  resource_generation: number | null;
  observed_generation: number | null;
  operation: string;
  matched_operation: string | null;
  supported: boolean;
  reason: string | null;
  message: string | null;
  command: string | null;
  verb: string | null;
  subresource: string | null;
  action: string | null;
  requires_running_run: boolean | null;
}

export interface RuntimeResourceEventList {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceEventList";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  resource: RuntimeResourceRecord;
  event_filter: RuntimeEventListFilter;
  metadata: RuntimeEventListMetadata;
  events: RuntimeEventRecord[];
}

export interface RuntimeResourceMetrics {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceMetrics";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  resource_ref: RuntimeResourceWatchRef;
  resource_version: string | null;
  phase: string | null;
  summary: RuntimeResourceMetricsSummary;
  metrics: RuntimeResourceMetric[];
}

export interface RuntimeResourceMetricsList {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceMetricsList";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  metadata: {
    resourceVersion: string;
    continue: string | null;
    remainingItemCount: number;
    total: number;
    returned: number;
  };
  summary: RuntimeResourceMetricsListSummary;
  resources: RuntimeResourceMetrics[];
}

export interface RuntimeResourceMetricsListSummary extends RuntimeResourceMetricsSummary {
  resources_total: number;
  resources_returned: number;
}

export interface RuntimeResourceMetricsSummary {
  metrics_total: number;
  lifecycle_metrics: number;
  condition_metrics: number;
  access_metrics: number;
  event_metrics: number;
  numeric_spec_metrics: number;
  numeric_status_metrics: number;
  events_total: number;
}

export interface RuntimeResourceMetric {
  name: string;
  value: number;
  unit: string | null;
  source: "lifecycle" | "condition" | "access" | "event" | "spec" | "status";
  description: string;
  labels: JsonObject;
}

export interface RuntimeResourceStatus {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceStatus";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  resource_ref: RuntimeResourceWatchRef;
  generation: number;
  observedGeneration: number;
  resourceVersion: string;
  deletionTimestamp: string | null;
  phase: string | null;
  reason: string | null;
  message: string | null;
  conditions: RuntimeResourceCondition[];
  actions: string[];
  status: JsonObject;
  audit: JsonObject;
}

export type RuntimeResourceHealthState = "ready" | "degraded" | "problem" | "unknown";

export interface RuntimeResourceHealth {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: "RuntimeResourceHealth";
  cloud_run_id: string;
  generated_at: string;
  core_run_ids: string[];
  summary: RuntimeResourceHealthSummary;
  resources: RuntimeResourceHealthRow[];
}

export interface RuntimeResourceHealthSummary {
  total: number;
  ready: number;
  degraded: number;
  problem: number;
  unknown: number;
  access_targets: number;
  reachable_access_targets: number;
  port_forward_ready: number;
  exec_ready: number;
  actions_available: number;
  observed_resources: number;
  observed_current: number;
  observed_stale: number;
  observed_unknown: number;
}

export interface RuntimeResourceAccessStatus {
  [key: string]: JsonValue | undefined;
  reachable?: boolean;
  reason?: string;
  port_forward?: boolean;
  exec?: boolean;
  runner_instance_id?: string;
  runner_instance_status?: string;
  attempt_id?: string;
  worker_id?: string;
}

export interface RuntimeResourceHealthRow {
  resource: string;
  resource_ref: RuntimeResourceWatchRef;
  health: RuntimeResourceHealthState;
  observed: "current" | "stale" | "unknown";
  phase: string | null;
  ready: RuntimeResourceCondition | null;
  reason: string | null;
  message: string | null;
  condition_summary: string | null;
  degraded_conditions: RuntimeResourceCondition[];
  actions: string[];
  access: RuntimeResourceAccessStatus;
  access_summary: string | null;
  source: string | null;
  updated_at: string | null;
  resource_version: string | null;
}

export interface RuntimeRelatedResourceRecord {
  relationship: "owner" | "dependent";
  resource: RuntimeResourceRecord;
}

export interface RuntimeResourceActionInput {
  run: CloudRunRecord;
  resourceKind: string;
  resourceName: string;
  resourceVersion?: string | null | undefined;
  requester?: string | null | undefined;
  reason?: string | null | undefined;
}

export interface RuntimeResourceLogsInput {
  kind: string;
  name: string;
  stream?: string | null;
  tailLines?: number | undefined;
  requester?: string | null | undefined;
}

export interface RuntimeResourceArtifactContentInput {
  kind: string;
  name: string;
  requester?: string | null | undefined;
}

export interface RuntimeResourceOperationReviewInput {
  kind: string;
  name: string;
  operation: string;
  requester?: string | null | undefined;
}

export interface RuntimeResourceMetricsListInput extends RuntimeResourceFilter {
  limit?: number | undefined;
  continueToken?: string | null | undefined;
  requester?: string | null | undefined;
}

export interface RuntimeResourceLogs {
  resource: RuntimeResourceRecord;
  stream: "stdout" | "stderr";
  media_type: string;
  bytes: Uint8Array;
  object: RuntimeAttemptObjectRecord;
}

export interface RuntimeResourceArtifactContent {
  resource: RuntimeResourceRecord;
  media_type: string;
  bytes: Uint8Array;
  object: RuntimeAttemptObjectRecord;
}

export interface RuntimeResourceRecord {
  apiVersion: "bucephalus.dev/v1alpha1";
  kind: string;
  metadata: {
    name: string;
    uid: string;
    generation: number;
    resourceVersion: string;
    labels: Record<string, string>;
    annotations: Record<string, string>;
    ownerReferences: Array<{
      apiVersion: "bucephalus.dev/v1alpha1";
      kind: string;
      name: string;
      uid?: string;
    }>;
    creationTimestamp?: string;
    deletionTimestamp?: string;
    created_at?: string;
    updated_at?: string;
  };
  spec: JsonObject;
  status: JsonObject;
  audit: JsonObject;
}

type RuntimeResourceDraft = Omit<RuntimeResourceRecord, "metadata"> & {
  metadata: Omit<RuntimeResourceRecord["metadata"], "uid" | "generation" | "resourceVersion"> & {
    uid?: string;
    generation?: number;
    resourceVersion?: string;
  };
};

type RuntimeResourceReadable = RuntimeResourceRecord | RuntimeResourceDraft;

export interface RuntimeAccessTarget {
  kind: string;
  name: string;
  uid?: string;
  resourceVersion?: string;
  runnerInstanceId?: string | null;
  attemptId?: string | null;
  workerId?: string | null;
}

export interface RuntimeResourceCondition extends JsonObject {
  type: string;
  status: "True" | "False" | "Unknown";
  reason: string;
  message: string;
  lastTransitionTime?: string;
}

interface RuntimeProvisionRequestRecord {
  provision_request_id: string;
  runner_pool_id: string;
  run_id: string | null;
  status: string;
  provider: string;
  provider_instance_id: string | null;
  instance_name: string | null;
  runner_instance_id: string | null;
  requirements: JsonObject;
  metadata: JsonObject;
  error_message: string | null;
  created_at: string;
  updated_at: string;
}

interface RuntimeRunnerPoolRecord {
  runner_pool_id: string;
  name: string;
  status: string;
  capabilities: WorkerCapabilities;
  metadata: JsonObject;
  created_at: string;
  updated_at: string;
}

interface RuntimeTrialContainerRecord {
  core_run_id: string;
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  role: string;
  container_id: string;
  status: string;
  image: string | null;
  workdir: string | null;
  updated_at_ms: number;
}

interface RuntimeRunAttemptRecord extends RunAttemptRecord {
  runner_pool_id: string | null;
  runner_instance_status: string | null;
  runner_instance_name: string | null;
  runner_instance_provider_instance_id: string | null;
  runner_instance_capabilities: WorkerCapabilities | null;
  runner_instance_metadata: JsonObject;
  runner_instance_last_heartbeat_at: string | null;
  runner_instance_created_at: string | null;
  runner_instance_updated_at: string | null;
}

export interface RuntimeAccessRequestRecord {
  access_request_id: string;
  run_id: string;
  kind: "port_forward" | "exec";
  status: string;
  resource_kind: string;
  resource_name: string;
  target_uid: string | null;
  target_resource_version: string | null;
  protocol: string;
  target_port: number | null;
  local_port: number | null;
  command: string[];
  runner_instance_id: string | null;
  attempt_id: string | null;
  worker_id: string | null;
  requester: string | null;
  reason: string | null;
  connection: JsonObject;
  error_message: string | null;
  expires_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreatePortForwardRequestInput {
  run: CloudRunRecord;
  resourceKind?: string | null;
  resourceName?: string | null;
  resourceVersion?: string | null;
  protocol?: string | null;
  targetPort: number;
  localPort?: number | null;
  ttlSeconds?: number | null;
  requester?: string | null;
  reason?: string | null;
}

export interface CreateExecRequestInput {
  run: CloudRunRecord;
  resourceKind?: string | null;
  resourceName?: string | null;
  resourceVersion?: string | null;
  command: string[];
  ttlSeconds?: number | null;
  requester?: string | null;
  reason?: string | null;
}

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

  async trialProgressForCloudRuns(cloudRunIds: string[]): Promise<RuntimeTrialProgress[]> {
    const ids = [...new Set(cloudRunIds)].sort();
    if (ids.length === 0) {
      return [];
    }
    const coreRunIdsByCloudRunId = await this.coreRunIdsForCloudRuns(ids);
    const allCoreRunIds = [...new Set([...coreRunIdsByCloudRunId.values()].flat())].sort();
    const completedByCoreRunId = new Map<string, number>();
    const snapshotCompletedByCoreRunId = new Map<string, number>();
    const totalByCoreRunId = new Map<string, number>();
    if (allCoreRunIds.length > 0) {
      const [rows, snapshotRows, totals] = await Promise.all([
        this.sql`
          select run_id, count(distinct schedule_idx)::int as trials_completed
          from ${this.table("trial_conclusion_rows")}
          where run_id = any(${allCoreRunIds})
          group by run_id
        `.catch((error) => {
          if (isMissingRuntimeStore(error)) {
            return [];
          }
          throw error;
        }),
        this.snapshotTrialCountsForCloudRuns(ids),
        this.scheduleProgressTotalsForCoreRuns(ids, allCoreRunIds),
      ]);
      for (const row of rows) {
        completedByCoreRunId.set(String(row.run_id), Number(row.trials_completed));
      }
      for (const row of snapshotRows) {
        snapshotCompletedByCoreRunId.set(row.core_run_id, row.trials_completed);
      }
      for (const total of totals) {
        totalByCoreRunId.set(total.core_run_id, total.trials_total);
      }
    }
    return ids.map((cloudRunId) => {
      const coreRunIds = coreRunIdsByCloudRunId.get(cloudRunId) ?? [];
      if (coreRunIds.length === 0) {
        return { cloud_run_id: cloudRunId, trials_completed: null, trials_total: null };
      }
      const totals = coreRunIds
        .map((coreRunId) => totalByCoreRunId.get(coreRunId))
        .filter((total): total is number => total !== undefined);
      return {
        cloud_run_id: cloudRunId,
        trials_completed: coreRunIds.reduce((sum, coreRunId) => {
          const completed = Math.max(
            completedByCoreRunId.get(coreRunId) ?? 0,
            snapshotCompletedByCoreRunId.get(coreRunId) ?? 0,
          );
          return sum + completed;
        }, 0),
        trials_total: totals.length === 0 ? null : totals.reduce((sum, total) => sum + total, 0),
      };
    });
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
    const [trialRows, metricRows, contractStageRows, attemptObjectRows, attemptObjectContentRows] = await Promise.all([
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
      this.attemptObjectContentRows(coreRunIds, limit * 10),
    ]).catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [[], [], [], [], []];
      }
      throw error;
    });
    const attemptObjectContentByKey = new Map(
      attemptObjectContentRows.map((row) => [runtimeAttemptObjectKey(row), row]),
    );
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
    const legacyAttemptObjects = attemptObjectRows.map((row) => {
      const record = enrichAttemptObject({
        core_run_id: String(row.run_id),
        trial_id: String(row.trial_id),
        schedule_idx: Number(row.schedule_idx),
        attempt: Number(row.attempt),
        role: String(row.role),
        object_ref: String(row.object_ref),
        metadata: row.metadata_json === null ? null : parseJson(row.metadata_json),
        recorded_at_ms: Number(row.recorded_at_ms),
      });
      return enrichAttemptObjectWithContent(record, attemptObjectContentByKey.get(runtimeAttemptObjectKey(record)));
    });
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

  async upsertAttemptObjectContent(input: RuntimeAttemptObjectContentInsert): Promise<RuntimeAttemptObjectContentRecord> {
    const metadata = input.metadata ?? {
      source: "worker_runtime_artifact_upload",
      relative_path: input.relative_path ?? null,
      byte_size: input.byte_size,
      media_type: input.media_type,
      sha256: input.sha256,
    };
    return await this.sql.begin(async (tx) => {
      await tx`
        insert into ${this.table("attempt_objects")} (
          account_id,
          run_id,
          trial_id,
          schedule_idx,
          attempt,
          role,
          object_ref,
          metadata_json,
          recorded_at_ms
        )
        values (
          ${WORKER_INGEST_ACCOUNT_ID},
          ${input.core_run_id},
          ${input.trial_id},
          ${input.schedule_idx},
          ${input.attempt},
          ${input.role},
          ${input.object_ref},
          ${JSON.stringify(metadata)},
          ${input.recorded_at_ms}
        )
        on conflict (account_id, run_id, trial_id, schedule_idx, attempt, role)
        do update set
          object_ref = excluded.object_ref,
          metadata_json = excluded.metadata_json,
          recorded_at_ms = excluded.recorded_at_ms
      `;
      const rows = await tx`
        insert into ${this.table("attempt_object_contents")} (
          account_id,
          run_id,
          trial_id,
          schedule_idx,
          attempt,
          role,
          object_ref,
          storage_path,
          media_type,
          byte_size,
          sha256,
          relative_path,
          metadata_json,
          recorded_at_ms
        )
        values (
          ${WORKER_INGEST_ACCOUNT_ID},
          ${input.core_run_id},
          ${input.trial_id},
          ${input.schedule_idx},
          ${input.attempt},
          ${input.role},
          ${input.object_ref},
          ${input.storage_path},
          ${input.media_type},
          ${input.byte_size},
          ${input.sha256},
          ${input.relative_path ?? null},
          ${JSON.stringify(metadata)},
          ${input.recorded_at_ms}
        )
        on conflict (account_id, run_id, trial_id, schedule_idx, attempt, role)
        do update set
          object_ref = excluded.object_ref,
          storage_path = excluded.storage_path,
          media_type = excluded.media_type,
          byte_size = excluded.byte_size,
          sha256 = excluded.sha256,
          relative_path = excluded.relative_path,
          metadata_json = excluded.metadata_json,
          recorded_at_ms = excluded.recorded_at_ms
        returning *
      `;
      return attemptObjectContentRecordFromRow(rows[0]);
    });
  }

  async attemptObjectContent(
    cloudRunId: string,
    input: { trialId: string; role: string; attempt?: number; coreRunId?: string | undefined },
  ): Promise<RuntimeAttemptObjectContentRecord | null> {
    const coreRunIds = await this.coreRunIdsForCloudRun(cloudRunId);
    const scopedCoreRunIds = input.coreRunId
      ? coreRunIds.includes(input.coreRunId) ? [input.coreRunId] : []
      : coreRunIds;
    if (scopedCoreRunIds.length === 0) {
      return null;
    }
    const rows = await this.sql`
      select
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        role,
        object_ref,
        storage_path,
        media_type,
        byte_size,
        sha256,
        relative_path,
        metadata_json,
        recorded_at_ms
      from ${this.table("attempt_object_contents")}
      where run_id = any(${scopedCoreRunIds})
        and trial_id = ${input.trialId}
        and role = ${input.role}
        and (${input.attempt ?? null}::bigint is null or attempt = ${input.attempt ?? null})
      order by run_id, schedule_idx, attempt desc
      limit 1
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows[0] ? attemptObjectContentRecordFromRow(rows[0]) : null;
  }

  async workerLifecycleEvents(cloudRunId: string, input?: { limit?: number; afterRowSeq?: number | undefined }): Promise<WorkerLifecycleEventRecord[]> {
    const limit = boundedLimit(input?.limit, 200);
    const rows = await this.sql`
      select event_id, seq, event_type, payload, created_at
      from cloud.run_events
      where run_id = ${cloudRunId}
        and seq > ${input?.afterRowSeq ?? -1}
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

  async apiResources(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeApiResourcesInput = {},
  ): Promise<RuntimeApiResourceList> {
    let apiResources: RuntimeApiResourceList;
    try {
      const inventory = await this.resources(cloudRunId, run);
      apiResources = runtimeApiResourceList(cloudRunId, inventory.resources, inventory.core_run_ids);
    } catch (error) {
      await appendRuntimeApiResourcesReadAuditEvent(this.sql, {
        runId: cloudRunId,
        operation: "api-resources",
        requester: input.requester,
        error,
      });
      throw error;
    }
    await appendRuntimeApiResourcesReadAuditEvent(this.sql, {
      runId: cloudRunId,
      operation: "api-resources",
      requester: input.requester,
      apiResources,
    });
    return apiResources;
  }

  async apiResource(
    cloudRunId: string,
    run: CloudRunRecord,
    kind: string,
    input: RuntimeApiResourcesInput = {},
  ): Promise<RuntimeApiResourceRecord> {
    let apiResource: RuntimeApiResourceRecord;
    try {
      const inventory = await this.resources(cloudRunId, run);
      apiResource = runtimeApiResourceForKind(cloudRunId, inventory.resources, kind, inventory.core_run_ids);
    } catch (error) {
      await appendRuntimeApiResourcesReadAuditEvent(this.sql, {
        runId: cloudRunId,
        operation: "api-resource",
        requester: input.requester,
        selectedKind: kind,
        error,
      });
      throw error;
    }
    await appendRuntimeApiResourcesReadAuditEvent(this.sql, {
      runId: cloudRunId,
      operation: "api-resource",
      requester: input.requester,
      selectedKind: kind,
      apiResource,
    });
    return apiResource;
  }

  runtimeAccessRequestResource(
    run: CloudRunRecord,
    request: RuntimeAccessRequestRecord,
  ): RuntimeResourceRecord {
    return accessRequestResource(run, request);
  }

  runtimeAccessRequestResourceForRunId(
    runId: string,
    request: RuntimeAccessRequestRecord,
  ): RuntimeResourceRecord {
    return accessRequestResource({ run_id: runId } as CloudRunRecord, request);
  }

  async inspectBundle(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeInspectBundleInput = {},
  ): Promise<RuntimeInspectBundle> {
    const filter = runtimeInspectBundleFilter(input);
    const eventLimit = boundedLimit(input.eventLimit, 250);
    const resourceFilter = runtimeInspectBundleFilterView(filter);
    let bundle: RuntimeInspectBundle;
    try {
      const [resourceInventory, events] = await Promise.all([
        this.resources(cloudRunId, run, filter),
        this.eventRows(cloudRunId, { limit: Math.min(eventLimit + 1, 1000) }),
      ]);
      const eventList = runtimeEventListView(cloudRunId, {}, eventLimit, events);
      const resourceMetricsPage = paginateRuntimeResources(resourceInventory.resources, { limit: 100 });
      const resourceMetricsInventory: RuntimeResourceList = {
        ...resourceInventory,
        metadata: resourceMetricsPage.metadata,
        resources: resourceMetricsPage.resources,
      };
      const resourceMetrics = await Promise.all(resourceMetricsPage.resources
        .map(async (resource) => {
          const eventInput = runtimeResourceEventScanFilter(resource, undefined);
          const eventRows = await this.eventRows(cloudRunId, { limit: RUNTIME_EVENT_RESOURCE_LIMIT, ...eventInput });
          return runtimeResourceMetricsView(
            cloudRunId,
            resourceInventory.core_run_ids,
            resource,
            runtimeResourceEventsForResource(eventRows, resource),
          );
        }));
      bundle = {
        apiVersion: RUNTIME_API_VERSION,
        kind: "RuntimeInspectBundle",
        cloud_run_id: cloudRunId,
        generated_at: new Date().toISOString(),
        resource_filter: resourceFilter,
        api_resources: runtimeApiResourceList(cloudRunId, resourceInventory.resources, resourceInventory.core_run_ids),
        resource_inventory: resourceInventory,
        resource_health: runtimeResourceHealthSummary(resourceInventory),
        resource_metrics: runtimeResourceMetricsListView(
          cloudRunId,
          resourceMetricsInventory,
          resourceMetrics,
          resourceInventory.resources.length,
        ),
        event_list: eventList,
        log_refs: runtimeInspectLogRefs(cloudRunId, resourceInventory.resources),
      };
    } catch (error) {
      await appendRuntimeInspectBundleAuditEvent(this.sql, {
        runId: cloudRunId,
        requester: input.requester,
        eventLimit,
        resourceFilter,
        error,
      });
      throw error;
    }
    await appendRuntimeInspectBundleAuditEvent(this.sql, {
      runId: cloudRunId,
      requester: input.requester,
      eventLimit,
      resourceFilter,
      bundle,
    });
    return bundle;
  }

  async resources(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeResourceListInput = {},
  ): Promise<RuntimeResourceList> {
    let list: RuntimeResourceList;
    try {
      const eventRows = typeof this.eventRows === "function"
        ? this.eventRows(cloudRunId, { limit: RUNTIME_EVENT_RESOURCE_LIMIT })
        : Promise.resolve([]);
      const [coreRunIds, runtimeSnapshots, attempts, provisionRequests, portForwards, execRequests, events] = await Promise.all([
        this.coreRunIdsForCloudRun(cloudRunId),
        this.workerRuntimeSnapshots(cloudRunId),
        this.runAttempts(cloudRunId),
        this.provisionRequests(cloudRunId),
        this.listPortForwards(cloudRunId),
        this.listExecRequests(cloudRunId),
        eventRows,
      ]);
      const accessRequests = [...portForwards, ...execRequests];
      const runnerPoolIds = runtimeRunnerPoolIds(attempts, provisionRequests);
      const coreRunRows = typeof this.coreRuns === "function"
        ? this.coreRuns(coreRunIds)
        : Promise.resolve([]);
      const runManifestRows = typeof this.runManifests === "function"
        ? this.runManifests(coreRunIds)
        : Promise.resolve([]);
      const metricDefinitionRows = typeof this.metricDefinitions === "function"
        ? runManifestRows.then((manifests) => this.metricDefinitions(runtimeManifestExperimentIds(manifests)))
        : Promise.resolve([]);
      const trialAttemptRows = typeof this.trialAttempts === "function"
        ? this.trialAttempts(coreRunIds)
        : Promise.resolve([]);
      const attemptObjectRows = typeof this.attemptObjects === "function"
        ? this.attemptObjects(coreRunIds, runtimeSnapshots)
        : Promise.resolve(runtimeAttemptObjectsFromSnapshots(runtimeSnapshots));
      const contractStageRows = typeof this.contractStages === "function"
        ? this.contractStages(coreRunIds, runtimeSnapshots)
        : Promise.resolve(runtimeContractStagesFromSnapshots(runtimeSnapshots));
      const runtimeValueRows = typeof this.runtimeValueRecords === "function"
        ? this.runtimeValueRecords(coreRunIds, runtimeSnapshots)
        : Promise.resolve(runtimeValueRecordsFromSnapshots(runtimeSnapshots));
      const metricObservationRows = typeof this.metricObservations === "function"
        ? this.metricObservations(coreRunIds, runtimeSnapshots)
        : Promise.resolve(runtimeMetricObservationsFromTrialResults(runtimeTrialResultsFromSnapshots(runtimeSnapshots)));
      const performanceSampleRows = typeof this.performanceSamples === "function"
        ? this.performanceSamples(coreRunIds)
        : Promise.resolve([]);
      const runtimeOperationRows = typeof this.runtimeOperations === "function"
        ? this.runtimeOperations(coreRunIds)
        : Promise.resolve([]);
      const slotCommitRows = typeof this.slotCommitRecords === "function"
        ? this.slotCommitRecords(coreRunIds)
        : Promise.resolve([]);
      const pendingCompletionRows = typeof this.pendingTrialCompletions === "function"
        ? this.pendingTrialCompletions(coreRunIds)
        : Promise.resolve([]);
      const variantSnapshotRows = typeof this.variantSnapshots === "function"
        ? this.variantSnapshots(coreRunIds)
        : Promise.resolve([]);
      const evidenceRows = typeof this.evidenceRows === "function"
        ? this.evidenceRows(coreRunIds)
        : Promise.resolve([]);
      const chainStateRows = typeof this.chainStateRows === "function"
        ? this.chainStateRows(coreRunIds)
        : Promise.resolve([]);
      const trialConclusionRows = typeof this.trialConclusionRows === "function"
        ? this.trialConclusionRows(coreRunIds)
        : Promise.resolve([]);
      const lineageVersionRows = typeof this.lineageVersions === "function"
        ? this.lineageVersions(coreRunIds)
        : Promise.resolve([]);
      const lineageHeadRows = typeof this.lineageHeads === "function"
        ? this.lineageHeads(coreRunIds)
        : Promise.resolve([]);
      const [runnerPools, coreRuns, runManifests, metricDefinitions, slots, slotCommits, pendingCompletions, variantSnapshots, evidenceRecords, chainStates, trialConclusions, lineageVersions, lineageHeads, trialAttempts, containers, contractStages, runtimeValues, metricObservations, performanceSamples, runtimeOperations, attemptObjects] = await Promise.all([
        this.runnerPools(runnerPoolIds),
        coreRunRows,
        runManifestRows,
        metricDefinitionRows,
        this.scheduleSlots(coreRunIds),
        slotCommitRows,
        pendingCompletionRows,
        variantSnapshotRows,
        evidenceRows,
        chainStateRows,
        trialConclusionRows,
        lineageVersionRows,
        lineageHeadRows,
        trialAttemptRows,
        this.trialContainers(coreRunIds),
        contractStageRows,
        runtimeValueRows,
        metricObservationRows,
        performanceSampleRows,
        runtimeOperationRows,
        attemptObjectRows,
      ]);
      const trials = runtimeTrialRecords(trialAttempts, runtimeSnapshots);
      const filteredResources = filterRuntimeResources([
        ...declaredRuntimeResources(run, attempts, events),
        ...coreRuns.map((coreRun) => coreRunResource(run, coreRun)),
        ...runManifests.map((manifest) => runManifestResource(run, manifest)),
        ...metricDefinitions.map((definition) => metricDefinitionResource(run, definition, runManifests)),
        ...runnerPools.map((pool) => runnerPoolResource(run, pool, attempts, provisionRequests)),
        ...runnerInstanceResources(run, attempts),
        ...attempts.map((attempt) => attemptResource(run, attempt)),
        ...provisionRequests.map((request) => provisionRequestResource(run, request)),
        ...trials.map((trial) => trialResource(run, trial, slots, containers, attempts)),
        ...slots.map((slot) => scheduleSlotResource(run, slot, attempts)),
        ...slotCommits.map((commit) => slotCommitResource(run, commit)),
        ...pendingCompletions.map((completion) => pendingTrialCompletionResource(run, completion)),
        ...variantSnapshots.map((snapshot) => variantSnapshotResource(run, snapshot)),
        ...evidenceRecords.map((record) => provenanceRowResource(run, "EvidenceRecord", record)),
        ...chainStates.map((record) => provenanceRowResource(run, "ChainState", record)),
        ...trialConclusions.map((record) => provenanceRowResource(run, "TrialConclusion", record)),
        ...lineageVersions.map((version) => lineageVersionResource(run, version, lineageHeads)),
        ...lineageHeads.map((head) => lineageHeadResource(run, head)),
        ...containers.map((container) => trialContainerResource(run, container, slots, attempts)),
        ...contractStages.map((stage) => trialStageResource(run, stage)),
        ...runtimeValues.map((value) => runtimeValueResource(run, value)),
        ...metricObservations.map((observation) => metricObservationResource(run, observation)),
        ...performanceSamples.map((sample) => performanceSampleResource(run, sample)),
        ...runtimeOperations.map((operation) => runtimeOperationResource(run, operation)),
        ...attemptObjects.map((object) => trialArtifactResource(run, object)),
        ...runtimeSnapshots.map((snapshot) => runtimeSnapshotResource(run, snapshot)),
        ...accessRequests.map((request) => accessRequestResource(run, request)),
        ...runtimeEventResources(run, events),
      ], input);
      const page = paginateRuntimeResources(filteredResources, input);
      list = {
        apiVersion: RUNTIME_API_VERSION,
        kind: "RuntimeResourceList",
        metadata: page.metadata,
        cloud_run_id: cloudRunId,
        core_run_ids: coreRunIds,
        resources: page.resources,
      };
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.list.read.failed",
        operation: "list",
        requester: input.requester,
        input,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.list.read",
      operation: "list",
      requester: input.requester,
      input,
      list,
    });
    return list;
  }

  async watchResources(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeResourceWatchInput = {},
  ): Promise<RuntimeResourceWatchList> {
    let watch: RuntimeResourceWatchList;
    try {
      const inventory = await this.resources(cloudRunId, run, input.filter ?? {});
      const currentListVersion = inventory.metadata.resourceVersion;
      const known = runtimeResourceKnownVersions(input.knownResourceVersions);
      const currentVersions: Record<string, string> = {};
      const currentKeys = new Set<string>();
      const events: RuntimeResourceWatchEvent[] = [];

      for (const resource of inventory.resources) {
        const key = runtimeResourceWatchKey(resource);
        const version = resource.metadata.resourceVersion ?? runtimeResourceVersion(resource);
        currentKeys.add(key);
        currentVersions[key] = version;
        if (input.resourceVersion && input.resourceVersion === currentListVersion && known.size === 0) {
          continue;
        }
        const previousVersion = known.get(key);
        if (!previousVersion) {
          events.push({
            type: "ADDED",
            resource_ref: runtimeResourceWatchRef(resource),
            resource_version: version,
            resource,
          });
        } else if (previousVersion !== version) {
          events.push({
            type: "MODIFIED",
            resource_ref: runtimeResourceWatchRef(resource),
            resource_version: version,
            previous_resource_version: previousVersion,
            resource,
          });
        }
      }

      if (!(input.resourceVersion && input.resourceVersion === currentListVersion && known.size === 0)) {
        for (const [key, previousVersion] of known.entries()) {
          if (currentKeys.has(key)) {
            continue;
          }
          events.push({
            type: "DELETED",
            resource_ref: runtimeResourceWatchRefFromKey(key),
            previous_resource_version: previousVersion,
          });
        }
      }

      if (input.allowBookmarks && events.length === 0) {
        events.push({
          type: "BOOKMARK",
          resource_ref: {
            apiVersion: RUNTIME_API_VERSION,
            kind: "RuntimeResourceList",
            name: cloudRunId,
          },
          resource_version: currentListVersion,
        });
      }

      watch = {
        apiVersion: RUNTIME_API_VERSION,
        kind: "RuntimeResourceWatchList",
        cloud_run_id: cloudRunId,
        generated_at: new Date().toISOString(),
        core_run_ids: inventory.core_run_ids,
        resource_versions: currentVersions,
        events,
        resource_inventory: inventory,
      };
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.watch.read.failed",
        operation: "watch",
        requester: input.requester,
        input,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.watch.read",
      operation: "watch",
      requester: input.requester,
      input,
      watch,
    });
    return watch;
  }

  async resourceHealth(
    cloudRunId: string,
    run: CloudRunRecord,
    filter: RuntimeResourceFilter & { requester?: string | null | undefined } = {},
  ): Promise<RuntimeResourceHealth> {
    let health: RuntimeResourceHealth;
    try {
      health = runtimeResourceHealthSummary(await this.resources(cloudRunId, run, runtimeResourceFilterOnly(filter)));
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.health.read.failed",
        operation: "health",
        requester: filter.requester,
        input: filter,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.health.read",
      operation: "health",
      requester: filter.requester,
      input: filter,
      health,
    });
    return health;
  }

  async describeResource(
    cloudRunId: string,
    run: CloudRunRecord,
    input: { kind: string; name: string; eventLimit?: number | undefined; requester?: string | null | undefined },
  ): Promise<RuntimeResourceDescribe> {
    let described: RuntimeResourceDescribe;
    try {
      const list = await this.resources(cloudRunId, run);
      const resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity(input));
      if (!resource) {
        throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
          kind: input.kind,
          name: input.name,
        });
      }
      const eventLimit = boundedLimit(input.eventLimit, 100);
      const eventScanLimit = Math.max(eventLimit + 1, RUNTIME_EVENT_RESOURCE_LIMIT);
      const eventInput: RuntimeEventRowsInput = runtimeResourceEventScanFilter(resource, undefined);
      const relatedEvents = runtimeResourceEventsForResource(
        await this.eventRows(cloudRunId, { limit: eventScanLimit, ...eventInput }),
        resource,
      );
      const eventList = runtimeResourceEventListView(
        cloudRunId,
        list.core_run_ids,
        resource,
        eventInput,
        eventLimit,
        relatedEvents,
      );
      described = {
        apiVersion: RUNTIME_API_VERSION,
        kind: "RuntimeResourceDescribe",
        cloud_run_id: cloudRunId,
        generated_at: new Date().toISOString(),
        core_run_ids: list.core_run_ids,
        resource,
        operations: runtimeResourceOperationsForDescribe(cloudRunId, run, list.resources, resource),
        related_resources: relatedRuntimeResources(list.resources, resource),
        event_list: eventList,
      };
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.describe.read.failed",
        operation: "describe",
        requester: input.requester,
        input,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.describe.read",
      operation: "describe",
      requester: input.requester,
      input,
      resource: described.resource,
      eventList: described.event_list,
      relatedResources: described.related_resources.length,
    });
    return described;
  }

  async reviewResourceOperation(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeResourceOperationReviewInput,
  ): Promise<RuntimeResourceOperationReview> {
    let review: RuntimeResourceOperationReview;
    try {
      const requestedOperation = runtimeResourceOperationKey(input.operation);
      if (!requestedOperation) {
        throw new HttpError(400, "runtime_operation_required", "Runtime operation is required", {
          operation: input.operation,
        });
      }
      const list = await this.resources(cloudRunId, run);
      const resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity(input));
      if (!resource) {
        throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
          kind: input.kind,
          name: input.name,
        });
      }
      const operations = runtimeResourceOperationsForDescribe(cloudRunId, run, list.resources, resource);
      const matched = operations.find((operation) => runtimeResourceOperationKey(operation.purpose) === requestedOperation)
        ?? operations.find((operation) => runtimeResourceOperationMatches(operation.purpose, requestedOperation));
      const reviewEvidence = runtimeResourceOperationReviewEvidence(resource);
      if (matched) {
        review = {
          apiVersion: RUNTIME_API_VERSION,
          kind: "RuntimeResourceOperationReview",
          cloud_run_id: cloudRunId,
          generated_at: new Date().toISOString(),
          core_run_ids: list.core_run_ids,
          resource_ref: runtimeResourceWatchRef(resource),
          ...reviewEvidence,
          operation: input.operation,
          matched_operation: matched.purpose,
          supported: matched.supported,
          reason: matched.reason,
          message: matched.message,
          command: matched.command,
          verb: matched.verb,
          subresource: matched.subresource,
          action: matched.action,
          requires_running_run: matched.requires_running_run,
        };
      } else {
        review = {
          apiVersion: RUNTIME_API_VERSION,
          kind: "RuntimeResourceOperationReview",
          cloud_run_id: cloudRunId,
          generated_at: new Date().toISOString(),
          core_run_ids: list.core_run_ids,
          resource_ref: runtimeResourceWatchRef(resource),
          ...reviewEvidence,
          operation: input.operation,
          matched_operation: null,
          supported: false,
          reason: "operation_unavailable",
          message: `${resource.kind}/${resource.metadata.name} does not currently advertise runtime operation ${input.operation}`,
          command: null,
          verb: null,
          subresource: null,
          action: null,
          requires_running_run: null,
        };
      }
    } catch (error) {
      await appendRuntimeOperationReviewAuditEvent(this.sql, {
        runId: cloudRunId,
        requester: input.requester,
        requestedKind: input.kind,
        requestedName: input.name,
        operation: input.operation,
        error,
      });
      throw error;
    }
    await appendRuntimeOperationReviewAuditEvent(this.sql, {
      runId: cloudRunId,
      requester: input.requester,
      review,
    });
    return review;
  }

  async getResource(
    cloudRunId: string,
    run: CloudRunRecord,
    input: { kind: string; name: string; requester?: string | null | undefined },
  ): Promise<RuntimeResourceRecord> {
    let resource: RuntimeResourceRecord;
    try {
      const list = await this.resources(cloudRunId, run);
      const found = list.resources.find((item) => resourceIdentity(item) === resourceIdentity(input));
      if (!found) {
        throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
          kind: input.kind,
          name: input.name,
        });
      }
      resource = found;
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.get.read.failed",
        operation: "get",
        requester: input.requester,
        input,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.get.read",
      operation: "get",
      requester: input.requester,
      input,
      resource,
    });
    return resource;
  }

  async resourceEvents(
    cloudRunId: string,
    run: CloudRunRecord,
    input: {
      kind: string;
      name: string;
      limit?: number | undefined;
      afterRowSeq?: number | undefined;
      continueToken?: string | null | undefined;
      filter?: RuntimeEventFilter | undefined;
      requester?: string | null | undefined;
    },
  ): Promise<RuntimeResourceEventList> {
    let eventList: RuntimeResourceEventList;
    let resource: RuntimeResourceRecord | undefined;
    try {
      const list = await this.resources(cloudRunId, run);
      resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity(input));
      if (!resource) {
        throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
          kind: input.kind,
          name: input.name,
        });
      }
      const eventLimit = boundedLimit(input.limit, 100);
      const eventScanLimit = Math.max(eventLimit + 1, RUNTIME_EVENT_RESOURCE_LIMIT);
      const resourceEventFilter = runtimeResourceEventScanFilter(resource, input.filter);
      const eventInput = runtimeEventRowsInputWithContinue({
        afterRowSeq: input.afterRowSeq,
        continueToken: input.continueToken,
        ...resourceEventFilter,
      });
      const relatedEvents = runtimeResourceEventsForResource(
        await this.eventRows(cloudRunId, {
          limit: eventScanLimit,
          ...eventInput,
        }),
        resource,
      );
      eventList = runtimeResourceEventListView(
        cloudRunId,
        list.core_run_ids,
        resource,
        eventInput,
        eventLimit,
        relatedEvents,
      );
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.events.read.failed",
        operation: "events",
        requester: input.requester,
        input,
        resource,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.events.read",
      operation: "events",
      requester: input.requester,
      input,
      resource,
      eventList,
    });
    return eventList;
  }

  async resourceStatus(
    cloudRunId: string,
    run: CloudRunRecord,
    input: { kind: string; name: string; requester?: string | null | undefined },
  ): Promise<RuntimeResourceStatus> {
    let status: RuntimeResourceStatus;
    let resource: RuntimeResourceRecord | undefined;
    try {
      const list = await this.resources(cloudRunId, run);
      resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity(input));
      if (!resource) {
        throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
          kind: input.kind,
          name: input.name,
        });
      }
      status = runtimeResourceStatusView(cloudRunId, list.core_run_ids, resource);
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.status.read.failed",
        operation: "status",
        requester: input.requester,
        input,
        resource,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.status.read",
      operation: "status",
      requester: input.requester,
      input,
      resource,
      status,
    });
    return status;
  }

  async resourceMetrics(
    cloudRunId: string,
    run: CloudRunRecord,
    input: { kind: string; name: string; requester?: string | null | undefined },
  ): Promise<RuntimeResourceMetrics> {
    let metrics: RuntimeResourceMetrics;
    let resource: RuntimeResourceRecord | undefined;
    try {
      const list = await this.resources(cloudRunId, run);
      resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity(input));
      if (!resource) {
        throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
          kind: input.kind,
          name: input.name,
        });
      }
      const eventInput = runtimeResourceEventScanFilter(resource, undefined);
      const events = runtimeResourceEventsForResource(
        await this.eventRows(cloudRunId, { limit: RUNTIME_EVENT_RESOURCE_LIMIT, ...eventInput }),
        resource,
      );
      metrics = runtimeResourceMetricsView(cloudRunId, list.core_run_ids, resource, events);
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.metrics.read.failed",
        operation: "metrics",
        requester: input.requester,
        input,
        resource,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.metrics.read",
      operation: "metrics",
      requester: input.requester,
      input,
      resource,
      metrics,
    });
    return metrics;
  }

  async resourceMetricsList(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeResourceMetricsListInput = {},
  ): Promise<RuntimeResourceMetricsList> {
    let metricsList: RuntimeResourceMetricsList;
    try {
      const { limit, continueToken } = input;
      const list = await this.resources(cloudRunId, run, runtimeResourceFilterOnly(input));
      const page = paginateRuntimeResources(list.resources, {
        limit: boundedLimit(limit, 100),
        continueToken,
      });
      const pagedList: RuntimeResourceList = {
        ...list,
        metadata: page.metadata,
        resources: page.resources,
      };
      const resources = await Promise.all(page.resources
        .map(async (resource) => {
          const eventInput = runtimeResourceEventScanFilter(resource, undefined);
          const eventRows = await this.eventRows(cloudRunId, { limit: RUNTIME_EVENT_RESOURCE_LIMIT, ...eventInput });
          return runtimeResourceMetricsView(
            cloudRunId,
            list.core_run_ids,
            resource,
            runtimeResourceEventsForResource(eventRows, resource),
          );
        }));
      metricsList = runtimeResourceMetricsListView(cloudRunId, pagedList, resources, list.resources.length);
    } catch (error) {
      await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.metrics.list.read.failed",
        operation: "metrics/list",
        requester: input.requester,
        input,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceQueryReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.metrics.list.read",
      operation: "metrics/list",
      requester: input.requester,
      input,
      metricsList,
    });
    return metricsList;
  }

  async resourceLogs(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeResourceLogsInput,
  ): Promise<RuntimeResourceLogs> {
    let stream: "stdout" | "stderr";
    try {
      stream = runtimeLogStream(input.stream);
    } catch (error) {
      await appendRuntimeResourceReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.logs.read.failed",
        operation: "logs",
        requester: input.requester,
        requestedKind: input.kind,
        requestedName: input.name,
        stream: input.stream,
        tailLines: input.tailLines,
        error,
      });
      throw error;
    }
    let described: RuntimeResourceDescribe;
    try {
      described = await this.describeResource(cloudRunId, run, {
        kind: input.kind,
        name: input.name,
        eventLimit: 25,
      });
    } catch (error) {
      await appendRuntimeResourceReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.logs.read.failed",
        operation: "logs",
        requester: input.requester,
        requestedKind: input.kind,
        requestedName: input.name,
        stream,
        tailLines: input.tailLines,
        error,
      });
      throw error;
    }
    let logs: RuntimeResourceLogs;
    try {
      if (described.resource.kind === "RunnerAttempt" || described.resource.kind === "RunnerInstance") {
        const eventInput = runtimeResourceLogEventScanFilter(described.resource);
        const events = runtimeResourceEventsForResource(
          await this.eventRows(cloudRunId, { limit: RUNTIME_EVENT_RESOURCE_LIMIT, ...eventInput }),
          described.resource,
        );
        logs = runnerResourceLogsFromEvents(cloudRunId, described.resource, events, stream, input.tailLines);
      } else if (described.resource.kind === "Exec") {
        logs = execResourceLogs(cloudRunId, described.resource, stream, input.tailLines);
      } else {
        const target = runtimeLogTargetForResource(described.resource);
        const artifactInput: { trialId: string; role: string; coreRunId?: string | null; attempt?: number | undefined } = {
          trialId: target.trialId,
          role: stream,
        };
        if (target.coreRunId !== undefined) {
          artifactInput.coreRunId = target.coreRunId;
        }
        if (target.attempt !== undefined) {
          artifactInput.attempt = target.attempt;
        }
        const content = await this.artifactContent(cloudRunId, artifactInput);
        const bytes = input.tailLines === undefined
          ? content.bytes
          : tailTextBytes(content.bytes, input.tailLines);
        logs = {
          resource: described.resource,
          stream,
          media_type: content.media_type,
          bytes,
          object: content.object,
        };
      }
    } catch (error) {
      await appendRuntimeResourceReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.logs.read.failed",
        operation: "logs",
        requester: input.requester,
        resource: described.resource,
        stream,
        tailLines: input.tailLines,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.logs.read",
      operation: "logs",
      requester: input.requester,
      resource: logs.resource,
      object: logs.object,
      mediaType: logs.media_type,
      byteSize: logs.bytes.byteLength,
      stream: logs.stream,
      tailLines: input.tailLines,
    });
    return logs;
  }

  async resourceArtifactContent(
    cloudRunId: string,
    run: CloudRunRecord,
    input: RuntimeResourceArtifactContentInput,
  ): Promise<RuntimeResourceArtifactContent> {
    let described: RuntimeResourceDescribe;
    try {
      described = await this.describeResource(cloudRunId, run, {
        kind: input.kind,
        name: input.name,
        eventLimit: 25,
      });
    } catch (error) {
      await appendRuntimeResourceReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.content.read.failed",
        operation: "content",
        requester: input.requester,
        requestedKind: input.kind,
        requestedName: input.name,
        error,
      });
      throw error;
    }
    let artifact: RuntimeResourceArtifactContent;
    try {
      if (described.resource.kind !== "TrialArtifact") {
        throw new HttpError(404, "runtime_resource_content_not_found", "Runtime resource content subresource is only available for TrialArtifact resources", {
          kind: described.resource.kind,
          name: described.resource.metadata.name,
        });
      }
      const trialId = stringField(described.resource.spec.trial_id);
      const role = stringField(described.resource.spec.role);
      if (!trialId || !role) {
        throw new HttpError(409, "runtime_artifact_resource_incomplete", "TrialArtifact resource is missing artifact identity fields", {
          kind: described.resource.kind,
          name: described.resource.metadata.name,
        });
      }
      const attempt = numberField(described.resource.spec.attempt);
      const content = await this.artifactContent(cloudRunId, {
        trialId,
        role,
        coreRunId: stringField(described.resource.spec.core_run_id),
        attempt: attempt ?? undefined,
        objectRef: stringField(described.resource.spec.object_ref),
      });
      artifact = {
        resource: described.resource,
        media_type: content.media_type,
        bytes: content.bytes,
        object: content.object,
      };
    } catch (error) {
      await appendRuntimeResourceReadAuditEvent(this.sql, {
        runId: cloudRunId,
        eventType: "runtime.resource.content.read.failed",
        operation: "content",
        requester: input.requester,
        resource: described.resource,
        error,
      });
      throw error;
    }
    await appendRuntimeResourceReadAuditEvent(this.sql, {
      runId: cloudRunId,
      eventType: "runtime.resource.content.read",
      operation: "content",
      requester: input.requester,
      resource: artifact.resource,
      object: artifact.object,
      mediaType: artifact.media_type,
      byteSize: artifact.bytes.byteLength,
    });
    return artifact;
  }

  async cordonRunnerInstanceResource(input: RuntimeResourceActionInput): Promise<RuntimeResourceDescribe> {
    const list = await this.resources(input.run.run_id, input.run);
    const resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity({
      kind: input.resourceKind,
      name: input.resourceName,
    }));
    if (!resource) {
      throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
        kind: input.resourceKind,
        name: input.resourceName,
      });
    }
    assertRuntimeResourceVersionPrecondition(resource, input.resourceVersion, { required: true, operation: "cordon" });
    if (resource.kind !== "RunnerInstance") {
      throw new HttpError(409, "runtime_resource_action_unsupported", "Only RunnerInstance resources support cordon", {
        action: "cordon",
        resource_kind: resource.kind,
        resource_name: resource.metadata.name,
        supported_kinds: ["RunnerInstance"],
      });
    }
    assertRuntimeResourceSupportsAction(resource, "cordon");
    const runnerInstanceId = stringField(resource.spec.runner_instance_id) ?? resource.metadata.uid ?? null;
    if (!runnerInstanceId) {
      throw new HttpError(409, "runtime_resource_action_unavailable", "RunnerInstance is missing runner_instance_id");
    }
    const previousStatus = stringField(resource.status.phase) ?? "unknown";
    const requester = normalizeOptionalString(input.requester);
    const reason = normalizeOptionalString(input.reason);
    const attemptId = stringField(resource.status.current_attempt_id)
      ?? stringFields(resource.spec.attempt_ids)[0]
      ?? null;
    await this.sql.begin(async (tx) => {
      const rows = await tx`
        update cloud.runner_instances
        set status = 'cordoned',
            metadata = metadata || ${this.sql.json({
              last_runtime_action: {
                action: "cordon",
                requested_by: requester,
                reason,
                recorded_at: new Date().toISOString(),
              },
            })}::jsonb,
            updated_at = now()
        where runner_instance_id = ${runnerInstanceId}
          and status in ('online', 'cordoned')
        returning *
      `;
      if (!rows[0]) {
        throw new HttpError(409, "runtime_resource_action_unavailable", "RunnerInstance is not online or cordoned");
      }
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.run.run_id,
        attemptId,
        eventType: "runtime.resource.runner_instance.cordoned",
        payload: compactObject({
          action: "cordon",
          resource_ref: runtimeResourceEventRef(resource),
          resource_kind: resource.kind,
          resource_name: resource.metadata.name,
          resource_uid: resource.metadata.uid,
          runner_instance_id: runnerInstanceId,
          attempt_id: attemptId,
          previous_status: previousStatus,
          status: "cordoned",
          requester,
          reason,
          resource_version_precondition: normalizeOptionalString(input.resourceVersion),
        }),
      });
    });
    return await this.describeResource(input.run.run_id, input.run, {
      kind: resource.kind,
      name: resource.metadata.name,
      eventLimit: 100,
    });
  }

  async drainRunnerInstanceResource(input: RuntimeResourceActionInput): Promise<RuntimeResourceDescribe> {
    const list = await this.resources(input.run.run_id, input.run);
    const resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity({
      kind: input.resourceKind,
      name: input.resourceName,
    }));
    if (!resource) {
      throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
        kind: input.resourceKind,
        name: input.resourceName,
      });
    }
    assertRuntimeResourceVersionPrecondition(resource, input.resourceVersion, { required: true, operation: "drain" });
    if (resource.kind !== "RunnerInstance") {
      throw new HttpError(409, "runtime_resource_action_unsupported", "Only RunnerInstance resources support drain", {
        action: "drain",
        resource_kind: resource.kind,
        resource_name: resource.metadata.name,
        supported_kinds: ["RunnerInstance"],
      });
    }
    assertRuntimeResourceSupportsAction(resource, "drain");
    const runnerInstanceId = stringField(resource.spec.runner_instance_id) ?? resource.metadata.uid ?? null;
    if (!runnerInstanceId) {
      throw new HttpError(409, "runtime_resource_action_unavailable", "RunnerInstance is missing runner_instance_id");
    }
    const previousStatus = stringField(resource.status.phase) ?? "unknown";
    const requester = normalizeOptionalString(input.requester);
    const reason = normalizeOptionalString(input.reason);
    const attemptId = stringField(resource.status.current_attempt_id)
      ?? stringFields(resource.spec.attempt_ids)[0]
      ?? null;
    await this.sql.begin(async (tx) => {
      const rows = await tx`
        update cloud.runner_instances
        set status = 'draining',
            metadata = metadata || ${this.sql.json({
              last_runtime_action: {
                action: "drain",
                requested_by: requester,
                reason,
                recorded_at: new Date().toISOString(),
              },
            })}::jsonb,
            updated_at = now()
        where runner_instance_id = ${runnerInstanceId}
          and status in ('online', 'cordoned', 'draining')
        returning *
      `;
      if (!rows[0]) {
        throw new HttpError(409, "runtime_resource_action_unavailable", "RunnerInstance is not online, cordoned, or draining");
      }
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.run.run_id,
        attemptId,
        eventType: "runtime.resource.runner_instance.drained",
        payload: compactObject({
          action: "drain",
          resource_ref: runtimeResourceEventRef(resource),
          resource_kind: resource.kind,
          resource_name: resource.metadata.name,
          resource_uid: resource.metadata.uid,
          runner_instance_id: runnerInstanceId,
          attempt_id: attemptId,
          previous_status: previousStatus,
          status: "draining",
          requester,
          reason,
          resource_version_precondition: normalizeOptionalString(input.resourceVersion),
        }),
      });
    });
    return await this.describeResource(input.run.run_id, input.run, {
      kind: resource.kind,
      name: resource.metadata.name,
      eventLimit: 100,
    });
  }

  async uncordonRunnerInstanceResource(input: RuntimeResourceActionInput): Promise<RuntimeResourceDescribe> {
    const list = await this.resources(input.run.run_id, input.run);
    const resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity({
      kind: input.resourceKind,
      name: input.resourceName,
    }));
    if (!resource) {
      throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
        kind: input.resourceKind,
        name: input.resourceName,
      });
    }
    assertRuntimeResourceVersionPrecondition(resource, input.resourceVersion, { required: true, operation: "uncordon" });
    if (resource.kind !== "RunnerInstance") {
      throw new HttpError(409, "runtime_resource_action_unsupported", "Only RunnerInstance resources support uncordon", {
        action: "uncordon",
        resource_kind: resource.kind,
        resource_name: resource.metadata.name,
        supported_kinds: ["RunnerInstance"],
      });
    }
    assertRuntimeResourceSupportsAction(resource, "uncordon");
    const runnerInstanceId = stringField(resource.spec.runner_instance_id) ?? resource.metadata.uid ?? null;
    if (!runnerInstanceId) {
      throw new HttpError(409, "runtime_resource_action_unavailable", "RunnerInstance is missing runner_instance_id");
    }
    const previousStatus = stringField(resource.status.phase) ?? "unknown";
    const requester = normalizeOptionalString(input.requester);
    const reason = normalizeOptionalString(input.reason);
    const attemptId = stringField(resource.status.current_attempt_id)
      ?? stringFields(resource.spec.attempt_ids)[0]
      ?? null;
    await this.sql.begin(async (tx) => {
      const rows = await tx`
        update cloud.runner_instances
        set status = 'online',
            metadata = metadata || ${this.sql.json({
              last_runtime_action: {
                action: "uncordon",
                requested_by: requester,
                reason,
                recorded_at: new Date().toISOString(),
              },
            })}::jsonb,
            updated_at = now()
        where runner_instance_id = ${runnerInstanceId}
          and status in ('draining', 'cordoned')
        returning *
      `;
      if (!rows[0]) {
        throw new HttpError(409, "runtime_resource_action_unavailable", "RunnerInstance is not draining or cordoned");
      }
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.run.run_id,
        attemptId,
        eventType: "runtime.resource.runner_instance.uncordoned",
        payload: compactObject({
          action: "uncordon",
          resource_ref: runtimeResourceEventRef(resource),
          resource_kind: resource.kind,
          resource_name: resource.metadata.name,
          resource_uid: resource.metadata.uid,
          runner_instance_id: runnerInstanceId,
          attempt_id: attemptId,
          previous_status: previousStatus,
          status: "online",
          requester,
          reason,
          resource_version_precondition: normalizeOptionalString(input.resourceVersion),
        }),
      });
    });
    return await this.describeResource(input.run.run_id, input.run, {
      kind: resource.kind,
      name: resource.metadata.name,
      eventLimit: 100,
    });
  }

  async cancelRuntimeAccessResource(input: RuntimeResourceActionInput): Promise<RuntimeResourceDescribe> {
    const list = await this.resources(input.run.run_id, input.run);
    const resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity({
      kind: input.resourceKind,
      name: input.resourceName,
    }));
    if (!resource) {
      throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
        kind: input.resourceKind,
        name: input.resourceName,
      });
    }
    assertRuntimeResourceVersionPrecondition(resource, input.resourceVersion, { required: true, operation: "cancel" });
    if (resource.kind !== "PortForward" && resource.kind !== "Exec") {
      throw new HttpError(409, "runtime_resource_action_unsupported", "Only PortForward and Exec resources support cancel", {
        action: "cancel",
        resource_kind: resource.kind,
        resource_name: resource.metadata.name,
        supported_kinds: ["PortForward", "Exec"],
      });
    }
    assertRuntimeResourceSupportsAction(resource, "cancel");
    const accessRequestId = stringField(resource.spec.access_request_id) ?? resource.metadata.uid ?? null;
    if (!accessRequestId) {
      throw new HttpError(409, "runtime_resource_action_unavailable", `${resource.kind} is missing access_request_id`);
    }
    if (resource.kind === "PortForward") {
      await this.cancelPortForwardRequest({
        cloudRunId: input.run.run_id,
        accessRequestId,
        requester: input.requester ?? null,
        reason: input.reason ?? null,
        resourceVersion: input.resourceVersion ?? null,
      });
    } else {
      await this.cancelExecRequest({
        cloudRunId: input.run.run_id,
        accessRequestId,
        requester: input.requester ?? null,
        reason: input.reason ?? null,
        resourceVersion: input.resourceVersion ?? null,
      });
    }
    return await this.describeResource(input.run.run_id, input.run, {
      kind: resource.kind,
      name: resource.metadata.name,
      eventLimit: 100,
    });
  }

  async completeRuntimeAccessResource(input: RuntimeResourceActionInput): Promise<RuntimeResourceDescribe> {
    const list = await this.resources(input.run.run_id, input.run);
    const resource = list.resources.find((item) => resourceIdentity(item) === resourceIdentity({
      kind: input.resourceKind,
      name: input.resourceName,
    }));
    if (!resource) {
      throw new HttpError(404, "runtime_resource_not_found", "Runtime resource not found", {
        kind: input.resourceKind,
        name: input.resourceName,
      });
    }
    assertRuntimeResourceVersionPrecondition(resource, input.resourceVersion, { required: true, operation: "complete" });
    if (resource.kind !== "PortForward") {
      throw new HttpError(409, "runtime_resource_action_unsupported", "Only PortForward resources support complete", {
        action: "complete",
        resource_kind: resource.kind,
        resource_name: resource.metadata.name,
        supported_kinds: ["PortForward"],
      });
    }
    assertRuntimeResourceSupportsAction(resource, "complete");
    const accessRequestId = stringField(resource.spec.access_request_id) ?? resource.metadata.uid ?? null;
    if (!accessRequestId) {
      throw new HttpError(409, "runtime_resource_action_unavailable", `${resource.kind} is missing access_request_id`);
    }
    await this.completePortForwardRequest({
      cloudRunId: input.run.run_id,
      accessRequestId,
      requester: input.requester ?? null,
      reason: input.reason ?? null,
      resourceVersion: input.resourceVersion ?? null,
    });
    return await this.describeResource(input.run.run_id, input.run, {
      kind: resource.kind,
      name: resource.metadata.name,
      eventLimit: 100,
    });
  }

  async events(cloudRunId: string, input: RuntimeEventRowsInput = {}): Promise<RuntimeEventList> {
    const eventInput = runtimeEventRowsInputWithContinue(input);
    const limit = boundedLimit(eventInput.limit, 50);
    const events = await this.eventRows(cloudRunId, {
      ...eventInput,
      limit: Math.min(limit + 1, 1000),
    });
    return runtimeEventListView(cloudRunId, eventInput, limit, events);
  }

  async eventRows(cloudRunId: string, input: RuntimeEventRowsInput = {}): Promise<RuntimeEventRecord[]> {
    const eventInput = runtimeEventRowsInputWithContinue(input);
    const limit = boundedLimit(eventInput.limit, 200);
    const scanLimit = hasRuntimeEventFilter(eventInput) ? 1000 : limit;
    const [coreRunIds, runtimeSnapshots, cloudEvents] = await Promise.all([
      this.coreRunIdsForCloudRun(cloudRunId),
      this.workerRuntimeSnapshots(cloudRunId),
      this.cloudControlPlaneEventRows(cloudRunId, eventInput),
    ]);
    let legacyEvents: RuntimeEventRecord[] = [];
    if (coreRunIds.length > 0) {
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
          and row_seq > ${eventInput.afterRowSeq ?? -1}
        order by run_id, schedule_idx, attempt, row_seq
        limit ${scanLimit}
      `.catch((error) => {
        if (isMissingRuntimeStore(error)) {
          return [];
        }
        throw error;
      });
      legacyEvents = rows.map((row) => ({
        source: "runtime.event_rows",
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
        resource_refs: [],
        payload: parseObject(row.payload_json),
        row: parseObject(row.row_json),
      }));
    }
    const snapshotEvents = runtimeEventRowsFromSnapshots(runtimeSnapshots)
      .filter((row) => row.row_seq > (eventInput.afterRowSeq ?? -1))
      .filter((row) => !legacyEvents.some((legacy) => runtimeEventKey(legacy) === runtimeEventKey(row)));
    return [
      ...cloudEvents,
      ...legacyEvents,
      ...snapshotEvents,
    ]
      .filter((event) => runtimeEventMatchesFilter(event, eventInput))
      .sort((a, b) => {
        const time = eventTime(a) - eventTime(b);
        if (time !== 0) {
          return time;
        }
        const core = a.core_run_id.localeCompare(b.core_run_id);
        if (core !== 0) {
          return core;
        }
        return a.schedule_idx - b.schedule_idx
          || a.attempt - b.attempt
          || a.row_seq - b.row_seq
          || a.seq - b.seq;
      })
      .slice(0, limit)
      .map(runtimeEventRecordWithResourceRefs);
  }

  private async artifactContent(
    cloudRunId: string,
    input: { trialId: string; role: string; coreRunId?: string | null; attempt?: number | undefined; objectRef?: string | null },
  ): Promise<RuntimeArtifactContent> {
    const coreRunIds = await this.coreRunIdsForCloudRun(cloudRunId);
    if (coreRunIds.length === 0) {
      throw new HttpError(404, "runtime_artifact_not_found", "Runtime artifact not found for this run");
    }
    const rows = await this.sql`
      select
        objects.run_id,
        objects.trial_id,
        objects.schedule_idx,
        objects.attempt,
        objects.role,
        objects.object_ref,
        objects.metadata_json,
        objects.recorded_at_ms,
        runtime_runs.artifact_root
      from ${this.table("attempt_objects")} objects
      join ${this.table("runs")} runtime_runs
        on runtime_runs.account_id = objects.account_id
       and runtime_runs.run_id = objects.run_id
      where objects.run_id = any(${coreRunIds})
        and objects.trial_id = ${input.trialId}
        and objects.role = ${input.role}
        and (${input.coreRunId ?? null}::text is null or objects.run_id = ${input.coreRunId ?? null})
        and (${input.attempt ?? null}::bigint is null or objects.attempt = ${input.attempt ?? null})
        and (${input.objectRef ?? null}::text is null or objects.object_ref = ${input.objectRef ?? null})
      order by objects.run_id, objects.attempt desc, objects.recorded_at_ms desc
      limit 2
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    if (rows.length === 0) {
      throw new HttpError(404, "runtime_artifact_not_found", "Runtime artifact not found for this run");
    }
    if (rows.length > 1 && !input.coreRunId && input.attempt === undefined && !input.objectRef) {
      throw new HttpError(409, "runtime_artifact_ambiguous", "Runtime artifact selection is ambiguous", {
        trial_id: input.trialId,
        role: input.role,
      });
    }
    const row = rows[0];
    if (!row) {
      throw new HttpError(404, "runtime_artifact_not_found", "Runtime artifact not found for this run");
    }
    const object = enrichAttemptObject({
      core_run_id: String(row.run_id),
      trial_id: String(row.trial_id),
      schedule_idx: Number(row.schedule_idx),
      attempt: Number(row.attempt),
      role: String(row.role),
      object_ref: String(row.object_ref),
      metadata: row.metadata_json === null ? null : parseJson(row.metadata_json),
      recorded_at_ms: Number(row.recorded_at_ms),
    });
    const artifactPath = artifactStoreBlobPath(String(row.artifact_root), object.object_ref);
    const file = await stat(artifactPath).catch((error) => {
      if (error && typeof error === "object" && "code" in error && error.code === "ENOENT") {
        throw new HttpError(409, "runtime_artifact_content_unavailable", "Runtime artifact content is unavailable to the Cloud API", {
          object_ref: object.object_ref,
        });
      }
      throw error;
    });
    if (!file.isFile()) {
      throw new HttpError(409, "runtime_artifact_content_unavailable", "Runtime artifact content is unavailable to the Cloud API", {
        object_ref: object.object_ref,
      });
    }
    const maxBytes = maxRuntimeArtifactBytes();
    if (file.size > maxBytes) {
      throw new HttpError(413, "runtime_artifact_too_large", "Runtime artifact exceeds the configured preview limit", {
        object_ref: object.object_ref,
        byte_size: file.size,
        max_runtime_artifact_bytes: maxBytes,
      });
    }
    return {
      object: {
        ...object,
        byte_size: file.size,
      },
      media_type: object.media_type,
      bytes: await readFile(artifactPath),
    };
  }

  private async cloudControlPlaneEventRows(
    cloudRunId: string,
    input?: Pick<RuntimeEventRowsInput, "afterRowSeq" | "eventTypes" | "sources">,
  ): Promise<RuntimeEventRecord[]> {
    if (!matchesEventSet("cloud.run_events", input?.sources)) {
      return [];
    }
    const eventTypes = runtimeEventFilterValues(input?.eventTypes);
    const rows = await this.sql`
      select
        event_id,
        run_id,
        attempt_id,
        seq,
        event_type,
        payload,
        created_at
      from cloud.run_events
      where run_id = ${cloudRunId}
        and seq > ${input?.afterRowSeq ?? -1}
        and (${eventTypes.length === 0} or event_type = any(${eventTypes}))
      order by seq
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map((row) => ({
      source: "cloud.run_events",
      core_run_id: "",
      trial_id: "",
      schedule_idx: -1,
      attempt: 0,
      row_seq: Number(row.seq),
      slot_commit_id: "",
      variant_id: "",
      task_id: "",
      repl_idx: 0,
      seq: Number(row.seq),
      event_type: String(row.event_type),
      ts: row.created_at === null ? null : String(row.created_at),
      resource_refs: [],
      payload: parseObject(row.payload),
      row: compactObject({
        source: "cloud.run_events",
        event_id: String(row.event_id),
        cloud_run_id: String(row.run_id),
        attempt_id: row.attempt_id === null ? undefined : String(row.attempt_id),
        seq: Number(row.seq),
        created_at: row.created_at === null ? undefined : String(row.created_at),
      }),
    }));
  }

  async listPortForwards(cloudRunId: string): Promise<RuntimeAccessRequestRecord[]> {
    await this.expireRuntimeAccessRequests();
    const rows = await this.sql`
      select *
      from cloud.runtime_access_requests
      where run_id = ${cloudRunId}
        and kind = 'port_forward'
      order by created_at desc
      limit 200
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(accessRequestFromRow);
  }

  async listExecRequests(cloudRunId: string): Promise<RuntimeAccessRequestRecord[]> {
    await this.expireRuntimeAccessRequests();
    const rows = await this.sql`
      select *
      from cloud.runtime_access_requests
      where run_id = ${cloudRunId}
        and kind = 'exec'
      order by created_at desc
      limit 200
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(accessRequestFromRow);
  }

  async createPortForwardRequest(input: CreatePortForwardRequestInput): Promise<RuntimeAccessRequestRecord> {
    const targetPort = validatePort(input.targetPort, "/target_port");
    const localPort = input.localPort == null ? null : validatePort(input.localPort, "/local_port");
    const ttlSeconds = validateRuntimeAccessTtlSeconds(input.ttlSeconds);
    const protocol = (input.protocol ?? "tcp").trim().toLowerCase();
    if (protocol !== "tcp") {
      throw new HttpError(400, "unsupported_protocol", "Only tcp port forwarding is supported");
    }
    const reason = normalizeOptionalString(input.reason);
    const requester = normalizeOptionalString(input.requester);
    const requestedTarget = await this.resolveRuntimeAccessTarget(input.run, {
      resourceKind: input.resourceKind,
      resourceName: input.resourceName,
      resourceVersion: input.resourceVersion,
    }, "port-forward");
    const targetRunnerInstanceId = requestedTarget.runnerInstanceId ?? null;
    const targetAttemptId = requestedTarget.attemptId ?? null;
    const targetWorkerId = requestedTarget.workerId ?? null;

    return await this.sql.begin(async (tx) => {
      const attempts = await tx`
        select attempt.*
        from cloud.run_attempts attempt
        join cloud.runner_instances instance
          on instance.runner_instance_id = attempt.runner_instance_id
        where attempt.run_id = ${input.run.run_id}
          and attempt.status = 'running'
          and attempt.lease_expires_at >= now()
          and (${targetRunnerInstanceId}::text is null or attempt.runner_instance_id::text = ${targetRunnerInstanceId})
          and (${targetAttemptId}::text is null or attempt.attempt_id = ${targetAttemptId})
          and (${targetWorkerId}::text is null or attempt.worker_id = ${targetWorkerId})
          and instance.status in ('online', 'cordoned', 'draining')
          and exists (
            select 1
            from jsonb_array_elements_text(coalesce(instance.capabilities->'resources', '[]'::jsonb)) resource(name)
            where resource.name = 'runtime_port_forward'
          )
        order by attempt.started_at desc
        for update
        limit 1
      `;
      const attempt = attempts[0] as RunAttemptRecord | undefined;
      if (!attempt || !attempt.runner_instance_id) {
        throw new HttpError(
          409,
          "runtime_port_forward_unavailable",
          "Port forwarding requires an active runner attempt whose runner advertises runtime_port_forward",
        );
      }
      const resourceKind = requestedTarget.kind;
      const resourceName = requestedTarget.name;
      const rows = await tx`
        insert into cloud.runtime_access_requests (
          run_id,
          kind,
          status,
          resource_kind,
          resource_name,
          target_uid,
          target_resource_version,
          protocol,
          target_port,
          local_port,
          runner_instance_id,
          attempt_id,
          worker_id,
          requester,
          reason,
          connection,
          expires_at
        )
        values (
          ${input.run.run_id},
          'port_forward',
          'requested',
          ${resourceKind},
          ${resourceName},
          ${requestedTarget.uid ?? null},
          ${requestedTarget.resourceVersion ?? null},
          ${protocol},
          ${targetPort},
          ${localPort},
          ${attempt.runner_instance_id},
          ${attempt.attempt_id},
          ${attempt.worker_id ?? targetWorkerId},
          ${requester},
          ${reason},
          ${this.sql.json({
            mode: "runner_reverse_tunnel",
            data_plane: "pending_runner_accept",
          })},
          case when ${ttlSeconds}::int is null then null else now() + (${ttlSeconds}::int * interval '1 second') end
        )
        returning *
      `;
      const row = rows[0];
      if (!row) {
        throw new HttpError(500, "port_forward_not_created", "Port-forward request was not created");
      }
      const request = accessRequestFromRow(row);
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.run.run_id,
        attemptId: attempt.attempt_id,
        eventType: "runtime.access.port_forward.requested",
        payload: accessRequestEventPayload(request, null, input.resourceVersion, requestedTarget),
      });
      return request;
    });
  }

  async cancelPortForwardRequest(input: {
    cloudRunId: string;
    accessRequestId: string;
    requester?: string | null;
    reason?: string | null;
    resourceVersion?: string | null;
  }): Promise<RuntimeAccessRequestRecord> {
    return await this.sql.begin(async (tx) => {
      const rows = await tx`
        with selected as (
          select access_request_id, status as previous_status
          from cloud.runtime_access_requests
          where run_id = ${input.cloudRunId}
            and access_request_id = ${input.accessRequestId}
            and kind = 'port_forward'
            and status in ('requested', 'accepted', 'active')
          for update
        ),
        updated as (
          update cloud.runtime_access_requests request
          set status = 'cancelled',
              requester = coalesce(${normalizeOptionalString(input.requester)}, request.requester),
              reason = coalesce(${normalizeOptionalString(input.reason)}, request.reason),
              updated_at = now()
          from selected
          where request.access_request_id = selected.access_request_id
          returning request.*, selected.previous_status
        )
        select * from updated
      `;
      const row = rows[0];
      if (!row) {
        throw new HttpError(404, "port_forward_not_found", "Active port-forward request not found");
      }
      const request = accessRequestFromRow(row);
      const previousStatus = row.previous_status === null ? null : String(row.previous_status);
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.cloudRunId,
        attemptId: request.attempt_id,
        eventType: "runtime.access.port_forward.cancelled",
        payload: accessRequestCancelEventPayload(request, previousStatus, input.resourceVersion),
      });
      return request;
    });
  }

  async completePortForwardRequest(input: {
    cloudRunId: string;
    accessRequestId: string;
    requester?: string | null;
    reason?: string | null;
    resourceVersion?: string | null;
  }): Promise<RuntimeAccessRequestRecord> {
    return await this.sql.begin(async (tx) => {
      const rows = await tx`
        with selected as (
          select access_request_id, status as previous_status
          from cloud.runtime_access_requests
          where run_id = ${input.cloudRunId}
            and access_request_id = ${input.accessRequestId}
            and kind = 'port_forward'
            and status = 'active'
          for update
        ),
        updated as (
          update cloud.runtime_access_requests request
          set status = 'completed',
              requester = coalesce(${normalizeOptionalString(input.requester)}, request.requester),
              reason = coalesce(${normalizeOptionalString(input.reason)}, request.reason),
              updated_at = now()
          from selected
          where request.access_request_id = selected.access_request_id
          returning request.*, selected.previous_status
        )
        select * from updated
      `;
      const row = rows[0];
      if (!row) {
        throw new HttpError(404, "port_forward_not_found", "Active port-forward request not found");
      }
      const request = accessRequestFromRow(row);
      const previousStatus = row.previous_status === null ? null : String(row.previous_status);
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.cloudRunId,
        attemptId: request.attempt_id,
        eventType: "runtime.access.port_forward.completed",
        payload: accessRequestCompleteEventPayload(request, previousStatus, input.resourceVersion),
      });
      return request;
    });
  }

  async createExecRequest(input: CreateExecRequestInput): Promise<RuntimeAccessRequestRecord> {
    const command = validateCommand(input.command, "/command");
    const ttlSeconds = validateRuntimeAccessTtlSeconds(input.ttlSeconds);
    const reason = normalizeOptionalString(input.reason);
    const requester = normalizeOptionalString(input.requester);
    const requestedTarget = await this.resolveRuntimeAccessTarget(input.run, {
      resourceKind: input.resourceKind,
      resourceName: input.resourceName,
      resourceVersion: input.resourceVersion,
    }, "exec");
    const targetRunnerInstanceId = requestedTarget.runnerInstanceId ?? null;
    const targetAttemptId = requestedTarget.attemptId ?? null;
    const targetWorkerId = requestedTarget.workerId ?? null;

    return await this.sql.begin(async (tx) => {
      const attempts = await tx`
        select attempt.*
        from cloud.run_attempts attempt
        join cloud.runner_instances instance
          on instance.runner_instance_id = attempt.runner_instance_id
        where attempt.run_id = ${input.run.run_id}
          and attempt.status = 'running'
          and attempt.lease_expires_at >= now()
          and (${targetRunnerInstanceId}::text is null or attempt.runner_instance_id::text = ${targetRunnerInstanceId})
          and (${targetAttemptId}::text is null or attempt.attempt_id = ${targetAttemptId})
          and (${targetWorkerId}::text is null or attempt.worker_id = ${targetWorkerId})
          and instance.status in ('online', 'cordoned', 'draining')
          and exists (
            select 1
            from jsonb_array_elements_text(coalesce(instance.capabilities->'resources', '[]'::jsonb)) resource(name)
            where resource.name = 'runtime_exec'
          )
        order by attempt.started_at desc
        for update
        limit 1
      `;
      const attempt = attempts[0] as RunAttemptRecord | undefined;
      if (!attempt || !attempt.runner_instance_id) {
        throw new HttpError(
          409,
          "runtime_exec_unavailable",
          "Runtime exec requires an active runner attempt whose runner advertises runtime_exec",
        );
      }
      const resourceKind = requestedTarget.kind;
      const resourceName = requestedTarget.name;
      const rows = await tx`
        insert into cloud.runtime_access_requests (
          run_id,
          kind,
          status,
          resource_kind,
          resource_name,
          target_uid,
          target_resource_version,
          protocol,
          target_port,
          local_port,
          command,
          runner_instance_id,
          attempt_id,
          worker_id,
          requester,
          reason,
          connection,
          expires_at
        )
        values (
          ${input.run.run_id},
          'exec',
          'requested',
          ${resourceKind},
          ${resourceName},
          ${requestedTarget.uid ?? null},
          ${requestedTarget.resourceVersion ?? null},
          'exec',
          ${null},
          ${null},
          ${this.sql.json(command)},
          ${attempt.runner_instance_id},
          ${attempt.attempt_id},
          ${attempt.worker_id ?? targetWorkerId},
          ${requester},
          ${reason},
          ${this.sql.json({
            mode: "worker_exec",
            data_plane: "pending_runner_accept",
          })},
          case when ${ttlSeconds}::int is null then null else now() + (${ttlSeconds}::int * interval '1 second') end
        )
        returning *
      `;
      const row = rows[0];
      if (!row) {
        throw new HttpError(500, "exec_not_created", "Exec request was not created");
      }
      const request = accessRequestFromRow(row);
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.run.run_id,
        attemptId: attempt.attempt_id,
        eventType: "runtime.access.exec.requested",
        payload: accessRequestEventPayload(request, null, input.resourceVersion, requestedTarget),
      });
      return request;
    });
  }

  private async resolveRuntimeAccessTarget(
    run: CloudRunRecord,
    input: { resourceKind?: string | null | undefined; resourceName?: string | null | undefined; resourceVersion?: string | null | undefined },
    operation: string,
  ): Promise<RuntimeAccessTarget> {
    const resourceKind = normalizeOptionalString(input.resourceKind);
    const resourceName = normalizeOptionalString(input.resourceName);
    if (!resourceKind || !resourceName) {
      throw new HttpError(
        400,
        "runtime_access_target_required",
        "Runtime access requests must target a concrete runtime resource",
        {
          required: ["resourceKind", "resourceName"],
          examples: ["Run/<run-id>", "RunnerInstance/<name>", "RunnerAttempt/<attempt-id>", "Trial/<trial-id>", "TrialContainer/<name>"],
        },
      );
    }
    const inventory = await this.resources(run.run_id, run);
    return runtimeAccessTargetFromInventory(inventory.resources, {
      resourceKind,
      resourceName,
      resourceVersion: input.resourceVersion,
    }, { required: true, operation });
  }

  async cancelExecRequest(input: {
    cloudRunId: string;
    accessRequestId: string;
    requester?: string | null;
    reason?: string | null;
    resourceVersion?: string | null;
  }): Promise<RuntimeAccessRequestRecord> {
    return await this.sql.begin(async (tx) => {
      const rows = await tx`
        with selected as (
          select access_request_id, status as previous_status
          from cloud.runtime_access_requests
          where run_id = ${input.cloudRunId}
            and access_request_id = ${input.accessRequestId}
            and kind = 'exec'
            and status in ('requested', 'accepted', 'active')
          for update
        ),
        updated as (
          update cloud.runtime_access_requests request
          set status = 'cancelled',
              requester = coalesce(${normalizeOptionalString(input.requester)}, request.requester),
              reason = coalesce(${normalizeOptionalString(input.reason)}, request.reason),
              updated_at = now()
          from selected
          where request.access_request_id = selected.access_request_id
          returning request.*, selected.previous_status
        )
        select * from updated
      `;
      const row = rows[0];
      if (!row) {
        throw new HttpError(404, "exec_not_found", "Active exec request not found");
      }
      const request = accessRequestFromRow(row);
      const previousStatus = row.previous_status === null ? null : String(row.previous_status);
      await appendAccessRequestEvent(tx, this.sql, {
        runId: input.cloudRunId,
        attemptId: request.attempt_id,
        eventType: "runtime.access.exec.cancelled",
        payload: accessRequestCancelEventPayload(request, previousStatus, input.resourceVersion),
      });
      return request;
    });
  }

  async portForwardRequestsForAttempt(input: {
    attemptId: string;
    runnerInstanceId: string;
  }): Promise<RuntimeAccessRequestRecord[]> {
    await this.expireRuntimeAccessRequests();
    const rows = await this.sql`
      select *
      from cloud.runtime_access_requests
      where attempt_id = ${input.attemptId}
        and runner_instance_id = ${input.runnerInstanceId}
        and kind = 'port_forward'
        and status in ('requested', 'accepted', 'active')
      order by created_at asc
      limit 100
    `;
    return rows.map(accessRequestFromRow);
  }

  async updatePortForwardRequest(input: {
    attemptId: string;
    runnerInstanceId: string;
    accessRequestId: string;
    status: "accepted" | "active" | "completed" | "failed" | "expired";
    connection?: JsonObject | null;
    errorMessage?: string | null;
  }): Promise<RuntimeAccessRequestRecord> {
    validatePortForwardStatusConnection(input.status, input.connection ?? null);
    return await this.sql.begin(async (tx) => {
      await expireRuntimeAccessRequestsInTransaction(tx, this.sql);
      const allowedPreviousStatuses = portForwardWorkerTransitionPreviousStatuses(input.status);
      const connection = input.connection ? this.sql.json(input.connection) : null;
      const rows = await tx`
        with candidate as (
          select access_request_id, status as previous_status
          from cloud.runtime_access_requests
          where access_request_id = ${input.accessRequestId}
            and attempt_id = ${input.attemptId}
            and runner_instance_id = ${input.runnerInstanceId}
            and kind = 'port_forward'
            and status = any(${allowedPreviousStatuses})
          for update
        ),
        updated as (
          update cloud.runtime_access_requests as request
          set status = ${input.status},
              connection = coalesce(${connection}::jsonb, request.connection),
              error_message = ${normalizeOptionalString(input.errorMessage)},
              updated_at = now()
          from candidate
          where request.access_request_id = candidate.access_request_id
          returning request.*, candidate.previous_status
        )
        select * from updated
      `;
      const row = rows[0];
      if (!row) {
        await throwRejectedRuntimeAccessWorkerUpdate(tx, {
          accessRequestId: input.accessRequestId,
          attemptId: input.attemptId,
          runnerInstanceId: input.runnerInstanceId,
          kind: "port_forward",
          targetStatus: input.status,
          allowedPreviousStatuses,
        });
        throw new HttpError(404, "port_forward_not_found", "Port-forward request not found for worker attempt");
      }
      const request = accessRequestFromRow(row);
      const previousStatus = row.previous_status === null ? null : String(row.previous_status);
      await appendAccessRequestEvent(tx, this.sql, {
        runId: request.run_id,
        attemptId: input.attemptId,
        eventType: `runtime.access.port_forward.${input.status}`,
        payload: accessRequestEventPayload(request, previousStatus),
      });
      return request;
    });
  }

  async execRequestsForAttempt(input: {
    attemptId: string;
    runnerInstanceId: string;
  }): Promise<RuntimeAccessRequestRecord[]> {
    await this.expireRuntimeAccessRequests();
    const rows = await this.sql`
      select *
      from cloud.runtime_access_requests
      where attempt_id = ${input.attemptId}
        and runner_instance_id = ${input.runnerInstanceId}
        and kind = 'exec'
        and status in ('requested', 'accepted', 'active')
      order by created_at asc
      limit 100
    `;
    return rows.map(accessRequestFromRow);
  }

  async updateExecRequest(input: {
    attemptId: string;
    runnerInstanceId: string;
    accessRequestId: string;
    status: "accepted" | "active" | "completed" | "failed" | "expired";
    connection?: JsonObject | null;
    errorMessage?: string | null;
  }): Promise<RuntimeAccessRequestRecord> {
    validateExecStatusConnection(input.status, input.connection ?? null);
    return await this.sql.begin(async (tx) => {
      await expireRuntimeAccessRequestsInTransaction(tx, this.sql);
      const allowedPreviousStatuses = execWorkerTransitionPreviousStatuses(input.status);
      const connection = input.connection ? this.sql.json(input.connection) : null;
      const rows = await tx`
        with candidate as (
          select access_request_id, status as previous_status
          from cloud.runtime_access_requests
          where access_request_id = ${input.accessRequestId}
            and attempt_id = ${input.attemptId}
            and runner_instance_id = ${input.runnerInstanceId}
            and kind = 'exec'
            and status = any(${allowedPreviousStatuses})
          for update
        ),
        updated as (
          update cloud.runtime_access_requests as request
          set status = ${input.status},
              connection = coalesce(${connection}::jsonb, request.connection),
              error_message = ${normalizeOptionalString(input.errorMessage)},
              updated_at = now()
          from candidate
          where request.access_request_id = candidate.access_request_id
          returning request.*, candidate.previous_status
        )
        select * from updated
      `;
      const row = rows[0];
      if (!row) {
        await throwRejectedRuntimeAccessWorkerUpdate(tx, {
          accessRequestId: input.accessRequestId,
          attemptId: input.attemptId,
          runnerInstanceId: input.runnerInstanceId,
          kind: "exec",
          targetStatus: input.status,
          allowedPreviousStatuses,
        });
        throw new HttpError(404, "exec_not_found", "Exec request not found for worker attempt");
      }
      const request = accessRequestFromRow(row);
      const previousStatus = row.previous_status === null ? null : String(row.previous_status);
      await appendAccessRequestEvent(tx, this.sql, {
        runId: request.run_id,
        attemptId: input.attemptId,
        eventType: `runtime.access.exec.${input.status}`,
        payload: accessRequestEventPayload(request, previousStatus),
      });
      return request;
    });
  }

  async expireRuntimeAccessRequests(): Promise<RuntimeAccessRequestRecord[]> {
    return await this.sql.begin(async (tx) => {
      return await expireRuntimeAccessRequestsInTransaction(tx, this.sql);
    }).catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
  }

  private async coreRunIdsForCloudRun(cloudRunId: string): Promise<string[]> {
    const [cloudEventIds, legacyDiscoveredIds] = await Promise.all([
      this.cloudEventCoreRunIds(cloudRunId),
      this.legacyDiscoveredCoreRunIds(cloudRunId),
    ]);
    return [...new Set([...cloudEventIds, ...legacyDiscoveredIds])].sort();
  }

  private async coreRunIdsForCloudRuns(cloudRunIds: string[]): Promise<Map<string, string[]>> {
    const [cloudEventIds, legacyDiscoveredIds] = await Promise.all([
      this.cloudEventCoreRunIdsForCloudRuns(cloudRunIds),
      this.legacyDiscoveredCoreRunIdsForCloudRuns(cloudRunIds),
    ]);
    const out = new Map<string, string[]>();
    for (const cloudRunId of cloudRunIds) {
      out.set(cloudRunId, [
        ...new Set([
          ...(cloudEventIds.get(cloudRunId) ?? []),
          ...(legacyDiscoveredIds.get(cloudRunId) ?? []),
        ]),
      ].sort());
    }
    return out;
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

  private async cloudEventCoreRunIdsForCloudRuns(cloudRunIds: string[]): Promise<Map<string, string[]>> {
    if (cloudRunIds.length === 0) {
      return new Map();
    }
    const rows = await this.sql`
      with
      cleanup_ids as (
        select events.run_id as cloud_run_id, core_ids.core_run_id
        from cloud.run_events events
        cross join lateral jsonb_array_elements_text(events.payload->'core_run_ids') as core_ids(core_run_id)
        where events.run_id = any(${cloudRunIds})
          and jsonb_typeof(events.payload->'core_run_ids') = 'array'
      ),
      snapshot_ids as (
        select run_id as cloud_run_id, payload->>'core_run_id' as core_run_id
        from cloud.run_events
        where run_id = any(${cloudRunIds})
          and event_type = 'worker.runtime.snapshot'
          and payload ? 'core_run_id'
          and nullif(payload->>'core_run_id', '') is not null
      )
      select cloud_run_id, core_run_id from cleanup_ids
      union
      select cloud_run_id, core_run_id from snapshot_ids
      order by cloud_run_id, core_run_id
    `;
    return groupCoreRunIds(rows);
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

  private async legacyDiscoveredCoreRunIdsForCloudRuns(cloudRunIds: string[]): Promise<Map<string, string[]>> {
    if (cloudRunIds.length === 0) {
      return new Map();
    }
    const rows = await this.sql`
      with run_roots as (
        select distinct run_id as cloud_run_id, payload->>'run_root_dir' as run_root_dir
        from cloud.run_events
        where run_id = any(${cloudRunIds})
          and payload ? 'run_root_dir'
          and nullif(payload->>'run_root_dir', '') is not null
      ),
      discovered_ids as (
        select distinct run_roots.cloud_run_id, runtime_runs.run_id as core_run_id
        from ${this.table("runs")} runtime_runs
        join run_roots on runtime_runs.run_dir like run_roots.run_root_dir || '/%'
      )
      select cloud_run_id, core_run_id from discovered_ids
      order by cloud_run_id, core_run_id
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return groupCoreRunIds(rows);
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

  private async snapshotTrialCountsForCloudRuns(
    cloudRunIds: string[],
  ): Promise<Array<{ core_run_id: string; trials_completed: number }>> {
    if (cloudRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select payload->>'core_run_id' as run_id,
             max(jsonb_array_length(payload->'trial_summaries'))::int as trials_completed
      from cloud.run_events
      where run_id = any(${cloudRunIds})
        and event_type = 'worker.runtime.snapshot'
        and payload ? 'core_run_id'
        and nullif(payload->>'core_run_id', '') is not null
        and jsonb_typeof(payload->'trial_summaries') = 'array'
      group by payload->>'core_run_id'
    `;
    return rows.flatMap((row) => {
      const coreRunId = String(row.run_id);
      const completed = Number(row.trials_completed);
      return coreRunId && Number.isSafeInteger(completed) && completed >= 0
        ? [{ core_run_id: coreRunId, trials_completed: completed }]
        : [];
    });
  }

  private async scheduleProgressTotalsForCoreRuns(
    cloudRunIds: string[],
    coreRunIds: string[],
  ): Promise<Array<{ core_run_id: string; trials_total: number }>> {
    const [storedRows, snapshotProgressRows, snapshotCountRows] = await Promise.all([
      this.sql`
        select run_id, max((value_json::jsonb->>'total_slots')::int)::int as trials_total
        from ${this.table("runtime_kv")}
        where run_id = any(${coreRunIds})
          and key = 'schedule_progress_v2'
          and value_json::jsonb->>'total_slots' ~ '^[0-9]+$'
        group by run_id
      `.catch((error) => {
        if (isMissingRuntimeStore(error)) {
          return [];
        }
        throw error;
      }),
      this.sql`
        select payload->>'core_run_id' as run_id,
               max((payload#>>'{runtime_values,schedule_progress_v2,total_slots}')::int)::int as trials_total
        from cloud.run_events
        where run_id = any(${cloudRunIds})
          and event_type = 'worker.runtime.snapshot'
          and payload ? 'core_run_id'
          and payload#>>'{runtime_values,schedule_progress_v2,total_slots}' ~ '^[0-9]+$'
        group by payload->>'core_run_id'
      `,
      this.sql`
        select payload->>'core_run_id' as run_id,
               max(jsonb_array_length(payload->'trial_summaries'))::int as trials_total
        from cloud.run_events
        where run_id = any(${cloudRunIds})
          and event_type = 'worker.runtime.snapshot'
          and payload ? 'core_run_id'
          and nullif(payload->>'core_run_id', '') is not null
          and jsonb_typeof(payload->'trial_summaries') = 'array'
        group by payload->>'core_run_id'
      `,
    ]);
    const totals = new Map<string, number>();
    for (const row of [...storedRows, ...snapshotProgressRows, ...snapshotCountRows]) {
      const coreRunId = String(row.run_id);
      const total = Number(row.trials_total);
      if (Number.isSafeInteger(total) && total >= 0) {
        totals.set(coreRunId, Math.max(totals.get(coreRunId) ?? 0, total));
      }
    }
    return [...totals.entries()].map(([core_run_id, trials_total]) => ({ core_run_id, trials_total }));
  }

  private async runAttempts(cloudRunId: string): Promise<RuntimeRunAttemptRecord[]> {
    const rows = await this.sql`
      select
        attempt.*,
        instance.runner_pool_id,
        instance.status as runner_instance_status,
        instance.instance_name as runner_instance_name,
        instance.provider_instance_id as runner_instance_provider_instance_id,
        instance.capabilities as runner_instance_capabilities,
        instance.metadata as runner_instance_metadata,
        instance.last_heartbeat_at as runner_instance_last_heartbeat_at,
        instance.created_at as runner_instance_created_at,
        instance.updated_at as runner_instance_updated_at
      from cloud.run_attempts attempt
      left join cloud.runner_instances instance
        on instance.runner_instance_id = attempt.runner_instance_id
      where attempt.run_id = ${cloudRunId}
      order by attempt.created_at asc
    `;
    return rows.map(runtimeRunAttemptFromRow);
  }

  private async provisionRequests(cloudRunId: string): Promise<RuntimeProvisionRequestRecord[]> {
    const rows = await this.sql`
      select *
      from cloud.runner_provision_requests
      where run_id = ${cloudRunId}
      order by created_at asc
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows as unknown as RuntimeProvisionRequestRecord[];
  }

  private async runnerPools(runnerPoolIds: string[]): Promise<RuntimeRunnerPoolRecord[]> {
    const ids = [...stringSet(runnerPoolIds)].sort();
    if (ids.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select *
      from cloud.runner_pools
      where runner_pool_id = any(${ids})
      order by name asc, runner_pool_id asc
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeRunnerPoolFromRow);
  }

  private async runtimeValueRecords(
    coreRunIds: string[],
    runtimeSnapshots: RuntimeSnapshotRecord[] = [],
  ): Promise<RuntimeValueRecord[]> {
    if (coreRunIds.length === 0) {
      return runtimeValueRecordsFromSnapshots(runtimeSnapshots);
    }
    const rows = await this.sql`
      select
        run_id,
        key,
        value_json,
        updated_at_ms
      from ${this.table("runtime_kv")}
      where run_id = any(${coreRunIds})
      order by run_id, key
      limit 1000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    const stored = rows.map(runtimeValueFromRow);
    const storedKeys = new Set(stored.map(runtimeValueRecordKey));
    const snapshotByKey = new Map<string, RuntimeValueRecord>();
    for (const value of runtimeValueRecordsFromSnapshots(runtimeSnapshots)) {
      const key = runtimeValueRecordKey(value);
      if (!storedKeys.has(key)) {
        snapshotByKey.set(key, value);
      }
    }
    return [...stored, ...snapshotByKey.values()].slice(0, 1000);
  }

  private async trialAttempts(coreRunIds: string[]): Promise<RuntimeTrialAttemptRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        phase,
        paused_from_phase,
        variant_id,
        task_id,
        repl_idx,
        state_json,
        updated_at_ms
      from ${this.table("trial_attempts")}
      where run_id = any(${coreRunIds})
      order by run_id, schedule_idx, attempt, trial_id
      limit 1000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map((row) => ({
      core_run_id: String(row.run_id),
      trial_id: String(row.trial_id),
      schedule_idx: Number(row.schedule_idx),
      attempt: Number(row.attempt),
      phase: String(row.phase),
      paused_from_phase: row.paused_from_phase === null ? null : String(row.paused_from_phase),
      variant_id: String(row.variant_id),
      task_id: String(row.task_id),
      repl_idx: Number(row.repl_idx),
      state: parseObject(row.state_json),
      updated_at_ms: Number(row.updated_at_ms),
    }));
  }

  private async scheduleSlots(coreRunIds: string[]): Promise<RuntimeSlotRecord[]> {
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
      order by run_id, schedule_idx
      limit 500
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

  private async coreRuns(coreRunIds: string[]): Promise<RuntimeCoreRunRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        experiment_id,
        project_root,
        run_dir,
        artifact_root,
        status,
        manifest_json,
        created_at_ms,
        updated_at_ms
      from ${this.table("runs")}
      where run_id = any(${coreRunIds})
      order by run_id
      limit 100
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeCoreRunFromRow);
  }

  private async runManifests(coreRunIds: string[]): Promise<RuntimeRunManifestRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        manifests.run_id,
        runs.experiment_id,
        runs.project_root,
        runs.run_dir,
        runs.artifact_root,
        runs.status,
        manifests.manifest_json,
        manifests.updated_at_ms
      from ${this.table("run_manifests")} as manifests
      left join ${this.table("runs")} as runs
        on runs.account_id = manifests.account_id
       and runs.run_id = manifests.run_id
      where manifests.run_id = any(${coreRunIds})
      order by manifests.run_id
      limit 100
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeRunManifestFromRow);
  }

  private async metricDefinitions(experimentIds: string[]): Promise<RuntimeMetricDefinitionRecord[]> {
    const ids = [...new Set(experimentIds.filter(Boolean))];
    if (ids.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        experiment_id,
        metric_id,
        semantic_key,
        label,
        value_type,
        unit,
        direction,
        source_type,
        source_pointer,
        required,
        primary_metric,
        definition_json,
        updated_at_ms
      from ${this.table("metric_definitions")}
      where experiment_id = any(${ids})
      order by experiment_id, primary_metric desc, metric_id
      limit 1000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeMetricDefinitionFromRow);
  }

  private async slotCommitRecords(coreRunIds: string[]): Promise<RuntimeSlotCommitRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        schedule_idx,
        attempt,
        record_type,
        slot_commit_id,
        record_json,
        recorded_at_ms
      from ${this.table("slot_commit_records")}
      where run_id = any(${coreRunIds})
      order by run_id, schedule_idx, attempt, record_type
      limit 5000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeSlotCommitFromRow);
  }

  private async pendingTrialCompletions(coreRunIds: string[]): Promise<RuntimePendingTrialCompletionRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        schedule_idx,
        trial_result_json,
        updated_at_ms
      from ${this.table("pending_trial_completions")}
      where run_id = any(${coreRunIds})
      order by run_id, schedule_idx
      limit 1000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimePendingTrialCompletionFromRow);
  }

  private async variantSnapshots(coreRunIds: string[]): Promise<RuntimeVariantSnapshotRecord[]> {
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
        baseline_id,
        task_id,
        repl_idx,
        binding_name,
        binding_value_json,
        binding_value_text,
        row_json
      from ${this.table("variant_snapshot_rows")}
      where run_id = any(${coreRunIds})
      order by run_id, schedule_idx, attempt, row_seq
      limit 5000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeVariantSnapshotFromRow);
  }

  private async evidenceRows(coreRunIds: string[]): Promise<RuntimeProvenanceRowRecord[]> {
    return this.runtimeProvenanceRows(coreRunIds, "evidence_rows");
  }

  private async chainStateRows(coreRunIds: string[]): Promise<RuntimeProvenanceRowRecord[]> {
    return this.runtimeProvenanceRows(coreRunIds, "chain_state_rows");
  }

  private async trialConclusionRows(coreRunIds: string[]): Promise<RuntimeProvenanceRowRecord[]> {
    return this.runtimeProvenanceRows(coreRunIds, "trial_conclusion_rows");
  }

  private async lineageVersions(coreRunIds: string[]): Promise<RuntimeLineageVersionRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        version_id,
        chain_key,
        step_index,
        trial_id,
        parent_version_id,
        pre_snapshot_ref,
        post_snapshot_ref,
        diff_incremental_ref,
        diff_cumulative_ref,
        patch_incremental_ref,
        patch_cumulative_ref,
        workspace_ref,
        checkpoint_labels_json
      from ${this.table("lineage_versions")}
      where run_id = any(${coreRunIds})
      order by run_id, chain_key, step_index, version_id
      limit 5000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeLineageVersionFromRow);
  }

  private async lineageHeads(coreRunIds: string[]): Promise<RuntimeLineageHeadRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        chain_key,
        latest_version_id,
        step_index,
        latest_workspace_ref
      from ${this.table("lineage_heads")}
      where run_id = any(${coreRunIds})
      order by run_id, chain_key
      limit 1000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeLineageHeadFromRow);
  }

  private async runtimeProvenanceRows(coreRunIds: string[], tableName: string): Promise<RuntimeProvenanceRowRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        schedule_idx,
        attempt,
        row_seq,
        slot_commit_id,
        row_json
      from ${this.table(tableName)}
      where run_id = any(${coreRunIds})
      order by run_id, schedule_idx, attempt, row_seq
      limit 5000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeProvenanceRowFromRow);
  }

  private async trialContainers(coreRunIds: string[]): Promise<RuntimeTrialContainerRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        role,
        container_id,
        status,
        image,
        workdir,
        updated_at_ms
      from ${this.table("trial_attempt_containers")}
      where run_id = any(${coreRunIds})
      order by run_id, schedule_idx, attempt, role, container_id
      limit 500
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map((row) => ({
      core_run_id: String(row.run_id),
      trial_id: String(row.trial_id),
      schedule_idx: Number(row.schedule_idx),
      attempt: Number(row.attempt),
      role: String(row.role),
      container_id: String(row.container_id),
      status: String(row.status),
      image: row.image === null ? null : String(row.image),
      workdir: row.workdir === null ? null : String(row.workdir),
      updated_at_ms: Number(row.updated_at_ms),
    }));
  }

  private async contractStages(
    coreRunIds: string[],
    runtimeSnapshots: RuntimeSnapshotRecord[] = [],
  ): Promise<RuntimeContractStageRecord[]> {
    if (coreRunIds.length === 0) {
      return runtimeContractStagesFromSnapshots(runtimeSnapshots);
    }
    const rows = await this.sql`
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
      limit 5000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    const stored = rows.map(runtimeContractStageFromRow);
    const snapshot = runtimeContractStagesFromSnapshots(runtimeSnapshots)
      .filter((row) => !stored.some((existing) => runtimeContractStageKey(existing) === runtimeContractStageKey(row)));
    return [...stored, ...snapshot].slice(0, 5000);
  }

  private async metricObservations(
    coreRunIds: string[],
    runtimeSnapshots: RuntimeSnapshotRecord[] = [],
  ): Promise<RuntimeMetricObservationRecord[]> {
    if (coreRunIds.length === 0) {
      return runtimeMetricObservationsFromTrialResults(runtimeTrialResultsFromSnapshots(runtimeSnapshots));
    }
    const rows = await this.sql`
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
      limit 5000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    const stored = rows.map(runtimeMetricObservationFromRow);
    const snapshot = runtimeMetricObservationsFromTrialResults(runtimeTrialResultsFromSnapshots(runtimeSnapshots))
      .filter((row) => !stored.some((existing) => runtimeMetricObservationKey(existing) === runtimeMetricObservationKey(row)));
    return [...stored, ...snapshot].slice(0, 5000);
  }

  private async performanceSamples(coreRunIds: string[]): Promise<RuntimePerformanceSampleRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        sample_id,
        trial_id,
        schedule_idx,
        attempt,
        sample_seq,
        sample_kind,
        stage,
        duration_ms,
        process_rss_kb,
        payload_json,
        recorded_at_ms
      from ${this.table("performance_samples")}
      where run_id = any(${coreRunIds})
      order by recorded_at_ms desc, sample_seq desc
      limit 5000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimePerformanceSampleFromRow);
  }

  private async runtimeOperations(coreRunIds: string[]): Promise<RuntimeOperationRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        op_kind,
        op_id,
        payload_json,
        updated_at_ms
      from ${this.table("runtime_ops")}
      where run_id = any(${coreRunIds})
      order by updated_at_ms desc, op_kind, op_id
      limit 1000
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(runtimeOperationFromRow);
  }

  private async attemptObjects(
    coreRunIds: string[],
    runtimeSnapshots: RuntimeSnapshotRecord[] = [],
  ): Promise<RuntimeAttemptObjectRecord[]> {
    if (coreRunIds.length === 0) {
      return runtimeAttemptObjectsFromSnapshots(runtimeSnapshots);
    }
    const [rows, contentRows] = await Promise.all([
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
      limit 5000
      `.catch((error) => {
        if (isMissingRuntimeStore(error)) {
          return [];
        }
        throw error;
      }),
      this.attemptObjectContentRows(coreRunIds, 5000),
    ]);
    const contentByKey = new Map(contentRows.map((row) => [runtimeAttemptObjectKey(row), row]));
    const stored = rows.map((row) => enrichAttemptObject({
      core_run_id: String(row.run_id),
      trial_id: String(row.trial_id),
      schedule_idx: Number(row.schedule_idx),
      attempt: Number(row.attempt),
      role: String(row.role),
      object_ref: String(row.object_ref),
      metadata: row.metadata_json === null ? null : parseJson(row.metadata_json),
      recorded_at_ms: Number(row.recorded_at_ms),
    })).map((row) => enrichAttemptObjectWithContent(row, contentByKey.get(runtimeAttemptObjectKey(row))));
    const snapshot = runtimeAttemptObjectsFromSnapshots(runtimeSnapshots)
      .filter((row) => !stored.some((existing) => runtimeAttemptObjectKey(existing) === runtimeAttemptObjectKey(row)));
    return [...stored, ...snapshot];
  }

  private async attemptObjectContentRows(coreRunIds: string[], limit: number): Promise<RuntimeAttemptObjectContentRecord[]> {
    if (coreRunIds.length === 0) {
      return [];
    }
    const rows = await this.sql`
      select
        run_id,
        trial_id,
        schedule_idx,
        attempt,
        role,
        object_ref,
        storage_path,
        media_type,
        byte_size,
        sha256,
        relative_path,
        metadata_json,
        recorded_at_ms
      from ${this.table("attempt_object_contents")}
      where run_id = any(${coreRunIds})
      order by run_id, schedule_idx, attempt, role
      limit ${limit}
    `.catch((error) => {
      if (isMissingRuntimeStore(error)) {
        return [];
      }
      throw error;
    });
    return rows.map(attemptObjectContentRecordFromRow);
  }

  private table(name: string): ReturnType<Sql["unsafe"]> {
    return this.sql.unsafe(`${quoteIdentifier(this.schema)}.${quoteIdentifier(name)}`);
  }
}

function runtimeResourceOperationReviewEvidence(resource: RuntimeResourceRecord): {
  resource_version: string;
  resource_generation: number | null;
  observed_generation: number | null;
} {
  return {
    resource_version: resource.metadata.resourceVersion ?? runtimeResourceVersion(resource),
    resource_generation: numberField(resource.metadata.generation),
    observed_generation: numberField(resource.status.observedGeneration),
  };
}

export function runtimeApiResourceList(
  cloudRunId: string,
  resources: RuntimeResourceRecord[],
  coreRunIds: string[] = [],
): RuntimeApiResourceList {
  const counts = resources.reduce((map, resource) => {
    map.set(resource.kind, (map.get(resource.kind) ?? 0) + 1);
    return map;
  }, new Map<string, number>());
  const generatedAt = new Date().toISOString();
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RuntimeApiResourceList",
    cloud_run_id: cloudRunId,
    generated_at: generatedAt,
    core_run_ids: [...coreRunIds],
    resources: RUNTIME_API_RESOURCE_DEFINITIONS.map((resource) => ({
      ...resource,
      cloud_run_id: cloudRunId,
      generated_at: generatedAt,
      core_run_ids: [...coreRunIds],
      shortNames: [...resource.shortNames],
      categories: [...resource.categories],
      verbs: [...resource.verbs],
      subresources: [...resource.subresources],
      actions: [...resource.actions],
      access: [...resource.access],
      exampleCommands: resource.exampleCommands.map((command) => ({ ...command })),
      printerColumns: resource.printerColumns.map((column) => ({ ...column })),
      fieldSelectors: [...resource.fieldSelectors],
      labelSelectors: [...resource.labelSelectors],
      count: counts.get(resource.kind) ?? 0,
    })),
  };
}

export function runtimeApiResourceForKind(
  cloudRunId: string,
  resources: RuntimeResourceRecord[],
  kind: string,
  coreRunIds: string[] = [],
): RuntimeApiResourceRecord {
  const requested = runtimeApiResourceAliasKey(kind);
  const resource = runtimeApiResourceList(cloudRunId, resources, coreRunIds).resources.find((candidate) =>
    runtimeApiResourceAliases(candidate).some((alias) => runtimeApiResourceAliasKey(alias) === requested)
  );
  if (!resource) {
    throw new HttpError(404, "runtime_api_resource_not_found", `Runtime API resource kind not found: ${kind}`, {
      kind,
      available_kinds: RUNTIME_API_RESOURCE_DEFINITIONS.map((candidate) => candidate.kind),
    });
  }
  return resource;
}

function runtimeApiResourceAliases(resource: RuntimeApiResourceRecord): string[] {
  return [resource.kind, resource.name, resource.singularName, ...resource.shortNames];
}

function runtimeApiResourceAliasKey(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]/g, "");
}

const RUNTIME_API_RESOURCE_DEFINITIONS: Omit<RuntimeApiResourceRecord, "cloud_run_id" | "generated_at" | "core_run_ids" | "count">[] = [
  runtimeApiResource("runs", "run", "Run", ["run"], ["core", "access-target"], ["list", "get", "watch", "describe"], ["port-forward", "exec"], [], ["port-forward", "exec"], "Cloud run lifecycle resource."),
  runtimeApiResource("coreruns", "corerun", "CoreRun", ["core"], ["core", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Core runtime run recorded by the runner store."),
  runtimeApiResource("packages", "package", "Package", ["pkg"], ["declared"], ["list", "get", "watch", "describe"], [], [], [], "Accepted experiment package backing the run."),
  runtimeApiResource("runnershapes", "runnershape", "RunnerShape", ["shape"], ["declared", "runner"], ["list", "get", "watch", "describe"], [], [], [], "Declared runner compute shape required by the run."),
  runtimeApiResource("capabilityrequirements", "capabilityrequirement", "CapabilityRequirement", ["capreq"], ["declared"], ["list", "get", "watch", "describe"], [], [], [], "Declared runtime capability requirement."),
  runtimeApiResource("imagepulls", "imagepull", "ImagePull", ["img"], ["declared"], ["list", "get", "watch", "describe"], [], [], [], "Digest-pinned image pull required by the run."),
  runtimeApiResource("secretbindings", "secretbinding", "SecretBinding", ["secret"], ["declared"], ["list", "get", "watch", "describe"], [], [], [], "Declared secret binding without secret values."),
  runtimeApiResource("networkperimeters", "networkperimeter", "NetworkPerimeter", ["net"], ["declared"], ["list", "get", "watch", "describe"], [], [], [], "Declared network perimeter and egress policy."),
  runtimeApiResource("sidecarrequirements", "sidecarrequirement", "SidecarRequirement", ["sidecar"], ["declared"], ["list", "get", "watch", "describe"], [], [], [], "Declared sidecar runtime dependency."),
  runtimeApiResource("acceleratorrequirements", "acceleratorrequirement", "AcceleratorRequirement", ["accel"], ["declared"], ["list", "get", "watch", "describe"], [], [], [], "Declared accelerator runtime dependency."),
  runtimeApiResource("runmanifests", "runmanifest", "RunManifest", ["manifest"], ["declared", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Resolved runtime run manifest recorded by the runner."),
  runtimeApiResource("metricdefinitions", "metricdefinition", "MetricDefinition", ["metricdef"], ["declared", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Declared metric definition used to score trial results."),
  runtimeApiResource("runnerpools", "runnerpool", "RunnerPool", ["pool"], ["runner"], ["list", "get", "watch", "describe"], [], [], [], "Runner pool able to satisfy run requirements."),
  runtimeApiResource("runnerinstances", "runnerinstance", "RunnerInstance", ["runner"], ["runner", "access-target"], ["list", "get", "watch", "describe"], ["logs", "actions/cordon", "actions/drain", "actions/uncordon", "port-forward", "exec"], ["cordon", "drain", "uncordon"], ["logs", "port-forward", "exec"], "Concrete runner VM instance for low-level runtime operations."),
  runtimeApiResource("runnerattempts", "runnerattempt", "RunnerAttempt", ["attempt"], ["runner", "access-target"], ["list", "get", "watch", "describe"], ["logs", "port-forward", "exec"], [], ["logs", "port-forward", "exec"], "Worker attempt currently executing the run on a runner instance."),
  runtimeApiResource("runnerprovisionrequests", "runnerprovisionrequest", "RunnerProvisionRequest", ["provision"], ["runner"], ["list", "get", "watch", "describe"], [], [], [], "Cloud provider runner VM provisioning request."),
  runtimeApiResource("trials", "trial", "Trial", ["trial"], ["trial", "access-target"], ["list", "get", "watch", "describe"], ["logs", "port-forward", "exec"], [], ["logs", "port-forward", "exec"], "Trial lifecycle, outcome, metrics, events, and active runner binding."),
  runtimeApiResource("scheduleslots", "scheduleslot", "ScheduleSlot", ["slot"], ["trial", "access-target"], ["list", "get", "watch", "describe"], ["logs", "port-forward", "exec"], [], ["logs", "port-forward", "exec"], "Core schedule slot assigned to a trial attempt."),
  runtimeApiResource("slotcommits", "slotcommit", "SlotCommit", ["commit"], ["scheduler", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Durable scheduler slot commit intent or commit journal row."),
  runtimeApiResource("pendingtrialcompletions", "pendingtrialcompletion", "PendingTrialCompletion", ["pendingcompletion"], ["scheduler", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Trial completion result waiting for deterministic scheduler commit reconciliation."),
  runtimeApiResource("variantsnapshots", "variantsnapshot", "VariantSnapshot", ["variantbind"], ["trial", "provenance", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Variant binding snapshot row durably committed for a trial slot."),
  runtimeApiResource("evidencerecords", "evidencerecord", "EvidenceRecord", ["evidence"], ["trial", "provenance", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Runtime evidence row committed with scheduler slot identity."),
  runtimeApiResource("chainstates", "chainstate", "ChainState", ["chain"], ["provenance", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Runtime chain state row committed with scheduler slot identity."),
  runtimeApiResource("trialconclusions", "trialconclusion", "TrialConclusion", ["conclusion"], ["trial", "provenance", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Trial conclusion row committed during scheduler reconciliation."),
  runtimeApiResource("lineageversions", "lineageversion", "LineageVersion", ["lineagever"], ["provenance", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Recorded lineage version produced by runtime chain state materialization."),
  runtimeApiResource("lineageheads", "lineagehead", "LineageHead", ["lineage"], ["provenance", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Current lineage head for a runtime chain key."),
  runtimeApiResource("trialcontainers", "trialcontainer", "TrialContainer", ["container"], ["trial", "access-target"], ["list", "get", "watch", "describe"], ["logs", "port-forward", "exec"], [], ["logs", "port-forward", "exec"], "Trial container identity with stdout/stderr log access."),
  runtimeApiResource("trialstages", "trialstage", "TrialStage", ["stage"], ["trial", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Per-trial contract stage lifecycle and detail."),
  runtimeApiResource("runtimevalues", "runtimevalue", "RuntimeValue", ["rv", "kv"], ["observability"], ["list", "get", "watch", "describe"], [], [], [], "Bounded runtime key/value state such as run control and schedule progress."),
  runtimeApiResource("metricobservations", "metricobservation", "MetricObservation", ["metricobs", "metric"], ["trial", "observability"], ["list", "get", "watch", "describe"], [], [], [], "Per-trial metric observation emitted by the runtime."),
  runtimeApiResource("performancesamples", "performancesample", "PerformanceSample", ["perf"], ["observability"], ["list", "get", "watch", "describe"], [], [], [], "Runtime performance sample with stage duration and process RSS evidence."),
  runtimeApiResource("runtimeoperations", "runtimeoperation", "RuntimeOperation", ["op"], ["observability"], ["list", "get", "watch", "describe"], [], [], [], "Runtime operation manifest such as replay or fork."),
  runtimeApiResource("trialartifacts", "trialartifact", "TrialArtifact", ["artifact"], ["trial", "observability"], ["list", "get", "watch", "describe"], ["content"], [], [], "Attempt artifact evidence emitted by a trial and owned by the Trial resource."),
  runtimeApiResource("runtimesnapshots", "runtimesnapshot", "RuntimeSnapshot", ["snapshot"], ["observability"], ["list", "get", "watch", "describe"], [], [], [], "Worker runtime snapshot and evidence summary."),
  runtimeApiResource("events", "event", "Event", ["ev"], ["observability"], ["list", "get", "watch", "describe"], [], [], [], "Runtime lifecycle and audit event projected as a resource."),
  runtimeApiResource("portforwards", "portforward", "PortForward", ["pf"], ["access"], ["list", "get", "watch", "describe", "create", "delete"], ["actions/cancel", "actions/complete"], ["cancel", "complete"], [], "Audited port-forward request and tunnel lifecycle."),
  runtimeApiResource("execs", "exec", "Exec", ["exec"], ["access"], ["list", "get", "watch", "describe", "create", "delete"], ["logs", "actions/cancel"], ["cancel"], ["logs"], "Audited runtime exec request, command output, and lifecycle."),
];

function runtimeApiResource(
  name: string,
  singularName: string,
  kind: string,
  shortNames: string[],
  categories: string[],
  verbs: string[],
  subresources: string[],
  actions: string[],
  access: string[],
  description: string,
): Omit<RuntimeApiResourceRecord, "cloud_run_id" | "generated_at" | "core_run_ids" | "count"> {
  const resourceSubresources = Array.from(new Set(["status", "events", "metrics", ...subresources]));
  const pathTemplates = runtimeApiResourcePathTemplates(kind, verbs, resourceSubresources);
  const exampleCommands = runtimeApiResourceExampleCommands(kind, verbs, resourceSubresources, actions, access);
  const printerColumns = runtimeApiResourcePrinterColumns(kind, categories, actions, access);
  return {
    group: "bucephalus.dev",
    version: "v1alpha1",
    name,
    singularName,
    namespaced: false,
    scope: "run",
    kind,
    shortNames,
    categories,
    verbs,
    subresources: resourceSubresources,
    actions,
    access,
    supports: {
      list: verbs.includes("list"),
      get: verbs.includes("get"),
      watch: verbs.includes("watch"),
      describe: verbs.includes("describe"),
      create: verbs.includes("create"),
      delete: verbs.includes("delete"),
      actions: actions.length > 0,
      access: access.length > 0,
      labelSelector: true,
      fieldSelector: true,
    },
    pathTemplates,
    exampleCommands,
    printerColumns,
    fieldSelectors: runtimeApiResourceFieldSelectors(kind),
    labelSelectors: runtimeApiResourceLabelSelectors(kind),
    labelSelector: true,
    description,
  };
}

function runtimeApiResourceLabelSelectors(kind: string): string[] {
  return [...new Set([
    ...RUNTIME_RESOURCE_BASE_LABEL_SELECTORS,
    ...(RUNTIME_RESOURCE_LABEL_SELECTOR_EXTRAS[kind] ?? []),
  ])];
}

function runtimeApiResourceFieldSelectors(kind: string): string[] {
  return [...new Set([
    ...RUNTIME_RESOURCE_FIELD_SELECTORS,
    ...(RUNTIME_RESOURCE_FIELD_SELECTOR_EXTRAS[kind] ?? []),
  ])];
}

function runtimeApiResourceExampleCommands(
  kind: string,
  verbs: string[],
  subresources: string[],
  actions: string[],
  access: string[],
): RuntimeApiResourceExampleCommand[] {
  const target = `${kind}/{name}`;
  const commands: RuntimeApiResourceExampleCommand[] = [
    { purpose: "list", command: `buc runs get {run_id} ${kind}` },
    { purpose: "list/name", command: `buc runs get {run_id} ${kind} --output name` },
    { purpose: "watch", command: `buc runs watch {run_id} --kind ${kind}` },
    { purpose: "watch/not-ready", command: `buc runs watch {run_id} --kind ${kind} --field-selector status.conditions.Ready!=True` },
    { purpose: "top", command: `buc runs top {run_id} --kind ${kind}` },
    { purpose: "get", command: `buc runs get {run_id} ${target}` },
    { purpose: "describe", command: `buc runs describe {run_id} ${target}` },
    { purpose: "status", command: `buc runs status {run_id} ${target}` },
    { purpose: "wait", command: `buc runs wait {run_id} ${target} --for condition=Ready` },
    { purpose: "metrics", command: `buc runs metrics {run_id} ${target}` },
    { purpose: "events", command: `buc runs events {run_id} ${target}` },
    { purpose: "audit", command: `buc runs audit {run_id} ${target}` },
  ];
  if (subresources.includes("logs") || access.includes("logs")) {
    commands.push(
      { purpose: "logs/stdout", command: `buc runs logs {run_id} ${target} --stream stdout --metadata-out FILE.metadata.json` },
      { purpose: "logs/stderr", command: `buc runs logs {run_id} ${target} --stream stderr --metadata-out FILE.metadata.json` },
    );
  }
  if (subresources.includes("content")) {
    commands.push({ purpose: "content", command: `buc runs content {run_id} ${target} --out FILE --metadata-out FILE.metadata.json` });
  }
  if (access.includes("port-forward")) {
    commands.push({ purpose: "port-forward", command: `buc runs port-forward {run_id} ${target} --target-port PORT --local-port PORT --attach --resource-version <metadata.resourceVersion>` });
  }
  if (kind === "PortForward") {
    commands.push({ purpose: "watch/client-unreachable", command: "buc runs watch {run_id} --kind PortForward --field-selector status.conditions.ClientReachable!=True" });
  }
  if (access.includes("exec")) {
    commands.push({ purpose: "exec", command: `buc runs exec {run_id} ${target} --resource-version <metadata.resourceVersion> -- COMMAND [ARG...]` });
  }
  if (verbs.includes("delete")) {
    commands.push(
      { purpose: "delete", command: `buc runs delete {run_id} ${target} --resource-version <metadata.resourceVersion>` },
      { purpose: "wait/delete", command: `buc runs wait {run_id} ${target} --for delete` },
    );
  }
  for (const action of actions) {
    commands.push({ purpose: action, command: `buc runs ${action} {run_id} ${target} --resource-version <metadata.resourceVersion>` });
  }
  return commands;
}

function runtimeResourceOperationsForDescribe(
  cloudRunId: string,
  run: CloudRunRecord,
  resources: RuntimeResourceRecord[],
  resource: RuntimeResourceRecord,
): RuntimeResourceOperation[] {
  const discovery = runtimeApiResourceForKind(cloudRunId, resources, resource.kind);
  const name = resource.metadata.name;
  const seen = new Set<string>();
  return discovery.exampleCommands
    .map((example) => {
      const support = runtimeResourceOperationSupport(run, resources, resource, example.purpose);
      return {
        purpose: example.purpose,
        command: runtimeResourceOperationCommand(example.command, cloudRunId, name, example.purpose, resource),
        supported: support.supported,
        reason: support.reason,
        message: support.message,
        verb: runtimeResourceOperationVerb(example.purpose),
        subresource: runtimeResourceOperationSubresource(example.purpose),
        action: runtimeResourceOperationAction(example.purpose),
        requires_running_run: runtimeResourceOperationRequiresRunningRun(example.purpose),
      };
    })
    .filter((operation) => {
      const key = `${operation.purpose}:${operation.command}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
}

function runtimeResourceOperationSupport(
  run: CloudRunRecord,
  resources: RuntimeResourceRecord[],
  resource: RuntimeResourceRecord,
  purpose: string,
): { supported: boolean; reason: string | null; message: string | null } {
  if (purpose === "logs/stdout" || purpose === "logs/stderr") {
    return runtimeResourceSupportsLogs(resource)
      ? runtimeResourceOperationSupported()
      : runtimeResourceOperationUnsupported(
        "logs_unavailable",
        `Logs are not available for ${resource.kind}/${resource.metadata.name}`,
      );
  }
  if (purpose === "content") {
    return booleanField(resource.status.content_available) === true
      ? runtimeResourceOperationSupported()
      : runtimeResourceOperationUnsupported(
        "content_unavailable",
        `Content is not available for ${resource.kind}/${resource.metadata.name}`,
      );
  }
  if (purpose === "port-forward") {
    return runtimeResourceAccessOperationSupport(run, resources, resource, "port_forward", "runtime_port_forward");
  }
  if (purpose === "exec") {
    return runtimeResourceAccessOperationSupport(run, resources, resource, "exec", "runtime_exec");
  }
  const action = runtimeResourceOperationAction(purpose);
  if (action) {
    const actions = runtimeResourceAdvertisedActions(resource);
    return actions.includes(action)
      ? runtimeResourceOperationSupported()
      : runtimeResourceOperationUnsupported(
        `${action}_unavailable`,
        `${resource.kind}/${resource.metadata.name} does not currently advertise ${action}`,
      );
  }
  if (runtimeResourceOperationReadOnlyPurpose(purpose)) {
    return runtimeResourceOperationSupported();
  }
  return runtimeResourceOperationUnsupported(
    "operation_support_unimplemented",
    `${resource.kind}/${resource.metadata.name} advertises runtime operation ${purpose}, but the server has no explicit support rule for it`,
  );
}

function runtimeResourceAccessOperationSupport(
  run: CloudRunRecord,
  resources: RuntimeResourceRecord[],
  resource: RuntimeResourceRecord,
  accessField: "port_forward" | "exec",
  capability: "runtime_port_forward" | "runtime_exec",
): { supported: boolean; reason: string | null; message: string | null } {
  const purpose = accessField === "port_forward" ? "port-forward" : "exec";
  if (!isRuntimeAccessTargetKind(resource.kind)) {
    return runtimeResourceOperationUnsupported(
      "access_target_unsupported",
      `${resource.kind}/${resource.metadata.name} is not a runtime access target`,
    );
  }
  if (run.status !== "running") {
    return runtimeResourceOperationUnsupported(
      "run_not_running",
      `${purpose} requires a running Cloud run`,
    );
  }
  const targetSupport = runtimeResourceAccessTargetOperationSupport(resources, resource);
  if (!targetSupport.supported) {
    return targetSupport;
  }
  if (targetSupport.target.runnerInstanceId === undefined && targetSupport.target.attemptId === undefined) {
    return runtimeResourceOperationUnsupported(
      "runtime_access_target_unreachable",
      `${resource.kind}/${resource.metadata.name} is not currently reachable for runtime access: target is not bound to an active runner attempt`,
    );
  }
  const access: RuntimeResourceAccessStatus = isRecord(resource.status.access)
    ? resource.status.access as RuntimeResourceAccessStatus
    : {};
  if (booleanField(access[accessField]) !== true) {
    return runtimeResourceOperationUnsupported(
      `${capability}_unavailable`,
      `${purpose} requires an active runner attempt whose runner advertises ${capability}`,
    );
  }
  return runtimeResourceOperationSupported();
}

function runtimeResourceAccessTargetOperationSupport(
  resources: RuntimeResourceRecord[],
  resource: RuntimeResourceRecord,
): ({ supported: true; reason: null; message: null; target: RuntimeAccessTarget } | { supported: false; reason: string; message: string; target?: undefined }) {
  try {
    return {
      ...runtimeResourceOperationSupported(),
      target: runtimeAccessTargetFromInventory(resources, {
        resourceKind: resource.kind,
        resourceName: resource.metadata.name,
      }),
    };
  } catch (error) {
    if (error instanceof HttpError) {
      return runtimeResourceOperationUnsupported(error.code, error.message);
    }
    throw error;
  }
}

function runtimeResourceOperationSupported(): { supported: true; reason: null; message: null } {
  return { supported: true, reason: null, message: null };
}

function runtimeResourceOperationUnsupported(
  reason: string,
  message: string,
): { supported: false; reason: string; message: string } {
  return { supported: false, reason, message };
}

function runtimeResourceOperationCommand(
  template: string,
  runId: string,
  name: string,
  purpose: string,
  resource: RuntimeResourceRecord,
): string {
  const command = template.replaceAll("{run_id}", runId).replaceAll("{name}", name);
  return runtimeResourceCommandWithVersionPrecondition(command, purpose, resource.metadata.resourceVersion ?? runtimeResourceVersion(resource));
}

function runtimeResourceCommandWithVersionPrecondition(command: string, purpose: string, resourceVersion: string | null | undefined): string {
  const version = normalizeOptionalString(resourceVersion);
  const trimmedCommand = command.trim();
  if (!version || !trimmedCommand || !runtimeResourceOperationAcceptsVersionPrecondition(purpose)) {
    return command;
  }
  const versionPlaceholder = "--resource-version <metadata.resourceVersion>";
  if (/\s--resource-version(?:\s|$)/.test(trimmedCommand)) {
    return trimmedCommand.includes(versionPlaceholder)
      ? trimmedCommand.replace(versionPlaceholder, `--resource-version ${shellQuoteCliToken(version)}`)
      : command;
  }
  const versionArg = `--resource-version ${shellQuoteCliToken(version)}`;
  const commandSeparator = trimmedCommand.indexOf(" -- ");
  if (commandSeparator >= 0) {
    return `${trimmedCommand.slice(0, commandSeparator)} ${versionArg}${trimmedCommand.slice(commandSeparator)}`;
  }
  return `${trimmedCommand} ${versionArg}`;
}

function runtimeResourceOperationAcceptsVersionPrecondition(purpose: string): boolean {
  return purpose === "port-forward" || purpose === "exec" || purpose === "delete" || runtimeResourceOperationAction(purpose) !== null;
}

function runtimeResourceOperationReadOnlyPurpose(purpose: string): boolean {
  return purpose === "list"
    || purpose === "list/name"
    || purpose === "watch"
    || purpose.startsWith("watch/")
    || purpose === "get"
    || purpose === "describe"
    || purpose === "status"
    || purpose === "wait"
    || purpose === "wait/delete"
    || purpose === "top"
    || purpose === "metrics"
    || purpose === "events"
    || purpose === "audit";
}

function shellQuoteCliToken(value: string): string {
  return /^[A-Za-z0-9_./:=@%+-]+$/.test(value)
    ? value
    : `'${value.replaceAll("'", "'\"'\"'")}'`;
}

function runtimeResourceOperationVerb(purpose: string): string | null {
  if (purpose === "list" || purpose === "list/name") return "list";
  if (purpose === "watch" || purpose === "get" || purpose === "describe") return purpose;
  if (purpose === "delete") return "delete";
  if (purpose === "manifest") return "get";
  if (purpose === "status" || purpose === "wait" || purpose === "wait/delete" || purpose === "top" || purpose === "metrics" || purpose === "content") return "get";
  if (purpose === "logs/stdout" || purpose === "logs/stderr") return "get";
  if (purpose === "events" || purpose === "audit") return "watch";
  if (purpose.startsWith("watch/")) return "watch";
  return null;
}

function runtimeResourceOperationSubresource(purpose: string): string | null {
  if (purpose === "wait" || purpose === "wait/delete") return "status";
  if (purpose === "top") return "metrics";
  if (purpose === "audit") return "events";
  if (purpose === "status" || purpose === "metrics" || purpose === "events" || purpose === "content") return purpose;
  if (purpose === "logs/stdout" || purpose === "logs/stderr") return "logs";
  if (purpose === "port-forward" || purpose === "exec") return purpose;
  const action = runtimeResourceOperationAction(purpose);
  return action ? `actions/${action}` : null;
}

function runtimeResourceOperationAction(purpose: string): string | null {
  if (purpose === "delete") return "cancel";
  return purpose === "cordon" || purpose === "drain" || purpose === "uncordon" || purpose === "cancel" || purpose === "complete"
    ? purpose
    : null;
}

function runtimeResourceOperationRequiresRunningRun(purpose: string): boolean {
  return purpose === "port-forward" || purpose === "exec";
}

function runtimeResourceOperationMatches(purpose: string, requestedOperation: string): boolean {
  const purposeKey = runtimeResourceOperationKey(purpose);
  return purposeKey === requestedOperation
    || (requestedOperation === "logs" && purposeKey.startsWith("logs"))
    || (requestedOperation === "cancel" && purposeKey === "delete")
    || (requestedOperation === "delete" && purposeKey === "cancel");
}

function runtimeResourceOperationKey(value: string): string {
  return value.toLowerCase().replaceAll(/[^a-z0-9]/g, "");
}

function runtimeApiResourcePrinterColumns(
  kind: string,
  categories: string[],
  actions: string[],
  access: string[],
): RuntimeApiResourcePrinterColumn[] {
  const columns: RuntimeApiResourcePrinterColumn[] = [
    { name: "Name", type: "string", jsonPath: ".metadata.name", description: "Runtime resource name.", priority: 0 },
    { name: "Phase", type: "string", jsonPath: ".status.phase", description: "Current lifecycle phase reported for the resource.", priority: 0 },
    { name: "Ready", type: "string", jsonPath: '.status.conditions[?(@.type=="Ready")].status', description: "Ready condition status when the resource reports Kubernetes-style conditions.", priority: 0 },
    { name: "Observed", type: "string", jsonPath: '.status.conditions[?(@.type=="Observed")].status', description: "Whether status.observedGeneration has caught up to metadata.generation.", priority: 0 },
    { name: "Reason", type: "string", jsonPath: ".status.reason", description: "Machine-readable reason for the current phase or health state.", priority: 1 },
    { name: "Source", type: "string", jsonPath: ".audit.source", description: "Runtime subsystem that projected the resource.", priority: 1 },
    { name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp", description: "Resource creation timestamp when available.", priority: 1 },
  ];
  if (access.length > 0 || categories.includes("access-target")) {
    columns.push(
      { name: "Reachable", type: "boolean", jsonPath: ".status.access.reachable", description: "Whether the resource is currently reachable for low-level runtime access.", priority: 0 },
      { name: "PortForward", type: "boolean", jsonPath: ".status.access.port_forward", description: "Whether the resource can be targeted by a port-forward request.", priority: 0 },
      { name: "Exec", type: "boolean", jsonPath: ".status.access.exec", description: "Whether the resource can be targeted by a runtime exec request.", priority: 0 },
      { name: "Runner", type: "string", jsonPath: ".status.access.runner_instance_id", description: "Runner instance currently backing the access target.", priority: 1 },
      { name: "RunnerStatus", type: "string", jsonPath: ".status.access.runner_instance_status", description: "Status of the runner instance currently backing the access target.", priority: 1 },
    );
  }
  if (kind === "RunnerInstance") {
    columns.push(
      { name: "Provider", type: "string", jsonPath: ".status.provider", description: "Cloud provider backing the runner VM when known.", priority: 0 },
      { name: "VM", type: "string", jsonPath: ".status.instance_name", description: "Cloud provider VM instance name when known.", priority: 0 },
      { name: "Zone", type: "string", jsonPath: ".status.zone", description: "Cloud provider zone for the runner VM when known.", priority: 1 },
      { name: "ProviderID", type: "string", jsonPath: ".status.provider_instance_id", description: "Cloud provider instance identity for low-level VM access.", priority: 1 },
    );
  }
  if (actions.length > 0) {
    columns.push({ name: "Actions", type: "string", jsonPath: ".status.actions", description: "Advertised lifecycle actions currently available on this resource.", priority: 1 });
  }
  if (kind === "Event") {
    columns.push(
      { name: "Involved", type: "string", jsonPath: ".status.involved", description: "Primary runtime resource involved in the event.", priority: 0 },
      { name: "Type", type: "string", jsonPath: ".spec.event_type", description: "Runtime event type.", priority: 0 },
      { name: "Seq", type: "integer", jsonPath: ".spec.row_seq", description: "Monotonic runtime event sequence number.", priority: 0 },
      { name: "InvolvedKind", type: "string", jsonPath: ".status.involved_kind", description: "Kind of the primary involved runtime resource.", priority: 1 },
      { name: "Message", type: "string", jsonPath: ".status.message", description: "Human-readable event summary.", priority: 1 },
    );
  }
  if (kind === "Trial") {
    columns.push(
      { name: "Task", type: "string", jsonPath: ".spec.task_id", description: "Task executed by the trial.", priority: 0 },
      { name: "Outcome", type: "string", jsonPath: ".status.outcome", description: "Trial outcome when available.", priority: 0 },
      { name: "Metric", type: "string", jsonPath: ".status.primary_metric_name", description: "Primary metric selected for the trial.", priority: 1 },
      { name: "Value", type: "number", jsonPath: ".status.primary_metric_value", description: "Primary metric value when numeric.", priority: 1 },
      { name: "Events", type: "integer", jsonPath: ".status.events_total", description: "Runtime event count reported for the trial.", priority: 1 },
      { name: "Containers", type: "integer", jsonPath: ".status.containers.total", description: "Visible runtime containers associated with the trial.", priority: 1 },
    );
  }
  if (kind === "CoreRun") {
    columns.push(
      { name: "CoreRun", type: "string", jsonPath: ".spec.core_run_id", description: "Core runtime run id.", priority: 0 },
      { name: "Experiment", type: "string", jsonPath: ".status.experiment_id", description: "Experiment id recorded by the core runtime store.", priority: 0 },
      { name: "Status", type: "string", jsonPath: ".status.runtime_status", description: "Core runtime run status.", priority: 0 },
      { name: "RunDir", type: "string", jsonPath: ".status.run_dir", description: "Core runtime run directory.", priority: 1 },
      { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: "When the core runtime run was last updated.", priority: 1 },
    );
  }
  if (kind === "RunManifest") {
    columns.push(
      { name: "CoreRun", type: "string", jsonPath: ".spec.core_run_id", description: "Core runtime run id.", priority: 0 },
      { name: "Experiment", type: "string", jsonPath: ".status.experiment_id", description: "Resolved experiment id when known.", priority: 0 },
      { name: "Workload", type: "string", jsonPath: ".status.workload_type", description: "Run workload type declared by the manifest.", priority: 0 },
      { name: "Baseline", type: "string", jsonPath: ".status.baseline_id", description: "Baseline id declared by the manifest.", priority: 1 },
      { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: "When this manifest was last persisted.", priority: 1 },
    );
  }
  if (kind === "MetricDefinition") {
    columns.push(
      { name: "Metric", type: "string", jsonPath: ".spec.metric_id", description: "Metric id.", priority: 0 },
      { name: "Primary", type: "boolean", jsonPath: ".status.primary_metric", description: "Whether this is the primary metric.", priority: 0 },
      { name: "Required", type: "boolean", jsonPath: ".status.required", description: "Whether this metric is required.", priority: 0 },
      { name: "Direction", type: "string", jsonPath: ".status.direction", description: "Optimization direction.", priority: 1 },
      { name: "Source", type: "string", jsonPath: ".spec.source_type", description: "Metric source type.", priority: 1 },
    );
  }
  if (kind === "SlotCommit") {
    columns.push(
      { name: "Schedule", type: "integer", jsonPath: ".spec.schedule_idx", description: "Schedule index committed by this journal row.", priority: 0 },
      { name: "Attempt", type: "integer", jsonPath: ".spec.attempt", description: "Commit attempt number.", priority: 0 },
      { name: "Record", type: "string", jsonPath: ".spec.record_type", description: "Commit journal record type.", priority: 0 },
      { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: "Trial associated with the committed slot.", priority: 0 },
      { name: "SlotStatus", type: "string", jsonPath: ".status.slot_status", description: "Slot status recorded by the scheduler.", priority: 1 },
    );
  }
  if (kind === "PendingTrialCompletion") {
    columns.push(
      { name: "Schedule", type: "integer", jsonPath: ".spec.schedule_idx", description: "Schedule index waiting for commit reconciliation.", priority: 0 },
      { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: "Trial waiting to be committed.", priority: 0 },
      { name: "SlotStatus", type: "string", jsonPath: ".status.slot_status", description: "Pending trial result slot status.", priority: 0 },
      { name: "Rows", type: "integer", jsonPath: ".status.deferred_rows.total", description: "Deferred rows waiting to be committed.", priority: 1 },
      { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: "When the pending completion was last persisted.", priority: 1 },
    );
  }
  if (kind === "VariantSnapshot") {
    columns.push(
      { name: "Schedule", type: "integer", jsonPath: ".spec.schedule_idx", description: "Schedule index for this variant snapshot row.", priority: 0 },
      { name: "Attempt", type: "integer", jsonPath: ".spec.attempt", description: "Scheduler commit attempt.", priority: 0 },
      { name: "Binding", type: "string", jsonPath: ".spec.binding_name", description: "Variant binding captured in this row.", priority: 0 },
      { name: "Variant", type: "string", jsonPath: ".spec.variant_id", description: "Variant id for the snapshot row.", priority: 1 },
      { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: "Trial associated with the snapshot.", priority: 1 },
    );
  }
  if (kind === "EvidenceRecord" || kind === "ChainState" || kind === "TrialConclusion") {
    columns.push(
      { name: "Schedule", type: "integer", jsonPath: ".spec.schedule_idx", description: "Schedule index for this committed row.", priority: 0 },
      { name: "Attempt", type: "integer", jsonPath: ".spec.attempt", description: "Scheduler commit attempt.", priority: 0 },
      { name: "Seq", type: "integer", jsonPath: ".spec.row_seq", description: "Row sequence within the committed payload.", priority: 0 },
      { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: "Trial associated with the row when declared.", priority: 1 },
      { name: "Record", type: "string", jsonPath: ".status.record_kind", description: "Payload schema, kind, or event type.", priority: 1 },
    );
  }
  if (kind === "TrialConclusion") {
    columns.push(
      { name: "Outcome", type: "string", jsonPath: ".status.outcome", description: "Conclusion outcome when declared.", priority: 0 },
      { name: "Status", type: "string", jsonPath: ".status.status", description: "Conclusion status when declared.", priority: 1 },
    );
  }
  if (kind === "LineageVersion") {
    columns.push(
      { name: "Chain", type: "string", jsonPath: ".spec.chain_key", description: "Runtime lineage chain key.", priority: 0 },
      { name: "Step", type: "integer", jsonPath: ".spec.step_index", description: "Lineage step index.", priority: 0 },
      { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: "Trial that produced this lineage version.", priority: 0 },
      { name: "Current", type: "boolean", jsonPath: ".status.current_head", description: "Whether this version is the current head for its chain.", priority: 1 },
      { name: "Workspace", type: "string", jsonPath: ".status.workspace_ref", description: "Workspace reference for this version.", priority: 1 },
    );
  }
  if (kind === "LineageHead") {
    columns.push(
      { name: "Chain", type: "string", jsonPath: ".spec.chain_key", description: "Runtime lineage chain key.", priority: 0 },
      { name: "Step", type: "integer", jsonPath: ".status.step_index", description: "Latest lineage step index.", priority: 0 },
      { name: "Version", type: "string", jsonPath: ".status.latest_version_id", description: "Latest lineage version id.", priority: 0 },
      { name: "Workspace", type: "string", jsonPath: ".status.latest_workspace_ref", description: "Latest workspace reference.", priority: 1 },
    );
  }
  if (kind === "TrialStage") {
    columns.push(
      { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: "Trial that owns this contract stage.", priority: 0 },
      { name: "Stage", type: "string", jsonPath: ".spec.stage", description: "Contract stage name.", priority: 0 },
      { name: "Status", type: "string", jsonPath: ".status.status", description: "Contract stage status.", priority: 0 },
      { name: "Recorded", type: "date", jsonPath: ".status.recorded_at", description: "When the stage was recorded.", priority: 1 },
    );
  }
  if (kind === "RuntimeValue") {
    columns.push(
      { name: "Key", type: "string", jsonPath: ".spec.key", description: "Runtime value key.", priority: 0 },
      { name: "Summary", type: "string", jsonPath: ".status.value_summary", description: "Compact summary of the runtime value object.", priority: 0 },
      { name: "Source", type: "string", jsonPath: ".status.source", description: "Runtime value source.", priority: 1 },
      { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: "When this runtime value was last observed.", priority: 1 },
    );
  }
  if (kind === "MetricObservation") {
    columns.push(
      { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: "Trial that emitted this metric observation.", priority: 0 },
      { name: "Metric", type: "string", jsonPath: ".spec.metric_name", description: "Metric stream name.", priority: 0 },
      { name: "Value", type: "number", jsonPath: ".status.metric_value", description: "Observed metric value when numeric.", priority: 0 },
      { name: "Source", type: "string", jsonPath: ".status.metric_source", description: "Metric observation source.", priority: 1 },
      { name: "Seq", type: "integer", jsonPath: ".spec.row_seq", description: "Runtime row sequence number.", priority: 1 },
    );
  }
  if (kind === "PerformanceSample") {
    columns.push(
      { name: "Stage", type: "string", jsonPath: ".spec.stage", description: "Runtime stage sampled.", priority: 0 },
      { name: "Kind", type: "string", jsonPath: ".spec.sample_kind", description: "Sample kind.", priority: 0 },
      { name: "DurationMs", type: "number", jsonPath: ".status.duration_ms", description: "Recorded duration in milliseconds.", priority: 0 },
      { name: "RssKb", type: "integer", jsonPath: ".status.process_rss_kb", description: "Process RSS in KiB when captured.", priority: 1 },
      { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: "Trial associated with this sample when available.", priority: 1 },
    );
  }
  if (kind === "RuntimeOperation") {
    columns.push(
      { name: "Kind", type: "string", jsonPath: ".spec.op_kind", description: "Runtime operation kind.", priority: 0 },
      { name: "OpId", type: "string", jsonPath: ".spec.op_id", description: "Runtime operation id.", priority: 0 },
      { name: "Trial", type: "string", jsonPath: ".status.trial_id", description: "Trial produced by this operation when available.", priority: 0 },
      { name: "Parent", type: "string", jsonPath: ".status.parent_trial_id", description: "Parent trial used by this operation when available.", priority: 1 },
      { name: "Updated", type: "date", jsonPath: ".status.observed_at", description: "When the operation manifest was last updated.", priority: 1 },
    );
  }
  if (kind === "TrialArtifact") {
    columns.push(
      { name: "Trial", type: "string", jsonPath: ".spec.trial_id", description: "Trial that emitted the artifact.", priority: 0 },
      { name: "Role", type: "string", jsonPath: ".spec.role", description: "Artifact role emitted by the attempt.", priority: 0 },
      { name: "Attempt", type: "integer", jsonPath: ".spec.attempt", description: "Trial attempt that emitted the artifact.", priority: 0 },
      { name: "Content", type: "boolean", jsonPath: ".status.content_available", description: "Whether artifact content is available through the artifact content endpoint.", priority: 0 },
      { name: "MediaType", type: "string", jsonPath: ".status.media_type", description: "Artifact media type.", priority: 1 },
      { name: "Bytes", type: "integer", jsonPath: ".status.byte_size", description: "Artifact byte size when known.", priority: 1 },
      { name: "Digest", type: "string", jsonPath: ".status.sha256", description: "Artifact sha256 digest when available.", priority: 1 },
    );
  }
  if (kind === "PortForward") {
    columns.push(
      { name: "Target", type: "string", jsonPath: ".spec.target_ref.name", description: "Resolved runtime resource targeted by the port-forward request.", priority: 0 },
      { name: "TargetKind", type: "string", jsonPath: ".spec.target_ref.kind", description: "Kind of the resolved runtime access target.", priority: 1 },
      { name: "TargetRV", type: "string", jsonPath: ".spec.target_ref.resourceVersion", description: "Resource version reviewed for the selected access target.", priority: 1 },
      { name: "TargetPort", type: "integer", jsonPath: ".spec.target_port", description: "Remote target port requested for the tunnel.", priority: 0 },
      { name: "LocalPort", type: "integer", jsonPath: ".spec.local_port", description: "Requested local port, if one was specified.", priority: 0 },
      { name: "ClientReachable", type: "string", jsonPath: '.status.conditions[?(@.type=="ClientReachable")].status', description: "Whether the active tunnel reports a client-reachable endpoint.", priority: 0 },
      { name: "Runner", type: "string", jsonPath: ".status.runner_binding.runner_instance_id", description: "Runner instance bound to the access request.", priority: 0 },
      { name: "Worker", type: "string", jsonPath: ".status.runner_binding.worker_id", description: "Worker id bound to the access request.", priority: 1 },
      { name: "Requester", type: "string", jsonPath: ".audit.requester", description: "Actor that requested the access resource.", priority: 1 },
      { name: "Mode", type: "string", jsonPath: ".status.connection.mode", description: "Connection mode reported by the worker.", priority: 1 },
      { name: "ProviderTunnel", type: "string", jsonPath: ".status.connection.provider_tunnel_url", description: "Provider tunnel handle for low-level attach, when available.", priority: 1 },
      { name: "Expires", type: "date", jsonPath: ".status.expires_at", description: "Control-plane expiration timestamp for the access request.", priority: 1 },
      { name: "Connection", type: "string", jsonPath: ".status.connection", description: "Port-forward connection metadata reported by the worker.", priority: 1 },
    );
  }
  if (kind === "Exec") {
    columns.push(
      { name: "Target", type: "string", jsonPath: ".spec.target_ref.name", description: "Resolved runtime resource targeted by the exec request.", priority: 0 },
      { name: "TargetKind", type: "string", jsonPath: ".spec.target_ref.kind", description: "Kind of the resolved runtime access target.", priority: 1 },
      { name: "TargetRV", type: "string", jsonPath: ".spec.target_ref.resourceVersion", description: "Resource version reviewed for the selected access target.", priority: 1 },
      { name: "Command", type: "string", jsonPath: ".spec.command", description: "Command requested for runtime exec.", priority: 0 },
      { name: "Exit", type: "integer", jsonPath: ".status.connection.exit_code", description: "Completed exec exit code.", priority: 0 },
      { name: "StdoutBytes", type: "integer", jsonPath: ".status.connection.stdout_bytes", description: "Total stdout bytes observed by the exec worker.", priority: 1 },
      { name: "StdoutTailBytes", type: "integer", jsonPath: ".status.connection.stdout_tail_bytes", description: "Stdout bytes retained in the bounded status tail.", priority: 1 },
      { name: "StdoutTruncated", type: "boolean", jsonPath: ".status.connection.stdout_tail_truncated", description: "Whether stdout status evidence was truncated before storage.", priority: 1 },
      { name: "StderrBytes", type: "integer", jsonPath: ".status.connection.stderr_bytes", description: "Total stderr bytes observed by the exec worker.", priority: 1 },
      { name: "StderrTailBytes", type: "integer", jsonPath: ".status.connection.stderr_tail_bytes", description: "Stderr bytes retained in the bounded status tail.", priority: 1 },
      { name: "StderrTruncated", type: "boolean", jsonPath: ".status.connection.stderr_tail_truncated", description: "Whether stderr status evidence was truncated before storage.", priority: 1 },
      { name: "Runner", type: "string", jsonPath: ".status.runner_binding.runner_instance_id", description: "Runner instance bound to the access request.", priority: 0 },
      { name: "Worker", type: "string", jsonPath: ".status.runner_binding.worker_id", description: "Worker id bound to the access request.", priority: 1 },
      { name: "Requester", type: "string", jsonPath: ".audit.requester", description: "Actor that requested the access resource.", priority: 1 },
      { name: "Mode", type: "string", jsonPath: ".status.connection.mode", description: "Connection mode reported by the worker.", priority: 1 },
      { name: "Expires", type: "date", jsonPath: ".status.expires_at", description: "Control-plane expiration timestamp for the access request.", priority: 1 },
    );
  }
  return columns;
}

function runtimeApiResourcePathTemplates(kind: string, verbs: string[], subresources: string[]): RuntimeApiResourcePathTemplates {
  const base = "/v1/runs/{run_id}/runtime/resources";
  return {
    collection: `${base}?kind=${kind}`,
    resource: `${base}/${kind}/{name}?view=resource`,
    describe: `${base}/${kind}/{name}`,
    operationReview: `${base}/${kind}/{name}/operations/{operation}`,
    watch: `${base}/watch?kind=${kind}`,
    ...(verbs.includes("create") ? { create: runtimeApiResourceCreatePathTemplate(base, kind) } : {}),
    ...(verbs.includes("delete") ? { delete: `${base}/${kind}/{name}` } : {}),
    subresources: Object.fromEntries(
      subresources.map((subresource) => [subresource, `${base}/${kind}/{name}/${subresource}`]),
    ),
  };
}

function runtimeApiResourceCreatePathTemplate(base: string, kind: string): string {
  if (kind === "PortForward") {
    return `${base}/{target_kind}/{target_name}/port-forward`;
  }
  if (kind === "Exec") {
    return `${base}/{target_kind}/{target_name}/exec`;
  }
  return `${base}?kind=${kind}`;
}

export function canonicalRuntimeResourceKind(value: string | null | undefined): string | null {
  const key = runtimeKindAliasKey(value);
  if (!key) {
    return null;
  }
  for (const resource of RUNTIME_API_RESOURCE_DEFINITIONS) {
    const aliases = [
      resource.kind,
      resource.name,
      resource.singularName,
      ...resource.shortNames,
    ];
    if (aliases.some((alias) => runtimeKindAliasKey(alias) === key)) {
      return resource.kind;
    }
  }
  return null;
}

function runtimeKindAliasKey(value: string | null | undefined): string {
  return (value ?? "").trim().toLowerCase().replace(/[^a-z0-9]/g, "");
}

function runtimeInspectLogRefs(cloudRunId: string, resources: RuntimeResourceRecord[]): RuntimeInspectLogRef[] {
  return resources.flatMap((resource) => {
    if (!runtimeResourceSupportsLogs(resource)) {
      return [];
    }
    const basePath = `/v1/runs/${encodeURIComponent(cloudRunId)}/runtime/resources/${encodeURIComponent(resource.kind)}/${encodeURIComponent(resource.metadata.name)}/logs`;
    return [{
      resource: runtimeResourceEventRef(resource),
      streams: ["stdout", "stderr"],
      urls: {
        stdout: `${basePath}?stream=stdout`,
        stderr: `${basePath}?stream=stderr`,
      },
    }];
  });
}

function runtimeResourceSupportsLogs(resource: RuntimeResourceRecord): boolean {
  if (resource.kind === "RunnerAttempt" || resource.kind === "RunnerInstance") {
    return true;
  }
  if (resource.kind === "Exec") {
    return true;
  }
  try {
    runtimeLogTargetForResource(resource);
    return true;
  } catch {
    return false;
  }
}

export function declaredRuntimeResources(
  run: CloudRunRecord,
  attempts: RuntimeRunAttemptRecord[] = [],
  events: RuntimeEventRecord[] = [],
): RuntimeResourceRecord[] {
  const requirements = run.run_requirements;
  const activeAttempts = attempts.filter(runtimeAttemptIsActive);
  const runnerShapeStatus = runtimeRunnerShapeRequirementStatus(requirements, activeAttempts);
  const resources: RuntimeResourceRecord[] = [
    runtimeResource({
      kind: "Run",
      name: run.run_id,
      uid: run.run_id,
      labels: {
        "bucephalus.dev/run-id": run.run_id,
        "bucephalus.dev/package-digest": run.package_digest,
      },
      ownerReferences: [],
      created_at: run.created_at,
      updated_at: run.updated_at,
      spec: {
        package_digest: run.package_digest,
        run_label: run.run_label,
        runtime_options: run.runtime_options,
        env_keys: Object.keys(run.env ?? {}).sort(),
        secret_ids: requirements.secret_ids,
      },
      status: compactObject({
        phase: run.status,
        started_at: run.started_at,
        completed_at: run.completed_at,
        error_message: run.error_message,
        access: runtimeRunAccessStatus(attempts),
      }),
      audit: {
        source: "cloud.runs",
        observed_at: run.updated_at,
      },
    }),
    runtimeResource({
      kind: "Package",
      name: resourceName(run.package_digest),
      uid: run.package_digest,
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        package_digest: run.package_digest,
        image_refs: requirements.image_refs,
      },
      status: {
        phase: "Bound",
      },
      audit: {
        source: "cloud.runs.package_digest",
        observed_at: run.updated_at,
      },
    }),
    runtimeResource({
      kind: "RunnerShape",
      name: "requested",
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        executor: requirements.executor,
        arch: requirements.arch,
        cpu_count: requirements.cpu_count,
        memory_mb: requirements.memory_mb,
        disk_mb: requirements.disk_mb,
        isolation: requirements.isolation,
        timeout_ms: requirements.timeout_ms,
        max_parallel_trials: requirements.max_parallel_trials,
      },
      status: runnerShapeStatus,
      audit: {
        source: "cloud.runs.run_requirements",
        observed_at: run.updated_at,
      },
    }),
  ];
  for (const required of requirements.requires) {
    const requirementStatus = runtimeCapabilityRequirementStatus(required, activeAttempts);
    resources.push(runtimeResource({
      kind: "CapabilityRequirement",
      name: resourceName(required),
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        resource: required,
      },
      status: requirementStatus,
      audit: {
        source: "cloud.runs.run_requirements.requires",
        observed_at: run.updated_at,
      },
    }));
  }
  for (const image of requirements.image_refs) {
    resources.push(runtimeResource({
      kind: "ImagePull",
      name: resourceName(image),
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        image_ref: image,
      },
      status: runtimeImagePullStatus(image, activeAttempts, events),
      audit: {
        source: "cloud.runs.run_requirements.image_refs",
        observed_at: run.updated_at,
      },
    }));
  }
  for (const secretId of requirements.secret_ids) {
    resources.push(runtimeResource({
      kind: "SecretBinding",
      name: resourceName(secretId),
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        secret_id: secretId,
      },
      status: runtimeSecretBindingStatus(secretId, activeAttempts, events),
      audit: {
        source: "cloud.runs.run_requirements.secret_ids",
        observed_at: run.updated_at,
      },
    }));
  }
  if (requirements.network_perimeter.egress_hosts.length > 0) {
    resources.push(runtimeResource({
      kind: "NetworkPerimeter",
      name: "declared",
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        default: requirements.network_perimeter.default,
        task_sandbox: requirements.network_perimeter.task_sandbox,
        agent: requirements.network_perimeter.agent,
        egress_hosts: requirements.network_perimeter.egress_hosts,
      },
      status: runtimeNetworkPerimeterStatus(activeAttempts, events),
      audit: {
        source: "cloud.runs.run_requirements.network_perimeter",
        observed_at: run.updated_at,
      },
    }));
  }
  for (const sidecar of requirements.sidecars) {
    resources.push(runtimeResource({
      kind: "SidecarRequirement",
      name: resourceName(sidecar),
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        sidecar,
      },
      status: runtimeSidecarRequirementStatus(sidecar, activeAttempts, events),
      audit: {
        source: "cloud.runs.run_requirements.sidecars",
        observed_at: run.updated_at,
      },
    }));
  }
  for (const accelerator of requirements.accelerators) {
    resources.push(runtimeResource({
      kind: "AcceleratorRequirement",
      name: resourceName(accelerator),
      labels: {
        "bucephalus.dev/run-id": run.run_id,
      },
      ownerReferences: [runOwnerReference(run)],
      spec: {
        accelerator,
      },
      status: runtimeAcceleratorRequirementStatus(accelerator, activeAttempts, events),
      audit: {
        source: "cloud.runs.run_requirements.accelerators",
        observed_at: run.updated_at,
      },
    }));
  }
  return resources;
}

function runtimeRunnerShapeRequirementStatus(
  requirements: CloudRunRecord["run_requirements"],
  activeAttempts: RuntimeRunAttemptRecord[],
): JsonObject {
  if (activeAttempts.length === 0) {
    return {
      phase: "Pending",
      reason: "WaitingForRunner",
      message: "No active runner attempt has reported capabilities for this run.",
    };
  }
  const reachableAttempts = activeAttempts.filter(runtimeAttemptRunnerReachable);
  if (reachableAttempts.length === 0) {
    return runtimeUnavailableRunnerRequirementStatus(activeAttempts, "No active runner attempt is on a reachable runner instance.");
  }
  const evaluations = reachableAttempts.map((attempt) => runtimeRunnerShapeFit(requirements, attempt));
  const matched = evaluations.filter((evaluation) => evaluation.matched);
  return compactObject({
    phase: matched.length > 0 ? "Satisfied" : "Unsatisfied",
    reason: matched.length > 0 ? "RunnerShapeSatisfied" : "RunnerShapeMismatch",
    message: matched.length > 0
      ? `Active runner attempt ${matched[0]?.attempt_id} satisfies the requested runner shape.`
      : "No active runner attempt satisfies the requested runner shape.",
    active_attempt_ids: activeAttempts.map((attempt) => attempt.attempt_id).sort(),
    reachable_attempt_ids: reachableAttempts.map((attempt) => attempt.attempt_id).sort(),
    satisfied_attempt_ids: matched.map((attempt) => attempt.attempt_id).sort(),
    mismatches: evaluations
      .filter((evaluation) => evaluation.mismatches.length > 0)
      .map((evaluation) => ({
        attempt_id: evaluation.attempt_id,
        runner_instance_id: evaluation.runner_instance_id,
        reasons: evaluation.mismatches,
      })),
  });
}

function runtimeRunnerShapeFit(
  requirements: CloudRunRecord["run_requirements"],
  attempt: RuntimeRunAttemptRecord,
): { attempt_id: string; runner_instance_id: string | null; matched: boolean; mismatches: string[] } {
  const capabilities = normalizeRuntimeCapabilities(attempt.runner_instance_capabilities);
  const mismatches: string[] = [];
  if (!capabilities.executors.includes(requirements.executor)) {
    mismatches.push(`executor ${requirements.executor} not advertised`);
  }
  if (capabilities.arch && capabilities.arch !== requirements.arch) {
    mismatches.push(`arch ${capabilities.arch} does not match ${requirements.arch}`);
  } else if (!capabilities.arch) {
    mismatches.push(`arch ${requirements.arch} not advertised`);
  }
  if (!capabilityAtLeast(capabilities.cpu_count, requirements.cpu_count)) {
    mismatches.push(`cpu_count ${capabilities.cpu_count ?? "unknown"} is below ${requirements.cpu_count}`);
  }
  if (!capabilityAtLeast(capabilities.memory_mb, requirements.memory_mb)) {
    mismatches.push(`memory_mb ${capabilities.memory_mb ?? "unknown"} is below ${requirements.memory_mb}`);
  }
  if (!capabilityAtLeast(capabilities.disk_mb, requirements.disk_mb)) {
    mismatches.push(`disk_mb ${capabilities.disk_mb ?? "unknown"} is below ${requirements.disk_mb}`);
  }
  if (!capabilities.isolation?.includes(requirements.isolation)) {
    mismatches.push(`isolation ${requirements.isolation} not advertised`);
  }
  return {
    attempt_id: attempt.attempt_id,
    runner_instance_id: attempt.runner_instance_id,
    matched: mismatches.length === 0,
    mismatches,
  };
}

function runtimeCapabilityRequirementStatus(
  required: string,
  activeAttempts: RuntimeRunAttemptRecord[],
): JsonObject {
  return runtimeCapabilityBackedRequirementStatus(required, activeAttempts, {
    pendingMessage: `No active runner attempt has reported whether ${required} is available.`,
    satisfiedReason: "CapabilitySatisfied",
    missingReason: "CapabilityMissing",
    satisfiedMessage: (attemptId) => `Active runner attempt ${attemptId} advertises ${required}.`,
    missingMessage: `No active runner attempt advertises ${required}.`,
  });
}

function runtimeCapabilityBackedRequirementStatus(
  required: string,
  activeAttempts: RuntimeRunAttemptRecord[],
  options: {
    pendingMessage: string;
    satisfiedReason: string;
    missingReason: string;
    satisfiedMessage: (attemptId: string) => string;
    missingMessage: string;
  },
): JsonObject {
  if (activeAttempts.length === 0) {
    return {
      phase: "Pending",
      reason: "WaitingForRunner",
      message: options.pendingMessage,
    };
  }
  const reachableAttempts = activeAttempts.filter(runtimeAttemptRunnerReachable);
  if (reachableAttempts.length === 0) {
    return runtimeUnavailableRunnerRequirementStatus(activeAttempts, "No active runner attempt is on a reachable runner instance.");
  }
  const satisfied = reachableAttempts.filter((attempt) =>
    normalizeRuntimeCapabilities(attempt.runner_instance_capabilities).resources.includes(required)
  );
  return compactObject({
    phase: satisfied.length > 0 ? "Satisfied" : "Unsatisfied",
    reason: satisfied.length > 0 ? options.satisfiedReason : options.missingReason,
    message: satisfied.length > 0
      ? options.satisfiedMessage(String(satisfied[0]?.attempt_id ?? "unknown"))
      : options.missingMessage,
    active_attempt_ids: activeAttempts.map((attempt) => attempt.attempt_id).sort(),
    reachable_attempt_ids: reachableAttempts.map((attempt) => attempt.attempt_id).sort(),
    satisfied_attempt_ids: satisfied.map((attempt) => attempt.attempt_id).sort(),
    missing_attempt_ids: reachableAttempts
      .filter((attempt) => !satisfied.includes(attempt))
      .map((attempt) => attempt.attempt_id)
      .sort(),
  });
}

function runtimeUnavailableRunnerRequirementStatus(
  activeAttempts: RuntimeRunAttemptRecord[],
  message: string,
): JsonObject {
  return compactObject({
    phase: "Unsatisfied",
    reason: "RunnerUnavailable",
    message,
    active_attempt_ids: activeAttempts.map((attempt) => attempt.attempt_id).sort(),
    unavailable_attempts: activeAttempts.map((attempt) => compactObject({
      attempt_id: attempt.attempt_id,
      runner_instance_id: attempt.runner_instance_id,
      runner_instance_status: runtimeAttemptRunnerStatus(attempt) ?? "unknown",
    })),
  });
}

function runtimeSecretBindingStatus(
  secretId: string,
  activeAttempts: RuntimeRunAttemptRecord[],
  events: RuntimeEventRecord[],
): JsonObject {
  const capabilityStatus = runtimeCapabilityBackedRequirementStatus("secret_resolver", activeAttempts, {
    pendingMessage: `No active runner attempt has reported whether secret resolution is available for ${secretId}.`,
    satisfiedReason: "SecretResolverAvailable",
    missingReason: "SecretResolverMissing",
    satisfiedMessage: (attemptId) => `Active runner attempt ${attemptId} advertises secret_resolver for ${secretId}.`,
    missingMessage: `No active runner attempt advertises secret_resolver for ${secretId}.`,
  });
  const materialized = latestRuntimeResourceLifecycleEvent(
    events,
    "SecretBinding",
    resourceName(secretId),
    ["worker.runtime.secret_binding.materialized"],
  );
  if (!materialized) {
    return capabilityStatus;
  }
  return {
    ...capabilityStatus,
    phase: "Materialized",
    reason: "SecretBindingMaterialized",
    message: `Worker attempt ${runtimeLifecycleEventAttemptId(materialized)} materialized SecretBinding/${resourceName(secretId)}.`,
    ...runtimeLifecycleEventStatusFields(materialized),
  };
}

function runtimeImagePullStatus(
  imageRef: string,
  activeAttempts: RuntimeRunAttemptRecord[],
  events: RuntimeEventRecord[],
): JsonObject {
  const capabilityStatus = runtimeCapabilityBackedRequirementStatus("registry_pull", activeAttempts, {
    pendingMessage: `No active runner attempt has reported whether registry pulls are available for ${imageRef}.`,
    satisfiedReason: "RegistryPullAvailable",
    missingReason: "RegistryPullMissing",
    satisfiedMessage: (attemptId) => `Active runner attempt ${attemptId} advertises registry_pull for pulling ${imageRef}.`,
    missingMessage: `No active runner attempt advertises registry_pull for pulling ${imageRef}.`,
  });
  const event = latestRuntimeResourceLifecycleEvent(events, "ImagePull", resourceName(imageRef), [
    "worker.runtime.image_pull.pulled",
    "worker.runtime.image_pull.failed",
    "worker.runtime.image_pull.pulling",
  ]);
  if (!event) {
    return capabilityStatus;
  }
  if (event.event_type === "worker.runtime.image_pull.failed") {
    return {
      ...capabilityStatus,
      phase: "Failed",
      reason: "ImagePullFailed",
      message: stringField(event.payload.error) ?? `Worker failed to pull ImagePull/${resourceName(imageRef)}.`,
      error_message: stringField(event.payload.error),
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  if (event.event_type === "worker.runtime.image_pull.pulling") {
    return {
      ...capabilityStatus,
      phase: "Pulling",
      reason: "ImagePulling",
      message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} is pulling ImagePull/${resourceName(imageRef)}.`,
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  return {
    ...capabilityStatus,
    phase: "Pulled",
    reason: "ImagePulled",
    message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} pulled ImagePull/${resourceName(imageRef)}.`,
    ...runtimeLifecycleEventStatusFields(event),
  };
}

function runtimeSidecarRequirementStatus(
  sidecar: string,
  activeAttempts: RuntimeRunAttemptRecord[],
  events: RuntimeEventRecord[],
): JsonObject {
  const capabilityStatus = runtimeCapabilityBackedRequirementStatus(`sidecar:${sidecar}`, activeAttempts, {
    pendingMessage: `No active runner attempt has reported whether sidecar ${sidecar} is available.`,
    satisfiedReason: "SidecarCapabilityAvailable",
    missingReason: "SidecarCapabilityMissing",
    satisfiedMessage: (attemptId) => `Active runner attempt ${attemptId} advertises sidecar:${sidecar}.`,
    missingMessage: `No active runner attempt advertises sidecar:${sidecar}.`,
  });
  const event = latestRuntimeResourceLifecycleEvent(events, "SidecarRequirement", resourceName(sidecar), [
    "worker.runtime.sidecar_requirement.available",
    "worker.runtime.sidecar_requirement.failed",
    "worker.runtime.sidecar_requirement.checking",
  ]);
  if (!event) {
    return capabilityStatus;
  }
  if (event.event_type === "worker.runtime.sidecar_requirement.failed") {
    return {
      ...capabilityStatus,
      phase: "Failed",
      reason: "SidecarRequirementFailed",
      message: stringField(event.payload.error) ?? `Worker failed to validate SidecarRequirement/${resourceName(sidecar)}.`,
      error_message: stringField(event.payload.error),
      required_capability: stringField(event.payload.required_capability),
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  if (event.event_type === "worker.runtime.sidecar_requirement.checking") {
    return {
      ...capabilityStatus,
      phase: "Checking",
      reason: "SidecarRequirementChecking",
      message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} is validating SidecarRequirement/${resourceName(sidecar)}.`,
      required_capability: stringField(event.payload.required_capability),
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  return {
    ...capabilityStatus,
    phase: "Available",
    reason: "SidecarRequirementAvailable",
    message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} validated SidecarRequirement/${resourceName(sidecar)}.`,
    required_capability: stringField(event.payload.required_capability),
    ...runtimeLifecycleEventStatusFields(event),
  };
}

function runtimeAcceleratorRequirementStatus(
  accelerator: string,
  activeAttempts: RuntimeRunAttemptRecord[],
  events: RuntimeEventRecord[],
): JsonObject {
  const capabilityStatus = runtimeCapabilityBackedRequirementStatus(`accelerator:${accelerator}`, activeAttempts, {
    pendingMessage: `No active runner attempt has reported whether accelerator ${accelerator} is available.`,
    satisfiedReason: "AcceleratorCapabilityAvailable",
    missingReason: "AcceleratorCapabilityMissing",
    satisfiedMessage: (attemptId) => `Active runner attempt ${attemptId} advertises accelerator:${accelerator}.`,
    missingMessage: `No active runner attempt advertises accelerator:${accelerator}.`,
  });
  const event = latestRuntimeResourceLifecycleEvent(events, "AcceleratorRequirement", resourceName(accelerator), [
    "worker.runtime.accelerator_requirement.available",
    "worker.runtime.accelerator_requirement.failed",
    "worker.runtime.accelerator_requirement.checking",
  ]);
  if (!event) {
    return capabilityStatus;
  }
  if (event.event_type === "worker.runtime.accelerator_requirement.failed") {
    return {
      ...capabilityStatus,
      phase: "Failed",
      reason: "AcceleratorRequirementFailed",
      message: stringField(event.payload.error) ?? `Worker failed to validate AcceleratorRequirement/${resourceName(accelerator)}.`,
      error_message: stringField(event.payload.error),
      required_capability: stringField(event.payload.required_capability),
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  if (event.event_type === "worker.runtime.accelerator_requirement.checking") {
    return {
      ...capabilityStatus,
      phase: "Checking",
      reason: "AcceleratorRequirementChecking",
      message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} is validating AcceleratorRequirement/${resourceName(accelerator)}.`,
      required_capability: stringField(event.payload.required_capability),
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  return {
    ...capabilityStatus,
    phase: "Available",
    reason: "AcceleratorRequirementAvailable",
    message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} validated AcceleratorRequirement/${resourceName(accelerator)}.`,
    required_capability: stringField(event.payload.required_capability),
    ...runtimeLifecycleEventStatusFields(event),
  };
}

function runtimeNetworkPerimeterStatus(
  activeAttempts: RuntimeRunAttemptRecord[],
  events: RuntimeEventRecord[],
): JsonObject {
  const capabilityStatus = runtimeCapabilityBackedRequirementStatus("network_perimeter", activeAttempts, {
    pendingMessage: "No active runner attempt has reported whether network perimeter enforcement is available.",
    satisfiedReason: "NetworkPerimeterAvailable",
    missingReason: "NetworkPerimeterMissing",
    satisfiedMessage: (attemptId) => `Active runner attempt ${attemptId} advertises network_perimeter.`,
    missingMessage: "No active runner attempt advertises network_perimeter.",
  });
  const event = latestRuntimeResourceLifecycleEvent(events, "NetworkPerimeter", "declared", [
    "worker.runtime.network_perimeter.applied",
    "worker.runtime.network_perimeter.failed",
    "worker.runtime.network_perimeter.applying",
  ]);
  if (!event) {
    return capabilityStatus;
  }
  if (event.event_type === "worker.runtime.network_perimeter.failed") {
    return {
      ...capabilityStatus,
      phase: "Failed",
      reason: "NetworkPerimeterApplyFailed",
      message: stringField(event.payload.error) ?? "Worker failed to apply NetworkPerimeter/declared.",
      error_message: stringField(event.payload.error),
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  if (event.event_type === "worker.runtime.network_perimeter.applying") {
    return {
      ...capabilityStatus,
      phase: "Applying",
      reason: "NetworkPerimeterApplying",
      message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} is applying NetworkPerimeter/declared.`,
      ...runtimeLifecycleEventStatusFields(event),
    };
  }
  return {
    ...capabilityStatus,
    phase: "Applied",
    reason: "NetworkPerimeterApplied",
    message: `Worker attempt ${runtimeLifecycleEventAttemptId(event)} applied NetworkPerimeter/declared.`,
    ...runtimeLifecycleEventStatusFields(event),
  };
}

function latestRuntimeResourceLifecycleEvent(
  events: RuntimeEventRecord[],
  kind: string,
  name: string,
  eventTypes: string[],
): RuntimeEventRecord | undefined {
  const typeSet = new Set(eventTypes);
  return events
    .filter((event) => typeSet.has(event.event_type))
    .filter((event) => runtimeEventResourceRefs(event).some((ref) => {
      const refKind = ref.kind ? canonicalRuntimeResourceKind(ref.kind) ?? ref.kind : "";
      return refKind === kind && ref.name === name;
    }))
    .sort((left, right) =>
      eventTime(right) - eventTime(left)
      || right.row_seq - left.row_seq
      || right.seq - left.seq
    )[0];
}

function runtimeLifecycleEventStatusFields(event: RuntimeEventRecord): JsonObject {
  return compactObject({
    event_type: event.event_type,
    event_row_seq: event.row_seq,
    event_seq: event.seq,
    observed_at: event.ts ?? stringField(event.row.created_at),
    attempt_id: runtimeLifecycleEventAttemptId(event),
    runner_instance_id: stringField(event.payload.runner_instance_id),
    worker_id: stringField(event.payload.worker_id),
  });
}

function runtimeLifecycleEventAttemptId(event: RuntimeEventRecord): string {
  return stringField(event.payload.attempt_id) ?? stringField(event.row.attempt_id) ?? "unknown";
}

function capabilityAtLeast(value: number | null | undefined, required: number): boolean {
  return typeof value === "number" && Number.isFinite(value) && value >= required;
}

function runtimeRunnerPoolIds(
  attempts: RuntimeRunAttemptRecord[],
  provisionRequests: RuntimeProvisionRequestRecord[],
): string[] {
  return [...stringSet([
    ...attempts.map((attempt) => attempt.runner_pool_id),
    ...provisionRequests.map((request) => request.runner_pool_id),
  ])].sort();
}

function runnerPoolResource(
  run: CloudRunRecord,
  pool: RuntimeRunnerPoolRecord,
  attempts: RuntimeRunAttemptRecord[],
  provisionRequests: RuntimeProvisionRequestRecord[],
): RuntimeResourceRecord {
  const capabilities = normalizeRuntimeCapabilities(pool.capabilities);
  const runStatus = runnerPoolRunStatus(pool.runner_pool_id, attempts, provisionRequests);
  return runtimeResource({
    kind: "RunnerPool",
    name: resourceName(pool.runner_pool_id),
    uid: pool.runner_pool_id,
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/runner-pool-id": pool.runner_pool_id,
    }),
    ownerReferences: [runOwnerReference(run)],
    created_at: pool.created_at,
    updated_at: pool.updated_at,
    spec: compactObject({
      runner_pool_id: pool.runner_pool_id,
      name: pool.name,
      capabilities: capabilities as unknown as JsonObject,
      metadata: pool.metadata,
    }),
    status: compactObject({
      phase: pool.status,
      ...runStatus,
    }),
    audit: {
      source: "cloud.runner_pools",
      observed_at: pool.updated_at,
    },
  });
}

function runnerPoolRunStatus(
  runnerPoolId: string,
  attempts: RuntimeRunAttemptRecord[],
  provisionRequests: RuntimeProvisionRequestRecord[],
): JsonObject {
  const poolAttempts = attempts.filter((attempt) => attempt.runner_pool_id === runnerPoolId);
  const poolRequests = provisionRequests.filter((request) => request.runner_pool_id === runnerPoolId);
  const runnerInstanceIds = [...stringSet(poolAttempts.map((attempt) => attempt.runner_instance_id))].sort();
  const runnerInstancePhases = latestRunnerInstancePhases(poolAttempts);
  const workerIds = [...stringSet(poolAttempts.map((attempt) => attempt.worker_id))].sort();
  const activeAttempts = poolAttempts.filter((attempt) => attempt.status === "running" && !attempt.ended_at);
  return {
    run_scope: "current_run",
    runner_instance_ids: runnerInstanceIds,
    worker_ids: workerIds,
    active_attempt_ids: activeAttempts.map((attempt) => attempt.attempt_id).sort(),
    runner_instances: {
      total: runnerInstanceIds.length,
      by_phase: countStrings(runnerInstancePhases),
    },
    attempts: {
      total: poolAttempts.length,
      active: activeAttempts.length,
      by_phase: countStrings(poolAttempts.map((attempt) => attempt.status)),
    },
    provision_requests: {
      total: poolRequests.length,
      pending: poolRequests.filter((request) => request.status === "requested" || request.status === "provisioning").length,
      by_phase: countStrings(poolRequests.map((request) => request.status)),
    },
  };
}

function latestRunnerInstancePhases(attempts: RuntimeRunAttemptRecord[]): string[] {
  const byInstance = new Map<string, { phase: string; updatedAt: number }>();
  for (const attempt of attempts) {
    if (!attempt.runner_instance_id) {
      continue;
    }
    const updatedAt = Date.parse(attempt.runner_instance_updated_at ?? attempt.updated_at ?? attempt.heartbeat_at ?? attempt.created_at);
    const existing = byInstance.get(attempt.runner_instance_id);
    if (!existing || updatedAt >= existing.updatedAt) {
      byInstance.set(attempt.runner_instance_id, {
        phase: attempt.runner_instance_status ?? "unknown",
        updatedAt,
      });
    }
  }
  return [...byInstance.values()].map((instance) => instance.phase);
}

function runnerInstanceResources(run: CloudRunRecord, attempts: RuntimeRunAttemptRecord[]): RuntimeResourceRecord[] {
  const byRunnerInstance = new Map<string, RuntimeRunAttemptRecord[]>();
  for (const attempt of attempts) {
    if (!attempt.runner_instance_id) {
      continue;
    }
    const existing = byRunnerInstance.get(attempt.runner_instance_id) ?? [];
    existing.push(attempt);
    byRunnerInstance.set(attempt.runner_instance_id, existing);
  }
  return [...byRunnerInstance.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([runnerInstanceId, instanceAttempts]) => runnerInstanceResource(run, runnerInstanceId, instanceAttempts));
}

function runnerInstanceResource(
  run: CloudRunRecord,
  runnerInstanceId: string,
  attempts: RuntimeRunAttemptRecord[],
): RuntimeResourceRecord {
  const latest = [...attempts].sort((left, right) => {
    const leftTime = Date.parse(left.runner_instance_updated_at ?? left.updated_at ?? left.heartbeat_at ?? left.created_at);
    const rightTime = Date.parse(right.runner_instance_updated_at ?? right.updated_at ?? right.heartbeat_at ?? right.created_at);
    return rightTime - leftTime;
  })[0]!;
  const capabilities = normalizeRuntimeCapabilities(latest.runner_instance_capabilities);
  const access = runtimeAccessCapabilities(capabilities);
  const providerIdentity = runtimeRunnerProviderIdentity(latest.runner_instance_provider_instance_id, latest.runner_instance_name);
  const activeAttempts = attempts.filter((attempt) => attempt.status === "running" && !attempt.ended_at);
  const currentAttempt = activeAttempts[0] ?? latest;
  const workerIds = [...stringSet(attempts.map((attempt) => attempt.worker_id))].sort();
  return runtimeResource({
    kind: "RunnerInstance",
    name: resourceName(runnerInstanceId),
    uid: runnerInstanceId,
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/runner-instance-id": runnerInstanceId,
      "bucephalus.dev/runner-pool-id": latest.runner_pool_id,
      "bucephalus.dev/current-attempt-id": currentAttempt.attempt_id,
      "bucephalus.dev/provider": stringField(providerIdentity.provider),
      "topology.kubernetes.io/zone": stringField(providerIdentity.zone),
    }),
    ownerReferences: [
      runOwnerReference(run),
      ...(latest.runner_pool_id ? [runnerPoolOwnerReference(latest.runner_pool_id)] : []),
    ],
    created_at: latest.runner_instance_created_at ?? latest.created_at,
    updated_at: latest.runner_instance_updated_at ?? latest.updated_at,
    spec: compactObject({
      runner_instance_id: runnerInstanceId,
      runner_pool_id: latest.runner_pool_id,
      instance_name: latest.runner_instance_name,
      capabilities: capabilities as unknown as JsonObject,
      metadata: latest.runner_instance_metadata,
      attempt_ids: attempts.map((attempt) => attempt.attempt_id),
      worker_ids: workerIds,
    }),
    status: compactObject({
      phase: latest.runner_instance_status ?? "unknown",
      ...providerIdentity,
      last_heartbeat_at: latest.runner_instance_last_heartbeat_at ?? latest.heartbeat_at,
      current_attempt_id: currentAttempt.attempt_id,
      active_attempts: activeAttempts.length,
      attempt_phases: attempts.reduce<Record<string, string>>((acc, attempt) => {
        acc[attempt.attempt_id] = attempt.status;
        return acc;
      }, {}),
      access: {
        ...access,
        worker_id: currentAttempt.worker_id,
      },
    }),
    audit: compactObject({
      source: "cloud.runner_instances",
      observed_at: latest.runner_instance_last_heartbeat_at ?? latest.runner_instance_updated_at ?? latest.updated_at,
    }),
  });
}

function runtimeRunnerProviderIdentity(providerInstanceId: string | null, instanceName: string | null): JsonObject {
  if (!providerInstanceId) {
    return compactObject({
      instance_name: instanceName,
    });
  }
  const gce = parseGceProviderInstanceId(providerInstanceId);
  if (gce) {
    return compactObject({
      provider: "gce",
      provider_instance_id: providerInstanceId,
      project_id: gce.projectId,
      zone: gce.zone,
      instance_name: gce.instanceName,
    });
  }
  return compactObject({
    provider_instance_id: providerInstanceId,
    instance_name: instanceName,
  });
}

function parseGceProviderInstanceId(value: string): { projectId: string; zone: string; instanceName: string } | null {
  const match = /^gce:\/\/projects\/([^/]+)\/zones\/([^/]+)\/instances\/([^/]+)$/.exec(value);
  if (!match) {
    return null;
  }
  return {
    projectId: decodeURIComponent(match[1] ?? ""),
    zone: decodeURIComponent(match[2] ?? ""),
    instanceName: decodeURIComponent(match[3] ?? ""),
  };
}

function attemptResource(run: CloudRunRecord, attempt: RuntimeRunAttemptRecord): RuntimeResourceRecord {
  const capabilities = normalizeRuntimeCapabilities(attempt.runner_instance_capabilities);
  const access = runtimeAccessCapabilities(capabilities);
  return runtimeResource({
    kind: "RunnerAttempt",
    name: attempt.attempt_id,
    uid: attempt.attempt_id,
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/runner-instance-id": attempt.runner_instance_id,
      "bucephalus.dev/runner-pool-id": attempt.runner_pool_id,
      "bucephalus.dev/worker-id": attempt.worker_id,
    }),
    ownerReferences: [
      runOwnerReference(run),
      ...(attempt.runner_pool_id ? [runnerPoolOwnerReference(attempt.runner_pool_id)] : []),
      ...(attempt.runner_instance_id ? [runnerInstanceOwnerReference(attempt.runner_instance_id)] : []),
    ],
    created_at: attempt.created_at,
    updated_at: attempt.updated_at,
    spec: compactObject({
      worker_id: attempt.worker_id,
      runner_instance_id: attempt.runner_instance_id,
      runner_pool_id: attempt.runner_pool_id,
      runner_instance_name: attempt.runner_instance_name,
      capabilities: capabilities as unknown as JsonObject,
    }),
    status: compactObject({
      phase: attempt.status,
      runner_instance_status: attempt.runner_instance_status,
      access,
      lease_expires_at: attempt.lease_expires_at,
      heartbeat_at: attempt.heartbeat_at,
      started_at: attempt.started_at,
      ended_at: attempt.ended_at,
      error_message: attempt.error_message,
    }),
    audit: compactObject({
      source: "cloud.run_attempts",
      runner_source: attempt.runner_instance_id ? "cloud.runner_instances" : undefined,
      observed_at: attempt.updated_at,
    }),
  });
}

function provisionRequestResource(
  run: CloudRunRecord,
  request: RuntimeProvisionRequestRecord,
): RuntimeResourceRecord {
  return runtimeResource({
    kind: "RunnerProvisionRequest",
    name: request.provision_request_id,
    uid: request.provision_request_id,
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/runner-pool-id": request.runner_pool_id,
      "bucephalus.dev/runner-instance-id": request.runner_instance_id,
    }),
    ownerReferences: [
      runOwnerReference(run),
      runnerPoolOwnerReference(request.runner_pool_id),
    ],
    created_at: request.created_at,
    updated_at: request.updated_at,
    spec: {
      runner_pool_id: request.runner_pool_id,
      provider: request.provider,
      requirements: request.requirements,
      metadata: request.metadata,
    },
    status: compactObject({
      phase: request.status,
      provider_instance_id: request.provider_instance_id,
      instance_name: request.instance_name,
      runner_instance_id: request.runner_instance_id,
      error_message: request.error_message,
    }),
    audit: {
      source: "cloud.runner_provision_requests",
      observed_at: request.updated_at,
    },
  });
}

function runtimeTrialRecords(
  trialAttempts: RuntimeTrialAttemptRecord[],
  snapshots: RuntimeSnapshotRecord[],
): RuntimeTrialRecord[] {
  const byTrial = new Map<string, RuntimeTrialRecord>();
  for (const attempt of trialAttempts) {
    const key = runtimeTrialRecordKey(attempt.core_run_id, attempt.trial_id);
    const existing = byTrial.get(key);
    if (!existing || runtimeTrialRecordSortValue(attempt) >= runtimeTrialRecordSortValue(existing)) {
      byTrial.set(key, {
        ...attempt,
        source: "bucephalus_runtime.trial_attempts",
      });
    }
  }
  for (const result of runtimeTrialResultsFromSnapshots(snapshots)) {
    const key = runtimeTrialRecordKey(result.core_run_id, result.trial_id);
    const existing = byTrial.get(key);
    const fallback: RuntimeTrialRecord = {
      core_run_id: result.core_run_id,
      trial_id: result.trial_id,
      schedule_idx: result.schedule_idx,
      attempt: result.attempt,
      phase: runtimeTrialPhaseFromOutcome(result.outcome),
      paused_from_phase: null,
      variant_id: result.variant_id,
      task_id: result.task_id,
      repl_idx: result.repl_idx,
      state: {},
      updated_at_ms: 0,
      source: "worker_runtime_snapshot",
    };
    byTrial.set(key, {
      ...(existing ?? fallback),
      ...runtimeTrialResultFields(result),
    });
  }
  return [...byTrial.values()].sort((left, right) =>
    left.core_run_id.localeCompare(right.core_run_id)
      || left.schedule_idx - right.schedule_idx
      || left.trial_id.localeCompare(right.trial_id)
      || left.attempt - right.attempt
  );
}

function runtimeTrialResultFields(result: RuntimeTrialResultRecord): Partial<RuntimeTrialRecord> {
  return {
    row_seq: result.row_seq,
    outcome: result.outcome,
    primary_metric_name: result.primary_metric_name,
    primary_metric_value: result.primary_metric_value,
    metrics: result.metrics,
    bindings: result.bindings,
    events_total: result.events_total,
    has_events: result.has_events,
  };
}

function runtimeTrialRecordKey(coreRunId: string, trialId: string): string {
  return `${coreRunId}\0${trialId}`;
}

function runtimeTrialRecordSortValue(record: Pick<RuntimeTrialAttemptRecord, "updated_at_ms" | "attempt" | "schedule_idx">): number {
  return record.updated_at_ms || record.attempt * 1_000_000 + record.schedule_idx;
}

function runtimeTrialPhaseFromOutcome(outcome: string): string {
  const normalized = outcome.trim().toLowerCase();
  if (!normalized || normalized === "unknown") {
    return "unknown";
  }
  if (/fail|error|timeout|cancel|reject/.test(normalized)) {
    return "failed";
  }
  if (/success|succeed|pass|passed|ok|complete|correct/.test(normalized)) {
    return "succeeded";
  }
  return "completed";
}

interface RuntimeRunnerBinding {
  workerId?: string | null | undefined;
  attempt?: RuntimeRunAttemptRecord | undefined;
}

function trialResource(
  run: CloudRunRecord,
  trial: RuntimeTrialRecord,
  slots: RuntimeSlotRecord[],
  containers: RuntimeTrialContainerRecord[],
  attempts: RuntimeRunAttemptRecord[],
): RuntimeResourceRecord {
  const slot = runtimeSlotForTrial(slots, trial);
  const binding = runtimeRunnerBindingForWorker(attempts, slot?.worker_id);
  const trialContainers = containers.filter((container) =>
    container.core_run_id === trial.core_run_id
      && container.trial_id === trial.trial_id
      && container.attempt === trial.attempt
  );
  const updatedAt = runtimeIsoFromMs(trial.updated_at_ms);
  return runtimeResource({
    kind: "Trial",
    name: resourceName(trial.trial_id),
    uid: trialUid(trial.core_run_id, trial.trial_id),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": trial.core_run_id,
      "bucephalus.dev/trial-id": trial.trial_id,
      "bucephalus.dev/variant-id": trial.variant_id,
      "bucephalus.dev/task-id": trial.task_id,
      "bucephalus.dev/worker-id": binding.workerId,
      "bucephalus.dev/attempt-id": binding.attempt?.attempt_id,
      "bucephalus.dev/runner-instance-id": binding.attempt?.runner_instance_id,
      "bucephalus.dev/runner-pool-id": binding.attempt?.runner_pool_id,
    }),
    ownerReferences: [runOwnerReference(run)],
    ...(updatedAt ? { updated_at: updatedAt } : {}),
    spec: compactObject({
      core_run_id: trial.core_run_id,
      trial_id: trial.trial_id,
      schedule_idx: trial.schedule_idx,
      attempt: trial.attempt,
      variant_id: trial.variant_id,
      task_id: trial.task_id,
      repl_idx: trial.repl_idx,
      paused_from_phase: trial.paused_from_phase,
      state: trial.state,
      bindings: trial.bindings,
    }),
    status: compactObject({
      phase: trial.phase,
      outcome: trial.outcome,
      primary_metric_name: trial.primary_metric_name,
      primary_metric_value: trial.primary_metric_value,
      metrics: trial.metrics,
      events_total: trial.events_total,
      has_events: trial.has_events,
      row_seq: trial.row_seq,
      schedule_slot: slot ? `${resourceName(slot.core_run_id)}.${slot.schedule_idx}` : undefined,
      runner_binding: runtimeRunnerBindingStatus(binding),
      containers: {
        total: trialContainers.length,
        by_phase: countStrings(trialContainers.map((container) => container.status)),
        roles: [...stringSet(trialContainers.map((container) => container.role))].sort(),
      },
    }),
    audit: compactObject({
      source: trial.source,
      observed_at: updatedAt,
    }),
  });
}

function scheduleSlotResource(
  run: CloudRunRecord,
  slot: RuntimeSlotRecord,
  attempts: RuntimeRunAttemptRecord[],
): RuntimeResourceRecord {
  const binding = runtimeRunnerBindingForWorker(attempts, slot.worker_id);
  return runtimeResource({
    kind: "ScheduleSlot",
    name: `${resourceName(slot.core_run_id)}.${slot.schedule_idx}`,
    uid: scheduleSlotUid(slot),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": slot.core_run_id,
      "bucephalus.dev/trial-id": slot.trial_id,
      "bucephalus.dev/worker-id": slot.worker_id,
      "bucephalus.dev/attempt-id": binding.attempt?.attempt_id,
      "bucephalus.dev/runner-instance-id": binding.attempt?.runner_instance_id,
      "bucephalus.dev/runner-pool-id": binding.attempt?.runner_pool_id,
    }),
    ownerReferences: [
      ...(slot.trial_id ? [trialOwnerReference(slot.core_run_id, slot.trial_id)] : []),
      runOwnerReference(run),
      ...runtimeRunnerBindingOwnerReferences(binding),
    ],
    spec: {
      core_run_id: slot.core_run_id,
      schedule_idx: slot.schedule_idx,
      slot: slot.slot,
    },
    status: compactObject({
      phase: slot.state,
      trial_id: slot.trial_id,
      attempt: slot.attempt,
      worker_id: slot.worker_id,
      owner_id: slot.owner_id,
      lease_expires_at: slot.lease_expires_at,
      slot_commit_id: slot.slot_commit_id,
      slot_status: slot.slot_status,
      runner_binding: runtimeRunnerBindingStatus(binding),
    }),
    audit: {
      source: "bucephalus_runtime.schedule_slots",
    },
  });
}

function coreRunResource(
  run: CloudRunRecord,
  coreRun: RuntimeCoreRunRecord,
): RuntimeResourceRecord {
  const createdAt = runtimeIsoFromMs(coreRun.created_at_ms);
  const observedAt = runtimeIsoFromMs(coreRun.updated_at_ms);
  return runtimeResource({
    kind: "CoreRun",
    name: coreRunResourceName(coreRun),
    uid: coreRunUid(coreRun),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": coreRun.core_run_id,
      "bucephalus.dev/experiment-id": coreRun.experiment_id,
      "bucephalus.dev/runtime-status": coreRun.runtime_status,
    }),
    ownerReferences: [
      runOwnerReference(run),
    ],
    ...(createdAt ? { created_at: createdAt } : {}),
    ...(observedAt ? { updated_at: observedAt } : {}),
    spec: {
      core_run_id: coreRun.core_run_id,
    },
    status: compactObject({
      phase: coreRun.runtime_status,
      experiment_id: coreRun.experiment_id,
      runtime_status: coreRun.runtime_status,
      project_root: coreRun.project_root,
      run_dir: coreRun.run_dir,
      artifact_root: coreRun.artifact_root,
      created_at: createdAt,
      observed_at: observedAt,
      created_at_ms: coreRun.created_at_ms,
      updated_at_ms: coreRun.updated_at_ms,
      manifest: coreRun.manifest,
    }),
    audit: {
      source: "bucephalus_runtime.runs",
      row: coreRun.manifest,
    },
  });
}

function runManifestResource(
  run: CloudRunRecord,
  manifest: RuntimeRunManifestRecord,
): RuntimeResourceRecord {
  const observedAt = runtimeIsoFromMs(manifest.updated_at_ms);
  const workloadType = stringField(manifest.manifest.workload_type);
  const baselineId = stringField(manifest.manifest.baseline_id);
  return runtimeResource({
    kind: "RunManifest",
    name: runManifestResourceName(manifest),
    uid: runManifestUid(manifest),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": manifest.core_run_id,
      "bucephalus.dev/experiment-id": manifest.experiment_id,
      "bucephalus.dev/runtime-status": manifest.runtime_status,
      "bucephalus.dev/workload-type": workloadType,
      "bucephalus.dev/baseline-id": baselineId,
    }),
    ownerReferences: [
      runOwnerReference(run),
    ],
    ...(observedAt ? { created_at: observedAt, updated_at: observedAt } : {}),
    spec: {
      core_run_id: manifest.core_run_id,
    },
    status: compactObject({
      phase: "Current",
      experiment_id: manifest.experiment_id,
      runtime_status: manifest.runtime_status,
      workload_type: workloadType,
      baseline_id: baselineId,
      variants_total: jsonArrayLength(manifest.manifest.variant_ids),
      project_root: manifest.project_root,
      run_dir: manifest.run_dir,
      artifact_root: manifest.artifact_root,
      observed_at: observedAt,
      updated_at_ms: manifest.updated_at_ms,
      manifest: manifest.manifest,
    }),
    audit: {
      source: "bucephalus_runtime.run_manifests",
      row: manifest.manifest,
    },
  });
}

function metricDefinitionResource(
  run: CloudRunRecord,
  definition: RuntimeMetricDefinitionRecord,
  manifests: RuntimeRunManifestRecord[],
): RuntimeResourceRecord {
  const manifest = manifests.find((candidate) => candidate.experiment_id === definition.experiment_id);
  const observedAt = runtimeIsoFromMs(definition.updated_at_ms);
  return runtimeResource({
    kind: "MetricDefinition",
    name: metricDefinitionResourceName(definition),
    uid: metricDefinitionUid(definition),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": manifest?.core_run_id,
      "bucephalus.dev/experiment-id": definition.experiment_id,
      "bucephalus.dev/metric-id": definition.metric_id,
      "bucephalus.dev/semantic-key": definition.semantic_key,
      "bucephalus.dev/source-type": definition.source_type,
      "bucephalus.dev/direction": definition.direction,
      "bucephalus.dev/required": String(definition.required),
      "bucephalus.dev/primary-metric": String(definition.primary_metric),
    }),
    ownerReferences: [
      runOwnerReference(run),
      ...(manifest ? [runManifestOwnerReference(manifest)] : []),
    ],
    ...(observedAt ? { created_at: observedAt, updated_at: observedAt } : {}),
    spec: compactObject({
      experiment_id: definition.experiment_id,
      metric_id: definition.metric_id,
      semantic_key: definition.semantic_key,
      source_type: definition.source_type,
      source_pointer: definition.source_pointer,
    }),
    status: compactObject({
      phase: definition.primary_metric ? "Primary" : "Declared",
      label: definition.label,
      value_type: definition.value_type,
      unit: definition.unit,
      direction: definition.direction,
      required: definition.required,
      primary_metric: definition.primary_metric,
      observed_at: observedAt,
      updated_at_ms: definition.updated_at_ms,
      definition: definition.definition,
    }),
    audit: {
      source: "bucephalus_runtime.metric_definitions",
      row: definition.definition,
    },
  });
}

function slotCommitResource(
  run: CloudRunRecord,
  commit: RuntimeSlotCommitRecord,
): RuntimeResourceRecord {
  const trialId = stringField(commit.record.trial_id);
  const slotStatus = stringField(commit.record.slot_status);
  const recordedAt = stringField(commit.record.recorded_at) ?? runtimeIsoFromMs(commit.recorded_at_ms);
  return runtimeResource({
    kind: "SlotCommit",
    name: slotCommitResourceName(commit),
    uid: slotCommitUid(commit),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": commit.core_run_id,
      "bucephalus.dev/trial-id": trialId,
      "bucephalus.dev/schedule-idx": String(commit.schedule_idx),
      "bucephalus.dev/attempt": String(commit.attempt),
      "bucephalus.dev/record-type": commit.record_type,
      "bucephalus.dev/slot-commit-id": commit.slot_commit_id,
      "bucephalus.dev/slot-status": slotStatus,
    }),
    ownerReferences: [
      ...(trialId ? [trialOwnerReference(commit.core_run_id, trialId)] : []),
      runOwnerReference(run),
      scheduleSlotOwnerReference({
        core_run_id: commit.core_run_id,
        schedule_idx: commit.schedule_idx,
      }),
    ],
    ...(recordedAt ? { created_at: recordedAt, updated_at: recordedAt } : {}),
    spec: {
      core_run_id: commit.core_run_id,
      schedule_idx: commit.schedule_idx,
      attempt: commit.attempt,
      record_type: commit.record_type,
      slot_commit_id: commit.slot_commit_id,
    },
    status: compactObject({
      phase: slotCommitPhase(commit.record_type),
      trial_id: trialId,
      slot_status: slotStatus,
      recorded_at: recordedAt,
      recorded_at_ms: commit.recorded_at_ms,
      expected_rows: objectField(commit.record.expected_rows),
      written_rows: objectField(commit.record.written_rows),
      payload_digest: stringField(commit.record.payload_digest),
      facts_fsync_completed: booleanField(commit.record.facts_fsync_completed),
      runtime_fsync_completed: booleanField(commit.record.runtime_fsync_completed),
      record: commit.record,
    }),
    audit: {
      source: "bucephalus_runtime.slot_commit_records",
      row: commit.record,
    },
  });
}

function pendingTrialCompletionResource(
  run: CloudRunRecord,
  completion: RuntimePendingTrialCompletionRecord,
): RuntimeResourceRecord {
  const trialId = pendingTrialCompletionTrialId(completion);
  const slotStatus = stringField(completion.trial_result.slot_status);
  const observedAt = runtimeIsoFromMs(completion.updated_at_ms);
  return runtimeResource({
    kind: "PendingTrialCompletion",
    name: pendingTrialCompletionResourceName(completion),
    uid: pendingTrialCompletionUid(completion),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": completion.core_run_id,
      "bucephalus.dev/trial-id": trialId,
      "bucephalus.dev/schedule-idx": String(completion.schedule_idx),
      "bucephalus.dev/slot-status": slotStatus,
    }),
    ownerReferences: [
      ...(trialId ? [trialOwnerReference(completion.core_run_id, trialId)] : []),
      runOwnerReference(run),
      scheduleSlotOwnerReference({
        core_run_id: completion.core_run_id,
        schedule_idx: completion.schedule_idx,
      }),
    ],
    ...(observedAt ? { created_at: observedAt, updated_at: observedAt } : {}),
    spec: {
      core_run_id: completion.core_run_id,
      schedule_idx: completion.schedule_idx,
    },
    status: compactObject({
      phase: "Pending",
      trial_id: trialId,
      slot_status: slotStatus,
      observed_at: observedAt,
      updated_at_ms: completion.updated_at_ms,
      deferred_rows: pendingTrialCompletionDeferredRows(completion.trial_result),
      trial_result: completion.trial_result,
    }),
    audit: {
      source: "bucephalus_runtime.pending_trial_completions",
      row: {
        schema_version: "pending_trial_completion_v1",
        schedule_idx: completion.schedule_idx,
        trial_result: completion.trial_result,
      },
    },
  });
}

function variantSnapshotResource(
  run: CloudRunRecord,
  snapshot: RuntimeVariantSnapshotRecord,
): RuntimeResourceRecord {
  return runtimeResource({
    kind: "VariantSnapshot",
    name: variantSnapshotResourceName(snapshot),
    uid: variantSnapshotUid(snapshot),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": snapshot.core_run_id,
      "bucephalus.dev/trial-id": snapshot.trial_id,
      "bucephalus.dev/schedule-idx": String(snapshot.schedule_idx),
      "bucephalus.dev/attempt": String(snapshot.attempt),
      "bucephalus.dev/row-seq": String(snapshot.row_seq),
      "bucephalus.dev/slot-commit-id": snapshot.slot_commit_id,
      "bucephalus.dev/variant-id": snapshot.variant_id,
      "bucephalus.dev/baseline-id": snapshot.baseline_id,
      "bucephalus.dev/task-id": snapshot.task_id,
      "bucephalus.dev/binding-name": snapshot.binding_name,
    }),
    ownerReferences: [
      trialOwnerReference(snapshot.core_run_id, snapshot.trial_id),
      runOwnerReference(run),
      scheduleSlotOwnerReference(snapshot),
      slotCommitOwnerReference(snapshot),
    ],
    spec: {
      core_run_id: snapshot.core_run_id,
      trial_id: snapshot.trial_id,
      schedule_idx: snapshot.schedule_idx,
      attempt: snapshot.attempt,
      row_seq: snapshot.row_seq,
      slot_commit_id: snapshot.slot_commit_id,
      variant_id: snapshot.variant_id,
      baseline_id: snapshot.baseline_id,
      task_id: snapshot.task_id,
      repl_idx: snapshot.repl_idx,
      binding_name: snapshot.binding_name,
      binding_value: snapshot.binding_value,
    },
    status: {
      phase: "Committed",
      binding_value_text: snapshot.binding_value_text,
      row: snapshot.row,
    },
    audit: {
      source: "bucephalus_runtime.variant_snapshot_rows",
      row: snapshot.row,
    },
  });
}

function provenanceRowResource(
  run: CloudRunRecord,
  kind: RuntimeProvenanceResourceKind,
  record: RuntimeProvenanceRowRecord,
): RuntimeResourceRecord {
  const trialId = provenanceRowTrialId(record.row);
  const schemaVersion = provenanceRowString(record.row, "schema_version");
  const recordKind = provenanceRowRecordKind(record.row);
  const outcome = provenanceRowString(record.row, "outcome", "result.outcome", "conclusion.outcome");
  const status = provenanceRowString(record.row, "status", "slot_status", "result.status", "conclusion.status");
  return runtimeResource({
    kind,
    name: provenanceRowResourceName(kind, record),
    uid: provenanceRowUid(kind, record),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": record.core_run_id,
      "bucephalus.dev/trial-id": trialId,
      "bucephalus.dev/schedule-idx": String(record.schedule_idx),
      "bucephalus.dev/attempt": String(record.attempt),
      "bucephalus.dev/row-seq": String(record.row_seq),
      "bucephalus.dev/slot-commit-id": record.slot_commit_id,
      "bucephalus.dev/schema-version": schemaVersion,
      "bucephalus.dev/record-kind": recordKind,
      "bucephalus.dev/outcome": kind === "TrialConclusion" ? outcome : undefined,
      "bucephalus.dev/status": kind === "TrialConclusion" ? status : undefined,
    }),
    ownerReferences: [
      ...(trialId ? [trialOwnerReference(record.core_run_id, trialId)] : []),
      runOwnerReference(run),
      scheduleSlotOwnerReference(record),
      slotCommitOwnerReference(record),
    ],
    spec: {
      core_run_id: record.core_run_id,
      schedule_idx: record.schedule_idx,
      attempt: record.attempt,
      row_seq: record.row_seq,
      slot_commit_id: record.slot_commit_id,
    },
    status: compactObject({
      phase: "Committed",
      trial_id: trialId,
      schema_version: schemaVersion,
      record_kind: recordKind,
      outcome,
      status,
      row: record.row,
    }),
    audit: {
      source: provenanceRowAuditSource(kind),
      row: record.row,
    },
  });
}

function lineageVersionResource(
  run: CloudRunRecord,
  version: RuntimeLineageVersionRecord,
  heads: RuntimeLineageHeadRecord[],
): RuntimeResourceRecord {
  const currentHead = heads.some((head) =>
    head.core_run_id === version.core_run_id
    && head.chain_key === version.chain_key
    && head.latest_version_id === version.version_id);
  return runtimeResource({
    kind: "LineageVersion",
    name: lineageVersionResourceName(version),
    uid: lineageVersionUid(version),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": version.core_run_id,
      "bucephalus.dev/chain-key": version.chain_key,
      "bucephalus.dev/version-id": version.version_id,
      "bucephalus.dev/parent-version-id": version.parent_version_id,
      "bucephalus.dev/trial-id": version.trial_id,
      "bucephalus.dev/step-index": String(version.step_index),
    }),
    ownerReferences: [
      trialOwnerReference(version.core_run_id, version.trial_id),
      runOwnerReference(run),
    ],
    spec: compactObject({
      core_run_id: version.core_run_id,
      chain_key: version.chain_key,
      version_id: version.version_id,
      step_index: version.step_index,
      trial_id: version.trial_id,
      parent_version_id: version.parent_version_id,
    }),
    status: compactObject({
      phase: currentHead ? "Current" : "Recorded",
      current_head: currentHead,
      pre_snapshot_ref: version.pre_snapshot_ref,
      post_snapshot_ref: version.post_snapshot_ref,
      diff_incremental_ref: version.diff_incremental_ref,
      diff_cumulative_ref: version.diff_cumulative_ref,
      patch_incremental_ref: version.patch_incremental_ref,
      patch_cumulative_ref: version.patch_cumulative_ref,
      workspace_ref: version.workspace_ref,
      checkpoint_labels: version.checkpoint_labels,
    }),
    audit: {
      source: "bucephalus_runtime.lineage_versions",
    },
  });
}

function lineageHeadResource(
  run: CloudRunRecord,
  head: RuntimeLineageHeadRecord,
): RuntimeResourceRecord {
  return runtimeResource({
    kind: "LineageHead",
    name: lineageHeadResourceName(head),
    uid: lineageHeadUid(head),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": head.core_run_id,
      "bucephalus.dev/chain-key": head.chain_key,
      "bucephalus.dev/latest-version-id": head.latest_version_id,
      "bucephalus.dev/step-index": String(head.step_index),
    }),
    ownerReferences: [
      runOwnerReference(run),
    ],
    spec: {
      core_run_id: head.core_run_id,
      chain_key: head.chain_key,
    },
    status: compactObject({
      phase: "Current",
      latest_version_id: head.latest_version_id,
      step_index: head.step_index,
      latest_workspace_ref: head.latest_workspace_ref,
    }),
    audit: {
      source: "bucephalus_runtime.lineage_heads",
    },
  });
}

function trialContainerResource(
  run: CloudRunRecord,
  container: RuntimeTrialContainerRecord,
  slots: RuntimeSlotRecord[],
  attempts: RuntimeRunAttemptRecord[],
): RuntimeResourceRecord {
  const slot = runtimeSlotForContainer(slots, container);
  const binding = runtimeRunnerBindingForWorker(attempts, slot?.worker_id);
  const scheduleSlotName = slot ? `${resourceName(slot.core_run_id)}.${slot.schedule_idx}` : undefined;
  return runtimeResource({
    kind: "TrialContainer",
    name: [
      resourceName(container.trial_id),
      resourceName(container.role),
      shortResourceName(container.container_id),
    ].join("."),
    uid: container.container_id,
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": container.core_run_id,
      "bucephalus.dev/trial-id": container.trial_id,
      "bucephalus.dev/container-role": container.role,
      "bucephalus.dev/worker-id": binding.workerId,
      "bucephalus.dev/attempt-id": binding.attempt?.attempt_id,
      "bucephalus.dev/runner-instance-id": binding.attempt?.runner_instance_id,
      "bucephalus.dev/runner-pool-id": binding.attempt?.runner_pool_id,
    }),
    ownerReferences: [
      trialOwnerReference(container.core_run_id, container.trial_id),
      runOwnerReference(run),
      ...(slot ? [scheduleSlotOwnerReference(slot)] : []),
      ...runtimeRunnerBindingOwnerReferences(binding),
    ],
    spec: compactObject({
      core_run_id: container.core_run_id,
      trial_id: container.trial_id,
      schedule_idx: container.schedule_idx,
      attempt: container.attempt,
      role: container.role,
      container_id: container.container_id,
      image: container.image,
      workdir: container.workdir,
    }),
    status: compactObject({
      phase: container.status,
      updated_at_ms: container.updated_at_ms,
      schedule_slot: scheduleSlotName,
      worker_id: binding.workerId,
      runner_binding: runtimeRunnerBindingStatus(binding),
    }),
    audit: {
      source: "bucephalus_runtime.trial_attempt_containers",
    },
  });
}

function trialStageResource(
  run: CloudRunRecord,
  stage: RuntimeContractStageRecord,
): RuntimeResourceRecord {
  return runtimeResource({
    kind: "TrialStage",
    name: trialStageResourceName(stage),
    uid: trialStageUid(stage),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": stage.core_run_id,
      "bucephalus.dev/trial-id": stage.trial_id,
      "bucephalus.dev/stage": stage.stage,
      "bucephalus.dev/attempt": String(stage.attempt),
      "bucephalus.dev/variant-id": stage.variant_id,
      "bucephalus.dev/task-id": stage.task_id,
    }),
    ownerReferences: [
      trialOwnerReference(stage.core_run_id, stage.trial_id),
      runOwnerReference(run),
    ],
    ...(stage.recorded_at ? { created_at: stage.recorded_at, updated_at: stage.recorded_at } : {}),
    spec: compactObject({
      core_run_id: stage.core_run_id,
      trial_id: stage.trial_id,
      schedule_idx: stage.schedule_idx,
      attempt: stage.attempt,
      row_seq: stage.row_seq,
      variant_id: stage.variant_id,
      task_id: stage.task_id,
      repl_idx: stage.repl_idx,
      stage: stage.stage,
    }),
    status: compactObject({
      phase: stage.status,
      status: stage.status,
      recorded_at: stage.recorded_at,
      detail: stage.detail,
    }),
    audit: compactObject({
      source: "bucephalus_runtime.contract_stage_rows",
      observed_at: stage.recorded_at,
      row: stage.row,
    }),
  });
}

function runtimeValueResource(
  run: CloudRunRecord,
  value: RuntimeValueRecord,
): RuntimeResourceRecord {
  return runtimeResource({
    kind: "RuntimeValue",
    name: runtimeValueResourceName(value),
    uid: runtimeValueUid(value),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": value.core_run_id,
      "bucephalus.dev/runtime-key": value.key,
      "bucephalus.dev/value-source": value.source,
    }),
    ownerReferences: [runOwnerReference(run)],
    ...(value.observed_at ? { created_at: value.observed_at, updated_at: value.observed_at } : {}),
    spec: compactObject({
      core_run_id: value.core_run_id,
      key: value.key,
    }),
    status: compactObject({
      phase: "Current",
      source: value.source,
      observed_at: value.observed_at,
      updated_at_ms: value.updated_at_ms,
      value_summary: runtimeValueSummary(value.value),
      value: value.value,
    }),
    audit: compactObject({
      source: value.source,
      snapshot_seq: value.snapshot_seq,
      row: value.row,
    }),
  });
}

function metricObservationResource(
  run: CloudRunRecord,
  observation: RuntimeMetricObservationRecord,
): RuntimeResourceRecord {
  return runtimeResource({
    kind: "MetricObservation",
    name: metricObservationResourceName(observation),
    uid: metricObservationUid(observation),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": observation.core_run_id,
      "bucephalus.dev/trial-id": observation.trial_id,
      "bucephalus.dev/metric-name": observation.metric_name,
      "bucephalus.dev/metric-source": observation.metric_source,
      "bucephalus.dev/attempt": String(observation.attempt),
      "bucephalus.dev/variant-id": observation.variant_id,
      "bucephalus.dev/task-id": observation.task_id,
    }),
    ownerReferences: [
      trialOwnerReference(observation.core_run_id, observation.trial_id),
      runOwnerReference(run),
    ],
    spec: compactObject({
      core_run_id: observation.core_run_id,
      trial_id: observation.trial_id,
      schedule_idx: observation.schedule_idx,
      attempt: observation.attempt,
      row_seq: observation.row_seq,
      variant_id: observation.variant_id,
      task_id: observation.task_id,
      repl_idx: observation.repl_idx,
      outcome: observation.outcome,
      metric_name: observation.metric_name,
    }),
    status: compactObject({
      phase: "observed",
      metric_value: observation.metric_value,
      metric_source: observation.metric_source,
    }),
    audit: compactObject({
      source: observation.metric_source ?? "bucephalus_runtime.metric_rows",
      row: observation.row,
    }),
  });
}

function performanceSampleResource(
  run: CloudRunRecord,
  sample: RuntimePerformanceSampleRecord,
): RuntimeResourceRecord {
  const recordedAt = runtimeIsoFromMs(sample.recorded_at_ms);
  return runtimeResource({
    kind: "PerformanceSample",
    name: performanceSampleResourceName(sample),
    uid: performanceSampleUid(sample),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": sample.core_run_id,
      "bucephalus.dev/trial-id": sample.trial_id,
      "bucephalus.dev/sample-kind": sample.sample_kind,
      "bucephalus.dev/stage": sample.stage,
      "bucephalus.dev/attempt": sample.attempt === null ? undefined : String(sample.attempt),
    }),
    ownerReferences: [
      ...(sample.trial_id ? [trialOwnerReference(sample.core_run_id, sample.trial_id)] : []),
      runOwnerReference(run),
    ],
    ...(recordedAt ? { created_at: recordedAt, updated_at: recordedAt } : {}),
    spec: compactObject({
      core_run_id: sample.core_run_id,
      sample_id: sample.sample_id,
      trial_id: sample.trial_id,
      schedule_idx: sample.schedule_idx,
      attempt: sample.attempt,
      sample_seq: sample.sample_seq,
      sample_kind: sample.sample_kind,
      stage: sample.stage,
    }),
    status: compactObject({
      phase: "Recorded",
      duration_ms: sample.duration_ms,
      process_rss_kb: sample.process_rss_kb,
      recorded_at_ms: sample.recorded_at_ms,
      payload: sample.payload,
    }),
    audit: {
      source: "bucephalus_runtime.performance_samples",
      row: sample.payload,
    },
  });
}

function runtimeOperationResource(
  run: CloudRunRecord,
  operation: RuntimeOperationRecord,
): RuntimeResourceRecord {
  const observedAt = runtimeIsoFromMs(operation.updated_at_ms);
  const trialId = stringField(operation.payload.trial_id);
  const parentTrialId = stringField(operation.payload.parent_trial_id);
  return runtimeResource({
    kind: "RuntimeOperation",
    name: runtimeOperationResourceName(operation),
    uid: runtimeOperationUid(operation),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": operation.core_run_id,
      "bucephalus.dev/op-kind": operation.op_kind,
      "bucephalus.dev/op-id": operation.op_id,
      "bucephalus.dev/trial-id": trialId,
      "bucephalus.dev/parent-trial-id": parentTrialId,
    }),
    ownerReferences: [
      ...(trialId ? [trialOwnerReference(operation.core_run_id, trialId)] : []),
      runOwnerReference(run),
    ],
    ...(observedAt ? { created_at: observedAt, updated_at: observedAt } : {}),
    spec: {
      core_run_id: operation.core_run_id,
      op_kind: operation.op_kind,
      op_id: operation.op_id,
    },
    status: compactObject({
      phase: "Recorded",
      operation: stringField(operation.payload.operation) ?? operation.op_kind,
      trial_id: trialId,
      parent_trial_id: parentTrialId,
      strict: typeof operation.payload.strict === "boolean" ? operation.payload.strict : undefined,
      replay_grade: stringField(operation.payload.replay_grade),
      integration_level: stringField(operation.payload.integration_level),
      observed_at: observedAt,
      updated_at_ms: operation.updated_at_ms,
      payload: operation.payload,
    }),
    audit: {
      source: "bucephalus_runtime.runtime_ops",
      row: operation.payload,
    },
  });
}

function trialArtifactResource(
  run: CloudRunRecord,
  object: RuntimeAttemptObjectRecord,
): RuntimeResourceRecord {
  const observedAt = runtimeIsoFromMs(object.recorded_at_ms);
  return runtimeResource({
    kind: "TrialArtifact",
    name: trialArtifactResourceName(object),
    uid: trialArtifactUid(object),
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": object.core_run_id,
      "bucephalus.dev/trial-id": object.trial_id,
      "bucephalus.dev/artifact-role": object.role,
      "bucephalus.dev/attempt": String(object.attempt),
      "bucephalus.dev/sha256": object.sha256,
    }),
    ownerReferences: [
      trialOwnerReference(object.core_run_id, object.trial_id),
      runOwnerReference(run),
    ],
    ...(observedAt ? { created_at: observedAt, updated_at: observedAt } : {}),
    spec: compactObject({
      core_run_id: object.core_run_id,
      trial_id: object.trial_id,
      schedule_idx: object.schedule_idx,
      attempt: object.attempt,
      role: object.role,
      object_ref: object.object_ref,
      metadata: object.metadata,
    }),
    status: compactObject({
      phase: "recorded",
      content_available: object.content_available,
      media_type: object.media_type,
      byte_size: object.byte_size,
      sha256: object.sha256,
      relative_path: object.relative_path,
      recorded_at_ms: object.recorded_at_ms,
    }),
    audit: compactObject({
      source: "bucephalus_runtime.attempt_objects",
      observed_at: observedAt,
    }),
  });
}

function runtimeRunnerBindingForWorker(
  attempts: RuntimeRunAttemptRecord[],
  workerId: string | null | undefined,
): RuntimeRunnerBinding {
  const normalizedWorkerId = normalizeOptionalString(workerId);
  if (!normalizedWorkerId) {
    return {};
  }
  const workerAttempts = attempts.filter((attempt) => attempt.worker_id === normalizedWorkerId);
  if (workerAttempts.length === 0) {
    return { workerId: normalizedWorkerId };
  }
  return {
    workerId: normalizedWorkerId,
    attempt: workerAttempts.sort((left, right) => {
      const leftActive = left.status === "running" && !left.ended_at ? 1 : 0;
      const rightActive = right.status === "running" && !right.ended_at ? 1 : 0;
      if (leftActive !== rightActive) {
        return rightActive - leftActive;
      }
      return runtimeAttemptObservedAt(right) - runtimeAttemptObservedAt(left);
    })[0],
  };
}

function runtimeAttemptObservedAt(attempt: RuntimeRunAttemptRecord): number {
  return Date.parse(attempt.updated_at ?? attempt.heartbeat_at ?? attempt.started_at ?? attempt.created_at);
}

function runtimeRunnerBindingStatus(binding: RuntimeRunnerBinding): JsonObject | undefined {
  const access = binding.attempt
    ? runtimeAttemptRunnerReachable(binding.attempt)
      ? runtimeAccessCapabilities(normalizeRuntimeCapabilities(binding.attempt.runner_instance_capabilities))
      : { port_forward: false, exec: false }
    : undefined;
  const status = compactObject({
    worker_id: binding.workerId,
    attempt_id: binding.attempt?.attempt_id,
    attempt_phase: binding.attempt?.status,
    runner_instance_id: binding.attempt?.runner_instance_id,
    runner_instance_status: binding.attempt?.runner_instance_status,
    runner_pool_id: binding.attempt?.runner_pool_id,
    access,
  });
  return Object.keys(status).length ? status : undefined;
}

function runtimeAccessCapabilities(capabilities: WorkerCapabilities): JsonObject {
  return {
    port_forward: capabilities.resources.includes("runtime_port_forward"),
    exec: capabilities.resources.includes("runtime_exec"),
  };
}

function runtimeRunAccessStatus(attempts: RuntimeRunAttemptRecord[]): JsonObject {
  const activeAttempts = attempts.filter(runtimeAttemptIsActive);
  const reachableAttempts = activeAttempts.filter(runtimeAttemptRunnerReachable);
  const capabilities = reachableAttempts.map((attempt) => normalizeRuntimeCapabilities(attempt.runner_instance_capabilities));
  const selectedAttempt = reachableAttempts.find((attempt) => {
    const attemptCapabilities = normalizeRuntimeCapabilities(attempt.runner_instance_capabilities);
    return attemptCapabilities.resources.includes("runtime_port_forward")
      || attemptCapabilities.resources.includes("runtime_exec");
  }) ?? reachableAttempts[0] ?? activeAttempts[0];
  return compactObject({
    port_forward: capabilities.some((capability) => capability.resources.includes("runtime_port_forward")),
    exec: capabilities.some((capability) => capability.resources.includes("runtime_exec")),
    runner_instance_id: selectedAttempt?.runner_instance_id,
    runner_instance_status: selectedAttempt?.runner_instance_status,
    attempt_id: selectedAttempt?.attempt_id,
    worker_id: selectedAttempt?.worker_id,
  });
}

function runtimeAttemptIsActive(attempt: RuntimeRunAttemptRecord): boolean {
  return attempt.status.toLowerCase() === "running" && !attempt.ended_at;
}

function runtimeAttemptRunnerReachable(attempt: RuntimeRunAttemptRecord): boolean {
  return runtimeRunnerStatusAllowsAccess(runtimeAttemptRunnerStatus(attempt));
}

function runtimeAttemptRunnerStatus(attempt: RuntimeRunAttemptRecord): string | null {
  return normalizeOptionalString(attempt.runner_instance_status)?.toLowerCase() ?? null;
}

function runtimeRunnerStatusAllowsAccess(status: string | null | undefined): boolean {
  return Boolean(status && RUNTIME_REACHABLE_RUNNER_STATUSES.has(status.toLowerCase()));
}

function runtimeRunnerBindingOwnerReferences(
  binding: RuntimeRunnerBinding,
): RuntimeResourceRecord["metadata"]["ownerReferences"] {
  return [
    ...(binding.attempt?.runner_pool_id ? [runnerPoolOwnerReference(binding.attempt.runner_pool_id)] : []),
    ...(binding.attempt?.runner_instance_id ? [runnerInstanceOwnerReference(binding.attempt.runner_instance_id)] : []),
    ...(binding.attempt?.attempt_id ? [runnerAttemptOwnerReference(binding.attempt.attempt_id)] : []),
  ];
}

function runtimeSlotForContainer(
  slots: RuntimeSlotRecord[],
  container: RuntimeTrialContainerRecord,
): RuntimeSlotRecord | undefined {
  return slots.find((slot) =>
    slot.core_run_id === container.core_run_id
      && slot.schedule_idx === container.schedule_idx
      && slot.trial_id === container.trial_id
      && slot.attempt === container.attempt
  );
}

function runtimeSlotForTrial(
  slots: RuntimeSlotRecord[],
  trial: Pick<RuntimeTrialRecord, "core_run_id" | "trial_id" | "attempt" | "schedule_idx">,
): RuntimeSlotRecord | undefined {
  return slots.find((slot) =>
    slot.core_run_id === trial.core_run_id
      && slot.schedule_idx === trial.schedule_idx
      && slot.trial_id === trial.trial_id
      && slot.attempt === trial.attempt
  );
}

function runtimeSnapshotResource(
  run: CloudRunRecord,
  snapshot: RuntimeSnapshotRecord,
): RuntimeResourceRecord {
  return runtimeResource({
    kind: "RuntimeSnapshot",
    name: `${resourceName(snapshot.core_run_id)}.${snapshot.seq ?? "worker"}`,
    labels: {
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": snapshot.core_run_id,
    },
    ownerReferences: [runOwnerReference(run)],
    created_at: snapshot.created_at ?? null,
    spec: {
      core_run_id: snapshot.core_run_id,
      run_dir_name: snapshot.run_dir_name,
      runtime_value_keys: Object.keys(snapshot.runtime_values).sort(),
      omitted: snapshot.omitted,
    },
    status: {
      phase: "Reported",
      trial_summaries: snapshot.trial_summaries.length,
      evidence_records: snapshot.evidence_records.length,
    },
    audit: compactObject({
      source: "cloud.run_events",
      event_type: "worker.runtime.snapshot",
      event_seq: snapshot.seq,
      observed_at: snapshot.created_at,
    }),
  });
}

function runtimeEventResources(
  run: CloudRunRecord,
  events: RuntimeEventRecord[],
): RuntimeResourceRecord[] {
  return events.map((event) => runtimeEventResource(run, event));
}

function runtimeEventResource(
  run: CloudRunRecord,
  event: RuntimeEventRecord,
): RuntimeResourceRecord {
  const refs = runtimeEventResourceRefs(event);
  const primaryRef = runtimeEventPrimaryResourceRef(event, refs);
  const involvedObject = runtimeEventInvolvedObject(primaryRef);
  const involved = runtimeEventInvolvedSummary(primaryRef);
  const eventUid = runtimeEventUid(event);
  return runtimeResource({
    kind: "Event",
    name: runtimeEventResourceName(event),
    uid: eventUid,
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/core-run-id": event.core_run_id,
      "bucephalus.dev/trial-id": event.trial_id,
      "bucephalus.dev/task-id": event.task_id,
      "bucephalus.dev/event-type": event.event_type,
      "bucephalus.dev/event-source": event.source,
      "bucephalus.dev/resource-kind": primaryRef?.kind ?? null,
      "bucephalus.dev/resource-name": primaryRef?.name ?? null,
    }),
    annotations: compactStringRecord({
      "bucephalus.dev/event-uid": eventUid,
      "bucephalus.dev/event-type": event.event_type,
      "bucephalus.dev/event-source": event.source,
    }),
    ownerReferences: runtimeEventOwnerReferences(run, event),
    created_at: event.ts,
    updated_at: event.ts,
    spec: compactObject({
      event_uid: eventUid,
      event_type: event.event_type,
      source: event.source,
      event_time: event.ts,
      core_run_id: event.core_run_id,
      trial_id: event.trial_id,
      schedule_idx: event.schedule_idx >= 0 ? event.schedule_idx : undefined,
      attempt: event.attempt,
      row_seq: event.row_seq,
      seq: event.seq,
      slot_commit_id: event.slot_commit_id,
      variant_id: event.variant_id,
      task_id: event.task_id,
      repl_idx: event.repl_idx,
      involved_object: involvedObject,
      involved_resources: runtimeEventResourceRefsForSpec(event),
      resource_refs: runtimeEventResourceRefsForSpec(event),
      payload: event.payload,
      row: event.row,
    }),
    status: compactObject({
      phase: "recorded",
      reason: runtimeEventReason(event),
      message: runtimeEventMessage(event),
      involved,
      involved_object: involvedObject,
      involved_kind: primaryRef?.kind,
      involved_name: primaryRef?.name,
      involved_uid: primaryRef?.uid,
      involved_count: refs.filter((ref) => ref.kind && ref.name).length,
      event_time: event.ts,
      observed_at: event.ts,
    }),
    audit: compactObject({
      source: event.source,
      event_type: event.event_type,
      row_seq: event.row_seq,
      seq: event.seq,
      observed_at: event.ts,
    }),
  });
}

function runtimeEventUid(event: RuntimeEventRecord): string {
  return sha256Digest(canonicalJsonStringify({
    source: event.source,
    core_run_id: event.core_run_id,
    trial_id: event.trial_id,
    schedule_idx: event.schedule_idx,
    attempt: event.attempt,
    row_seq: event.row_seq,
    seq: event.seq,
    event_type: event.event_type,
    ts: event.ts,
    payload: event.payload,
    row: event.row,
  } as JsonObject));
}

function runtimeEventResourceName(event: RuntimeEventRecord): string {
  const suffix = runtimeEventUid(event).slice("sha256:".length, "sha256:".length + 12);
  const sequence = event.row_seq > 0 ? event.row_seq : event.seq > 0 ? event.seq : 0;
  return resourceName(`event-${event.source}-${event.event_type}-${sequence}-${suffix}`);
}

function runtimeEventOwnerReferences(
  run: CloudRunRecord,
  event: RuntimeEventRecord,
): RuntimeResourceRecord["metadata"]["ownerReferences"] {
  return uniqueOwnerReferences([
    runOwnerReference(run),
    ...runtimeEventResourceRefs(event).flatMap((ref) => {
      if (!ref.kind || !ref.name) {
        return [];
      }
      const ownerReference: RuntimeResourceRecord["metadata"]["ownerReferences"][number] = {
        apiVersion: RUNTIME_API_VERSION,
        kind: ref.kind,
        name: ref.name,
        ...(ref.uid ? { uid: ref.uid } : {}),
      };
      return [ownerReference];
    }),
  ]);
}

function runtimeEventResourceRefsForSpec(event: RuntimeEventRecord): JsonObject[] {
  return runtimeEventPrimaryFirstResourceRefs(event).map((ref) => compactObject({
    apiVersion: RUNTIME_API_VERSION,
    kind: ref.kind,
    name: ref.name,
    uid: ref.uid,
  }));
}

function runtimeEventPrimaryFirstResourceRefs(event: RuntimeEventRecord): Array<{ kind: string | null; name: string | null; uid: string | null }> {
  const refs = runtimeEventResourceRefs(event);
  const primaryRef = runtimeEventPrimaryResourceRef(event, refs);
  return primaryRef ? uniqueRuntimeEventResourceRefs([primaryRef, ...refs]) : refs;
}

function runtimeEventPrimaryResourceRef(
  event: RuntimeEventRecord,
  refs: Array<{ kind: string | null; name: string | null; uid: string | null }>,
): { kind: string | null; name: string | null; uid: string | null } | undefined {
  const payload = isRecord(event.payload) ? event.payload : {};
  const explicitRefs = [
    runtimeEventResourceRefFromObject(isRecord(payload.resource_ref) ? payload.resource_ref : null),
    runtimeEventResourceRefFromObject(isRecord(payload.access_resource_ref) ? payload.access_resource_ref : null),
    runtimeEventResourceRefFromObject(isRecord(payload.resolved_target) ? payload.resolved_target : null),
    runtimeEventResourceRefFromObject(isRecord(payload.target_ref) ? payload.target_ref : null),
  ];
  return uniqueRuntimeEventResourceRefs(explicitRefs).find((ref) => ref.kind && ref.name)
    ?? refs.find((ref) => ref.kind && ref.name);
}

function runtimeEventInvolvedObject(ref: { kind: string | null; name: string | null; uid: string | null } | undefined): JsonObject | undefined {
  if (!ref?.kind || !ref.name) {
    return undefined;
  }
  return compactObject({
    apiVersion: RUNTIME_API_VERSION,
    kind: ref.kind,
    name: ref.name,
    uid: ref.uid,
  });
}

function runtimeEventInvolvedSummary(ref: { kind: string | null; name: string | null; uid: string | null } | undefined): string | undefined {
  if (!ref?.kind || !ref.name) {
    return undefined;
  }
  return `${ref.kind}/${ref.name}`;
}

function runtimeEventReason(event: RuntimeEventRecord): string {
  const reason = event.event_type
    .split(/[._:/-]+/)
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => part.slice(0, 1).toUpperCase() + part.slice(1))
    .join("");
  return reason || "Recorded";
}

function runtimeEventMessage(event: RuntimeEventRecord): string {
  return stringField(event.payload.message)
    ?? stringField(event.payload.error_message)
    ?? stringField(event.payload.error)
    ?? stringField(event.payload.reason)
    ?? stringField(event.payload.status)
    ?? `Recorded ${event.event_type}`;
}

function accessRequestResource(
  run: CloudRunRecord,
  request: RuntimeAccessRequestRecord,
): RuntimeResourceRecord {
  const isExec = request.kind === "exec";
  const targetRef = accessRequestTargetRef(request);
  return runtimeResource({
    kind: isExec ? "Exec" : "PortForward",
    name: resourceName(request.access_request_id),
    uid: request.access_request_id,
    labels: compactStringRecord({
      "bucephalus.dev/run-id": run.run_id,
      "bucephalus.dev/runner-instance-id": request.runner_instance_id,
      "bucephalus.dev/attempt-id": request.attempt_id,
      "bucephalus.dev/worker-id": request.worker_id,
      "bucephalus.dev/resource-kind": request.resource_kind,
      "bucephalus.dev/resource-name": request.resource_name,
    }),
    ownerReferences: accessRequestOwnerReferences(run, request),
    created_at: request.created_at,
    updated_at: request.updated_at,
    spec: compactObject({
      access_request_id: request.access_request_id,
      resource_kind: request.resource_kind,
      resource_name: request.resource_name,
      target_ref: targetRef,
      protocol: request.protocol,
      target_port: isExec ? undefined : request.target_port,
      local_port: isExec ? undefined : request.local_port,
      command: isExec ? request.command : undefined,
      reason: request.reason,
    }),
    status: compactObject({
      phase: request.status,
      runner_instance_id: request.runner_instance_id,
      attempt_id: request.attempt_id,
      worker_id: request.worker_id,
      runner_binding: accessRequestRunnerBinding(request),
      connection: request.connection,
      error_message: request.error_message,
      expires_at: request.expires_at,
    }),
    audit: compactObject({
      source: "cloud.runtime_access_requests",
      requester: request.requester,
      target_ref: targetRef,
      target_uid: request.target_uid,
      target_resource_version: request.target_resource_version,
      runner_binding: accessRequestRunnerBinding(request),
      observed_at: request.updated_at,
    }),
  });
}

function accessRequestTargetRef(request: RuntimeAccessRequestRecord): JsonObject | undefined {
  if (!request.resource_kind || !request.resource_name) {
    return undefined;
  }
  return compactObject({
    apiVersion: RUNTIME_API_VERSION,
    kind: request.resource_kind,
    name: request.resource_name,
    uid: request.target_uid ?? undefined,
    resourceVersion: request.target_resource_version ?? undefined,
  });
}

function accessRequestRunnerBinding(request: RuntimeAccessRequestRecord): JsonObject | undefined {
  const binding = compactObject({
    runner_instance_id: request.runner_instance_id,
    attempt_id: request.attempt_id,
    worker_id: request.worker_id,
  });
  return Object.keys(binding).length ? binding : undefined;
}

function accessRequestResourceRef(request: RuntimeAccessRequestRecord): JsonObject {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: request.kind === "exec" ? "Exec" : "PortForward",
    name: resourceName(request.access_request_id),
    uid: request.access_request_id,
  };
}

function accessRequestOwnerReferences(
  run: CloudRunRecord,
  request: RuntimeAccessRequestRecord,
): RuntimeResourceRecord["metadata"]["ownerReferences"] {
  return uniqueOwnerReferences([
    runOwnerReference(run),
    ...(request.resource_kind && request.resource_name
      ? [targetResourceOwnerReference(request.resource_kind, request.resource_name)]
      : []),
    ...(request.runner_instance_id ? [runnerInstanceOwnerReference(request.runner_instance_id)] : []),
    ...(request.attempt_id ? [runnerAttemptOwnerReference(request.attempt_id)] : []),
  ]);
}

function targetResourceOwnerReference(
  kind: string,
  name: string,
): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind,
    name,
  };
}

function runtimeResourceEventRef(resource: RuntimeResourceRecord): JsonObject {
  return compactObject({
    apiVersion: resource.apiVersion,
    kind: resource.kind,
    name: resource.metadata.name,
    uid: resource.metadata.uid,
  });
}

function uniqueOwnerReferences(
  references: RuntimeResourceRecord["metadata"]["ownerReferences"],
): RuntimeResourceRecord["metadata"]["ownerReferences"] {
  const byIdentity = new Map<string, RuntimeResourceRecord["metadata"]["ownerReferences"][number]>();
  const order: string[] = [];
  for (const reference of references) {
    const key = `${reference.apiVersion}:${reference.kind}:${reference.name}`;
    const existing = byIdentity.get(key);
    if (!existing) {
      byIdentity.set(key, reference);
      order.push(key);
      continue;
    }
    if (!existing.uid && reference.uid) {
      byIdentity.set(key, reference);
    }
  }
  return order
    .map((key) => byIdentity.get(key))
    .filter((reference): reference is RuntimeResourceRecord["metadata"]["ownerReferences"][number] => Boolean(reference));
}

function relatedRuntimeResources(
  resources: RuntimeResourceRecord[],
  resource: RuntimeResourceRecord,
): RuntimeRelatedResourceRecord[] {
  const related = new Map<string, RuntimeRelatedResourceRecord>();
  const ownerReferences = Array.isArray(resource.metadata.ownerReferences) ? resource.metadata.ownerReferences : [];
  for (const ownerRef of ownerReferences) {
    const owner = resources.find((item) => ownerReferenceMatchesResource(ownerRef, item));
    if (owner && resourceIdentity(owner) !== resourceIdentity(resource)) {
      related.set(`owner:${resourceIdentity(owner)}`, {
        relationship: "owner",
        resource: owner,
      });
    }
  }
  for (const candidate of resources) {
    if (resourceIdentity(candidate) === resourceIdentity(resource)) {
      continue;
    }
    const candidateOwnerReferences = Array.isArray(candidate.metadata.ownerReferences) ? candidate.metadata.ownerReferences : [];
    if (candidateOwnerReferences.some((ownerRef) => ownerReferenceMatchesResource(ownerRef, resource))) {
      related.set(`dependent:${resourceIdentity(candidate)}`, {
        relationship: "dependent",
        resource: candidate,
      });
    }
  }
  return [...related.values()].sort((left, right) =>
    relatedResourceRank(left.relationship) - relatedResourceRank(right.relationship)
      || left.resource.kind.localeCompare(right.resource.kind)
      || left.resource.metadata.name.localeCompare(right.resource.metadata.name)
  );
}

function runtimeResourceStatusView(
  cloudRunId: string,
  coreRunIds: string[],
  resource: RuntimeResourceRecord,
): RuntimeResourceStatus {
  const conditions = Array.isArray(resource.status.conditions)
    ? resource.status.conditions.filter(runtimeResourceConditionRecord)
    : [];
  const actions = runtimeResourceAdvertisedActions(resource);
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RuntimeResourceStatus",
    cloud_run_id: cloudRunId,
    generated_at: new Date().toISOString(),
    core_run_ids: coreRunIds,
    resource_ref: runtimeResourceWatchRef(resource),
    generation: resource.metadata.generation,
    observedGeneration: numberField(resource.status.observedGeneration) ?? resource.metadata.generation,
    resourceVersion: resource.metadata.resourceVersion,
    deletionTimestamp: stringField(resource.metadata.deletionTimestamp),
    phase: stringField(resource.status.phase),
    reason: stringField(resource.status.reason),
    message: stringField(resource.status.message) ?? stringField(resource.status.error_message),
    conditions,
    actions,
    status: resource.status,
    audit: resource.audit,
  };
}

export function runtimeResourceMetricsView(
  cloudRunId: string,
  coreRunIds: string[],
  resource: RuntimeResourceRecord,
  events: RuntimeEventRecord[] = [],
): RuntimeResourceMetrics {
  const metrics: RuntimeResourceMetric[] = [];
  const usedNames = new Set<string>();
  const addMetric = (
    source: RuntimeResourceMetric["source"],
    name: string,
    value: number | null | undefined,
    description: string,
    labels: JsonObject = {},
    unit: string | null = runtimeMetricUnit(name),
  ) => {
    if (value === null || value === undefined || !Number.isFinite(value)) return;
    const metricName = uniqueRuntimeMetricName(usedNames, name);
    usedNames.add(metricName);
    metrics.push({
      name: metricName,
      value,
      unit,
      source,
      description,
      labels,
    });
  };

  const conditions = Array.isArray(resource.status.conditions)
    ? resource.status.conditions.filter(runtimeResourceConditionRecord)
    : [];
  const actions = runtimeResourceAdvertisedActions(resource);
  const observed = runtimeResourceObservedState(resource);
  addMetric("lifecycle", "lifecycle.generation", numberField(resource.metadata.generation), "Metadata generation for the selected runtime resource.", {}, "generation");
  addMetric("lifecycle", "lifecycle.observed_generation", numberField(resource.status.observedGeneration), "Status observedGeneration for freshness comparison.", {}, "generation");
  addMetric("lifecycle", "lifecycle.observed", observed === null ? undefined : observed ? 1 : 0, "Whether status.observedGeneration has caught up to metadata.generation.", {}, "boolean");
  addMetric("lifecycle", "lifecycle.actions_available", actions.length, "Number of lifecycle/action subresources currently advertised by status.actions.", {}, "count");
  addMetric("condition", "conditions.total", conditions.length, "Total condition rows on the selected runtime resource.", {}, "count");
  addMetric("condition", "conditions.true", conditions.filter((condition) => condition.status === "True").length, "Runtime conditions currently True.", {}, "count");
  addMetric("condition", "conditions.false", conditions.filter((condition) => condition.status === "False").length, "Runtime conditions currently False.", {}, "count");
  addMetric("condition", "conditions.unknown", conditions.filter((condition) => condition.status === "Unknown").length, "Runtime conditions currently Unknown.", {}, "count");
  for (const condition of conditions) {
    addMetric(
      "condition",
      `conditions.by_type.${runtimeMetricPathSegment(condition.type)}`,
      condition.status === "True" ? 1 : 0,
      `Whether the ${condition.type} condition is currently True.`,
      {
        type: condition.type,
        status: condition.status,
        reason: condition.reason,
      },
      "boolean",
    );
  }

  const access = isRecord(resource.status.access) ? resource.status.access : {};
  if (Object.keys(access).length > 0) {
    addMetric("access", "access.reachable", access.reachable === true ? 1 : 0, "Whether the selected resource is currently reachable for low-level runtime access.", {}, "boolean");
    addMetric("access", "access.port_forward_ready", access.port_forward === true ? 1 : 0, "Whether the bound runner advertises runtime_port_forward for this resource.", {}, "boolean");
    addMetric("access", "access.exec_ready", access.exec === true ? 1 : 0, "Whether the bound runner advertises runtime_exec for this resource.", {}, "boolean");
  }

  addMetric("event", "events.total", events.length, "Number of recent runtime events related to this resource.", {}, "count");
  for (const [eventType, count] of Object.entries(countStrings(events.map((event) => event.event_type)))) {
    addMetric("event", `events.by_type.${runtimeMetricPathSegment(eventType)}`, count, "Recent related runtime events grouped by event type.", { event_type: eventType }, "count");
  }
  for (const [source, count] of Object.entries(countStrings(events.map((event) => event.source)))) {
    addMetric("event", `events.by_source.${runtimeMetricPathSegment(source)}`, count, "Recent related runtime events grouped by source.", { source }, "count");
  }

  for (const metric of runtimeNumericLeafMetrics("spec", resource.spec, "spec")) {
    addMetric("spec", metric.name, metric.value, metric.description, metric.labels, metric.unit);
  }
  for (const metric of runtimeNumericLeafMetrics("status", resource.status, "status")) {
    addMetric("status", metric.name, metric.value, metric.description, metric.labels, metric.unit);
  }

  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RuntimeResourceMetrics",
    cloud_run_id: cloudRunId,
    generated_at: new Date().toISOString(),
    core_run_ids: coreRunIds,
    resource_ref: runtimeResourceWatchRef(resource),
    resource_version: stringField(resource.metadata.resourceVersion),
    phase: stringField(resource.status.phase),
    summary: {
      metrics_total: metrics.length,
      lifecycle_metrics: metrics.filter((metric) => metric.source === "lifecycle").length,
      condition_metrics: metrics.filter((metric) => metric.source === "condition").length,
      access_metrics: metrics.filter((metric) => metric.source === "access").length,
      event_metrics: metrics.filter((metric) => metric.source === "event").length,
      numeric_spec_metrics: metrics.filter((metric) => metric.source === "spec").length,
      numeric_status_metrics: metrics.filter((metric) => metric.source === "status").length,
      events_total: events.length,
    },
    metrics,
  };
}

function runtimeResourceMetricsListView(
  cloudRunId: string,
  inventory: RuntimeResourceList,
  resources: RuntimeResourceMetrics[],
  totalResources = inventory.resources.length,
): RuntimeResourceMetricsList {
  const summary = resources.reduce<RuntimeResourceMetricsListSummary>((acc, resource) => {
    acc.metrics_total += resource.summary.metrics_total;
    acc.lifecycle_metrics += resource.summary.lifecycle_metrics;
    acc.condition_metrics += resource.summary.condition_metrics;
    acc.access_metrics += resource.summary.access_metrics;
    acc.event_metrics += resource.summary.event_metrics;
    acc.numeric_spec_metrics += resource.summary.numeric_spec_metrics;
    acc.numeric_status_metrics += resource.summary.numeric_status_metrics;
    acc.events_total += resource.summary.events_total;
    return acc;
  }, {
    resources_total: totalResources,
    resources_returned: resources.length,
    metrics_total: 0,
    lifecycle_metrics: 0,
    condition_metrics: 0,
    access_metrics: 0,
    event_metrics: 0,
    numeric_spec_metrics: 0,
    numeric_status_metrics: 0,
    events_total: 0,
  });
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RuntimeResourceMetricsList",
    cloud_run_id: cloudRunId,
    generated_at: new Date().toISOString(),
    core_run_ids: inventory.core_run_ids,
    metadata: {
      resourceVersion: inventory.metadata.resourceVersion,
      continue: inventory.metadata.continue,
      remainingItemCount: inventory.metadata.remainingItemCount,
      total: totalResources,
      returned: resources.length,
    },
    summary,
    resources,
  };
}

function runtimeInspectBundleFilter(input: RuntimeInspectBundleInput): RuntimeResourceFilter {
  return {
    kinds: input.kinds?.map((kind) => kind.trim()).filter(Boolean) ?? [],
    categories: input.categories?.map((category) => category.trim()).filter(Boolean) ?? [],
    labelSelector: input.labelSelector?.trim() || null,
    fieldSelector: input.fieldSelector?.trim() || null,
  };
}

function runtimeNumericLeafMetrics(
  prefix: "spec" | "status",
  value: JsonObject,
  labelRoot: string,
): Array<{ name: string; value: number; unit: string | null; description: string; labels: JsonObject }> {
  const out: Array<{ name: string; value: number; unit: string | null; description: string; labels: JsonObject }> = [];
  const visit = (current: unknown, path: string[]) => {
    if (out.length >= 64) return;
    const number = numberField(current);
    if (number !== null) {
      const metricPath = path.map(runtimeMetricPathSegment).join(".");
      const name = `${prefix}.${metricPath}`;
      out.push({
        name,
        value: number,
        unit: runtimeMetricUnit(name),
        description: `Numeric ${labelRoot}.${path.join(".")} value from the selected runtime resource.`,
        labels: {
          path: `${labelRoot}.${path.join(".")}`,
        },
      });
      return;
    }
    if (!isRecord(current) || path.length >= 5) return;
    for (const [key, child] of Object.entries(current).sort(([left], [right]) => left.localeCompare(right))) {
      if (Array.isArray(child)) continue;
      visit(child, [...path, key]);
      if (out.length >= 64) return;
    }
  };
  visit(value, []);
  return out;
}

function runtimeMetricUnit(name: string): string | null {
  if (/(^|[._-])cpu(_count)?$/.test(name) || name.endsWith(".cpu_count")) return "cores";
  if (/(^|[._-])(memory|disk)(_mb)?$/.test(name) || /_mb$|\.mb$/.test(name)) return "MiB";
  if (/_ms$|\.ms$/.test(name)) return "ms";
  if (/_seconds$|\.seconds$|_secs$|\.secs$/.test(name)) return "s";
  if (/count|total|events|conditions|actions/.test(name)) return "count";
  return null;
}

function runtimeMetricPathSegment(value: string): string {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    || "unknown";
}

function uniqueRuntimeMetricName(usedNames: Set<string>, name: string): string {
  if (!usedNames.has(name)) return name;
  let suffix = 2;
  while (usedNames.has(`${name}_${suffix}`)) {
    suffix += 1;
  }
  return `${name}_${suffix}`;
}

export function runtimeResourceHealthSummary(inventory: RuntimeResourceList): RuntimeResourceHealth {
  const rows = inventory.resources
    .map(runtimeResourceHealthRow)
    .sort((left, right) =>
      runtimeHealthRank(left.health) - runtimeHealthRank(right.health)
      || left.resource.localeCompare(right.resource)
    );
  const summary = rows.reduce<RuntimeResourceHealthSummary>(
    (acc, row) => {
      acc.total += 1;
      acc[row.health] += 1;
      if (Object.keys(row.access).length > 0) acc.access_targets += 1;
      if (row.access.reachable === true) acc.reachable_access_targets += 1;
      if (row.access.port_forward === true) acc.port_forward_ready += 1;
      if (row.access.exec === true) acc.exec_ready += 1;
      if (row.actions.length > 0) acc.actions_available += 1;
      if (row.observed === "current") {
        acc.observed_current += 1;
        acc.observed_resources += 1;
      } else if (row.observed === "stale") {
        acc.observed_stale += 1;
        acc.observed_resources += 1;
      } else {
        acc.observed_unknown += 1;
      }
      return acc;
    },
    {
      total: 0,
      ready: 0,
      degraded: 0,
      problem: 0,
      unknown: 0,
      access_targets: 0,
      reachable_access_targets: 0,
      port_forward_ready: 0,
      exec_ready: 0,
      actions_available: 0,
      observed_resources: 0,
      observed_current: 0,
      observed_stale: 0,
      observed_unknown: 0,
    },
  );
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RuntimeResourceHealth",
    cloud_run_id: inventory.cloud_run_id,
    generated_at: new Date().toISOString(),
    core_run_ids: inventory.core_run_ids,
    summary,
    resources: rows,
  };
}

function runtimeResourceHealthRow(resource: RuntimeResourceRecord): RuntimeResourceHealthRow {
  const ready = runtimeResourceReadyCondition(resource);
  const degradedConditions = runtimeResourceDegradedConditions(resource);
  const summaryCondition = runtimeResourceSummaryCondition(resource);
  const access = isRecord(resource.status.access) ? resource.status.access : {};
  return {
    resource: `${resource.kind}/${resource.metadata.name}`,
    resource_ref: runtimeResourceWatchRef(resource),
    health: runtimeResourceHealthState(resource, ready, degradedConditions),
    observed: runtimeResourceObservedHealthState(resource),
    phase: stringField(resource.status.phase),
    ready,
    reason: stringField(resource.status.reason) ?? summaryCondition?.reason ?? null,
    message: stringField(resource.status.message) ?? stringField(resource.status.error_message) ?? summaryCondition?.message ?? null,
    condition_summary: summaryCondition ? `${summaryCondition.type}=${summaryCondition.status} ${summaryCondition.reason}` : null,
    degraded_conditions: degradedConditions,
    actions: runtimeResourceAdvertisedActions(resource),
    access,
    access_summary: runtimeAccessSummary(access),
    source: stringField(resource.audit.source),
    updated_at: runtimeResourceUpdatedAt(resource),
    resource_version: stringField(resource.metadata.resourceVersion),
  };
}

function runtimeResourceObservedHealthState(resource: RuntimeResourceRecord): RuntimeResourceHealthRow["observed"] {
  const observed = runtimeResourceObservedState(resource);
  if (observed === null) return "unknown";
  return observed ? "current" : "stale";
}

function runtimeResourceHealthState(
  resource: RuntimeResourceRecord,
  ready: RuntimeResourceCondition | null,
  degradedConditions: RuntimeResourceCondition[],
): RuntimeResourceHealthState {
  const phase = stringField(resource.status.phase) ?? "unknown";
  if (runtimeResourceProblemPhase(phase)) return "problem";
  if (ready?.status === "Unknown") return "unknown";
  if (ready?.status === "False") {
    if (runtimeResourceProgressingPhase(phase)) return "unknown";
    if (runtimeResourceSuccessfulPhase(phase) && !stringField(resource.status.error_message)) return "ready";
    return "problem";
  }
  if (ready?.status === "True" && degradedConditions.length > 0) return "degraded";
  if (ready?.status === "True") return "ready";
  return "unknown";
}

function runtimeResourceReadyCondition(resource: RuntimeResourceRecord): RuntimeResourceCondition | null {
  return runtimeResourceStatusConditions(resource).find((condition) => condition.type === "Ready") ?? null;
}

function runtimeResourceDegradedConditions(resource: RuntimeResourceRecord): RuntimeResourceCondition[] {
  return runtimeResourceStatusConditions(resource).filter((condition) => condition.type !== "Ready" && condition.status !== "True");
}

function runtimeResourceStatusConditions(resource: RuntimeResourceRecord): RuntimeResourceCondition[] {
  return Array.isArray(resource.status.conditions)
    ? resource.status.conditions.filter(runtimeResourceConditionRecord)
    : [];
}

function runtimeAccessSummary(access: JsonObject): string | null {
  const parts = [
    typeof access.reachable === "boolean" ? `reachable ${access.reachable ? "yes" : "no"}` : "",
    [
      access.port_forward === true ? "port-forward" : "",
      access.exec === true ? "exec" : "",
    ].filter(Boolean).join(","),
    stringField(access.runner_instance_id) ? `runner ${stringField(access.runner_instance_id)}` : "",
    stringField(access.attempt_id) ? `attempt ${stringField(access.attempt_id)}` : "",
    stringField(access.reason) && access.reason !== "reachable" ? stringField(access.reason) : "",
  ].filter((part): part is string => Boolean(part));
  return parts.length > 0 ? parts.join(" ") : null;
}

function runtimeResourceUpdatedAt(resource: RuntimeResourceRecord): string | null {
  const updatedAt = stringField(resource.metadata.updated_at)
    ?? stringField(resource.audit.observed_at)
    ?? stringField(resource.metadata.created_at);
  if (updatedAt) return updatedAt;
  const updatedAtMs = numberField(resource.status.updated_at_ms);
  if (updatedAtMs === null) return null;
  const date = new Date(updatedAtMs);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

function runtimeHealthRank(health: RuntimeResourceHealthState): number {
  switch (health) {
    case "problem":
      return 0;
    case "degraded":
      return 1;
    case "unknown":
      return 2;
    case "ready":
      return 3;
  }
}

function runtimeResourceProblemPhase(phase: string): boolean {
  return /fail|error|lost|dead|offline|unhealthy|timeout|cancel|expired|disabled/i.test(phase);
}

function runtimeResourceProgressingPhase(phase: string): boolean {
  return /declared|queued|pending|provisioning|starting|requested/i.test(phase);
}

function runtimeResourceSuccessfulPhase(phase: string): boolean {
  return /complete|succeed|reported|recorded/i.test(phase);
}

function relatedResourceRank(relationship: RuntimeRelatedResourceRecord["relationship"]): number {
  return relationship === "owner" ? 0 : 1;
}

function ownerReferenceMatchesResource(
  ownerRef: RuntimeResourceRecord["metadata"]["ownerReferences"][number],
  resource: RuntimeResourceRecord,
): boolean {
  if (ownerRef.apiVersion !== resource.apiVersion) return false;
  if (ownerRef.kind !== resource.kind) return false;
  if (ownerRef.uid && resource.metadata.uid && ownerRef.uid !== resource.metadata.uid) return false;
  return ownerRef.name === resource.metadata.name;
}

function runtimeResource(input: {
  kind: string;
  name: string;
  uid?: string;
  labels: Record<string, string>;
  annotations?: Record<string, string> | undefined;
  ownerReferences: RuntimeResourceRecord["metadata"]["ownerReferences"];
  created_at?: string | null;
  updated_at?: string | null;
  spec: JsonObject;
  status: JsonObject;
  audit: JsonObject;
}): RuntimeResourceRecord {
  const deletionTimestamp = runtimeResourceDeletionTimestamp(input.kind, input.status, input.updated_at);
  const resource: RuntimeResourceDraft = {
    apiVersion: RUNTIME_API_VERSION,
    kind: input.kind,
    metadata: {
      name: input.name,
      uid: normalizeOptionalString(input.uid) ?? runtimeResourceGeneratedUid(input),
      labels: input.labels,
      annotations: input.annotations ?? {},
      ownerReferences: input.ownerReferences,
      ...(input.created_at ? { creationTimestamp: input.created_at } : {}),
      ...(deletionTimestamp ? { deletionTimestamp } : {}),
      ...(input.created_at ? { created_at: input.created_at } : {}),
      ...(input.updated_at ? { updated_at: input.updated_at } : {}),
    },
    spec: input.spec,
    status: input.status,
    audit: input.audit,
  };
  const access = runtimeResourceAccessSummary(resource);
  if (access) {
    resource.status = {
      ...resource.status,
      access,
    };
  }
  const generation = runtimeResourceGeneration(resource);
  resource.metadata = {
    ...resource.metadata,
    generation,
  };
  if (numberField(resource.status.observedGeneration) === null) {
    resource.status = {
      ...resource.status,
      observedGeneration: generation,
    };
  }
  if (!Array.isArray(resource.status.conditions)) {
    resource.status = {
      ...resource.status,
      conditions: runtimeResourceConditions(resource),
    };
  }
  if (!Array.isArray(resource.status.actions)) {
    const actions = runtimeResourceActions(resource);
    if (actions.length > 0) {
      resource.status = {
        ...resource.status,
        actions,
      };
    }
  }
  resource.status = {
    ...resource.status,
    ...runtimeResourceStatusSummary(resource),
  };
  resource.metadata = {
    ...resource.metadata,
    annotations: runtimeResourceAnnotations(resource),
  };
  resource.metadata = {
    ...resource.metadata,
    resourceVersion: runtimeResourceVersion(resource),
  };
  return resource as RuntimeResourceRecord;
}

function runtimeResourceGeneratedUid(input: { kind: string; name: string; ownerReferences: RuntimeResourceRecord["metadata"]["ownerReferences"] }): string {
  return sha256Digest(canonicalJsonStringify({
    apiVersion: RUNTIME_API_VERSION,
    kind: input.kind,
    name: input.name,
    ownerReferences: input.ownerReferences,
  } as JsonObject));
}

function runtimeResourceDeletionTimestamp(kind: string, status: JsonObject, updatedAt: string | null | undefined): string | null {
  const phase = stringField(status.phase)?.toLowerCase();
  return (kind === "PortForward" || kind === "Exec") && phase === "cancelled" && updatedAt
    ? updatedAt
    : null;
}

function runtimeResourceAccessSummary(resource: RuntimeResourceReadable): JsonObject | undefined {
  if (!isRuntimeAccessTargetKind(resource.kind)) {
    return undefined;
  }
  const existing = isRecord(resource.status.access) ? resource.status.access : {};
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  const bindingAccess = isRecord(runnerBinding.access) ? runnerBinding.access : {};
  const unreachableReason = runtimeAccessTargetUnreachableReason(resource);
  return compactObject({
    ...existing,
    reachable: !unreachableReason,
    reason: unreachableReason ?? "reachable",
    port_forward: booleanField(existing.port_forward) ?? booleanField(bindingAccess.port_forward),
    exec: booleanField(existing.exec) ?? booleanField(bindingAccess.exec),
    runner_instance_id: stringField(existing.runner_instance_id) ?? stringField(runnerBinding.runner_instance_id) ?? resourceRunnerInstanceId(resource),
    runner_instance_status: stringField(existing.runner_instance_status) ?? resourceRunnerInstanceStatus(resource),
    attempt_id: stringField(existing.attempt_id) ?? stringField(runnerBinding.attempt_id) ?? resourceAttemptId(resource),
    worker_id: stringField(existing.worker_id) ?? stringField(runnerBinding.worker_id) ?? resourceWorkerId(resource),
  });
}

function booleanField(value: unknown): boolean | undefined {
  return typeof value === "boolean" ? value : undefined;
}

function objectField(value: unknown): JsonObject | undefined {
  if (!isRecord(value) || !isJsonValue(value)) return undefined;
  return value as JsonObject;
}

function runtimeResourceAnnotations(resource: RuntimeResourceReadable): Record<string, string> {
  const annotations: Record<string, string> = { ...resource.metadata.annotations };
  setAnnotation(annotations, "bucephalus.dev/audit-source", stringField(resource.audit.source));
  setAnnotation(annotations, "bucephalus.dev/phase", stringField(resource.status.phase));
  setAnnotation(annotations, "bucephalus.dev/error-message", stringField(resource.status.error_message));
  if (resource.kind === "PortForward" || resource.kind === "Exec") {
    const accessRequestId = stringField(resource.spec.access_request_id) ?? resource.metadata.uid;
    const targetKind = stringField(resource.spec.resource_kind);
    const targetName = stringField(resource.spec.resource_name);
    const targetRef = isRecord(resource.spec.target_ref) ? resource.spec.target_ref : {};
    setAnnotation(annotations, "bucephalus.dev/access-request-id", accessRequestId);
    setAnnotation(
      annotations,
      "bucephalus.dev/access-target",
      targetKind && targetName ? `${targetKind}/${targetName}` : undefined,
    );
    setAnnotation(annotations, "bucephalus.dev/access-target-uid", stringField(targetRef.uid));
    setAnnotation(annotations, "bucephalus.dev/access-target-resource-version", stringField(targetRef.resourceVersion));
    setAnnotation(annotations, "bucephalus.dev/requester", stringField(resource.audit.requester));
    setAnnotation(annotations, "bucephalus.dev/reason", stringField(resource.spec.reason) ?? stringField(resource.audit.reason));
  }
  if (resource.kind === "Event") {
    setAnnotation(annotations, "bucephalus.dev/event-uid", stringField(resource.spec.event_uid) ?? resource.metadata.uid);
    setAnnotation(annotations, "bucephalus.dev/event-type", stringField(resource.spec.event_type));
    setAnnotation(annotations, "bucephalus.dev/event-source", stringField(resource.spec.source) ?? stringField(resource.audit.source));
  }
  return annotations;
}

function setAnnotation(annotations: Record<string, string>, key: string, value: string | null | undefined): void {
  const normalized = value?.trim();
  if (normalized) {
    annotations[key] = normalized;
  }
}

function runtimeResourceVersion(resource: RuntimeResourceReadable): string {
  const { resourceVersion: _resourceVersion, ...metadata } = resource.metadata;
  return sha256Digest(canonicalJsonStringify({
    apiVersion: resource.apiVersion,
    audit: resource.audit,
    kind: resource.kind,
    metadata,
    spec: resource.spec,
    status: resource.status,
  } as JsonObject));
}

function assertRuntimeResourceVersionPrecondition(
  resource: RuntimeResourceRecord,
  expectedVersion: string | null | undefined,
  options: { required?: boolean; operation?: string } = {},
): void {
  const expected = normalizeOptionalString(expectedVersion);
  const current = resource.metadata.resourceVersion ?? runtimeResourceVersion(resource);
  if (!expected) {
    if (options.required) {
      throw new HttpError(
        428,
        "runtime_resource_version_required",
        `${resource.kind}/${resource.metadata.name} requires resource_version from operation review; refresh the runtime resource and retry`,
        {
          resource_kind: resource.kind,
          resource_name: resource.metadata.name,
          resource_version: current,
          resource_generation: numberField(resource.metadata.generation),
          observed_generation: numberField(resource.status.observedGeneration),
          operation: normalizeOptionalString(options.operation),
          required: ["resource_version"],
        },
      );
    }
    return;
  }
  if (expected === current) {
    return;
  }
  throw new HttpError(
    409,
    "runtime_resource_version_conflict",
    `${resource.kind}/${resource.metadata.name} changed since it was reviewed; refresh the runtime resource and retry`,
    {
      resource_kind: resource.kind,
      resource_name: resource.metadata.name,
      expected_resource_version: expected,
      resource_version: current,
      resource_generation: numberField(resource.metadata.generation),
      observed_generation: numberField(resource.status.observedGeneration),
    },
  );
}

function runtimeResourceListMeta(resources: RuntimeResourceRecord[]): RuntimeResourceListMeta {
  const items = resources
    .map((resource) => ({
      key: resourceIdentity(resource),
      resourceVersion: resource.metadata.resourceVersion ?? runtimeResourceVersion(resource),
    }))
    .sort((left, right) => left.key.localeCompare(right.key));
  return {
    resourceVersion: sha256Digest(canonicalJsonStringify({ resources: items })),
    continue: null,
    remainingItemCount: 0,
    total: resources.length,
    returned: resources.length,
  };
}

function paginateRuntimeResources(
  resources: RuntimeResourceRecord[],
  input: RuntimeResourceListInput,
): { metadata: RuntimeResourceListMeta; resources: RuntimeResourceRecord[] } {
  const collection = runtimeResourceListMeta(resources);
  const offset = runtimeResourceContinueOffset(input.continueToken, collection.resourceVersion, resources.length);
  const limit = input.limit;
  if (limit == null) {
    const page = resources.slice(offset);
    return {
      metadata: {
        ...collection,
        returned: page.length,
      },
      resources: page,
    };
  }
  const end = Math.min(resources.length, offset + limit);
  const remainingItemCount = Math.max(0, resources.length - end);
  const page = resources.slice(offset, end);
  return {
    metadata: {
      resourceVersion: collection.resourceVersion,
      continue: remainingItemCount > 0 ? runtimeResourceContinueToken(collection.resourceVersion, end) : null,
      remainingItemCount,
      total: resources.length,
      returned: page.length,
    },
    resources: page,
  };
}

function runtimeResourceContinueOffset(
  token: string | null | undefined,
  resourceVersion: string,
  resourceCount: number,
): number {
  if (token == null || token === "") {
    return 0;
  }
  if (typeof token !== "string") {
    throw new HttpError(400, "invalid_runtime_resource_continue", "Runtime resource continue token is not valid");
  }
  if (!token.trim()) {
    return 0;
  }
  let decoded: unknown;
  try {
    decoded = JSON.parse(Buffer.from(token, "base64url").toString("utf8"));
  } catch {
    throw new HttpError(400, "invalid_runtime_resource_continue", "Runtime resource continue token is not valid");
  }
  if (!isJsonObject(decoded)) {
    throw new HttpError(400, "invalid_runtime_resource_continue", "Runtime resource continue token is not valid");
  }
  const version = typeof decoded.resourceVersion === "string" ? decoded.resourceVersion : "";
  const offset = typeof decoded.offset === "number" && Number.isInteger(decoded.offset) ? decoded.offset : -1;
  if (!version || offset < 0) {
    throw new HttpError(400, "invalid_runtime_resource_continue", "Runtime resource continue token is not valid");
  }
  if (version !== resourceVersion) {
    throw new HttpError(410, "runtime_resource_continue_expired", "Runtime resource continue token has expired; restart the list without continue", {
      resource_version: resourceVersion,
      continue_resource_version: version,
    });
  }
  if (offset > resourceCount) {
    throw new HttpError(400, "invalid_runtime_resource_continue", "Runtime resource continue token offset is outside the filtered collection", {
      offset,
      resource_count: resourceCount,
    });
  }
  return offset;
}

function runtimeResourceContinueToken(resourceVersion: string, offset: number): string {
  return Buffer.from(JSON.stringify({ resourceVersion, offset }), "utf8").toString("base64url");
}

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function runtimeResourceGeneration(resource: RuntimeResourceReadable): number {
  const {
    annotations: _annotations,
    creationTimestamp: _creationTimestamp,
    created_at: _createdAt,
    deletionTimestamp: _deletionTimestamp,
    generation: _generation,
    resourceVersion: _resourceVersion,
    updated_at: _updatedAt,
    ...metadata
  } = resource.metadata;
  const digest = sha256Digest(canonicalJsonStringify({
    apiVersion: resource.apiVersion,
    kind: resource.kind,
    metadata,
    spec: resource.spec,
  } as JsonObject));
  const prefix = digest.slice("sha256:".length, "sha256:".length + 13);
  const value = Number.parseInt(prefix, 16);
  return Number.isSafeInteger(value) && value >= 0 ? value + 1 : 1;
}

function runtimeResourceStatusSummary(resource: RuntimeResourceReadable): { reason: string; message: string } {
  const condition = runtimeResourceSummaryCondition(resource);
  const phase = stringField(resource.status.phase) ?? "Unknown";
  const reason = stringField(resource.status.reason)
    ?? condition?.reason
    ?? conditionReason(phase);
  const message = stringField(resource.status.message)
    ?? stringField(resource.status.error_message)
    ?? condition?.message
    ?? `Resource phase is ${phase}`;
  return { reason, message };
}

function runtimeResourceSummaryCondition(resource: RuntimeResourceReadable): RuntimeResourceCondition | null {
  const conditions = Array.isArray(resource.status.conditions)
    ? resource.status.conditions.filter(runtimeResourceConditionRecord)
    : [];
  return conditions.find((condition) => condition.status === "False" || condition.status === "Unknown")
    ?? conditions.find((condition) => condition.type === "Ready")
    ?? conditions[0]
    ?? null;
}

function runtimeResourceConditionRecord(value: unknown): value is RuntimeResourceCondition {
  return isRecord(value)
    && Boolean(stringField(value.type))
    && (value.status === "True" || value.status === "False" || value.status === "Unknown")
    && Boolean(stringField(value.reason))
    && Boolean(stringField(value.message));
}

function assertRuntimeResourceSupportsAction(resource: RuntimeResourceRecord, action: string): void {
  const actions = runtimeResourceAdvertisedActions(resource);
  if (actions.includes(action)) {
    return;
  }
  throw new HttpError(409, "runtime_resource_action_unavailable", "Runtime resource does not advertise this action in status.actions", {
    action,
    resource_kind: resource.kind,
    resource_name: resource.metadata.name,
    available_actions: actions,
  });
}

function runtimeResourceAdvertisedActions(resource: RuntimeResourceRecord): string[] {
  return Array.isArray(resource.status.actions)
    ? resource.status.actions
      .map((action) => stringField(action))
      .filter((action): action is string => Boolean(action))
    : [];
}

function runtimeResourceActions(resource: RuntimeResourceReadable): string[] {
  const phase = (stringField(resource.status.phase) ?? "").toLowerCase();
  switch (resource.kind) {
    case "RunnerInstance":
      if (phase === "online") return ["cordon", "drain"];
      if (phase === "cordoned") return ["uncordon", "drain"];
      if (phase === "draining") return ["uncordon"];
      return [];
    case "PortForward":
      if (phase === "active") return ["cancel", "complete"];
      return phase === "requested" || phase === "accepted" ? ["cancel"] : [];
    case "Exec":
      return phase === "requested" || phase === "accepted" || phase === "active" ? ["cancel"] : [];
    default:
      return [];
  }
}

function runtimeResourceConditions(resource: RuntimeResourceReadable): RuntimeResourceCondition[] {
  const conditions = [runtimeReadyCondition(resource)];
  const observed = runtimeObservedCondition(resource);
  if (observed) {
    conditions.push(observed);
  }
  if (isRuntimeAccessTargetKind(resource.kind)) {
    conditions.push(runtimeReachableCondition(resource));
  }
  if (resource.kind === "Trial" || resource.kind === "ScheduleSlot" || resource.kind === "TrialContainer") {
    conditions.push(runtimeRunnerBoundCondition(resource));
  }
  if (isRuntimeAccessTargetKind(resource.kind)) {
    conditions.push(runtimeAccessCapabilityCondition(resource, "PortForwardReady", "port_forward", "runtime_port_forward"));
    conditions.push(runtimeAccessCapabilityCondition(resource, "ExecReady", "exec", "runtime_exec"));
  }
  if (resource.kind === "PortForward" || resource.kind === "Exec") {
    conditions.push(...runtimeAccessRequestConditions(resource));
  }
  return conditions;
}

function runtimeObservedCondition(resource: RuntimeResourceReadable): RuntimeResourceCondition | null {
  const observed = runtimeResourceObservedState(resource);
  if (observed === null) {
    return null;
  }
  const generation = numberField(resource.metadata.generation);
  const observedGeneration = numberField(resource.status.observedGeneration);
  return runtimeCondition(
    resource,
    "Observed",
    observed ? "True" : "False",
    observed ? "ObservedGenerationCurrent" : "ObservedGenerationStale",
    observed
      ? `Status observedGeneration ${observedGeneration} has caught up to metadata generation ${generation}`
      : `Status observedGeneration ${observedGeneration} is behind metadata generation ${generation}`,
  );
}

function runtimeResourceObservedState(resource: RuntimeResourceReadable): boolean | null {
  const generation = numberField(resource.metadata.generation);
  const observedGeneration = numberField(resource.status.observedGeneration);
  if (generation === null || observedGeneration === null) {
    return null;
  }
  return observedGeneration >= generation;
}

function runtimeReadyCondition(resource: RuntimeResourceReadable): RuntimeResourceCondition {
  const phase = stringField(resource.status.phase) ?? "Unknown";
  const normalized = phase.toLowerCase();
  const error = stringField(resource.status.error_message);
  if (normalized === "unsatisfied") {
    return runtimeCondition(resource, "Ready", "False", "Unsatisfied", stringField(resource.status.message) ?? "Declared runtime requirement is not satisfied");
  }
  if (normalized === "satisfied") {
    return runtimeCondition(resource, "Ready", "True", "Satisfied", stringField(resource.status.message) ?? "Declared runtime requirement is satisfied");
  }
  if (error || /fail|error|lost|dead|offline|unhealthy|timeout|cancel|disabled/.test(normalized)) {
    return runtimeCondition(resource, "Ready", "False", conditionReason(phase || "Error"), error ?? `Resource phase is ${phase}`);
  }
  if (/running|active|bound|reported|accepted|online|cordoned|draining|recorded|available|applied|materialized|pulled/.test(normalized)) {
    return runtimeCondition(resource, "Ready", "True", conditionReason(phase), `Resource phase is ${phase}`);
  }
  if (/complete|succeed/.test(normalized)) {
    return runtimeCondition(resource, "Ready", "False", "Terminal", `Resource reached terminal phase ${phase}`);
  }
  if (/declared|queued|pending|provisioning|starting|requested|checking|applying|pulling/.test(normalized)) {
    return runtimeCondition(resource, "Ready", "False", conditionReason(phase || "Pending"), `Resource is waiting in phase ${phase}`);
  }
  return runtimeCondition(resource, "Ready", "Unknown", "UnknownPhase", `Resource phase is ${phase}`);
}

function runtimeReachableCondition(resource: RuntimeResourceReadable): RuntimeResourceCondition {
  const unreachableReason = runtimeAccessTargetUnreachableReason(resource);
  return runtimeCondition(
    resource,
    "Reachable",
    unreachableReason ? "False" : "True",
    unreachableReason ? "Unreachable" : "Reachable",
    unreachableReason
      ? `Runtime access target is not reachable: ${unreachableReason}`
      : "Runtime access target is currently reachable",
  );
}

function runtimeRunnerBoundCondition(resource: RuntimeResourceReadable): RuntimeResourceCondition {
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  const workerId = stringField(runnerBinding.worker_id) ?? resourceWorkerId(resource);
  const attemptId = stringField(runnerBinding.attempt_id);
  const runnerInstanceId = stringField(runnerBinding.runner_instance_id);
  const phase = stringField(resource.status.phase) ?? "unknown";
  if (attemptId && runnerInstanceId) {
    return runtimeCondition(
      resource,
      "RunnerBound",
      "True",
      "Bound",
      `Resource is bound to runner attempt ${attemptId} on runner instance ${runnerInstanceId}`,
    );
  }
  if (workerId) {
    return runtimeCondition(
      resource,
      "RunnerBound",
      "False",
      "WorkerUnbound",
      `Resource has worker ${workerId}, but Cloud has not linked it to an active runner attempt`,
    );
  }
  return runtimeCondition(
    resource,
    "RunnerBound",
    "False",
    "Unbound",
    `Resource phase is ${phase} without an active runner binding`,
  );
}

function runtimeAccessCapabilityCondition(
  resource: RuntimeResourceReadable,
  type: "PortForwardReady" | "ExecReady",
  accessKey: "port_forward" | "exec",
  capability: "runtime_port_forward" | "runtime_exec",
): RuntimeResourceCondition {
  const unreachableReason = runtimeAccessTargetUnreachableReason(resource);
  if (unreachableReason) {
    return runtimeCondition(resource, type, "False", "AttemptUnreachable", `Runner attempt is not reachable: ${unreachableReason}`);
  }
  const access = isRecord(resource.status.access) ? resource.status.access : {};
  const hasCapability = access[accessKey] === true;
  return runtimeCondition(
    resource,
    type,
    hasCapability ? "True" : "False",
    hasCapability ? "CapabilityAdvertised" : "CapabilityMissing",
    hasCapability
      ? `Runner advertises ${capability}`
      : `Runner does not advertise ${capability}`,
  );
}

function runtimeAccessRequestConditions(resource: RuntimeResourceReadable): RuntimeResourceCondition[] {
  const phase = (stringField(resource.status.phase) ?? "unknown").toLowerCase();
  const error = stringField(resource.status.error_message);
  const accepted = phase === "accepted" || phase === "active" || phase === "completed";
  const active = phase === "active";
  const terminal = phase === "completed" || phase === "cancelled" || phase === "expired" || phase === "failed";
  const conditions = [
    runtimeCondition(
      resource,
      "Accepted",
      accepted ? "True" : "False",
      accepted ? "RunnerAccepted" : conditionReason(phase || "Pending"),
      accepted ? "Runner accepted the access request" : `Access request phase is ${phase}`,
    ),
    runtimeCondition(
      resource,
      "Active",
      active ? "True" : "False",
      active ? "TunnelActive" : conditionReason(phase || "Inactive"),
      active ? "Access data plane is active" : `Access data plane is not active; phase is ${phase}`,
    ),
  ];
  if (resource.kind === "PortForward") {
    conditions.push(runtimePortForwardClientReachableCondition(resource, phase));
  }
  if (terminal) {
    conditions.push(runtimeCondition(
      resource,
      "Completed",
      phase === "completed" ? "True" : "False",
      conditionReason(phase),
      error ?? `Access request reached terminal phase ${phase}`,
    ));
  }
  return conditions;
}

function runtimePortForwardClientReachableCondition(
  resource: RuntimeResourceReadable,
  phase: string,
): RuntimeResourceCondition {
  if (phase !== "active") {
    return runtimeCondition(
      resource,
      "ClientReachable",
      "Unknown",
      conditionReason(phase || "Inactive"),
      `PortForward client reachability is unknown while phase is ${phase}`,
    );
  }
  const connection = isRecord(resource.status.connection) ? resource.status.connection : {};
  if (runtimePortForwardConnectionClientReachable(connection)) {
    return runtimeCondition(
      resource,
      "ClientReachable",
      "True",
      "ClientEndpointReported",
      "PortForward reports a client-reachable tunnel endpoint",
    );
  }
  return runtimeCondition(
    resource,
    "ClientReachable",
    "False",
    "ClientEndpointMissing",
    "PortForward is active but has not reported a client-reachable endpoint",
  );
}

function runtimePortForwardConnectionClientReachable(connection: JsonObject): boolean {
  return connection.client_reachable === true
    || Boolean(stringField(connection.client_endpoint))
    || Boolean(stringField(connection.client_listen));
}

function runtimeCondition(
  resource: RuntimeResourceReadable,
  type: string,
  status: RuntimeResourceCondition["status"],
  reason: string,
  message: string,
): RuntimeResourceCondition {
  return compactObject({
    type,
    status,
    reason,
    message,
    lastTransitionTime: runtimeConditionTime(resource),
  }) as RuntimeResourceCondition;
}

function runtimeConditionTime(resource: RuntimeResourceReadable): string | undefined {
  return resource.metadata.updated_at
    ?? resource.metadata.created_at
    ?? stringField(resource.audit.observed_at)
    ?? undefined;
}

function conditionReason(value: string): string {
  const cleaned = value
    .split(/[^A-Za-z0-9]+/)
    .map((part) => part ? `${part[0]?.toUpperCase() ?? ""}${part.slice(1).toLowerCase()}` : "")
    .join("");
  return cleaned || "Unknown";
}

export function filterRuntimeResources(
  resources: RuntimeResourceRecord[],
  filter: RuntimeResourceFilter = {},
): RuntimeResourceRecord[] {
  const kinds = new Set((filter.kinds ?? [])
    .map((kind) => canonicalRuntimeResourceKind(kind) ?? kind.trim())
    .filter(Boolean));
  const categories = new Set((filter.categories ?? [])
    .map((category) => runtimeApiResourceAliasKey(category.trim()))
    .filter(Boolean));
  const labelRequirements = parseResourceSelector(filter.labelSelector, "label_selector", true);
  const fieldRequirements = normalizeRuntimeKindSelectorRequirements(
    parseResourceSelector(filter.fieldSelector, "field_selector", false),
  );
  return resources.filter((resource) => {
    if (kinds.size > 0 && !kinds.has(resource.kind)) {
      return false;
    }
    if (categories.size > 0 && !runtimeResourceMatchesAnyCategory(resource, categories)) {
      return false;
    }
    return labelRequirements.every((requirement) => resourceSelectorMatches(requirement, labelValue(resource, requirement.key)))
      && fieldRequirements.every((requirement) => resourceSelectorMatches(requirement, fieldValue(resource, requirement.key)));
  });
}

function runtimeResourceMatchesAnyCategory(resource: RuntimeResourceRecord, categories: Set<string>): boolean {
  const kind = canonicalRuntimeResourceKind(resource.kind) ?? resource.kind;
  const definition = RUNTIME_API_RESOURCE_DEFINITIONS.find((candidate) => candidate.kind === kind);
  return (definition?.categories ?? []).some((category) => categories.has(runtimeApiResourceAliasKey(category)));
}

export function runtimeAccessTargetFromInventory(
  resources: RuntimeResourceRecord[],
  input: { resourceKind?: string | null | undefined; resourceName?: string | null | undefined; resourceVersion?: string | null | undefined },
  options: { required?: boolean; operation?: string } = {},
): RuntimeAccessTarget {
  const resourceKind = normalizeOptionalString(input.resourceKind);
  const resourceName = normalizeOptionalString(input.resourceName);
  if (!resourceKind || !resourceName) {
    throw new HttpError(
      400,
      "invalid_runtime_access_target",
      "resource_kind and resource_name must be provided together",
      {
        required: ["resource_kind", "resource_name"],
      },
    );
  }
  const resource = resources.find((item) => resourceIdentity(item) === resourceIdentity({
    kind: canonicalRuntimeResourceKind(resourceKind) ?? resourceKind,
    name: resourceName,
  }));
  if (!resource) {
    throw new HttpError(404, "runtime_resource_not_found", "Runtime access target resource not found", {
      kind: resourceKind,
      name: resourceName,
    });
  }
  assertRuntimeResourceVersionPrecondition(resource, input.resourceVersion, options);
  if (!isRuntimeAccessTargetKind(resource.kind)) {
    throw new HttpError(
      409,
      "runtime_access_target_unsupported",
      `${resource.kind}/${resource.metadata.name} is not a supported runtime access target`,
      {
        kind: resource.kind,
        name: resource.metadata.name,
        supported_kinds: [...RUNTIME_ACCESS_TARGET_KINDS],
      },
    );
  }
  const unreachableReason = runtimeAccessTargetUnreachableReason(resource);
  if (unreachableReason) {
    throw new HttpError(
      409,
      "runtime_access_target_unreachable",
      `${resource.kind}/${resource.metadata.name} is not currently reachable for runtime access: ${unreachableReason}`,
      {
        kind: resource.kind,
        name: resource.metadata.name,
        phase: stringField(resource.status.phase) ?? null,
      },
    );
  }
  const binding = runtimeAccessTargetBinding(resources, resource);
  if ((resource.kind === "Trial" || resource.kind === "ScheduleSlot" || resource.kind === "TrialContainer") && !binding.workerId && !binding.attemptId) {
    throw new HttpError(
      409,
      "runtime_access_target_unreachable",
      `${resource.kind}/${resource.metadata.name} is not currently reachable for runtime access: target is not bound to an active runner attempt`,
      {
        kind: resource.kind,
        name: resource.metadata.name,
        phase: stringField(resource.status.phase) ?? null,
      },
    );
  }
  const target: RuntimeAccessTarget = {
    kind: resource.kind,
    name: resource.metadata.name,
    ...binding,
  };
  if (resource.metadata.uid) {
    target.uid = resource.metadata.uid;
  }
  if (resource.metadata.resourceVersion) {
    target.resourceVersion = resource.metadata.resourceVersion;
  }
  return target;
}

function runtimeAccessTargetBinding(
  resources: RuntimeResourceRecord[],
  resource: RuntimeResourceRecord,
): Pick<RuntimeAccessTarget, "runnerInstanceId" | "attemptId" | "workerId"> {
  if (resource.kind === "Run") {
    const access = isRecord(resource.status.access) ? resource.status.access : {};
    return compactObject({
      runnerInstanceId: stringField(access.runner_instance_id),
      attemptId: stringField(access.attempt_id),
      workerId: stringField(access.worker_id),
    }) as Pick<RuntimeAccessTarget, "runnerInstanceId" | "attemptId" | "workerId">;
  }
  if (resource.kind === "RunnerInstance") {
    const currentAttemptId = stringField(resource.status.current_attempt_id);
    const attempt = currentAttemptId
      ? resources.find((item) => item.kind === "RunnerAttempt" && resourceAttemptId(item) === currentAttemptId)
      : undefined;
    return compactObject({
      runnerInstanceId: resourceRunnerInstanceId(resource) ?? resource.metadata.uid,
      attemptId: currentAttemptId,
      workerId: attempt ? resourceWorkerId(attempt) : undefined,
    }) as Pick<RuntimeAccessTarget, "runnerInstanceId" | "attemptId" | "workerId">;
  }
  if (resource.kind === "RunnerAttempt") {
    return compactObject({
      runnerInstanceId: resourceRunnerInstanceId(resource),
      attemptId: resourceAttemptId(resource),
      workerId: resourceWorkerId(resource),
    }) as Pick<RuntimeAccessTarget, "runnerInstanceId" | "attemptId" | "workerId">;
  }
  if (resource.kind === "ScheduleSlot") {
    return runtimeAccessTargetBindingForWorker(resources, resourceWorkerId(resource));
  }
  if (resource.kind === "Trial") {
    const slot = runtimeScheduleSlotForTrial(resources, resource);
    return runtimeAccessTargetBindingForWorker(resources, slot ? resourceWorkerId(slot) : resourceWorkerId(resource));
  }
  if (resource.kind === "TrialContainer") {
    const slot = runtimeScheduleSlotForContainer(resources, resource);
    return runtimeAccessTargetBindingForWorker(resources, slot ? resourceWorkerId(slot) : undefined);
  }
  return {};
}

function runtimeAccessTargetBindingForWorker(
  resources: RuntimeResourceRecord[],
  workerId: string | null | undefined,
): Pick<RuntimeAccessTarget, "runnerInstanceId" | "attemptId" | "workerId"> {
  const normalizedWorkerId = normalizeOptionalString(workerId);
  if (!normalizedWorkerId) {
    return {};
  }
  const attempt = resources.find((resource) =>
    resource.kind === "RunnerAttempt"
      && resourceWorkerId(resource) === normalizedWorkerId
      && stringField(resource.status.phase)?.toLowerCase() === "running"
  );
  return compactObject({
    runnerInstanceId: attempt ? resourceRunnerInstanceId(attempt) : undefined,
    attemptId: attempt ? resourceAttemptId(attempt) : undefined,
    workerId: normalizedWorkerId,
  }) as Pick<RuntimeAccessTarget, "runnerInstanceId" | "attemptId" | "workerId">;
}

function runtimeScheduleSlotForTrial(
  resources: RuntimeResourceRecord[],
  trial: RuntimeResourceRecord,
): RuntimeResourceRecord | undefined {
  const coreRunId = stringField(trial.spec.core_run_id);
  const trialId = stringField(trial.spec.trial_id);
  const attempt = numberField(trial.spec.attempt);
  const scheduleIdx = numberField(trial.spec.schedule_idx);
  if (!trialId || attempt === null) {
    return undefined;
  }
  return resources.find((resource) => {
    if (resource.kind !== "ScheduleSlot" || runtimeAccessTargetUnreachableReason(resource)) {
      return false;
    }
    if (coreRunId && stringField(resource.spec.core_run_id) !== coreRunId) {
      return false;
    }
    if (scheduleIdx !== null && numberField(resource.spec.schedule_idx) !== scheduleIdx) {
      return false;
    }
    return stringField(resource.status.trial_id) === trialId
      && numberField(resource.status.attempt) === attempt;
  });
}

function runtimeScheduleSlotForContainer(
  resources: RuntimeResourceRecord[],
  container: RuntimeResourceRecord,
): RuntimeResourceRecord | undefined {
  const coreRunId = stringField(container.spec.core_run_id);
  const trialId = stringField(container.spec.trial_id);
  const attempt = numberField(container.spec.attempt);
  const scheduleIdx = numberField(container.spec.schedule_idx);
  if (!trialId || attempt === null) {
    return undefined;
  }
  return resources.find((resource) => {
    if (resource.kind !== "ScheduleSlot" || runtimeAccessTargetUnreachableReason(resource)) {
      return false;
    }
    if (coreRunId && stringField(resource.spec.core_run_id) !== coreRunId) {
      return false;
    }
    if (scheduleIdx !== null && numberField(resource.spec.schedule_idx) !== scheduleIdx) {
      return false;
    }
    return stringField(resource.status.trial_id) === trialId
      && numberField(resource.status.attempt) === attempt;
  });
}

function resourceRunnerInstanceId(resource: RuntimeResourceReadable): string | undefined {
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  return stringField(resource.spec.runner_instance_id)
    ?? stringField(resource.status.runner_instance_id)
    ?? stringField(runnerBinding.runner_instance_id)
    ?? resource.metadata.labels["bucephalus.dev/runner-instance-id"]
    ?? undefined;
}

function resourceRunnerInstanceStatus(resource: RuntimeResourceReadable): string | undefined {
  const access = isRecord(resource.status.access) ? resource.status.access : {};
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  return stringField(access.runner_instance_status)
    ?? stringField(resource.status.runner_instance_status)
    ?? stringField(runnerBinding.runner_instance_status)
    ?? (resource.kind === "RunnerInstance" ? stringField(resource.status.phase) : undefined)
    ?? undefined;
}

function resourceAttemptId(resource: RuntimeResourceReadable): string | undefined {
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  return resource.kind === "RunnerAttempt"
    ? resource.metadata.uid ?? resource.metadata.name
    : stringField(resource.spec.attempt_id)
      ?? stringField(resource.status.attempt_id)
      ?? stringField(resource.status.current_attempt_id)
      ?? stringField(runnerBinding.attempt_id)
      ?? resource.metadata.labels["bucephalus.dev/attempt-id"]
      ?? resource.metadata.labels["bucephalus.dev/current-attempt-id"]
      ?? undefined;
}

function resourceWorkerId(resource: RuntimeResourceReadable): string | undefined {
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  return stringField(resource.spec.worker_id)
    ?? stringField(resource.status.worker_id)
    ?? stringField(runnerBinding.worker_id)
    ?? resource.metadata.labels["bucephalus.dev/worker-id"]
    ?? undefined;
}

type RuntimeSelectorOperator = "exists" | "not_exists" | "equals" | "not_equals" | "in" | "not_in";
type RuntimeSelectorValue = string | string[] | undefined;

interface RuntimeSelectorRequirement {
  key: string;
  operator: RuntimeSelectorOperator;
  value?: string;
  values?: string[];
}

function parseResourceSelector(
  selector: string | null | undefined,
  pointer: string,
  allowExistence: boolean,
  allowSetExpressions = allowExistence,
): RuntimeSelectorRequirement[] {
  if (!selector?.trim()) {
    return [];
  }
  return splitResourceSelector(selector, pointer)
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => parseResourceSelectorRequirement(part, pointer, allowExistence, allowSetExpressions));
}

function splitResourceSelector(selector: string, pointer: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < selector.length; i += 1) {
    const char = selector[i];
    if (char === "(") {
      depth += 1;
    } else if (char === ")") {
      depth -= 1;
      if (depth < 0) {
        throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} contains an unmatched ')'`);
      }
    } else if (char === "," && depth === 0) {
      parts.push(selector.slice(start, i));
      start = i + 1;
    }
  }
  if (depth !== 0) {
    throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} contains an unmatched '('`);
  }
  parts.push(selector.slice(start));
  return parts;
}

function parseResourceSelectorRequirement(
  raw: string,
  pointer: string,
  allowExistence: boolean,
  allowSetExpressions: boolean,
): RuntimeSelectorRequirement {
  const setRequirement = parseResourceSetSelectorRequirement(raw, pointer, allowSetExpressions);
  if (setRequirement) {
    return setRequirement;
  }
  if (/\s+(?:notin|in)\b/.test(raw)) {
    throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} set expressions must use key in (value,...) or key notin (value,...)`);
  }
  if (raw.includes("!=")) {
    const [key, value] = raw.split("!=", 2);
    return selectorKeyValue(key, value, "not_equals", pointer);
  }
  if (raw.includes("==")) {
    const [key, value] = raw.split("==", 2);
    return selectorKeyValue(key, value, "equals", pointer);
  }
  if (raw.includes("=")) {
    const [key, value] = raw.split("=", 2);
    return selectorKeyValue(key, value, "equals", pointer);
  }
  if (!allowExistence) {
    throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} requires key=value or key!=value expressions`);
  }
  if (raw.startsWith("!")) {
    const key = raw.slice(1).trim();
    if (!key) {
      throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} contains an empty selector key`);
    }
    return { key, operator: "not_exists" };
  }
  return { key: raw, operator: "exists" };
}

function parseResourceSetSelectorRequirement(
  raw: string,
  pointer: string,
  allowSetExpressions: boolean,
): RuntimeSelectorRequirement | null {
  const match = raw.match(/^(.+?)\s+(notin|in)\s*\((.*)\)$/);
  if (!match) {
    return null;
  }
  if (!allowSetExpressions) {
    throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} does not support in/notin expressions`);
  }
  const key = match[1]?.trim() ?? "";
  const operator = match[2] === "notin" ? "not_in" : "in";
  const values = (match[3] ?? "")
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean);
  if (!key || values.length === 0) {
    throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} contains an empty selector key or value`);
  }
  return { key, operator, values };
}

function selectorKeyValue(
  rawKey: string | undefined,
  rawValue: string | undefined,
  operator: "equals" | "not_equals",
  pointer: string,
): RuntimeSelectorRequirement {
  const key = rawKey?.trim() ?? "";
  const value = rawValue?.trim() ?? "";
  if (!key || !value) {
    throw new HttpError(400, "invalid_runtime_resource_selector", `${pointer} contains an empty selector key or value`);
  }
  return { key, operator, value };
}

function normalizeRuntimeKindSelectorRequirements(requirements: RuntimeSelectorRequirement[]): RuntimeSelectorRequirement[] {
  return requirements.map((requirement) => {
    if (!runtimeKindSelectorKey(requirement.key)) {
      return requirement;
    }
    return {
      ...requirement,
      ...(requirement.value ? { value: canonicalRuntimeResourceKind(requirement.value) ?? requirement.value } : {}),
      ...(requirement.values ? { values: requirement.values.map((value) => canonicalRuntimeResourceKind(value) ?? value) } : {}),
    };
  });
}

function runtimeKindSelectorKey(key: string): boolean {
  return key === "kind"
    || key === "metadata.ownerReferences.kind"
    || key === "spec.resource_kind"
    || key === "status.resource_kind"
    || key === "audit.resource_kind";
}

function resourceSelectorMatches(requirement: RuntimeSelectorRequirement, value: RuntimeSelectorValue): boolean {
  const values = selectorValues(value);
  switch (requirement.operator) {
    case "exists":
      return values.length > 0;
    case "not_exists":
      return values.length === 0;
    case "equals":
      return values.some((item) => item === requirement.value);
    case "not_equals":
      return values.every((item) => item !== requirement.value);
    case "in":
      return values.some((item) => (requirement.values ?? []).includes(item));
    case "not_in":
      return values.every((item) => !(requirement.values ?? []).includes(item));
  }
}

function selectorValues(value: RuntimeSelectorValue): string[] {
  if (value === undefined) {
    return [];
  }
  if (Array.isArray(value)) {
    return value.filter((item) => item !== "");
  }
  return value === "" ? [] : [value];
}

function isRuntimeAccessTargetKind(kind: string): boolean {
  return RUNTIME_ACCESS_TARGET_KINDS.has(kind);
}

function runtimeAccessTargetUnreachableReason(resource: RuntimeResourceReadable): string | null {
  const phase = stringField(resource.status.phase)?.toLowerCase() ?? "";
  switch (resource.kind) {
    case "Run":
      if (phase !== "running") {
        return `phase is ${phase || "unknown"}`;
      }
      {
        const runnerStatus = resourceRunnerInstanceStatus(resource)?.toLowerCase() ?? null;
        if (runtimeResourceHasAccessBinding(resource) && !runtimeRunnerStatusAllowsAccess(runnerStatus)) {
          return `runner status is ${runnerStatus ?? "unknown"}`;
        }
      }
      if (!runtimeResourceHasAccessBinding(resource)) {
        return "run has no active runner attempt";
      }
      return null;
    case "RunnerInstance": {
      if (phase !== "online" && phase !== "cordoned" && phase !== "draining") {
        return `phase is ${phase || "unknown"}`;
      }
      if ((numberField(resource.status.active_attempts) ?? 0) <= 0 && !stringField(resource.status.current_attempt_id)) {
        return "runner instance has no active run attempt";
      }
      return null;
    }
    case "RunnerAttempt": {
      const runnerStatus = stringField(resource.status.runner_instance_status)?.toLowerCase() ?? "";
      if (phase !== "running") {
        return `phase is ${phase || "unknown"}`;
      }
      if (runnerStatus && runnerStatus !== "online" && runnerStatus !== "cordoned" && runnerStatus !== "draining") {
        return `runner status is ${runnerStatus}`;
      }
      return null;
    }
    case "Trial":
      if (phase !== "running" && phase !== "active" && phase !== "leased") {
        return `phase is ${phase || "unknown"}`;
      }
      if (!stringField(resource.spec.trial_id)) {
        return "trial is missing trial identity";
      }
      if (numberField(resource.spec.attempt) === null) {
        return "trial is missing attempt identity";
      }
      if (!runtimeResourceHasRunnerBinding(resource)) {
        return "trial is not bound to an active runner attempt";
      }
      {
        const reason = runtimeBoundRunnerUnreachableReason(resource);
        if (reason) return reason;
      }
      return null;
    case "ScheduleSlot":
      if (phase !== "active" && phase !== "leased" && phase !== "running") {
        return `phase is ${phase || "unknown"}`;
      }
      if (!stringField(resource.status.trial_id)) {
        return "slot has not been assigned to a trial";
      }
      if (numberField(resource.status.attempt) === null) {
        return "slot is missing attempt identity";
      }
      if (!resourceWorkerId(resource)) {
        return "slot has no active worker assignment";
      }
      if (!runtimeResourceHasRunnerBinding(resource)) {
        return "slot is not bound to an active runner attempt";
      }
      {
        const reason = runtimeBoundRunnerUnreachableReason(resource);
        if (reason) return reason;
      }
      return null;
    case "TrialContainer":
      if (phase !== "running" && phase !== "active") {
        return `phase is ${phase || "unknown"}`;
      }
      if (!stringField(resource.spec.trial_id)) {
        return "container is missing trial identity";
      }
      if (numberField(resource.spec.attempt) === null) {
        return "container is missing attempt identity";
      }
      if (!runtimeResourceHasRunnerBinding(resource)) {
        return "container is not bound to an active runner attempt";
      }
      {
        const reason = runtimeBoundRunnerUnreachableReason(resource);
        if (reason) return reason;
      }
      return null;
    default:
      return null;
  }
}

function runtimeBoundRunnerUnreachableReason(resource: RuntimeResourceReadable): string | null {
  const binding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  const runnerStatus = stringField(binding.runner_instance_status)?.toLowerCase() ?? null;
  if (runtimeResourceHasRunnerBinding(resource) && !runtimeRunnerStatusAllowsAccess(runnerStatus)) {
    return `runner status is ${runnerStatus ?? "unknown"}`;
  }
  return null;
}

function runtimeResourceHasRunnerBinding(resource: RuntimeResourceReadable): boolean {
  const binding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  return Boolean(stringField(binding.attempt_id) && stringField(binding.runner_instance_id));
}

function runtimeResourceHasAccessBinding(resource: RuntimeResourceReadable): boolean {
  const access = isRecord(resource.status.access) ? resource.status.access : {};
  return Boolean(stringField(access.attempt_id) && stringField(access.runner_instance_id));
}

function labelValue(resource: RuntimeResourceRecord, key: string): string | undefined {
  return resource.metadata.labels[key];
}

function fieldValue(resource: RuntimeResourceRecord, path: string): RuntimeSelectorValue {
  if (path === "kind") {
    return resource.kind;
  }
  if (path === "metadata.name") {
    return resource.metadata.name;
  }
  if (path === "metadata.uid") {
    return resource.metadata.uid;
  }
  if (path === "metadata.generation") {
    return selectorComparableValue(resource.metadata.generation);
  }
  if (path === "metadata.resourceVersion") {
    return resource.metadata.resourceVersion;
  }
  if (path === "metadata.creationTimestamp") {
    return resource.metadata.creationTimestamp;
  }
  if (path === "metadata.deletionTimestamp") {
    return resource.metadata.deletionTimestamp;
  }
  if (path.startsWith("metadata.labels.")) {
    return resource.metadata.labels[path.slice("metadata.labels.".length)];
  }
  if (path === "metadata.ownerReferences") {
    return resource.metadata.ownerReferences.map((owner) => `${owner.kind}/${owner.name}`);
  }
  if (path === "metadata.ownerReferences.apiVersion") {
    return resource.metadata.ownerReferences.map((owner) => owner.apiVersion);
  }
  if (path === "metadata.ownerReferences.kind") {
    return resource.metadata.ownerReferences.map((owner) => owner.kind);
  }
  if (path === "metadata.ownerReferences.name") {
    return resource.metadata.ownerReferences.map((owner) => owner.name);
  }
  if (path === "metadata.ownerReferences.uid") {
    return resource.metadata.ownerReferences.map((owner) => owner.uid ?? "").filter(Boolean);
  }
  if (path.startsWith("status.conditions.")) {
    return runtimeResourceConditionFieldValue(resource, path.slice("status.conditions.".length));
  }
  for (const [prefix, value] of [
    ["spec.", resource.spec],
    ["status.", resource.status],
    ["audit.", resource.audit],
  ] as const) {
    if (path.startsWith(prefix)) {
      return selectorComparableValue(jsonPathValue(value, path.slice(prefix.length)));
    }
  }
  throw new HttpError(
    400,
    "invalid_runtime_resource_selector",
    `field_selector path '${path}' is not supported; use kind, metadata.name, metadata.uid, metadata.generation, metadata.resourceVersion, metadata.creationTimestamp, metadata.deletionTimestamp, metadata.labels.<key>, metadata.ownerReferences, metadata.ownerReferences.apiVersion, metadata.ownerReferences.kind, metadata.ownerReferences.name, metadata.ownerReferences.uid, spec.<path>, status.conditions.<type>, status.conditions.<type>.status, status.conditions.<type>.reason, status.conditions.<type>.message, status.<path>, or audit.<path>`,
  );
}

function runtimeResourceConditionFieldValue(resource: RuntimeResourceRecord, conditionPath: string): RuntimeSelectorValue {
  const [conditionType, field = "status", ...rest] = conditionPath.split(".");
  if (!conditionType || rest.length > 0 || !["status", "reason", "message"].includes(field)) {
    return undefined;
  }
  const condition = runtimeResourceStatusConditions(resource)
    .find((item) => item.type.toLowerCase() === conditionType.toLowerCase());
  if (!condition) {
    return undefined;
  }
  if (field === "status") return condition.status;
  if (field === "reason") return condition.reason;
  return condition.message;
}

function jsonPathValue(root: JsonObject, path: string): unknown {
  if (!path) {
    return undefined;
  }
  return path.split(".").reduce<unknown>((cursor, key) => {
    if (!isRecord(cursor)) {
      return undefined;
    }
    return cursor[key];
  }, root);
}

function selectorComparableValue(value: unknown): string | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return JSON.stringify(value);
}

function runtimeEventListView(
  cloudRunId: string,
  input: RuntimeEventRowsInput,
  limit: number,
  events: RuntimeEventRecord[],
): RuntimeEventList {
  const page = runtimeEventListPage(input, limit, events);
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RuntimeEventList",
    cloud_run_id: cloudRunId,
    generated_at: new Date().toISOString(),
    event_filter: runtimeEventListFilter(input),
    metadata: page.metadata,
    events: page.events,
  };
}

function runtimeEventRowsInputWithContinue(input: RuntimeEventRowsInput = {}): RuntimeEventRowsInput {
  const afterRowSeq = runtimeEventAfterRowSeq(input);
  const normalized: RuntimeEventRowsInput = { ...input };
  delete normalized.continueToken;
  if (afterRowSeq === undefined) {
    delete normalized.afterRowSeq;
  } else {
    normalized.afterRowSeq = afterRowSeq;
  }
  return normalized;
}

function runtimeEventAfterRowSeq(input: Pick<RuntimeEventRowsInput, "afterRowSeq" | "continueToken">): number | undefined {
  const continueToken = input.continueToken?.trim() || null;
  if (input.afterRowSeq !== undefined && continueToken !== null) {
    throw new HttpError(400, "invalid_runtime_event_cursor", "Runtime event queries accept either afterRowSeq or continueToken, not both");
  }
  if (continueToken !== null) {
    return runtimeEventContinueRowSeq(continueToken);
  }
  return input.afterRowSeq;
}

function runtimeEventContinueRowSeq(token: string): number {
  const match = /^event-row-seq:(\d+)$/.exec(token.trim());
  if (!match) {
    throw new HttpError(400, "invalid_runtime_event_continue", "Runtime event continueToken must be formatted as event-row-seq:<row_seq>");
  }
  const rowSeq = Number.parseInt(match[1] ?? "", 10);
  if (!Number.isSafeInteger(rowSeq) || rowSeq < 0) {
    throw new HttpError(400, "invalid_runtime_event_continue", "Runtime event continueToken must be formatted as event-row-seq:<row_seq>");
  }
  return rowSeq;
}

function runtimeResourceEventListView(
  cloudRunId: string,
  coreRunIds: string[],
  resource: RuntimeResourceRecord,
  input: RuntimeEventRowsInput,
  limit: number,
  events: RuntimeEventRecord[],
): RuntimeResourceEventList {
  const page = runtimeEventListPage(input, limit, events);
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RuntimeResourceEventList",
    cloud_run_id: cloudRunId,
    generated_at: new Date().toISOString(),
    core_run_ids: coreRunIds,
    resource,
    event_filter: runtimeEventListFilter({
      ...input,
      resourceKind: resource.kind,
      resourceName: resource.metadata.name,
    }),
    metadata: page.metadata,
    events: page.events,
  };
}

function runtimeResourceEventScanFilter(resource: RuntimeResourceRecord, filter: RuntimeEventFilter | undefined): RuntimeEventFilter {
  if (resource.kind === "Run" || resource.kind === "Event") {
    return filter ?? {};
  }
  return {
    ...(filter ?? {}),
    resourceKind: resource.kind,
    resourceName: resource.metadata.name,
  };
}

const RUNTIME_RUNNER_LOG_EVENT_TYPES = ["worker.core.completed", "worker.core.failed"] as const;

function runtimeResourceLogEventScanFilter(resource: RuntimeResourceRecord): RuntimeEventFilter {
  if (resource.kind === "RunnerAttempt" || resource.kind === "RunnerInstance") {
    return {
      eventTypes: [...RUNTIME_RUNNER_LOG_EVENT_TYPES],
    };
  }
  return runtimeResourceEventScanFilter(resource, undefined);
}

function runtimeEventListFilter(input: RuntimeEventFilter): RuntimeEventListFilter {
  return {
    event_types: input.eventTypes?.filter((eventType) => eventType.trim()) ?? [],
    sources: input.sources?.filter((source) => source.trim()) ?? [],
    resource_kind: input.resourceKind?.trim() || null,
    resource_name: input.resourceName?.trim() || null,
    trial_id: input.trialId?.trim() || null,
    task_id: input.taskId?.trim() || null,
  };
}

function runtimeEventListPage(
  input: Pick<RuntimeEventRowsInput, "afterRowSeq">,
  limit: number,
  events: RuntimeEventRecord[],
): { metadata: RuntimeEventListMetadata; events: RuntimeEventRecord[] } {
  const hasMore = events.length > limit;
  const normalizedEvents = events.slice(0, limit).map(runtimeEventRecordWithResourceRefs);
  const nextAfterRowSeq = runtimeEventNextAfterRowSeq(input.afterRowSeq, normalizedEvents);
  return {
    metadata: {
      resourceVersion: runtimeEventListResourceVersion(input.afterRowSeq, normalizedEvents),
      continue: hasMore && nextAfterRowSeq !== null ? runtimeEventContinueToken(nextAfterRowSeq) : null,
      remainingItemCount: hasMore ? null : 0,
      limit,
      returned: normalizedEvents.length,
      after_row_seq: input.afterRowSeq ?? null,
      next_after_row_seq: nextAfterRowSeq,
    },
    events: normalizedEvents,
  };
}

function runtimeEventListResourceVersion(afterRowSeq: number | undefined, events: RuntimeEventRecord[]): string {
  return runtimeEventContinueToken(runtimeEventNextAfterRowSeq(afterRowSeq, events) ?? afterRowSeq ?? 0);
}

function runtimeEventContinueToken(rowSeq: number): string {
  return `event-row-seq:${rowSeq}`;
}

function runtimeEventNextAfterRowSeq(afterRowSeq: number | undefined, events: RuntimeEventRecord[]): number | null {
  const observed = events.reduce<number | null>((max, event) => {
    const value = Number.isFinite(event.row_seq) ? event.row_seq : event.seq;
    if (!Number.isFinite(value)) return max;
    return max === null ? value : Math.max(max, value);
  }, null);
  if (observed !== null) return Math.max(afterRowSeq ?? observed, observed);
  return afterRowSeq ?? null;
}

export function runtimeResourceEventsForResource(events: RuntimeEventRecord[], resource: RuntimeResourceRecord): RuntimeEventRecord[] {
  if (resource.kind === "Run") {
    return events;
  }
  if (resource.kind === "Event") {
    const eventUid = stringField(resource.spec.event_uid) ?? resource.metadata.uid;
    return events.filter((event) => {
      const currentUid = runtimeEventUid(event);
      return (eventUid && currentUid === eventUid)
        || resource.metadata.name === runtimeEventResourceName(event);
    });
  }
  const refs = resourceEventRefs(resource);
  return events.filter((event) => runtimeEventMatchesRefs(event, refs));
}

function hasRuntimeEventFilter(filter: RuntimeEventFilter): boolean {
  return Boolean(
    filter.eventTypes?.length
      || filter.sources?.length
      || normalizeOptionalString(filter.resourceKind)
      || normalizeOptionalString(filter.resourceName)
      || normalizeOptionalString(filter.trialId)
      || normalizeOptionalString(filter.taskId),
  );
}

function runtimeEventMatchesFilter(event: RuntimeEventRecord, filter: RuntimeEventFilter): boolean {
  if (!matchesEventSet(event.event_type, filter.eventTypes)) {
    return false;
  }
  if (!matchesEventSet(event.source, filter.sources)) {
    return false;
  }
  const resourceKind = normalizeOptionalString(filter.resourceKind);
  const resourceName = normalizeOptionalString(filter.resourceName);
  if ((resourceKind || resourceName) && !runtimeEventMatchesResourceFilter(event, resourceKind, resourceName)) {
    return false;
  }
  const trialId = normalizeOptionalString(filter.trialId);
  if (trialId && !eventValueEquals(trialId, event.trial_id || stringField(event.payload.trial_id) || stringField(event.row.trial_id))) {
    return false;
  }
  const taskId = normalizeOptionalString(filter.taskId);
  if (taskId && !eventValueEquals(taskId, event.task_id || stringField(event.payload.task_id) || stringField(event.row.task_id))) {
    return false;
  }
  return true;
}

function runtimeEventMatchesResourceFilter(
  event: RuntimeEventRecord,
  resourceKind: string | null,
  resourceName: string | null,
): boolean {
  const refs = runtimeEventResourceRefs(event);
  const expectedKind = resourceKind ? canonicalRuntimeResourceKind(resourceKind) ?? resourceKind : "";
  if (resourceKind && resourceName) {
    return refs.some((ref) =>
      eventValueEquals(expectedKind, ref.kind)
        && eventValueEquals(resourceName, ref.name)
    );
  }
  if (resourceKind) {
    return refs.some((ref) => eventValueEquals(expectedKind, ref.kind));
  }
  if (resourceName) {
    return refs.some((ref) => eventValueEquals(resourceName, ref.name));
  }
  return true;
}

function runtimeEventRecordWithResourceRefs(event: RuntimeEventRecord): RuntimeEventRecord {
  return {
    ...event,
    resource_refs: runtimeEventResourceRefsForWire(event),
  };
}

function runtimeEventResourceRefsForWire(event: RuntimeEventRecord): RuntimeResourceWatchRef[] {
  return runtimeEventPrimaryFirstResourceRefs(event).flatMap((ref) => {
    const kind = normalizeOptionalString(ref.kind);
    const name = normalizeOptionalString(ref.name);
    if (!kind || !name) {
      return [];
    }
    const wireRef: RuntimeResourceWatchRef = {
      apiVersion: RUNTIME_API_VERSION,
      kind: canonicalRuntimeResourceKind(kind) ?? kind,
      name,
    };
    const uid = normalizeOptionalString(ref.uid);
    if (uid) {
      wireRef.uid = uid;
    }
    return [wireRef];
  });
}

function runtimeEventResourceRefs(event: RuntimeEventRecord): Array<{ kind: string | null; name: string | null; uid: string | null }> {
  const refs = [
    ...runtimeEventResourceRefsFromArray((event as { resource_refs?: unknown }).resource_refs),
    runtimeEventResourceRefFromObject(event.payload),
    runtimeEventResourceRefFromObject(event.row),
    runtimeEventResourceRefFromObject(isRecord(event.payload.resource_ref) ? event.payload.resource_ref : null),
    runtimeEventResourceRefFromObject(isRecord(event.payload.access_resource_ref) ? event.payload.access_resource_ref : null),
    runtimeEventResourceRefFromObject(isRecord(event.payload.resolved_target) ? event.payload.resolved_target : null),
    runtimeEventResourceRefFromObject(isRecord(event.payload.target_ref) ? event.payload.target_ref : null),
  ];
  return uniqueRuntimeEventResourceRefs(refs);
}

function runtimeEventResourceRefsFromArray(value: unknown): Array<{ kind: string | null; name: string | null; uid: string | null }> {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.map((item) => runtimeEventResourceRefFromObject(item));
}

function runtimeEventResourceRefFromObject(value: unknown): { kind: string | null; name: string | null; uid: string | null } {
  const record = isRecord(value) ? value : {};
  return {
    kind: stringField(record.resource_kind) ?? stringField(record.kind),
    name: stringField(record.resource_name) ?? stringField(record.name),
    uid: stringField(record.resource_uid) ?? stringField(record.uid),
  };
}

function uniqueRuntimeEventResourceRefs(
  refs: Array<{ kind: string | null; name: string | null; uid: string | null }>,
): Array<{ kind: string | null; name: string | null; uid: string | null }> {
  const byIdentity = new Map<string, { kind: string | null; name: string | null; uid: string | null }>();
  const order: string[] = [];
  for (const ref of refs) {
    const rawKind = normalizeOptionalString(ref.kind);
    const kind = rawKind ? canonicalRuntimeResourceKind(rawKind) ?? rawKind : null;
    const name = normalizeOptionalString(ref.name);
    const uid = normalizeOptionalString(ref.uid);
    if (!kind && !name && !uid) {
      continue;
    }
    const key = kind && name
      ? `${kind.toLowerCase()}/${name.toLowerCase()}`
      : `${kind?.toLowerCase() ?? ""}/${name?.toLowerCase() ?? ""}/${uid?.toLowerCase() ?? ""}`;
    const existing = byIdentity.get(key);
    if (!existing) {
      byIdentity.set(key, { kind, name, uid });
      order.push(key);
      continue;
    }
    byIdentity.set(key, {
      kind: existing.kind ?? kind,
      name: existing.name ?? name,
      uid: existing.uid ?? uid,
    });
  }
  return order
    .map((key) => byIdentity.get(key))
    .filter((ref): ref is { kind: string | null; name: string | null; uid: string | null } => Boolean(ref));
}

function matchesEventSet(value: string, filters?: string[]): boolean {
  const normalized = new Set(runtimeEventFilterValues(filters));
  if (normalized.size === 0) {
    return true;
  }
  return normalized.has(value);
}

function runtimeEventFilterValues(filters?: readonly string[]): string[] {
  const values: string[] = [];
  const seen = new Set<string>();
  for (const filter of filters ?? []) {
    const value = filter.trim();
    if (!value || seen.has(value)) {
      continue;
    }
    values.push(value);
    seen.add(value);
  }
  return values;
}

function eventValueEquals(expected: string, actual: string | null | undefined): boolean {
  return Boolean(actual && actual.toLowerCase() === expected.toLowerCase());
}

export function runtimeLogTargetForResource(resource: RuntimeResourceRecord): { coreRunId?: string | null; trialId: string; attempt?: number | undefined } {
  if (resource.kind === "Trial") {
    const trialId = stringField(resource.spec.trial_id);
    if (!trialId) {
      throw new HttpError(409, "runtime_logs_unavailable", "Trial is missing trial identity");
    }
    return {
      coreRunId: stringField(resource.spec.core_run_id),
      trialId,
      attempt: numberField(resource.spec.attempt) ?? undefined,
    };
  }
  if (resource.kind === "TrialContainer") {
    const trialId = stringField(resource.spec.trial_id);
    if (!trialId) {
      throw new HttpError(409, "runtime_logs_unavailable", "TrialContainer is missing trial identity");
    }
    return {
      coreRunId: stringField(resource.spec.core_run_id),
      trialId,
      attempt: numberField(resource.spec.attempt) ?? undefined,
    };
  }
  if (resource.kind === "ScheduleSlot") {
    const trialId = stringField(resource.status.trial_id);
    if (!trialId) {
      throw new HttpError(409, "runtime_logs_unavailable", "ScheduleSlot has not been assigned to a trial");
    }
    return {
      coreRunId: stringField(resource.spec.core_run_id),
      trialId,
      attempt: numberField(resource.status.attempt) ?? undefined,
    };
  }
  throw new HttpError(409, "runtime_logs_unavailable", `Runtime logs are not available for ${resource.kind} resources`, {
    kind: resource.kind,
    name: resource.metadata.name,
  });
}

function runnerResourceLogsFromEvents(
  cloudRunId: string,
  resource: RuntimeResourceRecord,
  events: RuntimeEventRecord[],
  stream: "stdout" | "stderr",
  tailLines: number | undefined,
): RuntimeResourceLogs {
  const runnerInstanceId = resourceRunnerInstanceId(resource);
  const attemptId = resourceAttemptId(resource);
  const workerId = resourceWorkerId(resource);
  const attemptIds = runnerResourceAttemptIds(resource);
  const workerIds = runnerResourceWorkerIds(resource);
  const refs = {
    runnerInstanceId: resource.kind === "RunnerInstance" ? runnerInstanceId : undefined,
    attemptIds: new Set(attemptIds),
    workerIds: new Set(workerIds),
  };
  const logEvents = events.filter((event) => runnerResourceLogEventMatches(event, refs));
  const tailKey = stream === "stdout" ? "stdout_tail" : "stderr_tail";
  const chunks = logEvents
    .map((event) => ({ event, text: runtimeEventPayloadText(event, tailKey) }))
    .filter((chunk): chunk is { event: RuntimeEventRecord; text: string } => chunk.text !== null);
  const text = joinLogChunks(chunks.map((chunk) => chunk.text));
  const allBytes = new TextEncoder().encode(text);
  const bytes = tailLines === undefined ? allBytes : tailTextBytes(allBytes, tailLines);
  const sourceEvents = chunks.map((chunk) => chunk.event);
  const latestEvent = sourceEvents[sourceEvents.length - 1] ?? logEvents[logEvents.length - 1];
  const mediaType = "text/plain; charset=utf-8";
  const resourceSlug = resource.kind === "RunnerInstance" ? "runner-instance" : "runner-attempt";
  const objectRef = `runtime://cloud-run/${encodeURIComponent(cloudRunId)}/${resourceSlug}/${encodeURIComponent(resource.metadata.name)}/${stream}`;
  const object: RuntimeAttemptObjectRecord = {
    core_run_id: firstNonemptyString([
      stringField(resource.spec.core_run_id),
      ...sourceEvents.map((event) => event.core_run_id),
      latestEvent?.core_run_id,
    ]) ?? "",
    trial_id: "",
    schedule_idx: numberField(latestEvent?.schedule_idx) ?? -1,
    attempt: numberField(resource.status.attempt) ?? numberField(resource.spec.attempt) ?? numberField(latestEvent?.attempt) ?? 0,
    role: stream,
    object_ref: objectRef,
    metadata: compactObject({
      source: "cloud.run_events",
      event_types: uniqueStrings(sourceEvents.map((event) => event.event_type)),
      resource_kind: resource.kind,
      resource_name: resource.metadata.name,
      runner_instance_id: runnerInstanceId,
      attempt_id: resource.kind === "RunnerAttempt" ? attemptId : undefined,
      attempt_ids: resource.kind === "RunnerInstance" ? attemptIds : undefined,
      worker_id: resource.kind === "RunnerAttempt" ? workerId : undefined,
      worker_ids: resource.kind === "RunnerInstance" ? workerIds : undefined,
      log_events: sourceEvents.map((event) => compactObject({
        source: event.source,
        event_type: event.event_type,
        row_seq: event.row_seq,
        seq: event.seq,
        ts: event.ts ?? undefined,
      })),
    }),
    recorded_at_ms: latestEvent ? eventTime(latestEvent) : 0,
    content_available: true,
    media_type: mediaType,
    byte_size: bytes.byteLength,
    sha256: sha256Digest(bytes),
    relative_path: `${resourceSlug}/${resource.metadata.name}/${stream}.log`,
  };
  return {
    resource,
    stream,
    media_type: mediaType,
    bytes,
    object,
  };
}

function execResourceLogs(
  cloudRunId: string,
  resource: RuntimeResourceRecord,
  stream: "stdout" | "stderr",
  tailLines: number | undefined,
): RuntimeResourceLogs {
  const connection = isRecord(resource.status.connection) ? resource.status.connection : {};
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  const tailKey = stream === "stdout" ? "stdout_tail" : "stderr_tail";
  const streamKey = stream === "stdout" ? "stdout" : "stderr";
  const text = stringField(connection[tailKey]) ?? stringField(connection[streamKey]) ?? "";
  const allBytes = new TextEncoder().encode(text);
  const bytes = tailLines === undefined ? allBytes : tailTextBytes(allBytes, tailLines);
  const mediaType = "text/plain; charset=utf-8";
  const objectRef = `runtime://cloud-run/${encodeURIComponent(cloudRunId)}/exec/${encodeURIComponent(resource.metadata.name)}/${stream}`;
  const object: RuntimeAttemptObjectRecord = {
    core_run_id: "",
    trial_id: stringField(resource.spec.resource_name) ?? "",
    schedule_idx: -1,
    attempt: 0,
    role: stream,
    object_ref: objectRef,
    metadata: compactObject({
      source: "cloud.runtime_access_requests",
      resource_kind: resource.kind,
      resource_name: resource.metadata.name,
      target_ref: isRecord(resource.spec.target_ref) ? resource.spec.target_ref : undefined,
      runner_instance_id: stringField(resource.status.runner_instance_id) ?? stringField(runnerBinding.runner_instance_id),
      attempt_id: stringField(resource.status.attempt_id) ?? stringField(runnerBinding.attempt_id),
      phase: stringField(resource.status.phase),
      exit_code: numberField(connection.exit_code),
    }),
    recorded_at_ms: Date.parse(stringField(resource.metadata.updated_at) ?? stringField(resource.metadata.created_at) ?? "") || 0,
    content_available: true,
    media_type: mediaType,
    byte_size: bytes.byteLength,
    sha256: sha256Digest(bytes),
    relative_path: `exec/${resource.metadata.name}/${stream}.log`,
  };
  return {
    resource,
    stream,
    media_type: mediaType,
    bytes,
    object,
  };
}

function runnerResourceLogEventMatches(
  event: RuntimeEventRecord,
  refs: { runnerInstanceId?: string | undefined; attemptIds: Set<string>; workerIds: Set<string> },
): boolean {
  if (!matchesEventSet(event.event_type, [...RUNTIME_RUNNER_LOG_EVENT_TYPES])) {
    return false;
  }
  if (refs.runnerInstanceId && eventValueEquals(refs.runnerInstanceId, stringField(event.payload.runner_instance_id) ?? stringField(event.row.runner_instance_id))) {
    return true;
  }
  if (setHas(refs.attemptIds, stringField(event.payload.attempt_id) ?? stringField(event.row.attempt_id))) {
    return true;
  }
  if (setHas(refs.workerIds, stringField(event.payload.worker_id) ?? stringField(event.row.worker_id))) {
    return true;
  }
  return false;
}

function runnerResourceAttemptIds(resource: RuntimeResourceRecord): string[] {
  return uniqueStrings([
    resourceAttemptId(resource),
    stringField(resource.spec.attempt_id),
    stringField(resource.status.attempt_id),
    stringField(resource.status.current_attempt_id),
    ...stringFields(resource.spec.attempt_ids),
  ]);
}

function runnerResourceWorkerIds(resource: RuntimeResourceRecord): string[] {
  return uniqueStrings([
    resourceWorkerId(resource),
    stringField(resource.spec.worker_id),
    stringField(resource.status.worker_id),
    ...stringFields(resource.spec.worker_ids),
  ]);
}

function runtimeEventPayloadText(event: RuntimeEventRecord, key: string): string | null {
  const value = event.payload[key];
  return typeof value === "string" ? value : null;
}

function joinLogChunks(chunks: string[]): string {
  let text = "";
  for (const chunk of chunks) {
    if (text && chunk && !text.endsWith("\n") && !chunk.startsWith("\n")) {
      text += "\n";
    }
    text += chunk;
  }
  return text;
}

function firstNonemptyString(values: Array<string | null | undefined>): string | undefined {
  return values.find((value): value is string => Boolean(value?.trim()));
}

function uniqueStrings(values: Array<string | null | undefined>): string[] {
  return [...stringSet(values)];
}

function runtimeLogStream(raw: string | null | undefined): "stdout" | "stderr" {
  const stream = (raw ?? "stdout").trim().toLowerCase();
  if (stream === "stdout" || stream === "stderr") {
    return stream;
  }
  throw new HttpError(400, "invalid_log_stream", "stream must be stdout or stderr");
}

function tailTextBytes(bytes: Uint8Array, tailLines: number): Uint8Array {
  const lines = Math.max(0, Math.min(Math.trunc(tailLines), 10_000));
  if (lines === 0) {
    return new Uint8Array();
  }
  const text = new TextDecoder().decode(bytes);
  const parts = text.split(/\r?\n/);
  const hadTrailingNewline = parts.length > 0 && parts[parts.length - 1] === "";
  const body = hadTrailingNewline ? parts.slice(0, -1) : parts;
  const tailed = body.slice(-lines).join("\n") + (hadTrailingNewline ? "\n" : "");
  return new TextEncoder().encode(tailed);
}

function resourceIdentity(resource: Pick<RuntimeResourceRecord, "kind"> & { metadata?: { name?: string }; name?: string }): string {
  const name = "metadata" in resource ? resource.metadata?.name : resource.name;
  const kind = canonicalRuntimeResourceKind(resource.kind) ?? resource.kind;
  return `${kind.toLowerCase()}/${String(name ?? "").toLowerCase()}`;
}

function runtimeResourceWatchKey(resource: Pick<RuntimeResourceRecord, "kind"> & { metadata?: { name?: string }; name?: string }): string {
  return resourceIdentity(resource);
}

function runtimeResourceKnownVersions(versions: Map<string, string> | undefined): Map<string, string> {
  const normalized = new Map<string, string>();
  for (const [key, version] of versions ?? new Map<string, string>()) {
    normalized.set(runtimeResourceWatchKeyFromString(key), version);
  }
  return normalized;
}

function runtimeResourceWatchKeyFromString(key: string): string {
  const separator = key.indexOf("/");
  if (separator === -1) {
    return key.trim().toLowerCase();
  }
  const rawKind = key.slice(0, separator).trim();
  const name = key.slice(separator + 1).trim();
  const kind = canonicalRuntimeResourceKind(rawKind) ?? rawKind;
  return `${kind.toLowerCase()}/${name.toLowerCase()}`;
}

function runtimeResourceWatchRef(resource: RuntimeResourceRecord): RuntimeResourceWatchRef {
  const ref: RuntimeResourceWatchRef = {
    apiVersion: resource.apiVersion,
    kind: resource.kind,
    name: resource.metadata.name,
  };
  if (resource.metadata.uid) {
    ref.uid = resource.metadata.uid;
  }
  return ref;
}

function runtimeResourceWatchRefFromKey(key: string): RuntimeResourceWatchRef {
  const separator = key.indexOf("/");
  const rawKind = separator === -1 ? key : key.slice(0, separator);
  const kind = canonicalRuntimeResourceKind(rawKind) ?? rawKind;
  const name = separator === -1 ? "" : key.slice(separator + 1);
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind,
    name,
  };
}

interface RuntimeResourceEventRefs {
  resourceIdentity: Set<string>;
  uid: Set<string>;
  accessRequestId: Set<string>;
  runnerPoolId: Set<string>;
  runnerInstanceId: Set<string>;
  attemptId: Set<string>;
  workerId: Set<string>;
  coreRunId: Set<string>;
  trialId: Set<string>;
}

function resourceEventRefs(resource: RuntimeResourceRecord): RuntimeResourceEventRefs {
  const labels = resource.metadata.labels;
  const runnerPoolResource = resource.kind === "RunnerPool";
  const runnerInstanceResource = resource.kind === "RunnerInstance";
  const runnerAttemptResource = resource.kind === "RunnerAttempt";
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  return {
    resourceIdentity: new Set([
      resourceIdentity(resource),
      `${resource.kind}/${resource.metadata.name}`,
    ]),
    uid: stringSet([
      resource.metadata.uid,
      stringField(resource.spec.access_request_id),
      stringField(resource.spec.attempt_id),
    ]),
    accessRequestId: stringSet([
      resource.kind === "PortForward" || resource.kind === "Exec" ? resource.metadata.uid : null,
      stringField(resource.spec.access_request_id),
    ]),
    runnerPoolId: stringSet([
      runnerPoolResource ? resource.metadata.uid : null,
      labels["bucephalus.dev/runner-pool-id"],
      stringField(resource.spec.runner_pool_id),
      stringField(resource.status.runner_pool_id),
      stringField(runnerBinding.runner_pool_id),
    ]),
    runnerInstanceId: stringSet([
      runnerInstanceResource ? resource.metadata.uid : null,
      labels["bucephalus.dev/runner-instance-id"],
      stringField(resource.spec.runner_instance_id),
      stringField(resource.status.runner_instance_id),
      stringField(runnerBinding.runner_instance_id),
    ]),
    attemptId: stringSet([
      runnerAttemptResource ? resource.metadata.uid : null,
      labels["bucephalus.dev/attempt-id"],
      labels["bucephalus.dev/current-attempt-id"],
      stringField(resource.spec.attempt_id),
      stringField(resource.status.attempt_id),
      stringField(resource.status.current_attempt_id),
      stringField(runnerBinding.attempt_id),
      ...stringFields(resource.spec.attempt_ids),
    ]),
    workerId: stringSet([
      ...(runnerInstanceResource ? stringFields(resource.spec.worker_ids) : []),
      labels["bucephalus.dev/worker-id"],
      stringField(resource.spec.worker_id),
      stringField(resource.status.worker_id),
      stringField(runnerBinding.worker_id),
    ]),
    coreRunId: stringSet([
      labels["bucephalus.dev/core-run-id"],
      stringField(resource.spec.core_run_id),
    ]),
    trialId: stringSet([
      labels["bucephalus.dev/trial-id"],
      stringField(resource.spec.trial_id),
      stringField(resource.status.trial_id),
    ]),
  };
}

function runtimeEventMatchesRefs(event: RuntimeEventRecord, refs: RuntimeResourceEventRefs): boolean {
  for (const ref of runtimeEventResourceRefs(event)) {
    if (ref.kind && ref.name && refs.resourceIdentity.has(`${ref.kind}/${ref.name}`)) {
      return true;
    }
    if (setHas(refs.uid, ref.uid)) {
      return true;
    }
  }
  if (setHas(refs.accessRequestId, stringField(event.payload.access_request_id))) {
    return true;
  }
  const runnerBinding = isRecord(event.payload.runner_binding) ? event.payload.runner_binding : {};
  if (setHas(refs.runnerPoolId, stringField(event.row.runner_pool_id) ?? stringField(event.payload.runner_pool_id) ?? stringField(runnerBinding.runner_pool_id))) {
    return true;
  }
  if (setHas(refs.runnerInstanceId, stringField(event.row.runner_instance_id) ?? stringField(event.payload.runner_instance_id) ?? stringField(runnerBinding.runner_instance_id))) {
    return true;
  }
  if (setHas(refs.attemptId, stringField(event.row.attempt_id) ?? stringField(event.payload.attempt_id) ?? stringField(runnerBinding.attempt_id))) {
    return true;
  }
  if (setHas(refs.workerId, stringField(event.row.worker_id) ?? stringField(event.payload.worker_id) ?? stringField(runnerBinding.worker_id))) {
    return true;
  }
  if (setHas(refs.coreRunId, event.core_run_id) || setHas(refs.coreRunId, stringField(event.payload.core_run_id))) {
    return true;
  }
  if (setHas(refs.trialId, event.trial_id) || setHas(refs.trialId, stringField(event.payload.trial_id))) {
    return true;
  }
  return false;
}

function stringSet(values: Array<string | null | undefined>): Set<string> {
  return new Set(values.map((value) => value?.trim()).filter((value): value is string => Boolean(value)));
}

function countStrings(values: string[]): Record<string, number> {
  return values.reduce<Record<string, number>>((counts, value) => {
    const key = value.trim() || "unknown";
    counts[key] = (counts[key] ?? 0) + 1;
    return counts;
  }, {});
}

function stringFields(value: unknown): string[] {
  if (Array.isArray(value)) {
    return value.map(stringField).filter((item): item is string => Boolean(item));
  }
  const single = stringField(value);
  return single ? [single] : [];
}

function setHas(values: Set<string>, value: string | null | undefined): boolean {
  return value ? values.has(value) : false;
}

function runOwnerReference(run: CloudRunRecord): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "Run",
    name: run.run_id,
    uid: run.run_id,
  };
}

function trialOwnerReference(coreRunId: string, trialId: string): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "Trial",
    name: resourceName(trialId),
    uid: trialUid(coreRunId, trialId),
  };
}

function trialUid(coreRunId: string, trialId: string): string {
  return `${coreRunId}:${trialId}`;
}

function trialStageResourceName(stage: RuntimeContractStageRecord): string {
  return [
    resourceName(stage.trial_id),
    stage.attempt,
    resourceName(stage.stage),
  ].join(".");
}

function trialStageUid(stage: RuntimeContractStageRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: stage.core_run_id,
    trial_id: stage.trial_id,
    attempt: stage.attempt,
    stage: stage.stage,
  }));
}

function runtimeValueResourceName(value: RuntimeValueRecord): string {
  return [
    resourceName(value.core_run_id),
    resourceName(value.key),
  ].join(".");
}

function runtimeValueUid(value: RuntimeValueRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: value.core_run_id,
    key: value.key,
  }));
}

function metricObservationResourceName(observation: RuntimeMetricObservationRecord): string {
  return [
    resourceName(observation.trial_id),
    observation.row_seq,
    resourceName(observation.metric_name),
  ].join(".");
}

function metricObservationUid(observation: RuntimeMetricObservationRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: observation.core_run_id,
    trial_id: observation.trial_id,
    attempt: observation.attempt,
    row_seq: observation.row_seq,
    metric_name: observation.metric_name,
  }));
}

function performanceSampleResourceName(sample: RuntimePerformanceSampleRecord): string {
  return [
    resourceName(sample.stage),
    resourceName(sample.sample_kind),
    shortResourceName(sample.sample_id),
  ].join(".");
}

function performanceSampleUid(sample: RuntimePerformanceSampleRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: sample.core_run_id,
    sample_id: sample.sample_id,
  }));
}

function slotCommitResourceName(commit: RuntimeSlotCommitRecord): string {
  return [
    resourceName(commit.core_run_id),
    String(commit.schedule_idx),
    String(commit.attempt),
    resourceName(commit.record_type),
  ].join(".");
}

function slotCommitUid(commit: RuntimeSlotCommitRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: commit.core_run_id,
    schedule_idx: commit.schedule_idx,
    attempt: commit.attempt,
    record_type: commit.record_type,
  }));
}

function pendingTrialCompletionResourceName(completion: RuntimePendingTrialCompletionRecord): string {
  return [
    resourceName(completion.core_run_id),
    String(completion.schedule_idx),
  ].join(".");
}

function pendingTrialCompletionUid(completion: RuntimePendingTrialCompletionRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: completion.core_run_id,
    schedule_idx: completion.schedule_idx,
  }));
}

function coreRunResourceName(coreRun: RuntimeCoreRunRecord): string {
  return resourceName(coreRun.core_run_id);
}

function coreRunUid(coreRun: RuntimeCoreRunRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: coreRun.core_run_id,
  }));
}

function runManifestResourceName(manifest: RuntimeRunManifestRecord): string {
  return resourceName(manifest.core_run_id);
}

function runManifestUid(manifest: RuntimeRunManifestRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: manifest.core_run_id,
  }));
}

function runManifestOwnerReference(manifest: RuntimeRunManifestRecord): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RunManifest",
    name: runManifestResourceName(manifest),
    uid: runManifestUid(manifest),
  };
}

function metricDefinitionResourceName(definition: RuntimeMetricDefinitionRecord): string {
  return [
    resourceName(definition.experiment_id),
    resourceName(definition.metric_id),
  ].join(".");
}

function metricDefinitionUid(definition: RuntimeMetricDefinitionRecord): string {
  return sha256Digest(canonicalJsonStringify({
    experiment_id: definition.experiment_id,
    metric_id: definition.metric_id,
  }));
}

function runtimeManifestExperimentIds(manifests: RuntimeRunManifestRecord[]): string[] {
  return [...new Set(manifests.map((manifest) => manifest.experiment_id).filter((value): value is string => Boolean(value)))];
}

function variantSnapshotResourceName(snapshot: RuntimeVariantSnapshotRecord): string {
  return [
    resourceName(snapshot.core_run_id),
    String(snapshot.schedule_idx),
    String(snapshot.attempt),
    String(snapshot.row_seq),
    resourceName(snapshot.binding_name),
  ].join(".");
}

function variantSnapshotUid(snapshot: RuntimeVariantSnapshotRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: snapshot.core_run_id,
    trial_id: snapshot.trial_id,
    schedule_idx: snapshot.schedule_idx,
    attempt: snapshot.attempt,
    row_seq: snapshot.row_seq,
    binding_name: snapshot.binding_name,
  }));
}

function provenanceRowResourceName(kind: RuntimeProvenanceResourceKind, record: RuntimeProvenanceRowRecord): string {
  return [
    resourceName(record.core_run_id),
    String(record.schedule_idx),
    String(record.attempt),
    String(record.row_seq),
    resourceName(kind),
  ].join(".");
}

function provenanceRowUid(kind: RuntimeProvenanceResourceKind, record: RuntimeProvenanceRowRecord): string {
  return sha256Digest(canonicalJsonStringify({
    kind,
    core_run_id: record.core_run_id,
    schedule_idx: record.schedule_idx,
    attempt: record.attempt,
    row_seq: record.row_seq,
  }));
}

function provenanceRowAuditSource(kind: RuntimeProvenanceResourceKind): string {
  if (kind === "EvidenceRecord") return "bucephalus_runtime.evidence_rows";
  if (kind === "ChainState") return "bucephalus_runtime.chain_state_rows";
  return "bucephalus_runtime.trial_conclusion_rows";
}

function provenanceRowTrialId(row: JsonObject): string | null {
  return provenanceRowString(row, "trial_id", "ids.trial_id", "identity.trial_id", "subject.trial_id");
}

function provenanceRowRecordKind(row: JsonObject): string | null {
  return provenanceRowString(row, "kind", "type", "event_type", "evidence_type", "schema_version");
}

function provenanceRowString(row: JsonObject, ...paths: string[]): string | null {
  for (const path of paths) {
    const value = path.includes(".") ? jsonPathValue(row, path) : row[path];
    const text = stringField(value);
    if (text) return text;
  }
  return null;
}

function lineageVersionResourceName(version: RuntimeLineageVersionRecord): string {
  return [
    resourceName(version.chain_key),
    String(version.step_index),
    shortResourceName(version.version_id),
  ].join(".");
}

function lineageVersionUid(version: RuntimeLineageVersionRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: version.core_run_id,
    version_id: version.version_id,
  }));
}

function lineageHeadResourceName(head: RuntimeLineageHeadRecord): string {
  return resourceName(head.chain_key);
}

function lineageHeadUid(head: RuntimeLineageHeadRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: head.core_run_id,
    chain_key: head.chain_key,
  }));
}

function slotCommitPhase(recordType: string): string {
  if (recordType === "commit") return "Committed";
  if (recordType === "intent") return "IntentRecorded";
  return "Recorded";
}

function pendingTrialCompletionTrialId(completion: RuntimePendingTrialCompletionRecord): string | null {
  const direct = stringField(completion.trial_result.trial_id);
  if (direct) return direct;
  const deferredTrials = completion.trial_result.deferred_trial_records;
  if (!Array.isArray(deferredTrials)) return null;
  const first = deferredTrials.find((value): value is JsonObject => isRecord(value) && isJsonValue(value));
  return first ? stringField(first.trial_id) : null;
}

function pendingTrialCompletionDeferredRows(result: JsonObject): JsonObject {
  const counts: Record<string, number> = {
    trials: jsonArrayLength(result.deferred_trial_records),
    metrics: jsonArrayLength(result.deferred_metric_rows),
    events: jsonArrayLength(result.deferred_event_rows),
    contract_stages: jsonArrayLength(result.deferred_contract_stage_rows),
    variant_snapshots: jsonArrayLength(result.deferred_variant_snapshot_rows),
    evidence: jsonArrayLength(result.deferred_evidence_records),
    chain_states: jsonArrayLength(result.deferred_chain_state_records),
    conclusions: jsonArrayLength(result.deferred_trial_conclusion_records),
  };
  return {
    ...counts,
    total: Object.values(counts).reduce((sum, value) => sum + value, 0),
  };
}

function jsonArrayLength(value: unknown): number {
  return Array.isArray(value) ? value.length : 0;
}

function runtimeOperationResourceName(operation: RuntimeOperationRecord): string {
  return [
    resourceName(operation.op_kind),
    resourceName(operation.op_id),
  ].join(".");
}

function runtimeOperationUid(operation: RuntimeOperationRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: operation.core_run_id,
    op_kind: operation.op_kind,
    op_id: operation.op_id,
  }));
}

function trialArtifactResourceName(object: RuntimeAttemptObjectRecord): string {
  return [
    resourceName(object.trial_id),
    resourceName(object.role),
    shortResourceName(object.sha256 || object.object_ref),
  ].join(".");
}

function trialArtifactUid(object: RuntimeAttemptObjectRecord): string {
  return sha256Digest(canonicalJsonStringify({
    core_run_id: object.core_run_id,
    trial_id: object.trial_id,
    attempt: object.attempt,
    role: object.role,
    object_ref: object.object_ref,
  }));
}

function runnerInstanceOwnerReference(runnerInstanceId: string): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RunnerInstance",
    name: resourceName(runnerInstanceId),
    uid: runnerInstanceId,
  };
}

function runnerAttemptOwnerReference(attemptId: string): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RunnerAttempt",
    name: attemptId,
    uid: attemptId,
  };
}

function runnerPoolOwnerReference(runnerPoolId: string): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "RunnerPool",
    name: resourceName(runnerPoolId),
    uid: runnerPoolId,
  };
}

function scheduleSlotOwnerReference(slot: Pick<RuntimeSlotRecord, "core_run_id" | "schedule_idx">): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "ScheduleSlot",
    name: `${resourceName(slot.core_run_id)}.${slot.schedule_idx}`,
    uid: scheduleSlotUid(slot),
  };
}

function scheduleSlotUid(slot: Pick<RuntimeSlotRecord, "core_run_id" | "schedule_idx">): string {
  return `${slot.core_run_id}:${slot.schedule_idx}`;
}

function slotCommitOwnerReference(commit: Pick<RuntimeSlotCommitRecord, "core_run_id" | "schedule_idx" | "attempt">): RuntimeResourceRecord["metadata"]["ownerReferences"][number] {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "SlotCommit",
    name: [
      resourceName(commit.core_run_id),
      String(commit.schedule_idx),
      String(commit.attempt),
      "commit",
    ].join("."),
    uid: sha256Digest(canonicalJsonStringify({
      core_run_id: commit.core_run_id,
      schedule_idx: commit.schedule_idx,
      attempt: commit.attempt,
      record_type: "commit",
    })),
  };
}

function runtimeIsoFromMs(value: number | null | undefined): string | undefined {
  if (!value || !Number.isFinite(value)) {
    return undefined;
  }
  return new Date(value).toISOString();
}

function compactObject(input: Record<string, JsonValue | undefined>): JsonObject {
  const out: JsonObject = {};
  for (const [key, value] of Object.entries(input)) {
    if (value !== undefined && value !== null) {
      out[key] = value;
    }
  }
  return out;
}

function compactStringRecord(input: Record<string, string | null | undefined>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(input)) {
    if (value) {
      out[key] = value;
    }
  }
  return out;
}

function accessRequestFromRow(row: unknown): RuntimeAccessRequestRecord {
  const record = row as RuntimeAccessRequestRecord & { command?: unknown; connection?: unknown };
  const kind = record.kind === "exec" ? "exec" : "port_forward";
  return {
    access_request_id: String(record.access_request_id),
    run_id: String(record.run_id),
    kind,
    status: String(record.status),
    resource_kind: String(record.resource_kind),
    resource_name: String(record.resource_name),
    target_uid: stringField(record.target_uid),
    target_resource_version: stringField(record.target_resource_version),
    protocol: String(record.protocol),
    target_port: record.target_port === null ? null : Number(record.target_port),
    local_port: record.local_port === null ? null : Number(record.local_port),
    command: Array.isArray(record.command)
      ? record.command.filter((item): item is string => typeof item === "string")
      : [],
    runner_instance_id: record.runner_instance_id === null ? null : String(record.runner_instance_id),
    attempt_id: record.attempt_id === null ? null : String(record.attempt_id),
    worker_id: stringField(record.worker_id),
    requester: record.requester === null ? null : String(record.requester),
    reason: record.reason === null ? null : String(record.reason),
    connection: isRecord(record.connection) ? record.connection as JsonObject : parseObject(record.connection),
    error_message: record.error_message === null ? null : String(record.error_message),
    expires_at: record.expires_at === null ? null : String(record.expires_at),
    created_at: String(record.created_at),
    updated_at: String(record.updated_at),
  };
}

function runtimeRunnerPoolFromRow(row: unknown): RuntimeRunnerPoolRecord {
  const record = row as RuntimeRunnerPoolRecord & {
    capabilities?: unknown;
    metadata?: unknown;
  };
  return {
    runner_pool_id: String(record.runner_pool_id),
    name: String(record.name),
    status: String(record.status),
    capabilities: normalizeRuntimeCapabilities(record.capabilities),
    metadata: parseObject(record.metadata),
    created_at: String(record.created_at),
    updated_at: String(record.updated_at),
  };
}

function runtimeRunAttemptFromRow(row: unknown): RuntimeRunAttemptRecord {
  const record = row as RunAttemptRecord & {
    runner_pool_id?: unknown;
    runner_instance_status?: unknown;
    runner_instance_name?: unknown;
    runner_instance_provider_instance_id?: unknown;
    runner_instance_capabilities?: unknown;
    runner_instance_metadata?: unknown;
    runner_instance_last_heartbeat_at?: unknown;
    runner_instance_created_at?: unknown;
    runner_instance_updated_at?: unknown;
  };
  return {
    attempt_id: String(record.attempt_id),
    run_id: String(record.run_id),
    worker_id: String(record.worker_id),
    runner_instance_id: record.runner_instance_id === null ? null : String(record.runner_instance_id),
    status: String(record.status),
    lease_expires_at: String(record.lease_expires_at),
    heartbeat_at: String(record.heartbeat_at),
    started_at: String(record.started_at),
    ended_at: record.ended_at === null ? null : String(record.ended_at),
    error_message: record.error_message === null ? null : String(record.error_message),
    created_at: String(record.created_at),
    updated_at: String(record.updated_at),
    attempt_token: record.attempt_token === undefined || record.attempt_token === null ? null : String(record.attempt_token),
    runner_pool_id: record.runner_pool_id === null || record.runner_pool_id === undefined ? null : String(record.runner_pool_id),
    runner_instance_status: record.runner_instance_status === null || record.runner_instance_status === undefined ? null : String(record.runner_instance_status),
    runner_instance_name: record.runner_instance_name === null || record.runner_instance_name === undefined ? null : String(record.runner_instance_name),
    runner_instance_provider_instance_id: record.runner_instance_provider_instance_id === null || record.runner_instance_provider_instance_id === undefined ? null : String(record.runner_instance_provider_instance_id),
    runner_instance_capabilities: normalizeRuntimeCapabilities(record.runner_instance_capabilities),
    runner_instance_metadata: parseObject(record.runner_instance_metadata),
    runner_instance_last_heartbeat_at: record.runner_instance_last_heartbeat_at === null || record.runner_instance_last_heartbeat_at === undefined ? null : String(record.runner_instance_last_heartbeat_at),
    runner_instance_created_at: record.runner_instance_created_at === null || record.runner_instance_created_at === undefined ? null : String(record.runner_instance_created_at),
    runner_instance_updated_at: record.runner_instance_updated_at === null || record.runner_instance_updated_at === undefined ? null : String(record.runner_instance_updated_at),
  };
}

function normalizeRuntimeCapabilities(value: unknown): WorkerCapabilities {
  const record = isRecord(value) ? value : parseObject(value);
  return {
    executors: stringArray(record.executors),
    resources: stringArray(record.resources),
    arch: typeof record.arch === "string" ? record.arch : null,
    cpu_count: optionalPositiveInt(record.cpu_count),
    memory_mb: optionalPositiveInt(record.memory_mb),
    disk_mb: optionalPositiveInt(record.disk_mb),
    isolation: stringArray(record.isolation),
  };
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value
    .filter((item): item is string => typeof item === "string" && item.trim().length > 0)
    .map((item) => item.trim());
}

function optionalPositiveInt(value: unknown): number | null {
  if (typeof value === "number" && Number.isInteger(value) && value > 0) {
    return value;
  }
  if (typeof value === "string" && /^\d+$/.test(value)) {
    const parsed = Number.parseInt(value, 10);
    return parsed > 0 ? parsed : null;
  }
  return null;
}

function validatePort(value: number, pointer: string): number {
  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    throw new HttpError(400, "invalid_port", `${pointer} must be an integer from 1 to 65535`);
  }
  return value;
}

function validateRuntimeAccessTtlSeconds(value: number | null | undefined): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (!Number.isInteger(value) || value < 1 || value > 2_147_483_647) {
    throw new HttpError(400, "invalid_runtime_access_ttl", "/ttl_seconds must be a positive integer");
  }
  return value;
}

function validatePortForwardStatusConnection(
  status: "accepted" | "active" | "completed" | "failed" | "expired",
  connection: JsonObject | null,
): void {
  if (status !== "active") {
    return;
  }
  if (!connection || !portForwardConnectionHasConnectHandle(connection)) {
    throw new HttpError(
      400,
      "invalid_port_forward_connection",
      "Active port-forward updates must include an auditable connection handle; set connection.client_reachable or a client endpoint only when the tunnel is reachable from the user client",
    );
  }
}

function portForwardConnectionHasConnectHandle(connection: JsonObject): boolean {
  const localPort = numberField(connection.local_port);
  if (localPort !== null) {
    validatePort(localPort, "/connection/local_port");
    return true;
  }
  return [
    "local_probe",
    "endpoint",
    "url",
    "listen",
    "listen_address",
    "local_address",
    "address",
    "client_endpoint",
    "client_url",
    "client_listen",
    "provider_tunnel_url",
    "tunnel",
  ].some((key) => Boolean(stringField(connection[key])));
}

function portForwardWorkerTransitionPreviousStatuses(status: "accepted" | "active" | "completed" | "failed" | "expired"): string[] {
  switch (status) {
    case "accepted":
      return ["requested", "accepted"];
    case "active":
      return ["accepted", "active"];
    case "completed":
      return ["active"];
    case "failed":
    case "expired":
      return ["requested", "accepted", "active"];
  }
}

function execWorkerTransitionPreviousStatuses(status: "accepted" | "active" | "completed" | "failed" | "expired"): string[] {
  switch (status) {
    case "accepted":
      return ["requested", "accepted"];
    case "active":
      return ["accepted", "active"];
    case "completed":
      return ["accepted", "active"];
    case "failed":
    case "expired":
      return ["requested", "accepted", "active"];
  }
}

async function throwRejectedRuntimeAccessWorkerUpdate(
  tx: any,
  input: {
    accessRequestId: string;
    attemptId: string;
    runnerInstanceId: string;
    kind: "port_forward" | "exec";
    targetStatus: string;
    allowedPreviousStatuses: string[];
  },
): Promise<void> {
  const rows = await tx`
    select status
    from cloud.runtime_access_requests
    where access_request_id = ${input.accessRequestId}
      and attempt_id = ${input.attemptId}
      and runner_instance_id = ${input.runnerInstanceId}
      and kind = ${input.kind}
    limit 1
  `;
  const currentStatus = rows[0]?.status == null ? null : String(rows[0].status);
  if (!currentStatus) {
    return;
  }
  if (runtimeAccessRequestIsLive(currentStatus)) {
    if (input.allowedPreviousStatuses.includes(currentStatus)) {
      return;
    }
    throw new HttpError(
      409,
      "runtime_access_transition_invalid",
      `${runtimeAccessKindLabel(input.kind)} request cannot transition from ${currentStatus} to ${input.targetStatus}`,
      {
        access_request_id: input.accessRequestId,
        kind: input.kind,
        current_status: currentStatus,
        target_status: input.targetStatus,
        allowed_previous_statuses: input.allowedPreviousStatuses,
      },
    );
  }
  throw new HttpError(
    409,
    "runtime_access_request_not_active",
    `${runtimeAccessKindLabel(input.kind)} request is ${currentStatus} and cannot be advanced by a worker`,
    {
      access_request_id: input.accessRequestId,
      kind: input.kind,
      current_status: currentStatus,
      live_statuses: ["requested", "accepted", "active"],
    },
  );
}

function runtimeAccessRequestIsLive(status: string): boolean {
  return status === "requested" || status === "accepted" || status === "active";
}

function runtimeAccessKindLabel(kind: "port_forward" | "exec"): string {
  return kind === "exec" ? "Exec" : "Port-forward";
}

function validateExecStatusConnection(
  status: "accepted" | "active" | "completed" | "failed" | "expired",
  connection: JsonObject | null,
): void {
  if (status !== "completed") {
    return;
  }
  if (!connection || !validRuntimeExecExitCode(connection.exit_code)) {
    throw new HttpError(
      400,
      "invalid_exec_connection",
      "Completed exec updates must include a numeric connection.exit_code",
    );
  }
}

function validRuntimeExecExitCode(value: unknown): boolean {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 255;
}

function validateCommand(value: string[], pointer: string): string[] {
  if (!Array.isArray(value)) {
    throw new HttpError(400, "invalid_command", `${pointer} must be a non-empty string array`);
  }
  const command = value.map((item) => typeof item === "string" ? item.trim() : "");
  if (command.length === 0 || command.some((item) => item.length === 0)) {
    throw new HttpError(400, "invalid_command", `${pointer} must be a non-empty string array`);
  }
  if (command.length > 128) {
    throw new HttpError(400, "invalid_command", `${pointer} may contain at most 128 arguments`);
  }
  return command;
}

function normalizeOptionalString(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

async function appendRuntimeApiResourcesReadAuditEvent(
  sql: Sql | undefined,
  input: {
    runId: string;
    operation: "api-resources" | "api-resource";
    requester?: string | null | undefined;
    selectedKind?: string | null | undefined;
    apiResources?: RuntimeApiResourceList | undefined;
    apiResource?: RuntimeApiResourceRecord | undefined;
    error?: unknown;
  },
): Promise<void> {
  if (!isRuntimeSql(sql)) {
    return;
  }
  const error = runtimeReadAuditError(input.error);
  const payload = compactObject({
    operation: input.operation,
    status: error ? "failed" : undefined,
    requester: normalizeOptionalString(input.requester),
    resource_ref: runtimeInspectBundleRunResourceRef(input.runId),
    resource_kind: "Run",
    resource_name: input.runId,
    resource_uid: input.runId,
    selected_kind: normalizeOptionalString(input.selectedKind),
    api_resources_returned: input.apiResources?.resources.length,
    api_resource_kind: input.apiResource?.kind,
    api_resource_name: input.apiResource?.name,
    api_resource_singular: input.apiResource?.singularName,
    api_resource_categories: input.apiResource?.categories,
    api_resource_verbs: input.apiResource?.verbs,
    api_resource_subresources: input.apiResource?.subresources,
    api_resource_actions: input.apiResource?.actions,
    api_resource_access: input.apiResource?.access,
    api_resource_count: input.apiResource?.count,
    core_run_ids: input.apiResources?.core_run_ids ?? input.apiResource?.core_run_ids,
    error_code: error?.code,
    error_status: error?.status,
    error_message: error?.message,
  });
  try {
    await appendAccessRequestEvent(sql, sql, {
      runId: input.runId,
      attemptId: null,
      eventType: error ? "runtime.api_resources.read.failed" : "runtime.api_resources.read",
      payload,
    });
  } catch (insertError) {
    if (isMissingRuntimeStore(insertError)) {
      return;
    }
    throw insertError;
  }
}

type RuntimeResourceQueryReadEventType =
  | "runtime.resource.list.read"
  | "runtime.resource.list.read.failed"
  | "runtime.resource.watch.read"
  | "runtime.resource.watch.read.failed"
  | "runtime.resource.health.read"
  | "runtime.resource.health.read.failed"
  | "runtime.resource.describe.read"
  | "runtime.resource.describe.read.failed"
  | "runtime.resource.get.read"
  | "runtime.resource.get.read.failed"
  | "runtime.resource.events.read"
  | "runtime.resource.events.read.failed"
  | "runtime.resource.status.read"
  | "runtime.resource.status.read.failed"
  | "runtime.resource.metrics.read"
  | "runtime.resource.metrics.read.failed"
  | "runtime.resource.metrics.list.read"
  | "runtime.resource.metrics.list.read.failed";

async function appendRuntimeResourceQueryReadAuditEvent(
  sql: Sql | undefined,
  input: {
    runId: string;
    eventType: RuntimeResourceQueryReadEventType;
    operation: string;
    requester?: string | null | undefined;
    input?: unknown;
    list?: RuntimeResourceList | undefined;
    watch?: RuntimeResourceWatchList | undefined;
    health?: RuntimeResourceHealth | undefined;
    resource?: RuntimeResourceRecord | undefined;
    eventList?: RuntimeResourceEventList | undefined;
    status?: RuntimeResourceStatus | undefined;
    metrics?: RuntimeResourceMetrics | undefined;
    metricsList?: RuntimeResourceMetricsList | undefined;
    relatedResources?: number | undefined;
    error?: unknown;
  },
): Promise<void> {
  if (!isRuntimeSql(sql) || !runtimeReadAuditRequested(input.input)) {
    return;
  }
  const error = runtimeReadAuditError(input.error);
  const requested = runtimeReadAuditRequestedResource(input.input);
  const resourceKind = input.resource?.kind
    ?? requested.kind
    ?? (input.list || input.watch || input.health || input.metricsList ? "Run" : null);
  const resourceName = input.resource?.metadata.name
    ?? requested.name
    ?? (input.list || input.watch || input.health || input.metricsList ? input.runId : null);
  const payload = compactObject({
    operation: input.operation,
    status: error ? "failed" : undefined,
    requester: normalizeOptionalString(input.requester),
    resource_ref: input.resource
      ? runtimeResourceEventRef(input.resource)
      : runtimeRequestedResourceEventRef(resourceKind, resourceName),
    resource_kind: resourceKind,
    resource_name: resourceName,
    resource_uid: input.resource?.metadata.uid ?? (resourceKind === "Run" ? input.runId : undefined),
    resource_version: runtimeResourceReadAuditResourceVersion(input),
    resource_generation: input.resource?.metadata.generation,
    resource_filter: runtimeResourceReadAuditFilter(input.input),
    event_filter: runtimeResourceReadAuditEventFilter(input.input),
    limit: runtimeReadAuditNumber(input.input, "limit"),
    event_limit: runtimeReadAuditNumber(input.input, "eventLimit"),
    continue: runtimeReadAuditString(input.input, "continueToken")
      ?? input.list?.metadata.continue
      ?? input.watch?.resource_inventory.metadata.continue
      ?? input.eventList?.metadata.continue
      ?? input.metricsList?.metadata.continue,
    resource_version_cursor: runtimeReadAuditString(input.input, "resourceVersion"),
    known_resources: runtimeReadAuditKnownResourceCount(input.input),
    allow_bookmarks: runtimeReadAuditBoolean(input.input, "allowBookmarks"),
    total: input.list?.metadata.total
      ?? input.watch?.resource_inventory.metadata.total
      ?? input.metricsList?.metadata.total
      ?? input.health?.summary.total,
    returned: input.list?.metadata.returned
      ?? input.watch?.resource_inventory.metadata.returned
      ?? input.eventList?.metadata.returned
      ?? input.metricsList?.metadata.returned,
    remaining: input.list?.metadata.remainingItemCount
      ?? input.watch?.resource_inventory.metadata.remainingItemCount
      ?? input.eventList?.metadata.remainingItemCount
      ?? input.metricsList?.metadata.remainingItemCount,
    watch_events_returned: input.watch?.events.length,
    event_resource_version: input.eventList?.metadata.resourceVersion,
    event_returned: input.eventList?.metadata.returned,
    event_next_after_row_seq: input.eventList?.metadata.next_after_row_seq ?? undefined,
    metrics_total: input.metrics?.summary.metrics_total ?? input.metricsList?.summary.metrics_total,
    metrics_resources_returned: input.metricsList?.summary.resources_returned,
    health_ready: input.health?.summary.ready,
    health_degraded: input.health?.summary.degraded,
    health_problem: input.health?.summary.problem,
    health_unknown: input.health?.summary.unknown,
    phase: typeof input.status?.phase === "string" ? input.status.phase : undefined,
    reason: typeof input.status?.reason === "string" ? input.status.reason : undefined,
    related_resources: input.relatedResources,
    error_code: error?.code,
    error_status: error?.status,
    error_message: error?.message,
  });
  try {
    await appendAccessRequestEvent(sql, sql, {
      runId: input.runId,
      attemptId: input.resource ? runtimeResourceReadAttemptId(input.resource) : null,
      eventType: input.eventType,
      payload,
    });
  } catch (insertError) {
    if (isMissingRuntimeStore(insertError)) {
      return;
    }
    throw insertError;
  }
}

function runtimeReadAuditRequested(input: unknown): boolean {
  return isRecord(input) && Object.prototype.hasOwnProperty.call(input, "requester");
}

function runtimeReadAuditRequestedResource(input: unknown): { kind: string | null; name: string | null } {
  const record = isRecord(input) ? input : {};
  return {
    kind: canonicalRuntimeResourceKind(stringField(record.kind)) ?? stringField(record.kind),
    name: stringField(record.name),
  };
}

function runtimeResourceReadAuditResourceVersion(input: Parameters<typeof appendRuntimeResourceQueryReadAuditEvent>[1]): string | undefined {
  return input.resource?.metadata.resourceVersion
    ?? input.list?.metadata.resourceVersion
    ?? input.watch?.resource_inventory.metadata.resourceVersion
    ?? input.eventList?.resource.metadata.resourceVersion
    ?? input.status?.resourceVersion
    ?? input.metrics?.resource_version
    ?? input.metricsList?.metadata.resourceVersion
    ?? undefined;
}

function runtimeResourceReadAuditFilter(input: unknown): JsonObject | undefined {
  const record = isRecord(input) ? input : {};
  const filter = isRecord(record.filter) && !record.kind ? record.filter : record;
  const payload = compactObject({
    kinds: runtimeReadAuditStringArray(filter.kinds),
    categories: runtimeReadAuditStringArray(filter.categories),
    label_selector: stringField(filter.labelSelector),
    field_selector: stringField(filter.fieldSelector),
  });
  return Object.keys(payload).length > 0 ? payload : undefined;
}

function runtimeResourceReadAuditEventFilter(input: unknown): JsonObject | undefined {
  const record = isRecord(input) ? input : {};
  const filter = isRecord(record.filter) ? record.filter : {};
  const payload = compactObject({
    event_types: runtimeReadAuditStringArray(filter.eventTypes),
    sources: runtimeReadAuditStringArray(filter.sources),
    trial_id: stringField(filter.trialId),
    task_id: stringField(filter.taskId),
    after_row_seq: numberField(record.afterRowSeq),
  });
  return Object.keys(payload).length > 0 ? payload : undefined;
}

function runtimeReadAuditStringArray(value: unknown): string[] | undefined {
  if (!Array.isArray(value)) {
    return undefined;
  }
  const items = value.map((item) => typeof item === "string" ? item.trim() : "").filter(Boolean);
  return items.length > 0 ? items : undefined;
}

function runtimeReadAuditString(input: unknown, key: string): string | undefined {
  const record = isRecord(input) ? input : {};
  return stringField(record[key]) ?? undefined;
}

function runtimeReadAuditNumber(input: unknown, key: string): number | undefined {
  const record = isRecord(input) ? input : {};
  return numberField(record[key]) ?? undefined;
}

function runtimeReadAuditBoolean(input: unknown, key: string): boolean | undefined {
  const record = isRecord(input) ? input : {};
  return typeof record[key] === "boolean" ? record[key] : undefined;
}

function runtimeReadAuditKnownResourceCount(input: unknown): number | undefined {
  const record = isRecord(input) ? input : {};
  return record.knownResourceVersions instanceof Map ? record.knownResourceVersions.size : undefined;
}

function runtimeResourceFilterOnly(filter: RuntimeResourceFilter): RuntimeResourceFilter {
  return {
    kinds: filter.kinds,
    categories: filter.categories,
    labelSelector: filter.labelSelector,
    fieldSelector: filter.fieldSelector,
  };
}

async function appendRuntimeOperationReviewAuditEvent(
  sql: Sql | undefined,
  input: {
    runId: string;
    requester?: string | null | undefined;
    review?: RuntimeResourceOperationReview | undefined;
    requestedKind?: string | null | undefined;
    requestedName?: string | null | undefined;
    operation?: string | null | undefined;
    error?: unknown;
  },
): Promise<void> {
  if (!isRuntimeSql(sql)) {
    return;
  }
  const error = runtimeReadAuditError(input.error);
  const review = input.review;
  const resourceKind = review?.resource_ref.kind
    ?? canonicalRuntimeResourceKind(input.requestedKind)
    ?? normalizeOptionalString(input.requestedKind);
  const resourceName = review?.resource_ref.name
    ?? normalizeOptionalString(input.requestedName);
  const payload = compactObject({
    operation: review?.operation ?? normalizeOptionalString(input.operation),
    matched_operation: review?.matched_operation ?? undefined,
    supported: review?.supported,
    status: error ? "failed" : review ? (review.supported ? "supported" : "unsupported") : undefined,
    requester: normalizeOptionalString(input.requester),
    resource_ref: review ? runtimeOperationReviewResourceRef(review) : runtimeRequestedResourceEventRef(resourceKind, resourceName),
    resource_kind: resourceKind,
    resource_name: resourceName,
    resource_uid: review?.resource_ref.uid,
    resource_version: review?.resource_version ?? undefined,
    resource_generation: review?.resource_generation ?? undefined,
    observed_generation: review?.observed_generation ?? undefined,
    reason: review?.reason ?? undefined,
    message: review?.message ?? undefined,
    command: review?.command ?? undefined,
    verb: review?.verb ?? undefined,
    subresource: review?.subresource ?? undefined,
    action: review?.action ?? undefined,
    requires_running_run: review?.requires_running_run ?? undefined,
    error_code: error?.code,
    error_status: error?.status,
    error_message: error?.message,
  });
  try {
    await appendAccessRequestEvent(sql, sql, {
      runId: input.runId,
      attemptId: null,
      eventType: error ? "runtime.resource.operation.review.failed" : "runtime.resource.operation.reviewed",
      payload,
    });
  } catch (insertError) {
    if (isMissingRuntimeStore(insertError)) {
      return;
    }
    throw insertError;
  }
}

function runtimeOperationReviewResourceRef(review: RuntimeResourceOperationReview): JsonObject {
  return compactObject({
    apiVersion: review.resource_ref.apiVersion,
    kind: review.resource_ref.kind,
    name: review.resource_ref.name,
    uid: review.resource_ref.uid,
  });
}

async function appendRuntimeInspectBundleAuditEvent(
  sql: Sql | undefined,
  input: {
    runId: string;
    requester?: string | null | undefined;
    eventLimit: number;
    resourceFilter: RuntimeInspectBundleFilter;
    bundle?: RuntimeInspectBundle | undefined;
    error?: unknown;
  },
): Promise<void> {
  if (!isRuntimeSql(sql)) {
    return;
  }
  const error = runtimeReadAuditError(input.error);
  const payload = compactObject({
    operation: "inspect",
    status: error ? "failed" : undefined,
    requester: normalizeOptionalString(input.requester),
    resource_ref: runtimeInspectBundleRunResourceRef(input.runId),
    resource_filter: runtimeInspectBundleAuditFilter(input.bundle?.resource_filter ?? input.resourceFilter),
    event_limit: input.eventLimit,
    inventory_resource_version: input.bundle?.resource_inventory.metadata.resourceVersion,
    inventory_continue: input.bundle?.resource_inventory.metadata.continue,
    inventory_total: input.bundle?.resource_inventory.metadata.total,
    inventory_returned: input.bundle?.resource_inventory.metadata.returned,
    event_resource_version: input.bundle?.event_list.metadata.resourceVersion,
    event_continue: input.bundle?.event_list.metadata.continue,
    event_returned: input.bundle?.event_list.metadata.returned,
    event_next_after_row_seq: input.bundle?.event_list.metadata.next_after_row_seq ?? undefined,
    api_resources_returned: input.bundle?.api_resources.resources.length,
    health_summary: input.bundle ? runtimeInspectBundleAuditHealthSummary(input.bundle.resource_health.summary) : undefined,
    metrics_summary: input.bundle ? runtimeInspectBundleAuditMetricsSummary(input.bundle.resource_metrics.summary) : undefined,
    log_refs: input.bundle?.log_refs.length,
    error_code: error?.code,
    error_status: error?.status,
    error_message: error?.message,
  });
  try {
    await appendAccessRequestEvent(sql, sql, {
      runId: input.runId,
      attemptId: null,
      eventType: error ? "runtime.inspect.bundle.read.failed" : "runtime.inspect.bundle.read",
      payload,
    });
  } catch (error) {
    if (isMissingRuntimeStore(error)) {
      return;
    }
    throw error;
  }
}

function runtimeInspectBundleRunResourceRef(runId: string): JsonObject {
  return {
    apiVersion: RUNTIME_API_VERSION,
    kind: "Run",
    name: runId,
    uid: runId,
  };
}

function runtimeInspectBundleFilterView(filter: RuntimeResourceFilter): RuntimeInspectBundleFilter {
  return {
    kinds: filter.kinds ?? [],
    categories: filter.categories ?? [],
    label_selector: filter.labelSelector ?? null,
    field_selector: filter.fieldSelector ?? null,
  };
}

function runtimeInspectBundleAuditFilter(filter: RuntimeInspectBundleFilter): JsonObject {
  return compactObject({
    kinds: filter.kinds,
    categories: filter.categories,
    label_selector: filter.label_selector ?? undefined,
    field_selector: filter.field_selector ?? undefined,
  });
}

function runtimeInspectBundleAuditHealthSummary(summary: RuntimeResourceHealthSummary): JsonObject {
  return compactObject({
    total: summary.total,
    ready: summary.ready,
    degraded: summary.degraded,
    problem: summary.problem,
    unknown: summary.unknown,
    access_targets: summary.access_targets,
    reachable_access_targets: summary.reachable_access_targets,
    port_forward_ready: summary.port_forward_ready,
    exec_ready: summary.exec_ready,
    actions_available: summary.actions_available,
    observed_resources: summary.observed_resources,
    observed_current: summary.observed_current,
    observed_stale: summary.observed_stale,
    observed_unknown: summary.observed_unknown,
  });
}

function runtimeInspectBundleAuditMetricsSummary(summary: RuntimeResourceMetricsListSummary): JsonObject {
  return compactObject({
    resources_total: summary.resources_total,
    resources_returned: summary.resources_returned,
    metrics_total: summary.metrics_total,
    lifecycle_metrics: summary.lifecycle_metrics,
    condition_metrics: summary.condition_metrics,
    access_metrics: summary.access_metrics,
    event_metrics: summary.event_metrics,
    numeric_spec_metrics: summary.numeric_spec_metrics,
    numeric_status_metrics: summary.numeric_status_metrics,
    events_total: summary.events_total,
  });
}

async function appendRuntimeResourceReadAuditEvent(
  sql: Sql | undefined,
  input: {
    runId: string;
    eventType:
      | "runtime.resource.logs.read"
      | "runtime.resource.logs.read.failed"
      | "runtime.resource.content.read"
      | "runtime.resource.content.read.failed";
    operation: "logs" | "content";
    requester?: string | null | undefined;
    requestedKind?: string | null | undefined;
    requestedName?: string | null | undefined;
    resource?: RuntimeResourceRecord | undefined;
    object?: RuntimeAttemptObjectRecord | undefined;
    mediaType?: string | null | undefined;
    byteSize?: number | undefined;
    stream?: string | null | undefined;
    tailLines?: number | undefined;
    error?: unknown;
  },
): Promise<void> {
  if (!isRuntimeSql(sql)) {
    return;
  }
  const resourceKind = input.resource?.kind
    ?? canonicalRuntimeResourceKind(input.requestedKind)
    ?? normalizeOptionalString(input.requestedKind);
  const resourceName = input.resource?.metadata.name
    ?? normalizeOptionalString(input.requestedName);
  const error = runtimeReadAuditError(input.error);
  const payload = compactObject({
    operation: input.operation,
    status: error ? "failed" : undefined,
    requester: normalizeOptionalString(input.requester),
    resource_ref: input.resource ? runtimeResourceEventRef(input.resource) : runtimeRequestedResourceEventRef(resourceKind, resourceName),
    resource_kind: resourceKind,
    resource_name: resourceName,
    resource_uid: input.resource?.metadata.uid,
    resource_version: input.resource?.metadata.resourceVersion,
    resource_generation: input.resource?.metadata.generation,
    stream: input.stream ?? undefined,
    tail_lines: input.tailLines,
    core_run_id: input.object?.core_run_id,
    trial_id: input.object?.trial_id,
    trial_attempt: input.object?.attempt,
    artifact_role: input.object?.role,
    object_ref: input.object?.object_ref,
    sha256: input.object?.sha256,
    media_type: input.mediaType ?? undefined,
    byte_size: input.byteSize,
    error_code: error?.code,
    error_status: error?.status,
    error_message: error?.message,
  });
  try {
    await appendAccessRequestEvent(sql, sql, {
      runId: input.runId,
      attemptId: input.resource ? runtimeResourceReadAttemptId(input.resource) : null,
      eventType: input.eventType,
      payload,
    });
  } catch (error) {
    if (isMissingRuntimeStore(error)) {
      return;
    }
    throw error;
  }
}

function runtimeRequestedResourceEventRef(kind: string | null | undefined, name: string | null | undefined): JsonObject | undefined {
  if (!kind || !name) {
    return undefined;
  }
  return compactObject({
    apiVersion: RUNTIME_API_VERSION,
    kind,
    name,
  });
}

function runtimeReadAuditError(error: unknown): { status: number | null; code: string; message: string } | null {
  if (error instanceof HttpError) {
    return {
      status: error.status,
      code: error.code,
      message: error.message,
    };
  }
  if (error instanceof Error) {
    return {
      status: null,
      code: error.name || "runtime_read_failed",
      message: error.message,
    };
  }
  return null;
}

function isRuntimeSql(value: unknown): value is Sql {
  return typeof value === "function" && typeof (value as { json?: unknown }).json === "function";
}

function runtimeResourceReadAttemptId(resource: RuntimeResourceRecord): string | null {
  const runnerBinding = isRecord(resource.status.runner_binding) ? resource.status.runner_binding : {};
  return stringField(resource.status.attempt_id)
    ?? stringField(resource.status.current_attempt_id)
    ?? stringField(resource.spec.attempt_id)
    ?? stringField(runnerBinding.attempt_id);
}

async function appendAccessRequestEvent(
  tx: any,
  sql: Sql,
  input: {
    runId: string;
    attemptId: string | null;
    eventType: string;
    payload: JsonObject;
  },
): Promise<void> {
  await tx`
    insert into cloud.run_events (
      run_id,
      attempt_id,
      seq,
      event_type,
      payload
    )
    values (
      ${input.runId},
      ${input.attemptId},
      coalesce((select max(seq) + 1 from cloud.run_events where run_id = ${input.runId}), 1),
      ${input.eventType},
      ${sql.json(input.payload)}
    )
  `;
}

async function expireRuntimeAccessRequestsInTransaction(tx: any, sql: Sql): Promise<RuntimeAccessRequestRecord[]> {
  const rows = await tx`
    with candidates as (
      select access_request_id, status as previous_status
      from cloud.runtime_access_requests
      where status in ('requested', 'accepted', 'active')
        and expires_at is not null
        and expires_at <= now()
      for update
    ),
    updated as (
      update cloud.runtime_access_requests as request
      set status = 'expired',
          error_message = coalesce(request.error_message, 'runtime access request expired'),
          updated_at = now()
      from candidates
      where request.access_request_id = candidates.access_request_id
      returning request.*, candidates.previous_status
    )
    select * from updated
  `;
  const expired = rows.map(accessRequestFromRow);
  for (const [index, request] of expired.entries()) {
    const previousStatusValue = rows[index]?.previous_status;
    const previousStatus = previousStatusValue == null ? null : String(previousStatusValue);
    await appendAccessRequestEvent(tx, sql, {
      runId: request.run_id,
      attemptId: request.attempt_id,
      eventType: request.kind === "exec"
        ? "runtime.access.exec.expired"
        : "runtime.access.port_forward.expired",
      payload: accessRequestEventPayload(request, previousStatus || null),
    });
  }
  return expired;
}

function accessRequestEventPayload(
  request: RuntimeAccessRequestRecord,
  previousStatus?: string | null,
  resourceVersionPrecondition?: string | null,
  resolvedTarget?: RuntimeAccessTarget | null,
): JsonObject {
  return compactObject({
    access_request_id: request.access_request_id,
    kind: request.kind,
    previous_status: previousStatus,
    status: request.status,
    access_resource_ref: accessRequestResourceRef(request),
    target_ref: accessRequestTargetRef(request),
    resolved_target: resolvedTarget ? runtimeAccessResolvedTargetPayload(resolvedTarget) : undefined,
    resource_kind: request.resource_kind,
    resource_name: request.resource_name,
    protocol: request.protocol,
    target_port: request.target_port,
    local_port: request.local_port,
    command: request.command,
    runner_instance_id: request.runner_instance_id,
    attempt_id: request.attempt_id,
    runner_binding: accessRequestRunnerBinding(request),
    requester: request.requester,
    reason: request.reason,
    resource_version_precondition: normalizeOptionalString(resourceVersionPrecondition),
    connection: request.connection,
    error_message: request.error_message,
    expires_at: request.expires_at,
  });
}

function runtimeAccessResolvedTargetPayload(target: RuntimeAccessTarget): JsonObject {
  return compactObject({
    apiVersion: RUNTIME_API_VERSION,
    kind: target.kind,
    name: target.name,
    uid: target.uid,
    resourceVersion: target.resourceVersion,
    runner_instance_id: target.runnerInstanceId,
    attempt_id: target.attemptId,
    worker_id: target.workerId,
    runner_binding: accessTargetRunnerBinding(target),
  });
}

function accessTargetRunnerBinding(target: RuntimeAccessTarget): JsonObject | undefined {
  const binding = compactObject({
    runner_instance_id: target.runnerInstanceId,
    attempt_id: target.attemptId,
    worker_id: target.workerId,
  });
  return Object.keys(binding).length ? binding : undefined;
}

function accessRequestCancelEventPayload(
  request: RuntimeAccessRequestRecord,
  previousStatus: string | null,
  resourceVersionPrecondition?: string | null,
): JsonObject {
  return {
    ...accessRequestEventPayload(request, null, resourceVersionPrecondition),
    ...compactObject({
      action: "cancel",
      previous_status: previousStatus,
      cancelled_at: request.updated_at,
    }),
  };
}

function accessRequestCompleteEventPayload(
  request: RuntimeAccessRequestRecord,
  previousStatus: string | null,
  resourceVersionPrecondition?: string | null,
): JsonObject {
  return {
    ...accessRequestEventPayload(request, null, resourceVersionPrecondition),
    ...compactObject({
      action: "complete",
      previous_status: previousStatus,
      completed_at: request.updated_at,
    }),
  };
}

function resourceName(value: string): string {
  const normalized = value
    .toLowerCase()
    .replace(/^sha256:/, "sha256-")
    .replace(/[^a-z0-9_.-]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return normalized.slice(0, 96) || "resource";
}

function shortResourceName(value: string): string {
  return resourceName(value).slice(0, 32) || "resource";
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

function attemptObjectContentRecordFromRow(row: unknown): RuntimeAttemptObjectContentRecord {
  const record = row as Record<string, unknown>;
  return {
    core_run_id: String(record.run_id),
    trial_id: String(record.trial_id),
    schedule_idx: numberField(record.schedule_idx) ?? 0,
    attempt: numberField(record.attempt) ?? 0,
    role: String(record.role),
    object_ref: String(record.object_ref),
    storage_path: String(record.storage_path),
    media_type: String(record.media_type),
    byte_size: numberField(record.byte_size) ?? 0,
    sha256: String(record.sha256),
    relative_path: record.relative_path === null || record.relative_path === undefined ? null : String(record.relative_path),
    metadata: record.metadata_json === null || record.metadata_json === undefined ? null : parseJson(record.metadata_json),
    recorded_at_ms: numberField(record.recorded_at_ms) ?? 0,
  };
}

function enrichAttemptObjectWithContent(
  record: RuntimeAttemptObjectRecord,
  content: RuntimeAttemptObjectContentRecord | undefined,
): RuntimeAttemptObjectRecord {
  if (!content) {
    return record;
  }
  return {
    ...record,
    object_ref: content.object_ref,
    content_available: true,
    media_type: content.media_type,
    byte_size: content.byte_size,
    sha256: content.sha256,
    relative_path: content.relative_path ?? "",
  };
}

function groupCoreRunIds(rows: unknown[]): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const row of rows as Array<{ cloud_run_id: unknown; core_run_id: unknown }>) {
    const cloudRunId = String(row.cloud_run_id);
    const coreRunId = String(row.core_run_id);
    out.set(cloudRunId, [...(out.get(cloudRunId) ?? []), coreRunId]);
  }
  return out;
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

export function runtimeValueRecordsFromSnapshots(snapshots: RuntimeSnapshotRecord[]): RuntimeValueRecord[] {
  const rows: RuntimeValueRecord[] = [];
  for (const snapshot of snapshots) {
    const observedAt = snapshot.created_at ?? null;
    const updatedAtMs = observedAt ? Date.parse(observedAt) : null;
    for (const [key, value] of Object.entries(snapshot.runtime_values)) {
      rows.push({
        core_run_id: snapshot.core_run_id,
        key,
        value,
        source: "worker_runtime_snapshot",
        updated_at_ms: typeof updatedAtMs === "number" && Number.isFinite(updatedAtMs) ? updatedAtMs : null,
        observed_at: observedAt,
        snapshot_seq: snapshot.seq,
        row: compactObject({
          source: "worker_runtime_snapshot",
          core_run_id: snapshot.core_run_id,
          key,
        }),
      });
    }
  }
  return rows;
}

export function runtimeValuesFromSnapshots(snapshots: RuntimeSnapshotRecord[], key: string): JsonObject[] {
  return runtimeValueRecordsFromSnapshots(snapshots)
    .filter((row) => row.key === key)
    .map((row) => row.value);
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
          source: "worker_runtime_snapshot",
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
          resource_refs: [],
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
        rows.push(enrichAttemptObject({
          core_run_id: snapshot.core_run_id,
          trial_id: trialId,
          schedule_idx: scheduleIdx,
          attempt,
          role: roleRef.endsWith("_ref") ? roleRef.slice(0, -"_ref".length) : roleRef,
          object_ref: objectRef,
          metadata: record,
          recorded_at_ms: numberField(record.recorded_at_ms) ?? 0,
        }));
      }
    }
  }
  return rows;
}

function enrichAttemptObject(row: Omit<RuntimeAttemptObjectRecord, "content_available" | "media_type" | "byte_size" | "sha256" | "relative_path">): RuntimeAttemptObjectRecord {
  const digestHex = artifactDigestHex(row.object_ref);
  return {
    ...row,
    content_available: digestHex !== null,
    media_type: mediaTypeForAttemptObject(row),
    byte_size: null,
    sha256: digestHex ? `sha256:${digestHex}` : "",
    relative_path: "",
  };
}

function artifactStoreBlobPath(artifactRoot: string, objectRef: string): string {
  const digestHex = artifactDigestHex(objectRef);
  if (!digestHex) {
    throw new HttpError(409, "runtime_artifact_content_unavailable", "Runtime artifact content is not a local artifact-store ref", {
      object_ref: objectRef,
    });
  }
  const root = resolve(artifactRoot);
  const path = resolve(root, "sha256", digestHex, "blob");
  if (path !== root && !path.startsWith(`${root}${sep}`)) {
    throw new HttpError(409, "runtime_artifact_content_unavailable", "Runtime artifact content path escaped the artifact root", {
      object_ref: objectRef,
    });
  }
  return path;
}

function artifactDigestHex(objectRef: string): string | null {
  const match = objectRef.match(/^artifact:\/\/sha256\/([0-9a-f]{64})$/);
  const digestHex = match?.[1];
  return digestHex ?? null;
}

function mediaTypeForAttemptObject(row: Pick<RuntimeAttemptObjectRecord, "role" | "metadata">): string {
  const fromMetadata = metadataString(row.metadata, "media_type")
    ?? metadataString(row.metadata, "content_type")
    ?? metadataString(row.metadata, "mime_type");
  if (fromMetadata) {
    return fromMetadata;
  }
  switch (row.role) {
    case "trial_input":
    case "trial_output":
    case "harness_request":
    case "harness_response":
      return "application/json; charset=utf-8";
    case "events":
      return "application/x-ndjson; charset=utf-8";
    case "stdout":
    case "stderr":
      return "text/plain; charset=utf-8";
    default:
      return "application/octet-stream";
  }
}

function metadataString(metadata: JsonValue | null, key: string): string | null {
  if (!isRecord(metadata)) {
    return null;
  }
  const value = metadata[key];
  return typeof value === "string" && value.trim() ? value : null;
}

function maxRuntimeArtifactBytes(): number {
  const raw = process.env.BUCEPHALUS_CLOUD_MAX_RUNTIME_ARTIFACT_BYTES;
  if (!raw) {
    return DEFAULT_MAX_RUNTIME_ARTIFACT_BYTES;
  }
  const parsed = Number.parseInt(raw, 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : DEFAULT_MAX_RUNTIME_ARTIFACT_BYTES;
}

function runtimeValueFromRow(row: Record<string, unknown>): RuntimeValueRecord {
  const updatedAtMs = Number(row.updated_at_ms);
  return {
    core_run_id: String(row.run_id),
    key: String(row.key),
    value: parseObject(row.value_json),
    source: "bucephalus_runtime.runtime_kv",
    updated_at_ms: Number.isFinite(updatedAtMs) ? updatedAtMs : null,
    observed_at: Number.isFinite(updatedAtMs) ? runtimeIsoFromMs(updatedAtMs) ?? null : null,
    row: {
      source: "bucephalus_runtime.runtime_kv",
    },
  };
}

function runtimeValueRecordKey(row: Pick<RuntimeValueRecord, "core_run_id" | "key">): string {
  return `${row.core_run_id}\0${row.key}`;
}

function runtimeValueSummary(value: JsonObject): string {
  const preferred = [
    "status",
    "phase",
    "state",
    "active_trials",
    "committed",
    "running",
    "completed",
    "failed",
    "total",
  ].flatMap((key) => {
    const item = value[key];
    return item === undefined || item === null || typeof item === "object" ? [] : [`${key}=${String(item)}`];
  });
  if (preferred.length > 0) {
    return preferred.slice(0, 4).join(", ");
  }
  const keys = Object.keys(value).sort();
  return keys.length > 0 ? `${keys.slice(0, 4).join(", ")}${keys.length > 4 ? ", ..." : ""}` : "empty object";
}

function runtimeMetricObservationFromRow(row: Record<string, unknown>): RuntimeMetricObservationRecord {
  return {
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
  };
}

function runtimeMetricObservationKey(
  row: Pick<RuntimeMetricObservationRecord, "core_run_id" | "trial_id" | "attempt" | "row_seq" | "metric_name">,
): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}\0${row.row_seq}\0${row.metric_name}`;
}

function runtimePerformanceSampleFromRow(row: Record<string, unknown>): RuntimePerformanceSampleRecord {
  return {
    core_run_id: String(row.run_id),
    sample_id: String(row.sample_id),
    trial_id: row.trial_id === null ? null : String(row.trial_id),
    schedule_idx: row.schedule_idx === null ? null : Number(row.schedule_idx),
    attempt: row.attempt === null ? null : Number(row.attempt),
    sample_seq: Number(row.sample_seq),
    sample_kind: String(row.sample_kind),
    stage: String(row.stage),
    duration_ms: row.duration_ms === null ? null : Number(row.duration_ms),
    process_rss_kb: row.process_rss_kb === null ? null : Number(row.process_rss_kb),
    payload: parseObject(row.payload_json),
    recorded_at_ms: Number(row.recorded_at_ms),
  };
}

function runtimeRunManifestFromRow(row: Record<string, unknown>): RuntimeRunManifestRecord {
  return {
    core_run_id: String(row.run_id),
    experiment_id: row.experiment_id === null ? null : String(row.experiment_id),
    project_root: row.project_root === null ? null : String(row.project_root),
    run_dir: row.run_dir === null ? null : String(row.run_dir),
    artifact_root: row.artifact_root === null ? null : String(row.artifact_root),
    runtime_status: row.status === null ? null : String(row.status),
    manifest: parseObject(row.manifest_json),
    updated_at_ms: Number(row.updated_at_ms),
  };
}

function runtimeCoreRunFromRow(row: Record<string, unknown>): RuntimeCoreRunRecord {
  return {
    core_run_id: String(row.run_id),
    experiment_id: row.experiment_id === null ? null : String(row.experiment_id),
    project_root: row.project_root === null ? null : String(row.project_root),
    run_dir: String(row.run_dir),
    artifact_root: String(row.artifact_root),
    runtime_status: String(row.status),
    manifest: parseObject(row.manifest_json),
    created_at_ms: Number(row.created_at_ms),
    updated_at_ms: Number(row.updated_at_ms),
  };
}

function runtimeMetricDefinitionFromRow(row: Record<string, unknown>): RuntimeMetricDefinitionRecord {
  return {
    experiment_id: String(row.experiment_id),
    metric_id: String(row.metric_id),
    semantic_key: row.semantic_key === null ? null : String(row.semantic_key),
    label: row.label === null ? null : String(row.label),
    value_type: row.value_type === null ? null : String(row.value_type),
    unit: row.unit === null ? null : String(row.unit),
    direction: row.direction === null ? null : String(row.direction),
    source_type: String(row.source_type),
    source_pointer: row.source_pointer === null ? null : String(row.source_pointer),
    required: Number(row.required) !== 0,
    primary_metric: Number(row.primary_metric) !== 0,
    definition: parseObject(row.definition_json),
    updated_at_ms: Number(row.updated_at_ms),
  };
}

function runtimeSlotCommitFromRow(row: Record<string, unknown>): RuntimeSlotCommitRecord {
  return {
    core_run_id: String(row.run_id),
    schedule_idx: Number(row.schedule_idx),
    attempt: Number(row.attempt),
    record_type: String(row.record_type),
    slot_commit_id: String(row.slot_commit_id),
    record: parseObject(row.record_json),
    recorded_at_ms: Number(row.recorded_at_ms),
  };
}

function runtimePendingTrialCompletionFromRow(row: Record<string, unknown>): RuntimePendingTrialCompletionRecord {
  return {
    core_run_id: String(row.run_id),
    schedule_idx: Number(row.schedule_idx),
    trial_result: parseObject(row.trial_result_json),
    updated_at_ms: Number(row.updated_at_ms),
  };
}

function runtimeVariantSnapshotFromRow(row: Record<string, unknown>): RuntimeVariantSnapshotRecord {
  return {
    core_run_id: String(row.run_id),
    trial_id: String(row.trial_id),
    schedule_idx: Number(row.schedule_idx),
    attempt: Number(row.attempt),
    row_seq: Number(row.row_seq),
    slot_commit_id: String(row.slot_commit_id),
    variant_id: String(row.variant_id),
    baseline_id: String(row.baseline_id),
    task_id: String(row.task_id),
    repl_idx: Number(row.repl_idx),
    binding_name: String(row.binding_name),
    binding_value: parseJson(row.binding_value_json),
    binding_value_text: String(row.binding_value_text),
    row: parseObject(row.row_json),
  };
}

function runtimeProvenanceRowFromRow(row: Record<string, unknown>): RuntimeProvenanceRowRecord {
  return {
    core_run_id: String(row.run_id),
    schedule_idx: Number(row.schedule_idx),
    attempt: Number(row.attempt),
    row_seq: Number(row.row_seq),
    slot_commit_id: String(row.slot_commit_id),
    row: parseObject(row.row_json),
  };
}

function runtimeLineageVersionFromRow(row: Record<string, unknown>): RuntimeLineageVersionRecord {
  return {
    core_run_id: String(row.run_id),
    version_id: String(row.version_id),
    chain_key: String(row.chain_key),
    step_index: Number(row.step_index),
    trial_id: String(row.trial_id),
    parent_version_id: row.parent_version_id === null ? null : String(row.parent_version_id),
    pre_snapshot_ref: row.pre_snapshot_ref === null ? null : String(row.pre_snapshot_ref),
    post_snapshot_ref: row.post_snapshot_ref === null ? null : String(row.post_snapshot_ref),
    diff_incremental_ref: row.diff_incremental_ref === null ? null : String(row.diff_incremental_ref),
    diff_cumulative_ref: row.diff_cumulative_ref === null ? null : String(row.diff_cumulative_ref),
    patch_incremental_ref: row.patch_incremental_ref === null ? null : String(row.patch_incremental_ref),
    patch_cumulative_ref: row.patch_cumulative_ref === null ? null : String(row.patch_cumulative_ref),
    workspace_ref: row.workspace_ref === null ? null : String(row.workspace_ref),
    checkpoint_labels: parseObject(row.checkpoint_labels_json),
  };
}

function runtimeLineageHeadFromRow(row: Record<string, unknown>): RuntimeLineageHeadRecord {
  return {
    core_run_id: String(row.run_id),
    chain_key: String(row.chain_key),
    latest_version_id: String(row.latest_version_id),
    step_index: Number(row.step_index),
    latest_workspace_ref: row.latest_workspace_ref === null ? null : String(row.latest_workspace_ref),
  };
}

function runtimeOperationFromRow(row: Record<string, unknown>): RuntimeOperationRecord {
  return {
    core_run_id: String(row.run_id),
    op_kind: String(row.op_kind),
    op_id: String(row.op_id),
    payload: parseObject(row.payload_json),
    updated_at_ms: Number(row.updated_at_ms),
  };
}

function runtimeContractStageFromRow(row: Record<string, unknown>): RuntimeContractStageRecord {
  return {
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
  };
}

function runtimeContractStageKey(
  row: Pick<RuntimeContractStageRecord, "core_run_id" | "trial_id" | "attempt" | "stage">,
): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}\0${row.stage}`;
}

function runtimeTrialResultKey(row: Pick<RuntimeTrialResultRecord, "core_run_id" | "trial_id" | "attempt">): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}`;
}

function runtimeEventKey(row: Pick<RuntimeEventRecord, "core_run_id" | "trial_id" | "attempt" | "row_seq">): string {
  return `${row.core_run_id}\0${row.trial_id}\0${row.attempt}\0${row.row_seq}`;
}

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

function eventTime(row: Pick<RuntimeEventRecord, "ts">): number {
  if (!row.ts) {
    return 0;
  }
  const parsed = Date.parse(row.ts);
  return Number.isFinite(parsed) ? parsed : 0;
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
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "bigint") {
    const numeric = Number(value);
    return Number.isSafeInteger(numeric) ? numeric : null;
  }
  if (typeof value === "string" && value.trim().length > 0) {
    const numeric = Number(value);
    return Number.isFinite(numeric) && Number.isSafeInteger(numeric) ? numeric : null;
  }
  return null;
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

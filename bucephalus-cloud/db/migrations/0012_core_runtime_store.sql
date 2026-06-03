CREATE SCHEMA IF NOT EXISTS bucephalus_runtime;

CREATE TABLE IF NOT EXISTS bucephalus_runtime.account_profile (
  account_id TEXT PRIMARY KEY,
  profile_json TEXT NOT NULL,
  created_at_ms BIGINT NOT NULL,
  updated_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.runs (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  experiment_id TEXT,
  project_root TEXT,
  run_dir TEXT NOT NULL,
  artifact_root TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at_ms BIGINT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  manifest_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.runtime_kv (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  key TEXT NOT NULL,
  value_json TEXT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, key)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.run_manifests (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  manifest_json TEXT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.metric_definitions (
  account_id TEXT NOT NULL,
  experiment_id TEXT NOT NULL,
  metric_id TEXT NOT NULL,
  semantic_key TEXT,
  label TEXT,
  value_type TEXT,
  unit TEXT,
  direction TEXT,
  source_type TEXT NOT NULL,
  source_pointer TEXT,
  required BIGINT NOT NULL,
  primary_metric BIGINT NOT NULL,
  definition_json TEXT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, experiment_id, metric_id)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.slot_commit_records (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  record_type TEXT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  record_json TEXT NOT NULL,
  recorded_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, schedule_idx, attempt, record_type)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.pending_trial_completions (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  trial_result_json TEXT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, schedule_idx)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.schedule_slots (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  state TEXT NOT NULL,
  slot_json TEXT NOT NULL,
  trial_id TEXT,
  attempt BIGINT NOT NULL,
  worker_id TEXT,
  owner_id TEXT,
  lease_epoch BIGINT NOT NULL,
  lease_expires_at TEXT,
  slot_commit_id TEXT,
  slot_status TEXT,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, schedule_idx)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.trial_attempts (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  phase TEXT NOT NULL,
  paused_from_phase TEXT,
  variant_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  repl_idx BIGINT NOT NULL,
  state_json TEXT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, attempt)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.trial_attempt_containers (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  role TEXT NOT NULL,
  container_id TEXT NOT NULL,
  status TEXT NOT NULL,
  image TEXT,
  workdir TEXT,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, attempt, role, container_id)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.trial_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  baseline_id TEXT NOT NULL,
  workload_type TEXT NOT NULL,
  variant_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  repl_idx BIGINT NOT NULL,
  outcome TEXT NOT NULL,
  primary_metric_name TEXT NOT NULL,
  primary_metric_value_json TEXT NOT NULL,
  metrics_json TEXT NOT NULL,
  bindings_json TEXT NOT NULL,
  events_total BIGINT NOT NULL,
  has_events BIGINT NOT NULL,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.metric_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  variant_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  repl_idx BIGINT NOT NULL,
  outcome TEXT NOT NULL,
  metric_name TEXT NOT NULL,
  metric_value_json TEXT NOT NULL,
  metric_source TEXT,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.event_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  variant_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  repl_idx BIGINT NOT NULL,
  seq BIGINT NOT NULL,
  event_type TEXT NOT NULL,
  ts TEXT,
  payload_json TEXT NOT NULL,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.contract_stage_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  variant_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  repl_idx BIGINT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  detail_json TEXT NOT NULL,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.variant_snapshot_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  variant_id TEXT NOT NULL,
  baseline_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  repl_idx BIGINT NOT NULL,
  binding_name TEXT NOT NULL,
  binding_value_json TEXT NOT NULL,
  binding_value_text TEXT NOT NULL,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.evidence_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.chain_state_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.benchmark_conclusion_rows (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  row_seq BIGINT NOT NULL,
  slot_commit_id TEXT NOT NULL,
  row_json TEXT NOT NULL,
  PRIMARY KEY (account_id, run_id, schedule_idx, attempt, row_seq)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.attempt_objects (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  role TEXT NOT NULL,
  object_ref TEXT NOT NULL,
  metadata_json TEXT,
  recorded_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, role)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.lineage_versions (
  account_id TEXT NOT NULL,
  version_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  chain_key TEXT NOT NULL,
  step_index BIGINT NOT NULL,
  trial_id TEXT NOT NULL,
  parent_version_id TEXT,
  pre_snapshot_ref TEXT,
  post_snapshot_ref TEXT,
  diff_incremental_ref TEXT,
  diff_cumulative_ref TEXT,
  patch_incremental_ref TEXT,
  patch_cumulative_ref TEXT,
  workspace_ref TEXT,
  checkpoint_labels_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.lineage_heads (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  chain_key TEXT NOT NULL,
  latest_version_id TEXT NOT NULL,
  step_index BIGINT NOT NULL,
  latest_workspace_ref TEXT,
  PRIMARY KEY (account_id, run_id, chain_key)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.runtime_ops (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  op_kind TEXT NOT NULL,
  op_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, op_kind, op_id)
);

CREATE TABLE IF NOT EXISTS bucephalus_runtime.performance_samples (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  sample_id TEXT NOT NULL,
  trial_id TEXT,
  schedule_idx BIGINT,
  attempt BIGINT,
  sample_seq BIGINT NOT NULL,
  sample_kind TEXT NOT NULL,
  stage TEXT NOT NULL,
  duration_ms DOUBLE PRECISION,
  process_rss_kb BIGINT,
  payload_json TEXT NOT NULL,
  recorded_at_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, run_id, sample_id)
);

CREATE INDEX IF NOT EXISTS idx_schedule_slots_pending
  ON bucephalus_runtime.schedule_slots (account_id, run_id, state, schedule_idx);

CREATE INDEX IF NOT EXISTS idx_trial_attempts_phase
  ON bucephalus_runtime.trial_attempts (account_id, run_id, phase, schedule_idx);

CREATE INDEX IF NOT EXISTS idx_event_rows_run_order
  ON bucephalus_runtime.event_rows (account_id, run_id, schedule_idx, attempt, row_seq);

CREATE INDEX IF NOT EXISTS idx_event_rows_type
  ON bucephalus_runtime.event_rows (account_id, run_id, event_type);

CREATE INDEX IF NOT EXISTS idx_attempt_objects_trial_role
  ON bucephalus_runtime.attempt_objects (account_id, run_id, trial_id, role, attempt DESC);

CREATE INDEX IF NOT EXISTS idx_lineage_versions_trial
  ON bucephalus_runtime.lineage_versions (account_id, run_id, trial_id, step_index DESC);

CREATE INDEX IF NOT EXISTS idx_performance_samples_stage
  ON bucephalus_runtime.performance_samples (account_id, run_id, stage, recorded_at_ms DESC);

-- Bucephalus Cloud schema v1.
--
-- This schema is intentionally not a runtime control plane. Local Bucephalus
-- Core remains authoritative for active trials, leases, recovery, and local slot
-- commit decisions. Postgres stores registered content identities and committed
-- facts for cross-run analysis.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE SCHEMA IF NOT EXISTS registry;
CREATE SCHEMA IF NOT EXISTS fact;
CREATE SCHEMA IF NOT EXISTS ingest;

CREATE DOMAIN registry.sha256_digest AS text
  CHECK (VALUE ~ '^sha256:[0-9a-f]{64}$');

CREATE DOMAIN registry.nonempty_text AS text
  CHECK (length(btrim(VALUE)) > 0);

CREATE TYPE registry.content_kind AS ENUM (
  'agent_app',
  'case',
  'dataset',
  'experiment_package',
  'grader',
  'metric',
  'runtime_profile',
  'task_boundary',
  'trial_contract',
  'variant'
);

CREATE TYPE registry.metric_direction AS ENUM (
  'maximize',
  'minimize',
  'target',
  'none'
);

CREATE TYPE registry.metric_value_type AS ENUM (
  'boolean',
  'integer',
  'number',
  'string',
  'json'
);

CREATE TYPE fact.artifact_kind AS ENUM (
  'agent_result',
  'candidate_patch',
  'workspace_diff',
  'grader_output',
  'mapped_grader_output',
  'stdout',
  'stderr',
  'trace',
  'other'
);

CREATE TYPE ingest.batch_status AS ENUM (
  'received',
  'applied',
  'rejected'
);

CREATE TABLE registry.content_objects (
  content_digest registry.sha256_digest PRIMARY KEY,
  kind registry.content_kind NOT NULL,
  schema_version registry.nonempty_text NOT NULL,
  canonical_json jsonb NOT NULL,
  canonical_size_bytes bigint NOT NULL CHECK (canonical_size_bytes >= 0),
  created_at timestamptz NOT NULL DEFAULT now(),
  created_by text,
  source_uri text,
  CONSTRAINT content_objects_canonical_json_is_object
    CHECK (jsonb_typeof(canonical_json) = 'object')
);

ALTER TABLE registry.content_objects
  ADD CONSTRAINT content_objects_digest_kind_unique UNIQUE (content_digest, kind);

COMMENT ON TABLE registry.content_objects IS
  'Immutable content-addressed registry objects. Display names are not identity.';

CREATE TABLE registry.entity_aliases (
  alias_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  scope_type registry.nonempty_text NOT NULL DEFAULT 'global',
  scope_id text,
  kind registry.content_kind NOT NULL,
  alias registry.nonempty_text NOT NULL,
  content_digest registry.sha256_digest NOT NULL REFERENCES registry.content_objects(content_digest),
  created_at timestamptz NOT NULL DEFAULT now(),
  retired_at timestamptz,
  FOREIGN KEY (content_digest, kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE UNIQUE INDEX entity_aliases_active_unique_idx
  ON registry.entity_aliases(scope_type, COALESCE(scope_id, ''), kind, alias)
  WHERE retired_at IS NULL;

CREATE TABLE registry.agent_apps (
  agent_app_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'agent_app'
    CHECK (content_kind = 'agent_app'),
  display_name registry.nonempty_text NOT NULL,
  app_version text,
  command_fingerprint registry.sha256_digest,
  source_uri text,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (agent_app_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE TABLE registry.datasets (
  dataset_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'dataset'
    CHECK (content_kind = 'dataset'),
  display_name registry.nonempty_text NOT NULL,
  source_uri text,
  case_count integer CHECK (case_count IS NULL OR case_count >= 0),
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (dataset_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE TABLE registry.task_boundaries (
  task_boundary_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'task_boundary'
    CHECK (content_kind = 'task_boundary'),
  boundary_schema_version registry.nonempty_text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (task_boundary_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE TABLE registry.cases (
  case_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'case'
    CHECK (content_kind = 'case'),
  dataset_digest registry.sha256_digest REFERENCES registry.datasets(dataset_digest),
  task_boundary_digest registry.sha256_digest REFERENCES registry.task_boundaries(task_boundary_digest),
  external_case_id text,
  display_name text,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (case_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE INDEX cases_dataset_idx ON registry.cases(dataset_digest);
CREATE UNIQUE INDEX cases_dataset_external_id_idx
  ON registry.cases(dataset_digest, external_case_id)
  WHERE dataset_digest IS NOT NULL AND external_case_id IS NOT NULL;

CREATE TABLE registry.graders (
  grader_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'grader'
    CHECK (content_kind = 'grader'),
  display_name registry.nonempty_text NOT NULL,
  transport registry.nonempty_text NOT NULL,
  output_contract_digest registry.sha256_digest,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (grader_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE TABLE registry.metrics (
  metric_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'metric'
    CHECK (content_kind = 'metric'),
  stable_key registry.nonempty_text NOT NULL,
  display_name text,
  direction registry.metric_direction NOT NULL,
  value_type registry.metric_value_type NOT NULL,
  unit text,
  extraction_contract_digest registry.sha256_digest,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (metric_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE UNIQUE INDEX metrics_stable_key_digest_idx
  ON registry.metrics(stable_key, metric_digest);

CREATE TABLE registry.runtime_profiles (
  runtime_profile_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'runtime_profile'
    CHECK (content_kind = 'runtime_profile'),
  compute_backend registry.nonempty_text NOT NULL,
  storage_backend text,
  traces_backend text,
  network_profile text,
  resource_profile jsonb NOT NULL DEFAULT '{}'::jsonb,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (runtime_profile_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE TABLE registry.variants (
  variant_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'variant'
    CHECK (content_kind = 'variant'),
  agent_app_digest registry.sha256_digest REFERENCES registry.agent_apps(agent_app_digest),
  display_name registry.nonempty_text NOT NULL,
  config_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (variant_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE INDEX variants_agent_app_idx ON registry.variants(agent_app_digest);

CREATE TABLE registry.trial_contracts (
  trial_contract_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'trial_contract'
    CHECK (content_kind = 'trial_contract'),
  input_contract_digest registry.sha256_digest,
  output_contract_digest registry.sha256_digest,
  transport registry.nonempty_text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (trial_contract_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE TABLE registry.experiment_packages (
  package_digest registry.sha256_digest PRIMARY KEY
    REFERENCES registry.content_objects(content_digest),
  content_kind registry.content_kind NOT NULL DEFAULT 'experiment_package'
    CHECK (content_kind = 'experiment_package'),
  display_name registry.nonempty_text NOT NULL,
  experiment_id text,
  sealed_at timestamptz,
  trial_contract_digest registry.sha256_digest REFERENCES registry.trial_contracts(trial_contract_digest),
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  FOREIGN KEY (package_digest, content_kind)
    REFERENCES registry.content_objects(content_digest, kind)
);

CREATE TABLE registry.package_variants (
  package_digest registry.sha256_digest NOT NULL REFERENCES registry.experiment_packages(package_digest),
  variant_digest registry.sha256_digest NOT NULL REFERENCES registry.variants(variant_digest),
  local_variant_id registry.nonempty_text NOT NULL,
  baseline boolean NOT NULL DEFAULT false,
  PRIMARY KEY (package_digest, variant_digest),
  UNIQUE (package_digest, local_variant_id)
);

CREATE TABLE registry.package_cases (
  package_digest registry.sha256_digest NOT NULL REFERENCES registry.experiment_packages(package_digest),
  case_digest registry.sha256_digest NOT NULL REFERENCES registry.cases(case_digest),
  local_case_id text,
  PRIMARY KEY (package_digest, case_digest)
);

CREATE TABLE registry.package_metrics (
  package_digest registry.sha256_digest NOT NULL REFERENCES registry.experiment_packages(package_digest),
  metric_digest registry.sha256_digest NOT NULL REFERENCES registry.metrics(metric_digest),
  local_metric_id registry.nonempty_text NOT NULL,
  primary_metric boolean NOT NULL DEFAULT false,
  required boolean NOT NULL DEFAULT false,
  PRIMARY KEY (package_digest, metric_digest),
  UNIQUE (package_digest, local_metric_id)
);

CREATE TABLE fact.runs (
  run_id registry.nonempty_text PRIMARY KEY,
  package_digest registry.sha256_digest REFERENCES registry.experiment_packages(package_digest),
  run_label text,
  core_version text,
  runner_hostname text,
  started_at timestamptz,
  completed_at timestamptz,
  synced_at timestamptz NOT NULL DEFAULT now(),
  source_kind registry.nonempty_text NOT NULL DEFAULT 'core_upload',
  source_uri text,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE fact.trial_slots (
  run_id registry.nonempty_text NOT NULL REFERENCES fact.runs(run_id) ON DELETE CASCADE,
  schedule_idx integer NOT NULL CHECK (schedule_idx >= 0),
  slot_digest registry.sha256_digest,
  variant_digest registry.sha256_digest NOT NULL REFERENCES registry.variants(variant_digest),
  case_digest registry.sha256_digest NOT NULL REFERENCES registry.cases(case_digest),
  runtime_profile_digest registry.sha256_digest REFERENCES registry.runtime_profiles(runtime_profile_digest),
  repeat_idx integer CHECK (repeat_idx IS NULL OR repeat_idx >= 0),
  seed bigint,
  local_variant_id text,
  local_case_id text,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (run_id, schedule_idx)
);

CREATE INDEX trial_slots_variant_idx ON fact.trial_slots(variant_digest);
CREATE INDEX trial_slots_case_idx ON fact.trial_slots(case_digest);
CREATE INDEX trial_slots_variant_case_idx ON fact.trial_slots(variant_digest, case_digest);

CREATE TABLE fact.trial_attempts (
  run_id registry.nonempty_text NOT NULL REFERENCES fact.runs(run_id) ON DELETE CASCADE,
  trial_id registry.nonempty_text NOT NULL,
  schedule_idx integer NOT NULL,
  attempt_no integer NOT NULL CHECK (attempt_no >= 0),
  worker_id text,
  runtime_profile_digest registry.sha256_digest REFERENCES registry.runtime_profiles(runtime_profile_digest),
  started_at timestamptz,
  ended_at timestamptz,
  terminal_phase text,
  attempt_state jsonb NOT NULL DEFAULT '{}'::jsonb,
  PRIMARY KEY (run_id, trial_id, attempt_no),
  FOREIGN KEY (run_id, schedule_idx) REFERENCES fact.trial_slots(run_id, schedule_idx)
);

CREATE INDEX trial_attempts_slot_idx ON fact.trial_attempts(run_id, schedule_idx);

CREATE TABLE fact.slot_commits (
  slot_commit_id registry.nonempty_text PRIMARY KEY,
  run_id registry.nonempty_text NOT NULL REFERENCES fact.runs(run_id) ON DELETE CASCADE,
  schedule_idx integer NOT NULL,
  trial_id registry.nonempty_text NOT NULL,
  attempt_no integer NOT NULL CHECK (attempt_no >= 0),
  slot_status registry.nonempty_text NOT NULL,
  payload_digest registry.sha256_digest NOT NULL,
  committed_at timestamptz NOT NULL,
  committed_record jsonb NOT NULL,
  variant_digest registry.sha256_digest NOT NULL REFERENCES registry.variants(variant_digest),
  case_digest registry.sha256_digest NOT NULL REFERENCES registry.cases(case_digest),
  runtime_profile_digest registry.sha256_digest REFERENCES registry.runtime_profiles(runtime_profile_digest),
  UNIQUE (run_id, schedule_idx),
  FOREIGN KEY (run_id, schedule_idx) REFERENCES fact.trial_slots(run_id, schedule_idx)
);

CREATE INDEX slot_commits_variant_idx ON fact.slot_commits(variant_digest);
CREATE INDEX slot_commits_case_idx ON fact.slot_commits(case_digest);
CREATE INDEX slot_commits_status_idx ON fact.slot_commits(slot_status);
CREATE INDEX slot_commits_payload_digest_idx ON fact.slot_commits(payload_digest);

CREATE TABLE fact.metric_observations (
  slot_commit_id registry.nonempty_text NOT NULL REFERENCES fact.slot_commits(slot_commit_id) ON DELETE CASCADE,
  metric_digest registry.sha256_digest NOT NULL REFERENCES registry.metrics(metric_digest),
  row_seq integer NOT NULL DEFAULT 0 CHECK (row_seq >= 0),
  metric_value_number double precision,
  metric_value_text text,
  metric_value_json jsonb,
  observed_at timestamptz NOT NULL DEFAULT now(),
  source_payload jsonb NOT NULL DEFAULT '{}'::jsonb,
  PRIMARY KEY (slot_commit_id, metric_digest, row_seq),
  CONSTRAINT metric_observations_one_value CHECK (
    num_nonnulls(metric_value_number, metric_value_text, metric_value_json) = 1
  )
);

CREATE INDEX metric_observations_metric_idx ON fact.metric_observations(metric_digest);

CREATE TABLE fact.artifacts (
  artifact_digest registry.sha256_digest PRIMARY KEY,
  artifact_kind fact.artifact_kind NOT NULL,
  byte_size bigint CHECK (byte_size IS NULL OR byte_size >= 0),
  media_type text,
  storage_uri text,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE fact.slot_artifacts (
  slot_commit_id registry.nonempty_text NOT NULL REFERENCES fact.slot_commits(slot_commit_id) ON DELETE CASCADE,
  artifact_digest registry.sha256_digest NOT NULL REFERENCES fact.artifacts(artifact_digest),
  logical_name registry.nonempty_text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (slot_commit_id, logical_name, artifact_digest)
);

CREATE TABLE fact.event_records (
  slot_commit_id registry.nonempty_text NOT NULL REFERENCES fact.slot_commits(slot_commit_id) ON DELETE CASCADE,
  event_seq integer NOT NULL CHECK (event_seq >= 0),
  event_type registry.nonempty_text NOT NULL,
  occurred_at timestamptz,
  payload jsonb NOT NULL,
  PRIMARY KEY (slot_commit_id, event_seq)
);

CREATE INDEX event_records_type_idx ON fact.event_records(event_type);
CREATE INDEX event_records_payload_gin_idx ON fact.event_records USING gin(payload);

CREATE TABLE fact.failure_records (
  failure_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  slot_commit_id registry.nonempty_text NOT NULL REFERENCES fact.slot_commits(slot_commit_id) ON DELETE CASCADE,
  failure_kind registry.nonempty_text NOT NULL,
  failure_code text,
  message text,
  payload jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE ingest.batches (
  ingest_batch_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  source_run_id registry.nonempty_text NOT NULL,
  source_kind registry.nonempty_text NOT NULL DEFAULT 'core_upload',
  source_uri text,
  batch_digest registry.sha256_digest NOT NULL,
  status ingest.batch_status NOT NULL DEFAULT 'received',
  received_at timestamptz NOT NULL DEFAULT now(),
  applied_at timestamptz,
  error_message text,
  UNIQUE (source_kind, batch_digest)
);

CREATE TABLE ingest.batch_items (
  ingest_batch_id uuid NOT NULL REFERENCES ingest.batches(ingest_batch_id) ON DELETE CASCADE,
  item_seq integer NOT NULL CHECK (item_seq >= 0),
  item_kind registry.nonempty_text NOT NULL,
  content_digest registry.sha256_digest,
  slot_commit_id text,
  payload_digest registry.sha256_digest NOT NULL,
  applied_at timestamptz,
  PRIMARY KEY (ingest_batch_id, item_seq)
);

CREATE VIEW fact.variant_metric_summary AS
SELECT
  sc.variant_digest,
  sc.case_digest,
  mo.metric_digest,
  count(*) AS observation_count,
  avg(mo.metric_value_number) AS avg_value,
  min(mo.metric_value_number) AS min_value,
  max(mo.metric_value_number) AS max_value
FROM fact.slot_commits sc
JOIN fact.metric_observations mo
  ON mo.slot_commit_id = sc.slot_commit_id
WHERE mo.metric_value_number IS NOT NULL
GROUP BY sc.variant_digest, sc.case_digest, mo.metric_digest;

CREATE VIEW fact.cross_run_metric_observations AS
SELECT
  r.run_id,
  r.package_digest,
  ts.schedule_idx,
  ts.local_variant_id,
  ts.local_case_id,
  sc.slot_commit_id,
  sc.slot_status,
  sc.variant_digest,
  sc.case_digest,
  mo.metric_digest,
  m.stable_key AS metric_key,
  mo.metric_value_number,
  mo.metric_value_text,
  mo.metric_value_json,
  sc.committed_at
FROM fact.runs r
JOIN fact.trial_slots ts
  ON ts.run_id = r.run_id
JOIN fact.slot_commits sc
  ON sc.run_id = ts.run_id
 AND sc.schedule_idx = ts.schedule_idx
JOIN fact.metric_observations mo
  ON mo.slot_commit_id = sc.slot_commit_id
JOIN registry.metrics m
  ON m.metric_digest = mo.metric_digest;

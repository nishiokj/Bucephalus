CREATE SCHEMA IF NOT EXISTS cloud;

CREATE TYPE cloud.package_artifact_status AS ENUM (
  'accepted',
  'rejected'
);

CREATE TYPE cloud.run_status AS ENUM (
  'created',
  'waiting_for_runner',
  'running',
  'completed',
  'failed',
  'cancelled'
);

CREATE TABLE cloud.package_artifacts (
  package_digest registry.sha256_digest PRIMARY KEY,
  upload_id uuid REFERENCES ingest.uploads(upload_id),
  storage_path text,
  byte_size bigint CHECK (byte_size IS NULL OR byte_size >= 0),
  media_type text,
  manifest_json jsonb NOT NULL,
  resolved_experiment_json jsonb NOT NULL,
  target jsonb,
  diagnostics jsonb NOT NULL DEFAULT '[]'::jsonb,
  status cloud.package_artifact_status NOT NULL DEFAULT 'accepted',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT package_artifacts_manifest_is_object
    CHECK (jsonb_typeof(manifest_json) = 'object'),
  CONSTRAINT package_artifacts_resolved_experiment_is_object
    CHECK (jsonb_typeof(resolved_experiment_json) = 'object'),
  CONSTRAINT package_artifacts_target_is_object_or_null
    CHECK (target IS NULL OR jsonb_typeof(target) = 'object'),
  CONSTRAINT package_artifacts_diagnostics_is_array
    CHECK (jsonb_typeof(diagnostics) = 'array')
);

CREATE TABLE cloud.runs (
  run_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  package_digest registry.sha256_digest NOT NULL
    REFERENCES cloud.package_artifacts(package_digest),
  run_label text,
  status cloud.run_status NOT NULL DEFAULT 'created',
  env jsonb NOT NULL DEFAULT '{}'::jsonb,
  secret_refs jsonb NOT NULL DEFAULT '{}'::jsonb,
  runtime_options jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  started_at timestamptz,
  completed_at timestamptz,
  error_message text,
  CONSTRAINT runs_env_is_object
    CHECK (jsonb_typeof(env) = 'object'),
  CONSTRAINT runs_secret_refs_is_object
    CHECK (jsonb_typeof(secret_refs) = 'object'),
  CONSTRAINT runs_runtime_options_is_object
    CHECK (jsonb_typeof(runtime_options) = 'object')
);

CREATE INDEX package_artifacts_upload_idx ON cloud.package_artifacts(upload_id);
CREATE INDEX cloud_runs_package_idx ON cloud.runs(package_digest);
CREATE INDEX cloud_runs_status_idx ON cloud.runs(status);

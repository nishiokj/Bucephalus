CREATE TYPE ingest.upload_status AS ENUM (
  'created',
  'uploaded',
  'completed',
  'rejected'
);

CREATE TYPE ingest.import_status AS ENUM (
  'inspecting',
  'proposed',
  'applied',
  'failed'
);

CREATE TYPE ingest.proposal_status AS ENUM (
  'proposed',
  'registered',
  'skipped'
);

CREATE TABLE ingest.uploads (
  upload_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  filename registry.nonempty_text NOT NULL,
  media_type registry.nonempty_text NOT NULL DEFAULT 'application/octet-stream',
  expected_digest registry.sha256_digest,
  content_digest registry.sha256_digest,
  byte_size bigint CHECK (byte_size IS NULL OR byte_size >= 0),
  storage_path text,
  status ingest.upload_status NOT NULL DEFAULT 'created',
  created_at timestamptz NOT NULL DEFAULT now(),
  uploaded_at timestamptz,
  completed_at timestamptz,
  error_message text
);

CREATE TABLE ingest.import_jobs (
  import_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  upload_id uuid REFERENCES ingest.uploads(upload_id),
  import_type registry.nonempty_text NOT NULL,
  status ingest.import_status NOT NULL DEFAULT 'inspecting',
  label text,
  package_digest registry.sha256_digest,
  manifest_json jsonb,
  resolved_experiment_json jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  error_message text
);

CREATE TABLE ingest.import_proposals (
  proposal_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  import_id uuid NOT NULL REFERENCES ingest.import_jobs(import_id) ON DELETE CASCADE,
  kind registry.content_kind NOT NULL,
  content_digest registry.sha256_digest NOT NULL,
  schema_version registry.nonempty_text NOT NULL,
  canonical_json jsonb NOT NULL,
  canonical_size_bytes bigint NOT NULL CHECK (canonical_size_bytes >= 0),
  source_pointer registry.nonempty_text NOT NULL,
  suggested_aliases text[] NOT NULL DEFAULT array[]::text[],
  status ingest.proposal_status NOT NULL DEFAULT 'proposed',
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (import_id, content_digest)
);

CREATE TABLE ingest.import_action_results (
  result_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  import_id uuid NOT NULL REFERENCES ingest.import_jobs(import_id) ON DELETE CASCADE,
  proposal_id uuid NOT NULL REFERENCES ingest.import_proposals(proposal_id) ON DELETE CASCADE,
  action registry.nonempty_text NOT NULL,
  status registry.nonempty_text NOT NULL,
  message text,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX uploads_status_idx ON ingest.uploads(status);
CREATE INDEX import_jobs_status_idx ON ingest.import_jobs(status);
CREATE INDEX import_proposals_import_idx ON ingest.import_proposals(import_id);


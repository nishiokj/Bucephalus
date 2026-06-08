CREATE TABLE cloud.latch_submissions (
  submission_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  dispatch_id registry.nonempty_text NOT NULL UNIQUE,
  upload_id uuid NOT NULL REFERENCES ingest.uploads(upload_id) ON DELETE RESTRICT,
  owner_key text,
  benchmark_ref registry.nonempty_text NOT NULL,
  benchmark_digest registry.sha256_digest,
  resolution_id registry.nonempty_text,
  archive_digest registry.sha256_digest NOT NULL,
  grading_status registry.nonempty_text,
  summary_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  lifecycle_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  result_json jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT latch_submissions_summary_is_object CHECK (jsonb_typeof(summary_json) = 'object'),
  CONSTRAINT latch_submissions_lifecycle_is_object CHECK (jsonb_typeof(lifecycle_json) = 'object'),
  CONSTRAINT latch_submissions_result_is_object CHECK (jsonb_typeof(result_json) = 'object')
);

CREATE INDEX latch_submissions_owner_idx ON cloud.latch_submissions(owner_key, created_at DESC);
CREATE INDEX latch_submissions_benchmark_idx ON cloud.latch_submissions(benchmark_ref, created_at DESC);
CREATE INDEX latch_submissions_upload_idx ON cloud.latch_submissions(upload_id);

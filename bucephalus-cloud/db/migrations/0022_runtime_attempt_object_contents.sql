CREATE TABLE IF NOT EXISTS bucephalus_runtime.attempt_object_contents (
  account_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  trial_id TEXT NOT NULL,
  schedule_idx BIGINT NOT NULL,
  attempt BIGINT NOT NULL,
  role TEXT NOT NULL,
  object_ref TEXT NOT NULL,
  storage_path TEXT NOT NULL,
  media_type TEXT NOT NULL,
  byte_size BIGINT NOT NULL CHECK (byte_size >= 0),
  sha256 TEXT NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
  relative_path TEXT,
  metadata_json TEXT,
  recorded_at_ms BIGINT NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (account_id, run_id, trial_id, schedule_idx, attempt, role)
);

CREATE INDEX IF NOT EXISTS idx_attempt_object_contents_trial_role
  ON bucephalus_runtime.attempt_object_contents (account_id, run_id, trial_id, role, attempt DESC);

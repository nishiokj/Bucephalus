CREATE TYPE cloud.run_attempt_status AS ENUM (
  'running',
  'completed',
  'failed',
  'expired'
);

CREATE TABLE cloud.run_attempts (
  attempt_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  run_id uuid NOT NULL REFERENCES cloud.runs(run_id) ON DELETE CASCADE,
  worker_id registry.nonempty_text NOT NULL,
  status cloud.run_attempt_status NOT NULL DEFAULT 'running',
  lease_expires_at timestamptz NOT NULL,
  heartbeat_at timestamptz NOT NULL DEFAULT now(),
  started_at timestamptz NOT NULL DEFAULT now(),
  ended_at timestamptz,
  error_message text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cloud.run_events (
  event_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  run_id uuid NOT NULL REFERENCES cloud.runs(run_id) ON DELETE CASCADE,
  attempt_id uuid REFERENCES cloud.run_attempts(attempt_id) ON DELETE SET NULL,
  seq bigint NOT NULL,
  event_type registry.nonempty_text NOT NULL,
  payload jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (run_id, seq),
  CONSTRAINT run_events_payload_is_object
    CHECK (jsonb_typeof(payload) = 'object')
);

CREATE INDEX run_attempts_run_idx ON cloud.run_attempts(run_id);
CREATE INDEX run_attempts_worker_idx ON cloud.run_attempts(worker_id);
CREATE INDEX run_attempts_lease_idx
  ON cloud.run_attempts(lease_expires_at)
  WHERE status = 'running';
CREATE INDEX run_events_run_seq_idx ON cloud.run_events(run_id, seq);

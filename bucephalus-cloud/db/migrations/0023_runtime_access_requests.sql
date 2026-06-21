CREATE TYPE cloud.runtime_access_request_kind AS ENUM (
  'port_forward',
  'exec'
);

CREATE TYPE cloud.runtime_access_request_status AS ENUM (
  'requested',
  'accepted',
  'active',
  'completed',
  'cancelled',
  'expired',
  'failed'
);

CREATE TABLE cloud.runtime_access_requests (
  access_request_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  run_id uuid NOT NULL REFERENCES cloud.runs(run_id) ON DELETE CASCADE,
  kind cloud.runtime_access_request_kind NOT NULL,
  status cloud.runtime_access_request_status NOT NULL DEFAULT 'requested',
  resource_kind registry.nonempty_text NOT NULL,
  resource_name registry.nonempty_text NOT NULL,
  target_uid text,
  target_resource_version text,
  protocol registry.nonempty_text NOT NULL DEFAULT 'tcp',
  target_port integer CHECK (target_port IS NULL OR target_port BETWEEN 1 AND 65535),
  local_port integer CHECK (local_port IS NULL OR local_port BETWEEN 1 AND 65535),
  command jsonb NOT NULL DEFAULT '[]'::jsonb,
  runner_instance_id uuid REFERENCES cloud.runner_instances(runner_instance_id),
  attempt_id uuid REFERENCES cloud.run_attempts(attempt_id) ON DELETE SET NULL,
  worker_id text,
  requester text,
  reason text,
  connection jsonb NOT NULL DEFAULT '{}'::jsonb,
  error_message text,
  expires_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT runtime_access_requests_connection_is_object
    CHECK (jsonb_typeof(connection) = 'object'),
  CONSTRAINT runtime_access_requests_command_is_array
    CHECK (jsonb_typeof(command) = 'array'),
  CONSTRAINT runtime_access_requests_kind_shape
    CHECK (
      (kind = 'port_forward' AND target_port IS NOT NULL)
      OR
      (kind = 'exec' AND target_port IS NULL AND jsonb_array_length(command) > 0)
    )
);

CREATE INDEX runtime_access_requests_run_idx
  ON cloud.runtime_access_requests(run_id, created_at DESC);

CREATE INDEX runtime_access_requests_attempt_idx
  ON cloud.runtime_access_requests(attempt_id)
  WHERE status IN ('requested', 'accepted', 'active');

CREATE INDEX runtime_access_requests_runner_idx
  ON cloud.runtime_access_requests(runner_instance_id)
  WHERE status IN ('requested', 'accepted', 'active');

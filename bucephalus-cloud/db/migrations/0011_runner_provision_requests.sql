CREATE TYPE cloud.runner_provision_status AS ENUM (
  'requested',
  'provisioning',
  'active',
  'failed',
  'reaped'
);

CREATE TABLE cloud.runner_provision_requests (
  provision_request_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  runner_pool_id uuid NOT NULL REFERENCES cloud.runner_pools(runner_pool_id),
  run_id uuid REFERENCES cloud.runs(run_id) ON DELETE SET NULL,
  status cloud.runner_provision_status NOT NULL DEFAULT 'requested',
  provider registry.nonempty_text NOT NULL,
  provider_instance_id text,
  instance_name text,
  runner_instance_id uuid REFERENCES cloud.runner_instances(runner_instance_id),
  requirements jsonb NOT NULL DEFAULT '{}'::jsonb,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  error_message text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT runner_provision_requirements_is_object
    CHECK (jsonb_typeof(requirements) = 'object'),
  CONSTRAINT runner_provision_metadata_is_object
    CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE UNIQUE INDEX runner_provision_one_open_per_run_idx
  ON cloud.runner_provision_requests(run_id)
  WHERE run_id IS NOT NULL
    AND status IN ('requested', 'provisioning');

CREATE INDEX runner_provision_pool_status_idx
  ON cloud.runner_provision_requests(runner_pool_id, status);

CREATE INDEX runner_provision_runner_instance_idx
  ON cloud.runner_provision_requests(runner_instance_id);

CREATE INDEX run_attempts_running_runner_instance_idx
  ON cloud.run_attempts(runner_instance_id)
  WHERE status = 'running';

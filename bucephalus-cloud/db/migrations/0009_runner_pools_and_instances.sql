CREATE TYPE cloud.runner_pool_status AS ENUM (
  'active',
  'draining',
  'disabled'
);

CREATE TYPE cloud.runner_instance_status AS ENUM (
  'online',
  'draining',
  'offline'
);

CREATE TABLE cloud.runner_pools (
  runner_pool_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  name registry.nonempty_text NOT NULL,
  status cloud.runner_pool_status NOT NULL DEFAULT 'active',
  capabilities jsonb NOT NULL DEFAULT '{"executors":[],"resources":[]}'::jsonb,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT runner_pools_capabilities_is_object
    CHECK (jsonb_typeof(capabilities) = 'object'),
  CONSTRAINT runner_pools_metadata_is_object
    CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE TABLE cloud.runner_instances (
  runner_instance_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  runner_pool_id uuid NOT NULL REFERENCES cloud.runner_pools(runner_pool_id),
  instance_name registry.nonempty_text NOT NULL,
  status cloud.runner_instance_status NOT NULL DEFAULT 'online',
  capabilities jsonb NOT NULL DEFAULT '{"executors":[],"resources":[]}'::jsonb,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  last_heartbeat_at timestamptz NOT NULL DEFAULT now(),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT runner_instances_capabilities_is_object
    CHECK (jsonb_typeof(capabilities) = 'object'),
  CONSTRAINT runner_instances_metadata_is_object
    CHECK (jsonb_typeof(metadata) = 'object')
);

ALTER TABLE cloud.run_attempts
  ADD COLUMN runner_instance_id uuid REFERENCES cloud.runner_instances(runner_instance_id);

CREATE INDEX runner_pools_status_idx ON cloud.runner_pools(status);
CREATE INDEX runner_instances_pool_idx ON cloud.runner_instances(runner_pool_id);
CREATE INDEX runner_instances_status_idx ON cloud.runner_instances(status);
CREATE INDEX run_attempts_runner_instance_idx ON cloud.run_attempts(runner_instance_id);

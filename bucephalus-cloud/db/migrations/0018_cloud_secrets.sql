CREATE TABLE cloud.secrets (
  secret_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_key text NOT NULL,
  name registry.nonempty_text NOT NULL,
  store_name registry.nonempty_text NOT NULL UNIQUE,
  backing_ref registry.nonempty_text NOT NULL,
  version integer NOT NULL DEFAULT 1,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (owner_key, name)
);

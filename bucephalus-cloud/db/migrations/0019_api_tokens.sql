CREATE TABLE cloud.api_tokens (
  token_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  token_hash text NOT NULL UNIQUE,
  token_prefix registry.nonempty_text NOT NULL,
  kind registry.nonempty_text NOT NULL,
  owner_key text NOT NULL,
  issuer text NOT NULL,
  subject text NOT NULL,
  label text,
  ttl_seconds integer,
  expires_at timestamptz,
  last_used_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT api_tokens_kind CHECK (kind IN ('session', 'api_key')),
  CONSTRAINT api_tokens_session_expires CHECK (kind <> 'session' OR (ttl_seconds IS NOT NULL AND expires_at IS NOT NULL))
);

CREATE INDEX api_tokens_owner_idx ON cloud.api_tokens(owner_key, created_at DESC);

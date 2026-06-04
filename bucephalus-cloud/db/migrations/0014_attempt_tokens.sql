ALTER TABLE cloud.run_attempts
  ADD COLUMN attempt_token_hash registry.sha256_digest;

CREATE INDEX run_attempts_token_hash_idx
  ON cloud.run_attempts(attempt_token_hash)
  WHERE status = 'running';

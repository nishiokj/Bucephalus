ALTER TABLE ingest.uploads
  ADD COLUMN owner_key text;

ALTER TABLE ingest.import_jobs
  ADD COLUMN owner_key text;

ALTER TABLE cloud.runs
  ADD COLUMN owner_key text;

CREATE TABLE cloud.package_artifact_owners (
  package_digest registry.sha256_digest NOT NULL
    REFERENCES cloud.package_artifacts(package_digest) ON DELETE CASCADE,
  owner_key text NOT NULL,
  upload_id uuid REFERENCES ingest.uploads(upload_id) ON DELETE SET NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (package_digest, owner_key)
);

CREATE INDEX uploads_owner_idx ON ingest.uploads(owner_key, created_at DESC);
CREATE INDEX import_jobs_owner_idx ON ingest.import_jobs(owner_key, created_at DESC);
CREATE INDEX cloud_runs_owner_idx ON cloud.runs(owner_key, created_at DESC);
CREATE INDEX package_artifact_owners_owner_idx ON cloud.package_artifact_owners(owner_key, created_at DESC);

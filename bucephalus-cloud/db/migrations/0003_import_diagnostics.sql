ALTER TABLE ingest.import_jobs
  ADD COLUMN diagnostics jsonb NOT NULL DEFAULT '[]'::jsonb,
  ADD CONSTRAINT import_jobs_diagnostics_is_array
    CHECK (jsonb_typeof(diagnostics) = 'array');

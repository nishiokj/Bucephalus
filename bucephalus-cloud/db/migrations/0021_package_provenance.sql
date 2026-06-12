ALTER TABLE cloud.package_artifacts
  ADD COLUMN package_provenance jsonb NOT NULL DEFAULT jsonb_build_object(
    'schema_version', 'cloud_package_provenance_v1',
    'status', 'unknown_legacy',
    'source', 'pre_provenance_package_artifact',
    'message', 'This package artifact was created before Cloud persisted package-level authoring provenance.'
  );

ALTER TABLE cloud.package_artifacts
  ADD CONSTRAINT package_artifacts_package_provenance_is_object
    CHECK (jsonb_typeof(package_provenance) = 'object');

ALTER TABLE cloud.package_artifact_owners
  ADD COLUMN package_provenance jsonb NOT NULL DEFAULT jsonb_build_object(
    'schema_version', 'cloud_package_provenance_v1',
    'status', 'unknown_legacy',
    'source', 'pre_provenance_package_owner',
    'message', 'This package ownership row was created before Cloud persisted owner-scoped package provenance.'
  );

ALTER TABLE cloud.package_artifact_owners
  ADD COLUMN storage_path text,
  ADD COLUMN byte_size bigint CHECK (byte_size IS NULL OR byte_size >= 0),
  ADD COLUMN media_type text,
  ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();

ALTER TABLE cloud.package_artifact_owners
  ADD CONSTRAINT package_artifact_owners_package_provenance_is_object
    CHECK (jsonb_typeof(package_provenance) = 'object');

ALTER TABLE cloud.runs
  ADD COLUMN package_provenance jsonb NOT NULL DEFAULT jsonb_build_object(
    'schema_version', 'cloud_package_provenance_v1',
    'status', 'unknown_legacy',
    'source', 'pre_provenance_run',
    'message', 'This run was created before Cloud persisted package provenance on runs.'
  );

ALTER TABLE cloud.runs
  ADD CONSTRAINT runs_package_provenance_is_object
    CHECK (jsonb_typeof(package_provenance) = 'object');

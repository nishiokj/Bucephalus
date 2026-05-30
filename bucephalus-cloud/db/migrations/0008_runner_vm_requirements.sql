ALTER TABLE cloud.package_artifacts
  ADD COLUMN image_refs jsonb NOT NULL DEFAULT '[]'::jsonb,
  ADD CONSTRAINT package_artifacts_image_refs_is_array
    CHECK (jsonb_typeof(image_refs) = 'array');

ALTER TABLE cloud.runs
  ADD COLUMN run_requirements jsonb NOT NULL DEFAULT '{}'::jsonb,
  ADD CONSTRAINT runs_run_requirements_is_object
    CHECK (jsonb_typeof(run_requirements) = 'object');

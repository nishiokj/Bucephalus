CREATE TABLE cloud.runner_worker_images (
  runner_worker_image_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  image_ref registry.nonempty_text NOT NULL,
  registry_host registry.nonempty_text NOT NULL,
  repository registry.nonempty_text NOT NULL,
  digest registry.nonempty_text NOT NULL,
  release_version registry.nonempty_text,
  release_git_sha text,
  promotion_evidence_uri text,
  promotion_evidence_sha256 text,
  modal_launcher_sha256 text,
  worker_runner_sha256 text,
  boundary_verified_at timestamptz,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT runner_worker_images_image_ref_digest
    CHECK (image_ref ~ '^.+@sha256:[a-f0-9]{64}$'),
  CONSTRAINT runner_worker_images_digest_format
    CHECK (digest ~ '^sha256:[a-f0-9]{64}$'),
  CONSTRAINT runner_worker_images_release_git_sha_format
    CHECK (release_git_sha IS NULL OR release_git_sha ~ '^[a-f0-9]{40}$'),
  CONSTRAINT runner_worker_images_evidence_sha_format
    CHECK (promotion_evidence_sha256 IS NULL OR promotion_evidence_sha256 ~ '^sha256:[a-f0-9]{64}$'),
  CONSTRAINT runner_worker_images_modal_launcher_sha_format
    CHECK (modal_launcher_sha256 IS NULL OR modal_launcher_sha256 ~ '^[a-f0-9]{64}$'),
  CONSTRAINT runner_worker_images_worker_runner_sha_format
    CHECK (worker_runner_sha256 IS NULL OR worker_runner_sha256 ~ '^[a-f0-9]{64}$'),
  CONSTRAINT runner_worker_images_metadata_is_object
    CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE UNIQUE INDEX runner_worker_images_image_ref_idx
  ON cloud.runner_worker_images(image_ref);

CREATE INDEX runner_worker_images_digest_idx
  ON cloud.runner_worker_images(digest);

ALTER TABLE cloud.runner_pools
  ADD COLUMN active_worker_image_id uuid REFERENCES cloud.runner_worker_images(runner_worker_image_id);

CREATE INDEX runner_pools_active_worker_image_idx
  ON cloud.runner_pools(active_worker_image_id);

import { describe, expect, test } from "bun:test";
import { runRequirementsForArtifact } from "../src/routes/runs";
import type { PackageArtifactRecord } from "../src/packages/repository";

describe("Cloud run requirements", () => {
  test("materializes explicit VM shape from runtime options", () => {
    const requirements = runRequirementsForArtifact(artifact(), {
      arch: "arm64",
      cpu_count: 4,
      memory_mb: 8192,
      disk_mb: 65536,
      isolation: "single_use_vm",
      timeout_ms: 60000,
      max_parallel_trials: 2,
    });

    expect(requirements).toMatchObject({
      executor: "runner-docker",
      requires: ["core_runner", "docker_daemon", "registry_pull"],
      arch: "arm64",
      cpu_count: 4,
      memory_mb: 8192,
      disk_mb: 65536,
      isolation: "single_use_vm",
      timeout_ms: 60000,
      max_parallel_trials: 2,
    });
  });

  test("defaults to a small reusable x86 runner shape", () => {
    const requirements = runRequirementsForArtifact(artifact(), {});

    expect(requirements).toMatchObject({
      arch: "x86_64",
      cpu_count: 1,
      memory_mb: 1024,
      disk_mb: 20480,
      isolation: "reusable_vm",
      timeout_ms: null,
      max_parallel_trials: 1,
    });
  });

  test("rejects unsupported architecture", () => {
    expect(() => runRequirementsForArtifact(artifact(), { arch: "sparc" }))
      .toThrow("Unsupported Cloud runner architecture");
  });
});

function artifact(overrides: Partial<PackageArtifactRecord> = {}): PackageArtifactRecord {
  return {
    package_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    upload_id: null,
    storage_path: null,
    byte_size: null,
    media_type: null,
    manifest_json: {},
    resolved_experiment_json: {
      runtime: {
        compute: { backend: "local-docker" },
      },
    },
    target: null,
    image_refs: ["ghcr.io/acme/task@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
    diagnostics: [],
    status: "accepted",
    created_at: "2026-05-29T00:00:00Z",
    updated_at: "2026-05-29T00:00:00Z",
    ...overrides,
  };
}

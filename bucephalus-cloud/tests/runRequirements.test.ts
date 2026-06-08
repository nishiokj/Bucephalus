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
      secret_ids: [],
      network_perimeter: {
        default: "none",
        task_sandbox: "none",
        agent: "none",
        egress_hosts: [],
      },
      sidecars: [],
      accelerators: [],
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

  test("declares secret resolver and network perimeter requirements", () => {
    const requirements = runRequirementsForArtifact(
      artifact({
        resolved_experiment_json: {
          runtime: {
            compute: { backend: "local-docker" },
            network: {
              default: "allowlist_enforced",
              task_sandbox: "allowlist_enforced",
              agent: "allowlist_enforced",
              egress: ["api.openai.com", "storage.googleapis.com"],
            },
          },
        },
      }),
      {
        sidecars: ["redis"],
        accelerators: ["nvidia-l4"],
      },
      {
        OPENAI_API_KEY: "gcp-secret-manager://projects/dev/secrets/openai/versions/latest",
      },
    );

    expect(requirements).toMatchObject({
      requires: [
        "core_runner",
        "docker_daemon",
        "registry_pull",
        "secret_resolver",
        "network_perimeter",
        "sidecar:redis",
        "accelerator:nvidia-l4",
      ],
      secret_ids: ["OPENAI_API_KEY"],
      network_perimeter: {
        default: "allowlist_enforced",
        task_sandbox: "allowlist_enforced",
        agent: "allowlist_enforced",
        egress_hosts: ["api.openai.com", "storage.googleapis.com"],
      },
      sidecars: ["redis"],
      accelerators: ["nvidia-l4"],
    });
  });

  test("merges partial network runtime options with package egress declarations", () => {
    const requirements = runRequirementsForArtifact(
      artifact({
        resolved_experiment_json: {
          runtime: {
            compute: { backend: "local-docker" },
            network: {
              default: "allowlist_enforced",
              task_sandbox: "allowlist_enforced",
              agent: "allowlist_enforced",
              egress: ["auth.openai.com", "chatgpt.com"],
            },
          },
        },
      }),
      {
        network: {
          default: "allowlist_enforced",
        },
      },
    );

    expect(requirements.network_perimeter).toEqual({
      default: "allowlist_enforced",
      task_sandbox: "allowlist_enforced",
      agent: "allowlist_enforced",
      egress_hosts: ["auth.openai.com", "chatgpt.com"],
    });
  });

  test("rejects invalid secret declarations before queueing work", () => {
    expect(() => runRequirementsForArtifact(artifact(), {}, {
      "OPENAI API KEY": "gcp-secret-manager://projects/dev/secrets/openai/versions/latest",
    })).toThrow("Invalid Cloud secret id");

    expect(() => runRequirementsForArtifact(artifact(), {}, {
      OPENAI_API_KEY: "",
    })).toThrow("Invalid Cloud secret ref");

    expect(() => runRequirementsForArtifact(artifact(), {}, {
      OPENAI_API_KEY: "gcp-secret-manager://projects/dev/secrets/openai/versions/latest\n",
    })).toThrow("Invalid Cloud secret ref");

    expect(() => runRequirementsForArtifact(artifact(), {}, {
      OPENAI_API_KEY: "raw-openai-key",
    })).toThrow("Unsupported Cloud secret ref");
  });

  test("rejects Cloud control-plane secret refs before queueing work", () => {
    expect(() => runRequirementsForArtifact(artifact(), {}, {
      LEAK: "gcp-secret-manager://projects/dev/secrets/buc-prod-worker-token/versions/latest",
    })).toThrow("reserved Cloud control-plane secret name");

    expect(() => runRequirementsForArtifact(artifact(), {}, {
      DATABASE_URL: "gcp-secret-manager://projects/dev/secrets/app-db-url/versions/1",
    })).toThrow("reserved for Cloud control-plane credentials");
  });

  test("rejects host agent execution for Cloud runs", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        trial_runtime: {
          execution: {
            agent_site: "host",
          },
        },
      },
    }), {})).toThrow("agent_site=host");
  });

  test("rejects mutable image refs for Cloud runs", () => {
    expect(() => runRequirementsForArtifact(artifact({
      image_refs: ["ghcr.io/acme/task:latest"],
    }), {})).toThrow("digest-pinned remote registry refs");
  });

  test("rejects unsupported architecture", () => {
    expect(() => runRequirementsForArtifact(artifact(), { arch: "sparc" }))
      .toThrow("Unsupported Cloud runner architecture");
  });

  test("rejects ambient Cloud network modes", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            agent: "full",
            egress: ["api.openai.com"],
          },
        },
      },
    }), {})).toThrow("is not supported for Cloud runs");
  });

  test("rejects allowlisted Cloud network modes without egress hosts", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            agent: "allowlist_enforced",
          },
        },
      },
    }), {})).toThrow("must declare at least one hostname");
  });

  test("rejects local or wildcard egress declarations", () => {
    expect(() => runRequirementsForArtifact(artifact(), {
      network: {
        egress: ["localhost", "*.example.com"],
      },
    })).toThrow("Unsupported Cloud egress host");
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

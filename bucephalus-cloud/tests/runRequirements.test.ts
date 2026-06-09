import { describe, expect, test } from "bun:test";
import { runRequirementsForArtifact } from "../src/routes/runs";
import type { PackageArtifactRecord } from "../src/packages/repository";
import type { JsonObject, JsonValue } from "../src/primitives";

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

  test("materializes package-authored policy resources, timeout, and scheduling caps", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: {
            backend: "local-docker",
            config: { max_parallel: 2 },
          },
          network: {
            task_sandbox: "none",
            agent: "none",
          },
        },
        scheduling: {
          max_concurrency: 3,
        },
        policy: {
          timeout_ms: 450000,
          task_sandbox: {
            resources: {
              cpu_count: 8,
              memory_mb: 32768,
            },
          },
        },
      },
    }), {});

    expect(requirements).toMatchObject({
      cpu_count: 8,
      memory_mb: 32768,
      disk_mb: 20480,
      timeout_ms: 450000,
      max_parallel_trials: 3,
    });
  });

  test("runtime options override package-authored Cloud resource requirements", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: {
            backend: "local-docker",
            disk_mb: 65536,
            config: { max_parallel: 2 },
          },
        },
        scheduling: {
          max_concurrency: 3,
        },
        policy: {
          timeout_ms: 450000,
          task_sandbox: {
            resources: {
              cpu_count: 8,
              memory_mb: 32768,
            },
          },
        },
      },
    }), {
      cpu_count: "4",
      memory_mb: 16384,
      disk_mb: 40960,
      timeout_ms: 120000,
      max_parallel_trials: 1,
    });

    expect(requirements).toMatchObject({
      cpu_count: 4,
      memory_mb: 16384,
      disk_mb: 40960,
      timeout_ms: 120000,
      max_parallel_trials: 1,
    });
  });

  test("rejects invalid explicit Cloud resource numbers instead of defaulting", () => {
    for (const [field, value, pointer] of [
      ["cpu_count", 0, "/runtime_options/cpu_count"],
      ["memory_mb", "0", "/runtime_options/memory_mb"],
      ["disk_mb", -1, "/runtime_options/disk_mb"],
      ["timeout_ms", "soon", "/runtime_options/timeout_ms"],
      ["max_parallel_trials", 0, "/runtime_options/max_parallel_trials"],
      ["cpu_count", 2147483648, "/runtime_options/cpu_count"],
      ["memory_mb", "999999999999999999999999", "/runtime_options/memory_mb"],
    ] as const) {
      expect(() => runRequirementsForArtifact(artifact(), { [field]: value }))
        .toThrow(`${pointer} must be a positive integer`);
    }
  });

  test("rejects invalid package-authored Cloud resource numbers instead of defaulting", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: { compute: { backend: "local-docker" } },
        policy: {
          timeout_ms: 600000,
          task_sandbox: {
            resources: {
              cpu_count: 0,
            },
          },
        },
      },
    }), {})).toThrow("/policy/task_sandbox/resources/cpu_count must be a positive integer");

    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: {
            backend: "local-docker",
            config: {
              max_parallel: 0,
            },
          },
        },
        policy: {
          timeout_ms: 600000,
          task_sandbox: {},
        },
      },
    }), {})).toThrow("/runtime/compute/config/max_parallel must be a positive integer");
  });

  test("runtime options cannot mask invalid package-authored Cloud resource values", () => {
    const cases: Array<[JsonObject, JsonObject, string]> = [
      [
        { runtime: { compute: { backend: "local-docker", arch: "sparc" } } },
        { arch: "arm64" },
        "Unsupported Cloud runner architecture 'sparc'",
      ],
      [
        { runtime: { compute: { backend: "local-docker", memory_mb: 0 } } },
        { memory_mb: 4096 },
        "/runtime/compute/memory_mb must be a positive integer",
      ],
      [
        { runtime: { compute: { backend: "local-docker", cpu_count: 2147483648 } } },
        { cpu_count: 4 },
        "/runtime/compute/cpu_count must be a positive integer",
      ],
      [
        { runtime: { compute: { backend: "local-docker", disk_mb: -1 } } },
        { disk_mb: 65536 },
        "/runtime/compute/disk_mb must be a positive integer",
      ],
      [
        { runtime: { compute: { backend: "local-docker", isolation: "shared_host" } } },
        { isolation: "single_use_vm" },
        "Unsupported Cloud isolation mode 'shared_host'",
      ],
      [
        { policy: { timeout_ms: 0 } },
        { timeout_ms: 60000 },
        "/policy/timeout_ms must be a positive integer",
      ],
      [
        { scheduling: { max_concurrency: 0 } },
        { max_parallel_trials: 2 },
        "/scheduling/max_concurrency must be a positive integer",
      ],
      [
        { runtime: { compute: { backend: "local-docker", config: { max_parallel: 0 } } } },
        { max_parallel_trials: 2 },
        "/runtime/compute/config/max_parallel must be a positive integer",
      ],
    ];

    for (const [resolvedExperiment, runtimeOptions, message] of cases) {
      expect(() => runRequirementsForArtifact(artifact({
        resolved_experiment_json: resolvedExperiment,
      }), runtimeOptions)).toThrow(message);
    }
  });

  test("rejects malformed package-authored Cloud runtime objects instead of defaulting", () => {
    const cases: Array<[JsonObject, string]> = [
      [{ runtime: "local-docker" }, "/runtime must be an object"],
      [{ runtime: { compute: "modal" } }, "/runtime/compute must be an object"],
      [{ runtime: { compute: { backend: 7 } } }, "/runtime/compute/backend must be a string"],
      [{ runtime: { compute: { backend: "local-docker", arch: ["arm64"] } } }, "/runtime/compute/arch must be a string"],
      [{ runtime: { compute: { backend: "local-docker", isolation: ["single_use_vm"] } } }, "/runtime/compute/isolation must be a string"],
      [{ runtime: { compute: { backend: "local-docker", config: "serial" } } }, "/runtime/compute/config must be an object"],
      [{ runtime: { compute: { backend: "local-docker" }, network: "none" } }, "/runtime/network must be an object"],
      [{ policy: { task_sandbox: "locked-down" } }, "/policy/task_sandbox must be an object"],
      [{ policy: { task_sandbox: { resources: "large" } } }, "/policy/task_sandbox/resources must be an object"],
      [{ scheduling: "serial" }, "/scheduling must be an object"],
    ];
    for (const [resolvedExperiment, message] of cases) {
      expect(() => runRequirementsForArtifact(artifact({
        resolved_experiment_json: resolvedExperiment,
      }), {})).toThrow(message);
    }
  });

  test("rejects malformed runtime option network objects instead of defaulting", () => {
    for (const network of ["none", null, ["api.openai.com"]]) {
      expect(() => runRequirementsForArtifact(artifact(), {
        network,
      })).toThrow("/runtime_options/network must be an object");
    }
  });

  test("maps package-authored modal backend to modal runner resources", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
        },
      },
    }), {});

    expect(requirements).toMatchObject({
      executor: "modal",
      requires: ["core_runner", "modal", "registry_pull"],
    });
  });

  test("rejects modal Cloud runs that require network perimeter enforcement", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
          network: {
            default: "allowlist_enforced",
            egress: ["api.openai.com"],
          },
        },
      },
    }), {})).toThrow("modal Cloud runs do not support network perimeter");

    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
          network: {
            egress: ["api.openai.com"],
          },
        },
      },
    }), {})).toThrow("modal Cloud runs do not support network perimeter");
  });

  test("rejects modal runtime overrides that require network perimeter enforcement", () => {
    expect(() => runRequirementsForArtifact(artifact(), {
      backend: "modal",
      network: {
        default: "allowlist_enforced",
        egress: ["api.openai.com"],
      },
    })).toThrow("modal Cloud runs do not support network perimeter");

    expect(() => runRequirementsForArtifact(artifact(), {
      executor: "modal",
      network: {
        egress: ["api.openai.com"],
      },
    })).toThrow("modal Cloud runs do not support network perimeter");
  });

  test("runtime backend override cannot mask package-authored modal network perimeter", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
          network: {
            egress: ["api.openai.com"],
          },
        },
      },
    }), {
      backend: "runner-docker",
    })).toThrow("modal Cloud runs do not support network perimeter");
  });

  test("runtime backend override wins over package-authored local backend", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
        },
      },
    }), {
      backend: "runner-docker",
    });

    expect(requirements).toMatchObject({
      executor: "runner-docker",
      requires: ["core_runner", "docker_daemon", "registry_pull"],
    });
  });

  test("runtime backend override cannot mask unsupported package-authored backend", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "kubernetes" },
        },
      },
    }), {
      backend: "runner-docker",
    })).toThrow("Unsupported Cloud run backend 'kubernetes'");
  });

  test("normalizes Cloud and Core executor aliases at queue time", () => {
    for (const executor of ["runner-docker", "runner_docker", "local-docker", "local_docker"]) {
      expect(runRequirementsForArtifact(artifact(), { executor }).executor).toBe("runner-docker");
    }
    expect(runRequirementsForArtifact(artifact(), { executor: "modal" }).executor).toBe("modal");
  });

  test("rejects conflicting runtime backend and executor overrides at queue time", () => {
    expect(() => runRequirementsForArtifact(artifact(), {
      backend: "modal",
      executor: "runner-docker",
    })).toThrow("runtime_options.backend and runtime_options.executor");

    expect(runRequirementsForArtifact(artifact(), {
      backend: "local-docker",
      executor: "runner_docker",
    }).executor).toBe("runner-docker");
  });

  test("validates Core launch runtime options before queueing Cloud work", () => {
    for (const materialize of ["none", "metadata-only", "metadata_only", "outputs-only", "outputs_only", "full"]) {
      expect(runRequirementsForArtifact(artifact(), { materialize }).executor).toBe("runner-docker");
    }

    expect(() => runRequirementsForArtifact(artifact(), {
      materialize: "metadata",
    })).toThrow("Unsupported Core materialize mode");

    expect(() => runRequirementsForArtifact(artifact(), {
      materialize: false,
    })).toThrow("/runtime_options/materialize must be a string");

    expect(() => runRequirementsForArtifact(artifact(), {
      smoke_test: "true",
    })).toThrow("/runtime_options/smoke_test must be a boolean");
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

  test("declares package-authored sidecars as Cloud runner requirements", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        sidecars: {
          cache: {
            image: "ghcr.io/acme/cache@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
          telemetry: {
            image: "ghcr.io/acme/telemetry@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
          unused: {
            image: "ghcr.io/acme/unused@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            lifecycle: "per-trial",
          },
        },
        trial_runtime: {
          agent: {
            sidecars: ["cache", "telemetry"],
          },
          grader: {
            strategy: "none",
          },
        },
      },
    }), {
      sidecars: ["cache", "debug-proxy"],
    });

    expect(requirements.sidecars).toEqual(["cache", "debug-proxy", "telemetry"]);
    expect(requirements.requires).toEqual([
      "core_runner",
      "docker_daemon",
      "registry_pull",
      "sidecar:cache",
      "sidecar:debug-proxy",
      "sidecar:telemetry",
    ]);
  });

  test("declares modern package-authored ephemerals and stages as Cloud runner requirements", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        ephemerals: {
          "mcp-bash": {
            image: "ghcr.io/acme/mcp-bash@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
        },
        stages: {
          agent: {
            ephemerals: ["mcp-bash"],
          },
        },
      },
    }), {});

    expect(requirements.sidecars).toEqual(["mcp-bash"]);
    expect(requirements.requires).toEqual([
      "core_runner",
      "docker_daemon",
      "registry_pull",
      "sidecar:mcp-bash",
    ]);
    expect(requirements.image_refs).toEqual([
      "ghcr.io/acme/mcp-bash@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "ghcr.io/acme/task@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    ]);
  });

  test("declares top-level package-authored trial runtime aliases as Cloud requirements", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        ephemerals: {
          "mcp-bash": {
            image: "ghcr.io/acme/mcp-bash@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
        },
        task: {
          workspace: {
            image: "ghcr.io/acme/task-workspace@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          },
        },
        agent: {
          image: "ghcr.io/acme/agent@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
          ephemerals: ["mcp-bash"],
        },
        grader: {
          separate: {
            image: "ghcr.io/acme/grader@sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
          },
        },
      },
    }), {});

    expect(requirements.sidecars).toEqual(["mcp-bash"]);
    expect(requirements.requires).toContain("sidecar:mcp-bash");
    expect(requirements.image_refs).toEqual(expect.arrayContaining([
      "ghcr.io/acme/agent@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
      "ghcr.io/acme/grader@sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
      "ghcr.io/acme/mcp-bash@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "ghcr.io/acme/task-workspace@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
    ]));
  });

  test("rejects modal Cloud runs that require sidecars", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
        },
        sidecars: {
          cache: {
            image: "ghcr.io/acme/cache@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
        },
        trial_runtime: {
          agent: {
            sidecars: ["cache"],
          },
        },
      },
    }), {})).toThrow("modal Cloud runs do not support sidecars");

    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
        },
      },
    }), {
      sidecars: ["cache"],
    })).toThrow("modal Cloud runs do not support sidecars");

    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
        },
        ephemerals: {
          cache: {
            image: "ghcr.io/acme/cache@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
        },
        stages: {
          agent: {
            ephemerals: ["cache"],
          },
        },
      },
    }), {})).toThrow("modal Cloud runs do not support sidecars");
  });

  test("rejects package-authored sidecar references without declarations", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        trial_runtime: {
          agent: {
            sidecars: ["cache"],
          },
        },
      },
    }), {})).toThrow("sidecar 'cache' is referenced but not declared");

    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        stages: {
          agent: {
            ephemerals: ["cache"],
          },
        },
      },
    }), {})).toThrow("sidecar 'cache' is referenced but not declared");
  });

  test("rejects runtime option trial-runtime sidecars instead of silently ignoring local-style authoring", () => {
    for (const trial_runtime of [
      {
        agent: {
          sidecars: ["cache"],
        },
      },
      {
        grader: {
          sidecars: ["cache"],
        },
      },
      {
        agent: {
          ephemerals: ["cache"],
        },
      },
      {
        grader: {
          ephemerals: ["cache"],
        },
      },
    ]) {
      expect(() => runRequirementsForArtifact(artifact(), {
        trial_runtime,
      })).toThrow("declare sidecars in the package YAML or use runtime_options.sidecars");
    }
  });

  test("rejects malformed package-authored sidecar declarations before queueing work", () => {
    for (const [sidecars, message] of [
      ["cache", "/sidecars must be an object"],
      [{ cache: "redis:7" }, "/sidecars/cache must be an object"],
      [{ "Bad_ID": { image: "redis:7", lifecycle: "per-trial" } }, "/sidecars/Bad_ID id must be a portable runtime alias"],
      [{ cache: { lifecycle: "per-trial" } }, "/sidecars/cache image is required"],
      [{ cache: { image: "redis:7", lifecycle: "persistent" } }, "/sidecars/cache lifecycle must be per-trial"],
    ] as const) {
      expect(() => runRequirementsForArtifact(artifact({
        resolved_experiment_json: {
          runtime: {
            compute: { backend: "local-docker" },
          },
          sidecars,
        },
      }), {})).toThrow(message);
    }
  });

  test("rejects conflicting package sidecar and ephemeral alias authoring before queueing", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        sidecars: {
          cache: {
            image: "ghcr.io/acme/cache@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
        },
        ephemerals: {
          cache: {
            image: "ghcr.io/acme/cache@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            lifecycle: "per-trial",
          },
        },
        stages: {
          agent: {
            ephemerals: ["cache"],
          },
        },
      },
    }), {})).toThrow("/ephemerals/cache conflicts with another Cloud runtime alias");

    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        sidecars: {
          cache: {
            image: "ghcr.io/acme/cache@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            lifecycle: "per-trial",
          },
        },
        stages: {
          agent: {
            sidecars: ["cache"],
            ephemerals: ["cache-alt"],
          },
        },
      },
    }), {})).toThrow("/stages/agent/ephemerals conflicts with /stages/agent/sidecars");
  });

  test("declares package-authored accelerators as Cloud runner requirements", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: {
            backend: "local-docker",
            accelerators: ["nvidia-l4", "tpu-v5e"],
          },
        },
        policy: {
          task_sandbox: {
            resources: {
              accelerators: ["amd-mi300", "nvidia-l4"],
            },
          },
        },
      },
    }), {
      accelerators: ["tpu-v5e", "nvidia-a100"],
    });

    expect(requirements.accelerators).toEqual(["amd-mi300", "nvidia-a100", "nvidia-l4", "tpu-v5e"]);
    expect(requirements.requires).toEqual([
      "core_runner",
      "docker_daemon",
      "registry_pull",
      "accelerator:amd-mi300",
      "accelerator:nvidia-a100",
      "accelerator:nvidia-l4",
      "accelerator:tpu-v5e",
    ]);
  });

  test("rejects malformed package-authored accelerator collections before queueing work", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: {
            backend: "local-docker",
            accelerators: "nvidia-l4",
          },
        },
      },
    }), {})).toThrow("/runtime/compute/accelerators must be an array of strings");

    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        policy: {
          task_sandbox: {
            resources: {
              accelerators: ["NVIDIA-L4"],
            },
          },
        },
      },
    }), {})).toThrow("/policy/task_sandbox/resources/accelerators entries must be portable Cloud requirement aliases");
  });

  test("rejects modal Cloud runs that require accelerators", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: {
            backend: "modal",
            accelerators: ["nvidia-l4"],
          },
        },
      },
    }), {})).toThrow("modal Cloud runs do not support accelerator requirements");

    expect(() => runRequirementsForArtifact(artifact(), {
      executor: "modal",
      accelerators: ["nvidia-l4"],
    })).toThrow("modal Cloud runs do not support accelerator requirements");
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

  test("rejects malformed provider secret refs before queueing work", () => {
    for (const ref of [
      "gcp-secret-manager://projects/dev/secrets/openai",
      "gcp-secret-manager://projects/dev/secrets/openai/versions/candidate",
      "gcp-secret-manager://projects/dev/secrets/open ai/versions/latest",
      "aws-secrets-manager://",
      "aws-secrets-manager://prod/openai api key",
    ]) {
      expect(() => runRequirementsForArtifact(artifact(), {}, {
        OPENAI_API_KEY: ref,
      })).toThrow("Unsupported Cloud secret ref");
    }
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

  test("runtime options cannot mask package-authored host agent execution", () => {
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
    }), {
      trial_runtime: {
        execution: {
          agent_site: "agent_container",
        },
      },
    })).toThrow("agent_site=host");
  });

  test("rejects host agent execution supplied through top-level package alias", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        execution: {
          agent_site: "host",
        },
      },
    }), {})).toThrow("agent_site=host");
  });

  test("rejects host agent execution supplied through runtime options", () => {
    expect(() => runRequirementsForArtifact(artifact(), {
      trial_runtime: {
        execution: {
          agent_site: "host",
        },
      },
    })).toThrow("agent_site=host");
  });

  test("rejects host grader execution for Cloud runs", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        trial_runtime: {
          grader: {
            strategy: "host",
            command: ["__BUCEPHALUS_HOST_GRADER_CAPABILITY__/grader/run.sh"],
            host: {
              capability: "grader-capability",
            },
          },
        },
      },
    }), {})).toThrow("grader.strategy=host");
  });

  test("runtime options cannot mask package-authored host grader execution", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        trial_runtime: {
          grader: {
            strategy: "host",
            command: ["__BUCEPHALUS_HOST_GRADER_CAPABILITY__/grader/run.sh"],
            host: {
              capability: "grader-capability",
            },
          },
        },
      },
    }), {
      trial_runtime: {
        grader: {
          strategy: "separate",
        },
      },
    })).toThrow("grader.strategy=host");
  });

  test("rejects host grader execution supplied through top-level package alias", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        grader: {
          strategy: "host",
          command: ["__BUCEPHALUS_HOST_GRADER_CAPABILITY__/grader/run.sh"],
          host: {
            capability: "grader-capability",
          },
        },
      },
    }), {})).toThrow("grader.strategy=host");
  });

  test("rejects host grader execution supplied through runtime options", () => {
    expect(() => runRequirementsForArtifact(artifact(), {
      trial_runtime: {
        grader: {
          strategy: "host",
        },
      },
    })).toThrow("grader.strategy=host");
  });

  test("rejects malformed package-authored trial runtime shapes instead of ignoring them", () => {
    for (const [trialRuntime, message] of [
      ["host", "/trial_runtime must be an object"],
      [{ execution: "host" }, "/trial_runtime/execution must be an object"],
      [{ execution: { agent_site: 1 } }, "/trial_runtime/execution/agent_site must be a string"],
      [{ grader: { strategy: 7 } }, "/trial_runtime/grader/strategy must be a string"],
      [{ agent: "container" }, "/trial_runtime/agent must be an object"],
      [{ grader: "none" }, "/trial_runtime/grader must be an object"],
    ] as const) {
      expect(() => runRequirementsForArtifact(artifact({
        resolved_experiment_json: {
          runtime: {
            compute: { backend: "local-docker" },
          },
          trial_runtime: trialRuntime,
        },
      }), {})).toThrow(message);
    }
  });

  test("rejects malformed package-authored runtime image shapes instead of ignoring them", () => {
    const cases: Array<[JsonObject, string]> = [
      [{ agent: { image: 7 } }, "/trial_runtime/agent/image must be a non-empty image ref string"],
      [{ agent: { image: "" } }, "/trial_runtime/agent/image must be a non-empty image ref string"],
      [{ grader: { separate: "local" } }, "/trial_runtime/grader/separate must be an object"],
      [{ grader: { separate: { image: null } } }, "/trial_runtime/grader/separate/image must be a non-empty image ref string"],
      [{ task: "workspace" }, "/trial_runtime/task must be an object"],
      [{ task: { workspace: ["image"] } }, "/trial_runtime/task/workspace must be an object"],
      [{ task: { workspace: { image: false } } }, "/trial_runtime/task/workspace/image must be a non-empty image ref string"],
    ];
    for (const [trialRuntime, message] of cases) {
      expect(() => runRequirementsForArtifact(artifact({
        resolved_experiment_json: {
          runtime: {
            compute: { backend: "local-docker" },
          },
          trial_runtime: trialRuntime,
        },
      }), {})).toThrow(message);
    }
  });

  test("rejects conflicting top-level and trial-runtime package aliases before queueing", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
        },
        trial_runtime: {
          agent: {
            image: "ghcr.io/acme/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
          },
        },
        agent: {
          image: "ghcr.io/acme/agent@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        },
      },
    }), {})).toThrow("/agent conflicts with another Cloud runtime alias");
  });

  test("rejects malformed runtime option trial runtime shapes instead of ignoring them", () => {
    for (const [trialRuntime, message] of [
      ["host", "/runtime_options/trial_runtime must be an object"],
      [{ execution: "host" }, "/runtime_options/trial_runtime/execution must be an object"],
      [{ execution: { agent_site: 1 } }, "/runtime_options/trial_runtime/execution/agent_site must be a string"],
      [{ grader: { strategy: 7 } }, "/runtime_options/trial_runtime/grader/strategy must be a string"],
      [{ agent: { image: 7 } }, "/runtime_options/trial_runtime/agent/image must be a non-empty image ref string"],
      [{ grader: { separate: "local" } }, "/runtime_options/trial_runtime/grader/separate must be an object"],
      [{ grader: { separate: { image: null } } }, "/runtime_options/trial_runtime/grader/separate/image must be a non-empty image ref string"],
      [{ task: { workspace: { image: false } } }, "/runtime_options/trial_runtime/task/workspace/image must be a non-empty image ref string"],
    ] as const) {
      expect(() => runRequirementsForArtifact(artifact(), {
        trial_runtime: trialRuntime,
      })).toThrow(message);
    }
  });

  test("rejects mutable image refs for Cloud runs", () => {
    expect(() => runRequirementsForArtifact(artifact({
      image_refs: ["ghcr.io/acme/task:latest"],
    }), {})).toThrow("digest-pinned remote registry refs");
  });

  test("rejects package-authored mutable runtime images even if import metadata missed them", () => {
    for (const resolved_experiment_json of [
      {
        runtime: {
          compute: { backend: "local-docker" },
        },
        trial_runtime: {
          agent: {
            image: "ghcr.io/acme/agent:latest",
          },
        },
      },
      {
        runtime: {
          compute: { backend: "local-docker" },
        },
        trial_runtime: {
          grader: {
            separate: {
              image: "ghcr.io/acme/grader:latest",
            },
          },
        },
      },
      {
        runtime: {
          compute: { backend: "local-docker" },
        },
        ephemerals: {
          cache: {
            image: "redis:7",
            lifecycle: "per-trial",
          },
        },
        stages: {
          agent: {
            ephemerals: ["cache"],
          },
        },
      },
      {
        runtime: {
          compute: { backend: "local-docker" },
        },
        sidecars: {
          cache: {
            image: "redis:7",
            lifecycle: "per-trial",
          },
        },
      },
    ]) {
      expect(() => runRequirementsForArtifact(artifact({
        image_refs: [],
        resolved_experiment_json,
      }), {})).toThrow("digest-pinned remote registry refs");
    }
  });

  test("rejects runtime option mutable images before Cloud queueing", () => {
    for (const trial_runtime of [
      {
        agent: {
          image: "ghcr.io/acme/agent:latest",
        },
      },
      {
        grader: {
          separate: {
            image: "ghcr.io/acme/grader:latest",
          },
        },
      },
      {
        task: {
          workspace: {
            image: "ghcr.io/acme/workspace:latest",
          },
        },
      },
    ]) {
      expect(() => runRequirementsForArtifact(artifact(), {
        trial_runtime,
      })).toThrow("digest-pinned remote registry refs");
    }
  });

  test("records digest-pinned runtime option images as Cloud runner requirements", () => {
    const requirements = runRequirementsForArtifact(artifact(), {
      trial_runtime: {
        agent: {
          image: "ghcr.io/acme/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        },
        grader: {
          separate: {
            image: "ghcr.io/acme/grader@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          },
        },
        task: {
          workspace: {
            image: "ghcr.io/acme/workspace@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
          },
        },
      },
    });

    expect(requirements.image_refs).toEqual([
      "ghcr.io/acme/agent@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "ghcr.io/acme/grader@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      "ghcr.io/acme/task@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "ghcr.io/acme/workspace@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    ]);
  });

  test("rejects unsupported architecture", () => {
    expect(() => runRequirementsForArtifact(artifact(), { arch: "sparc" }))
      .toThrow("Unsupported Cloud runner architecture");
  });

  test("rejects malformed explicit Cloud shape strings instead of defaulting", () => {
    expect(() => runRequirementsForArtifact(artifact(), {
      arch: ["arm64"],
    })).toThrow("/runtime_options/arch must be a string");

    expect(() => runRequirementsForArtifact(artifact(), {
      isolation: true,
    })).toThrow("/runtime_options/isolation must be a string");
  });

  test("rejects unsupported runtime requirement collection shapes", () => {
    expect(() => runRequirementsForArtifact(artifact(), {
      sidecars: "redis",
    })).toThrow("/runtime_options/sidecars must be an array of strings");

    expect(() => runRequirementsForArtifact(artifact(), {
      accelerators: ["nvidia-l4", ""],
    })).toThrow("/runtime_options/accelerators entries must be non-empty strings");
  });

  test("rejects malformed runtime-authored Cloud requirement aliases", () => {
    const cases: Array<[string, JsonValue, string]> = [
      ["sidecars", ["redis:7"], "/runtime_options/sidecars entries must be portable Cloud requirement aliases"],
      ["sidecars", ["Debug_Proxy"], "/runtime_options/sidecars entries must be portable Cloud requirement aliases"],
      ["accelerators", ["NVIDIA-L4"], "/runtime_options/accelerators entries must be portable Cloud requirement aliases"],
      ["accelerators", ["gpu.large"], "/runtime_options/accelerators entries must be portable Cloud requirement aliases"],
    ];
    for (const [field, value, message] of cases) {
      expect(() => runRequirementsForArtifact(artifact(), {
        [field]: value,
      })).toThrow(message);
    }
  });

  test("deduplicates and sorts Cloud runtime requirement collections", () => {
    const requirements = runRequirementsForArtifact(artifact(), {
      sidecars: ["redis", "postgres", "redis"],
      accelerators: ["nvidia-l4", "nvidia-l4", "tpu-v5e"],
    });

    expect(requirements.sidecars).toEqual(["postgres", "redis"]);
    expect(requirements.accelerators).toEqual(["nvidia-l4", "tpu-v5e"]);
    expect(requirements.requires).toEqual([
      "core_runner",
      "docker_daemon",
      "registry_pull",
      "sidecar:postgres",
      "sidecar:redis",
      "accelerator:nvidia-l4",
      "accelerator:tpu-v5e",
    ]);
  });

  test("declares network perimeter for egress-only package-authored Cloud runs", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            egress: ["API.OpenAI.com", "api.openai.com:443", "storage.googleapis.com"],
          },
        },
      },
    }), {});

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: ["api.openai.com", "api.openai.com:443", "storage.googleapis.com"],
    });
  });

  test("declares network perimeter for package-authored external API declarations", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          externals: {
            apis: ["API.OpenAI.com", "storage.googleapis.com"],
          },
        },
      },
    }), {});

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: ["api.openai.com", "storage.googleapis.com"],
    });
  });

  test("declares network perimeter for top-level external API aliases", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            egress: ["storage.googleapis.com"],
          },
        },
        externals: {
          apis: ["api.openai.com", "storage.googleapis.com"],
        },
      },
    }), {});

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: ["api.openai.com", "storage.googleapis.com"],
    });
  });

  test("declares network perimeter for egress-only runtime option Cloud runs", () => {
    const requirements = runRequirementsForArtifact(artifact(), {
      network: {
        default: "none",
        egress: ["storage.googleapis.com", "API.OpenAI.com"],
      },
    });

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: ["api.openai.com", "storage.googleapis.com"],
    });
  });

  test("runtime network options cannot erase package-authored external APIs", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          externals: {
            apis: ["storage.googleapis.com"],
          },
        },
      },
    }), {
      network: {
        default: "none",
        egress: ["api.openai.com"],
      },
    });

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: ["api.openai.com", "storage.googleapis.com"],
    });
  });

  test("runtime network options cannot erase package-authored egress hosts", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            default: "none",
            egress: ["storage.googleapis.com"],
          },
        },
      },
    }), {
      network: {
        default: "none",
        egress: ["api.openai.com"],
      },
    });

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: ["api.openai.com", "storage.googleapis.com"],
    });
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

  test("rejects modal Cloud runs that declare external API egress", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "modal" },
          externals: {
            apis: ["api.openai.com"],
          },
        },
      },
    }), {})).toThrow("modal Cloud runs do not support network perimeter");
  });

  test("rejects malformed package external API declarations instead of ignoring them", () => {
    const cases: Array<[JsonObject, string]> = [
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            externals: "api.openai.com",
          },
        },
        "/runtime/externals must be an object",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            externals: { apis: "api.openai.com" },
          },
        },
        "/runtime/externals/apis must be an array of hostnames",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
          },
          externals: { apis: ["api.openai.com", ""] },
        },
        "/externals/apis entries must be non-empty hostnames",
      ],
      [
        {
          runtime: {
            compute: { backend: "local-docker" },
            externals: { apis: ["api.openai.com"] },
          },
          externals: { apis: ["storage.googleapis.com"] },
        },
        "/externals conflicts with another Cloud runtime alias",
      ],
    ];
    for (const [resolvedExperiment, message] of cases) {
      expect(() => runRequirementsForArtifact(artifact({
        resolved_experiment_json: resolvedExperiment,
      }), {})).toThrow(message);
    }
  });

  test("runtime network options cannot mask package-authored ambient network modes", () => {
    expect(() => runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            task_sandbox: "full",
          },
        },
      },
    }), {
      network: {
        task_sandbox: "none",
      },
    })).toThrow("is not supported for Cloud runs");
  });

  test("runtime network options cannot downscope package-authored allowlist requirements", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            default: "allowlist_enforced",
            egress: ["API.OpenAI.com", "api.openai.com"],
          },
        },
      },
    }), {
      network: {
        default: "none",
      },
    });

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "allowlist_enforced",
      task_sandbox: "allowlist_enforced",
      agent: "allowlist_enforced",
      egress_hosts: ["api.openai.com"],
    });
  });

  test("runtime network options can add stricter Cloud perimeter to package-authored none modes", () => {
    const requirements = runRequirementsForArtifact(artifact({
      resolved_experiment_json: {
        runtime: {
          compute: { backend: "local-docker" },
          network: {
            task_sandbox: "none",
            egress: ["storage.googleapis.com"],
          },
        },
      },
    }), {
      network: {
        default: "allowlist_enforced",
        egress: ["api.openai.com", "storage.googleapis.com"],
      },
    });

    expect(requirements.requires).toContain("network_perimeter");
    expect(requirements.network_perimeter).toEqual({
      default: "allowlist_enforced",
      task_sandbox: "allowlist_enforced",
      agent: "allowlist_enforced",
      egress_hosts: ["api.openai.com", "storage.googleapis.com"],
    });
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

  test("rejects IP literal egress declarations instead of treating them as Cloud hostnames", () => {
    for (const host of ["10.0.0.1", "192.168.1.10:443", "169.254.169.254", "8.8.8.8"]) {
      expect(() => runRequirementsForArtifact(artifact(), {
        network: {
          default: "allowlist_enforced",
          egress: [host],
        },
      })).toThrow(`Unsupported Cloud egress host '${host}'`);
    }
  });

  test("rejects malformed Cloud egress hostnames and ports", () => {
    for (const host of [
      ".example.com",
      "api..openai.com",
      "bad-.example.com",
      "api.openai.com:0",
      "api.openai.com:70000",
    ]) {
      expect(() => runRequirementsForArtifact(artifact(), {
        network: {
          default: "allowlist_enforced",
          egress: [host],
        },
      })).toThrow(`Unsupported Cloud egress host '${host}'`);
    }
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

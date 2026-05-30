import { describe, expect, test } from "bun:test";
import { matchesCapabilities } from "../src/poolController";

describe("pool controller matching", () => {
  test("matches executor and every required resource", () => {
    expect(matchesCapabilities(
      { executors: ["runner-docker"], resources: ["core_runner", "docker_daemon", "registry_pull"] },
      { executor: "runner-docker", requires: ["core_runner", "registry_pull"] },
    )).toBe(true);
  });

  test("rejects incompatible executor", () => {
    expect(matchesCapabilities(
      { executors: ["modal"], resources: ["core_runner", "modal"] },
      { executor: "runner-docker", requires: ["core_runner"] },
    )).toBe(false);
  });

  test("rejects missing resources", () => {
    expect(matchesCapabilities(
      { executors: ["runner-docker"], resources: ["core_runner"] },
      { executor: "runner-docker", requires: ["core_runner", "docker_daemon"] },
    )).toBe(false);
  });

  test("matches explicit VM shape requirements", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
        arch: "arm64",
        cpu_count: 4,
        memory_mb: 8192,
        disk_mb: 65536,
        isolation: ["reusable_vm"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        arch: "arm64",
        cpu_count: 2,
        memory_mb: 4096,
        disk_mb: 32768,
        isolation: "reusable_vm",
      },
    )).toBe(true);
  });

  test("rejects insufficient VM shape", () => {
    expect(matchesCapabilities(
      {
        executors: ["runner-docker"],
        resources: ["core_runner", "docker_daemon", "registry_pull"],
        arch: "x86_64",
        cpu_count: 2,
        memory_mb: 2048,
        disk_mb: 32768,
        isolation: ["reusable_vm"],
      },
      {
        executor: "runner-docker",
        requires: ["core_runner"],
        arch: "arm64",
        cpu_count: 4,
        memory_mb: 8192,
        disk_mb: 65536,
        isolation: "single_use_vm",
      },
    )).toBe(false);
  });
});

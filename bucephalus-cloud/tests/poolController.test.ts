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
});

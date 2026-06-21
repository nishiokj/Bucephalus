import { describe, expect, test } from "bun:test";
import { RunnerRepository } from "../src/runners/repository";
import type { JsonObject } from "../src/primitives";

describe("runner repository lifecycle", () => {
  test("heartbeats restore offline cordoned and draining runners without making them claimable", async () => {
    const { sql, observed } = sqlStub([
      [runnerInstanceRow({ status: "cordoned" }, { previous_status: "offline" })],
      [],
    ]);

    await new RunnerRepository(sql).heartbeatInstance({
      runnerInstanceId: "runner-instance-1",
    });

    const updateQuery = observed.queries.find((query) => query.includes("update cloud.runner_instances"));
    expect(updateQuery).toContain("selected.previous_status = 'offline'");
    expect(updateQuery).toContain("metadata #>> '{last_offline,previous_status}' in ('cordoned', 'draining')");
    expect(updateQuery).toContain("then (instance.metadata #>> '{last_offline,previous_status}')::cloud.runner_instance_status");
    expect(updateQuery).toContain("when selected.previous_status = 'offline' then 'online'::cloud.runner_instance_status");
    expect(updateQuery).toContain("returning instance.*, selected.previous_status");
    expect(observed.queries.find((query) => query.includes("insert into cloud.run_events"))).toContain("jsonb_build_object('attempt_id'");
    expect(observed.values.flat()).toContain("runtime.resource.runner_instance.heartbeat_restored");
    expect(observed.jsonPayloads).toContainEqual(expect.objectContaining({
      resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-instance-1",
        uid: "runner-instance-1",
      },
      resource_kind: "RunnerInstance",
      resource_name: "runner-instance-1",
      runner_instance_id: "runner-instance-1",
      previous_status: "offline",
      status: "cordoned",
      reason: "heartbeat_restored",
    }));
  });

  test("stale heartbeat cleanup marks cordoned runners offline with previous lifecycle status", async () => {
    const { sql, observed } = sqlStub([
      [runnerInstanceRow({ status: "offline" }, { previous_status: "cordoned" })],
      [],
    ]);

    await new RunnerRepository(sql).markStaleInstancesOffline({
      staleAfterSeconds: 60,
    });

    const updateQuery = observed.queries.find((query) => query.includes("update cloud.runner_instances"));
    expect(updateQuery).toContain("where status in ('online', 'cordoned', 'draining')");
    expect(updateQuery).toContain("'previous_status', candidates.previous_status::text");
    expect(updateQuery).toContain("'reason', 'heartbeat_stale'");
    expect(updateQuery).toContain("returning instance.*, candidates.previous_status");
    expect(observed.queries.find((query) => query.includes("insert into cloud.run_events"))).toContain("from cloud.run_attempts");
    expect(observed.values.flat()).toContain("runtime.resource.runner_instance.offline");
    expect(observed.jsonPayloads).toContainEqual(expect.objectContaining({
      resource_ref: {
        apiVersion: "bucephalus.dev/v1alpha1",
        kind: "RunnerInstance",
        name: "runner-instance-1",
        uid: "runner-instance-1",
      },
      resource_kind: "RunnerInstance",
      resource_name: "runner-instance-1",
      runner_instance_id: "runner-instance-1",
      previous_status: "cordoned",
      status: "offline",
      reason: "heartbeat_stale",
      stale_after_seconds: 60,
    }));
  });

  test("explicit runner status updates emit run-visible lifecycle audit events", async () => {
    const { sql, observed } = sqlStub([
      [runnerInstanceRow({ status: "unhealthy" }, { previous_status: "online" })],
      [],
    ]);

    await new RunnerRepository(sql).setInstanceStatus({
      runnerInstanceId: "runner-instance-1",
      status: "unhealthy",
      metadataPatch: {
        health: {
          status: "unhealthy",
          reason: "docker socket unreachable",
        },
      },
    });

    const updateQuery = observed.queries.find((query) => query.includes("update cloud.runner_instances"));
    expect(updateQuery).toContain("returning instance.*, selected.previous_status");
    expect(observed.values.flat()).toContain("runtime.resource.runner_instance.unhealthy");
    expect(observed.jsonPayloads).toContainEqual(expect.objectContaining({
      action: "set_status",
      resource_kind: "RunnerInstance",
      resource_name: "runner-instance-1",
      runner_instance_id: "runner-instance-1",
      previous_status: "online",
      status: "unhealthy",
      reason: "docker socket unreachable",
    }));
  });
});

function sqlStub(results: unknown[][]) {
  const observed: { queries: string[]; values: unknown[][]; jsonPayloads: JsonObject[] } = {
    queries: [],
    values: [],
    jsonPayloads: [],
  };
  const sql = ((strings: TemplateStringsArray, ...values: unknown[]) => {
    observed.queries.push(strings.join("?"));
    observed.values.push(values);
    return results.shift() ?? [];
  }) as any;
  sql.begin = async (callback: (tx: typeof sql) => Promise<unknown>) => await callback(sql);
  sql.json = (payload: JsonObject) => {
    observed.jsonPayloads.push(payload);
    return payload;
  };
  return { sql, observed };
}

function runnerInstanceRow(overrides: Partial<{
  runner_instance_id: string;
  runner_pool_id: string;
  instance_name: string;
  status: string;
  capabilities: JsonObject;
  metadata: JsonObject;
  last_heartbeat_at: string;
  created_at: string;
  updated_at: string;
}> = {}, extras: JsonObject = {}) {
  return {
    runner_instance_id: "runner-instance-1",
    runner_pool_id: "runner-pool-1",
    instance_name: "runner-1",
    status: "online",
    capabilities: { executors: ["runner-docker"], resources: ["core_runner"] },
    metadata: {},
    last_heartbeat_at: "2026-06-18T00:00:00.000Z",
    created_at: "2026-06-18T00:00:00.000Z",
    updated_at: "2026-06-18T00:00:00.000Z",
    ...overrides,
    ...extras,
  };
}

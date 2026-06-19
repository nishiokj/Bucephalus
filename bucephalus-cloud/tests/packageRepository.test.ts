import { describe, expect, test } from "bun:test";
import { RunRepository } from "../src/packages/repository";
import type { JsonObject } from "../src/primitives";

describe("run repository", () => {
  test("enriches worker run events with authenticated runner identities", async () => {
    const observed: { payload?: JsonObject } = {};
    const sql = ((_: TemplateStringsArray, ...values: unknown[]) => {
      return [{
        event_id: "event-1",
        run_id: "run-1",
        attempt_id: "attempt-1",
        seq: 1,
        event_type: String(values[1] ?? "worker.core.completed"),
        payload: observed.payload ?? {},
        created_at: "2026-06-04T00:00:00Z",
      }];
    }) as any;
    sql.json = (payload: JsonObject) => {
      observed.payload = payload;
      return payload;
    };

    await new RunRepository(sql).appendRunEvent({
      attemptId: "attempt-1",
      runnerInstanceId: "runner-instance-1",
      eventType: "worker.core.completed",
      payload: {
        runner_instance_id: "spoofed-runner",
        attempt_id: "spoofed-attempt",
        stdout_tail: "done",
      },
    });

    expect(observed.payload).toEqual({
      runner_instance_id: "runner-instance-1",
      attempt_id: "attempt-1",
      stdout_tail: "done",
    });
  });
});

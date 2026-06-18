import { appendFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, test } from "bun:test";
import {
  startEvidencePump,
  type EvidencePump,
  type EvidencePumpIo,
} from "../src/workerEvidence";
import type { RuntimeEventRowInsert } from "../src/runtime/repository";

const CORE_RUN_ID = "run_20260611_000001_000001_000001";

interface CapturedIo extends EvidencePumpIo {
  batches: RuntimeEventRowInsert[][];
  announcements: string[][];
  errors: { stage: string; fields: Record<string, unknown> }[];
  failNextPosts: number;
  failAnnounce: boolean;
}

function capturedIo(): CapturedIo {
  const io: CapturedIo = {
    batches: [],
    announcements: [],
    errors: [],
    failNextPosts: 0,
    failAnnounce: false,
    async postEventRows(rows) {
      if (io.failNextPosts > 0) {
        io.failNextPosts -= 1;
        throw new Error("control plane unavailable");
      }
      io.batches.push(rows);
    },
    async announceCoreRuns(coreRunIds) {
      if (io.failAnnounce) {
        throw new Error("announce failed");
      }
      io.announcements.push(coreRunIds);
    },
    onError(stage, fields) {
      io.errors.push({ stage, fields });
    },
  };
  return io;
}

function postedRows(io: CapturedIo): RuntimeEventRowInsert[] {
  return io.batches.flat();
}

async function until(check: () => boolean, timeoutMs = 5_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > deadline) {
      throw new Error("condition not reached in time");
    }
    await Bun.sleep(10);
  }
}

const cleanups: (() => Promise<void>)[] = [];

afterEach(async () => {
  while (cleanups.length > 0) {
    await cleanups.pop()!();
  }
});

async function workspace(): Promise<{ runRoot: string; trialDir: string; eventsPath: string }> {
  const root = await mkdtemp(join(tmpdir(), "buc-evidence-pump-"));
  cleanups.push(() => rm(root, { recursive: true, force: true }));
  const runRoot = join(root, "run-root");
  const trialDir = join(runRoot, CORE_RUN_ID, "trials", "trial-001");
  await mkdir(join(trialDir, "agent"), { recursive: true });
  return { runRoot, trialDir, eventsPath: join(trialDir, "agent", "events.jsonl") };
}

function pumpFor(io: EvidencePumpIo, runRoot: string, overrides: Record<string, number> = {}): EvidencePump {
  const pump = startEvidencePump(io, { runRootDir: runRoot, intervalMs: 10, ...overrides });
  cleanups.push(async () => {
    await pump.stop();
  });
  return pump;
}

function eventLine(seq: number, extra: Record<string, unknown> = {}): string {
  return `${JSON.stringify({ event_type: "agent.step", ts: `2026-06-11T00:00:0${seq % 10}Z`, seq, ...extra })}\n`;
}

describe("evidence pump tailing", () => {
  test("posts appended events incrementally with snapshot-parity row identity", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0) + eventLine(1) + "\n" + eventLine(2));
    const io = capturedIo();
    pumpFor(io, runRoot);

    await until(() => postedRows(io).length === 3);
    const rows = postedRows(io);
    // row_seq counts non-empty lines only, matching snapshot-derived rows.
    expect(rows.map((row) => row.row_seq)).toEqual([0, 1, 2]);
    expect(rows[0]).toMatchObject({
      core_run_id: CORE_RUN_ID,
      trial_id: "trial-001",
      schedule_idx: 0,
      attempt: 0,
      variant_id: "unknown",
      event_type: "agent.step",
      ts: "2026-06-11T00:00:00Z",
    });
    expect(io.announcements).toEqual([[CORE_RUN_ID]]);

    await appendFile(eventsPath, eventLine(3));
    await until(() => postedRows(io).length === 4);
    expect(postedRows(io).at(-1)).toMatchObject({ row_seq: 3, seq: 3 });
  });

  test("withholds rows until the core run announcement succeeds", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0));
    const io = capturedIo();
    io.failAnnounce = true;
    pumpFor(io, runRoot);

    await until(() => io.errors.some((error) => error.stage === "announce_core_runs"));
    expect(postedRows(io)).toHaveLength(0);

    io.failAnnounce = false;
    await until(() => postedRows(io).length === 1);
    expect(io.announcements).toEqual([[CORE_RUN_ID]]);
  });

  test("re-sends the same rows after a post failure (at-least-once)", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0) + eventLine(1));
    const io = capturedIo();
    io.failNextPosts = 1;
    pumpFor(io, runRoot);

    await until(() => postedRows(io).length === 2);
    expect(io.errors.some((error) => error.stage === "post_event_rows")).toBe(true);
    expect(postedRows(io).map((row) => row.row_seq)).toEqual([0, 1]);
  });

  test("re-sends enriched rows once trial identity appears", async () => {
    const { runRoot, trialDir, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0));
    const io = capturedIo();
    pumpFor(io, runRoot);
    await until(() => postedRows(io).length === 1);
    expect(postedRows(io)[0]).toMatchObject({ variant_id: "unknown", attempt: 0 });

    await mkdir(join(trialDir, "runner"), { recursive: true });
    await writeFile(
      join(trialDir, "runner", "contract_trace.json"),
      JSON.stringify({
        ids: {
          trial_id: "trial-001",
          schedule_idx: 4,
          attempt: 1,
          variant_id: "sonnet-4-6",
          task_id: "task-9",
          repl_idx: 2,
        },
      }),
    );
    await until(() => postedRows(io).some((row) => row.variant_id === "sonnet-4-6"));
    const enriched = postedRows(io).at(-1)!;
    expect(enriched).toMatchObject({
      row_seq: 0,
      schedule_idx: 4,
      attempt: 1,
      task_id: "task-9",
      repl_idx: 2,
    });
  });

  test("resets the cursor when the trajectory file is truncated", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0) + eventLine(1));
    const io = capturedIo();
    pumpFor(io, runRoot);
    await until(() => postedRows(io).length === 2);

    await writeFile(eventsPath, eventLine(0, { rewritten: true }));
    await until(() => postedRows(io).some((row) => row.payload.rewritten === true));
    expect(postedRows(io).at(-1)).toMatchObject({ row_seq: 0 });
  });

  test("redacts secrets and records malformed lines as parse errors", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(
      eventsPath,
      `${JSON.stringify({ event_type: "agent.env", ts: "2026-06-11T00:00:00Z", api_key: "sk-abcdefghijklmnopqrstuvwx" })}\n`
      + "this is not json\n",
    );
    const io = capturedIo();
    const pump = pumpFor(io, runRoot);
    await until(() => postedRows(io).length === 2);

    const [redacted, parseError] = postedRows(io);
    expect(redacted!.payload.api_key).toBe("[redacted]");
    expect(parseError!.event_type).toBe("trajectory_parse_error");
    expect(String(parseError!.payload.raw_line)).toContain("this is not json");
    const stats = await pump.stop();
    expect(stats.parse_errors).toBe(1);
  });

  test("skips a single oversized line without stalling the stream", async () => {
    const { runRoot, eventsPath } = await workspace();
    const huge = JSON.stringify({ event_type: "agent.blob", blob: "x".repeat(2_000) });
    await writeFile(eventsPath, `${huge}\n${eventLine(1)}`);
    const io = capturedIo();
    pumpFor(io, runRoot, { maxLineBytes: 512, maxBytesPerRead: 256 });

    await until(() => postedRows(io).some((row) => row.event_type === "agent.step"));
    const rows = postedRows(io);
    const stub = rows.find((row) => row.event_type === "trajectory_event_truncated");
    expect(stub).toBeDefined();
    expect(stub!.row_seq).toBe(0);
    expect(rows.at(-1)).toMatchObject({ event_type: "agent.step", row_seq: 1 });
  });

  test("stop drains events written after the last tick", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0));
    const io = capturedIo();
    const pump = startEvidencePump(io, { runRootDir: runRoot, intervalMs: 60_000 });
    await until(() => postedRows(io).length === 1);

    await appendFile(eventsPath, eventLine(1) + eventLine(2));
    const stats = await pump.stop();
    expect(postedRows(io)).toHaveLength(3);
    expect(stats.rows_posted).toBe(3);
    expect(stats.core_runs_announced).toBe(1);
  });

  test("stop does not hang forever behind a stuck event-row post", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0));
    const io = capturedIo();
    io.postEventRows = async () => {
      await new Promise(() => undefined);
    };
    const pump = startEvidencePump(io, {
      runRootDir: runRoot,
      intervalMs: 60_000,
      ioTimeoutMs: 20,
    });

    const stats = await pump.stop();

    expect(stats.post_failures).toBeGreaterThanOrEqual(1);
    expect(stats.rows_posted).toBe(0);
    expect(io.errors).toContainEqual(expect.objectContaining({
      stage: "post_event_rows",
    }));
    expect(String(io.errors.at(-1)?.fields.error)).toContain("timed out");
  });

  test("announcement timeout keeps rows withheld without blocking shutdown", async () => {
    const { runRoot, eventsPath } = await workspace();
    await writeFile(eventsPath, eventLine(0));
    const io = capturedIo();
    io.announceCoreRuns = async () => {
      await new Promise(() => undefined);
    };
    const pump = startEvidencePump(io, {
      runRootDir: runRoot,
      intervalMs: 60_000,
      ioTimeoutMs: 20,
    });

    const stats = await pump.stop();

    expect(stats.core_runs_announced).toBe(0);
    expect(stats.rows_posted).toBe(0);
    expect(io.errors).toContainEqual(expect.objectContaining({
      stage: "announce_core_runs",
    }));
    expect(String(io.errors.at(-1)?.fields.error)).toContain("timed out");
  });
});

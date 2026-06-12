import { open, readdir, readFile, stat } from "node:fs/promises";
import type { Dirent } from "node:fs";
import { join } from "node:path";
import { redactSensitiveJsonObject } from "./jsonRedaction";
import type { JsonObject as WireJsonObject } from "./primitives";
import type { RuntimeEventRowInsert } from "./runtime/repository";

type JsonObject = Record<string, unknown>;

/**
 * Live evidence pump.
 *
 * Trial workers append trajectory events to
 * `<run_root>/<core_run_id>/trials/<trial>/agent/events.jsonl` (the Modal
 * executor lands the same file when a trial's durable export syncs). This
 * pump tails those files while the core runner executes and posts the rows
 * to the control plane, so the user-facing event stream fills during the
 * run instead of after it. The end-of-run snapshot remains the repair pass;
 * the read path dedupes the two sources by logical row identity, which is
 * why row construction here mirrors snapshot semantics exactly: row_seq
 * counts non-empty lines, ids come from contract_trace/summary when
 * available, and payloads pass through the same redaction.
 *
 * Delivery is at-least-once over an idempotent upsert: cursors only advance
 * after the control plane acknowledges a batch, so retries re-send the same
 * logical rows and land on the same primary key.
 */

export interface EvidencePumpIo {
  postEventRows(rows: RuntimeEventRowInsert[]): Promise<void>;
  announceCoreRuns(coreRunIds: string[]): Promise<void>;
  onError(stage: string, fields: Record<string, unknown>): void;
}

export interface EvidencePumpOptions {
  runRootDir: string;
  intervalMs?: number;
  maxBatchRows?: number;
  maxBytesPerRead?: number;
  maxLineBytes?: number;
}

export interface EvidencePumpStats {
  ticks: number;
  rows_posted: number;
  batches_posted: number;
  post_failures: number;
  parse_errors: number;
  core_runs_announced: number;
}

const DEFAULT_INTERVAL_MS = 2_000;
const DEFAULT_MAX_BATCH_ROWS = 200;
const DEFAULT_MAX_BYTES_PER_READ = 2 * 1024 * 1024;
const DEFAULT_MAX_LINE_BYTES = 64 * 1024;
const IDENTITY_FILE_MAX_BYTES = 2 * 1024 * 1024;

interface TrialIdentity {
  trial_id: string;
  schedule_idx: number;
  attempt: number;
  variant_id: string;
  task_id: string;
  repl_idx: number;
  enriched: boolean;
}

interface TrialCursor {
  offset: number;
  rowSeq: number;
  remainder: Buffer;
  skippingOversizedLine: boolean;
  identity: TrialIdentity;
}

export class EvidencePump {
  private stopped = false;
  private wake: (() => void) | null = null;
  private readonly done: Promise<EvidencePumpStats>;
  private readonly announced = new Set<string>();
  private readonly cursors = new Map<string, TrialCursor>();
  private readonly stats: EvidencePumpStats = {
    ticks: 0,
    rows_posted: 0,
    batches_posted: 0,
    post_failures: 0,
    parse_errors: 0,
    core_runs_announced: 0,
  };

  constructor(
    private readonly io: EvidencePumpIo,
    private readonly options: Required<EvidencePumpOptions>,
  ) {
    this.done = this.runLoop();
  }

  /** Stops the loop after one final drain pass and reports stats. */
  async stop(): Promise<EvidencePumpStats> {
    this.stopped = true;
    this.wake?.();
    return this.done;
  }

  private async runLoop(): Promise<EvidencePumpStats> {
    while (!this.stopped) {
      await this.tickSafely();
      await this.sleep(this.options.intervalMs);
    }
    // Final drain: the core runner has exited, so whatever is on disk now is
    // the complete record; sweep it before the snapshot pass takes over.
    await this.tickSafely();
    return this.stats;
  }

  private sleep(ms: number): Promise<void> {
    if (this.stopped) {
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.wake = null;
        resolve();
      }, ms);
      this.wake = () => {
        clearTimeout(timer);
        this.wake = null;
        resolve();
      };
    });
  }

  private async tickSafely(): Promise<void> {
    this.stats.ticks += 1;
    try {
      await this.tick();
    } catch (error) {
      this.io.onError("tick", { error: errorMessage(error) });
    }
  }

  private async tick(): Promise<void> {
    const coreRunIds = await discoverCoreRunIdsFromRunRoot(this.options.runRootDir);
    const unannounced = coreRunIds.filter((id) => !this.announced.has(id));
    if (unannounced.length > 0) {
      try {
        await this.io.announceCoreRuns(unannounced);
        unannounced.forEach((id) => this.announced.add(id));
        this.stats.core_runs_announced += unannounced.length;
      } catch (error) {
        // Without the announcement the control plane cannot associate rows
        // with this cloud run, so hold the rows and retry next tick.
        this.io.onError("announce_core_runs", { core_run_ids: unannounced, error: errorMessage(error) });
        return;
      }
    }
    for (const coreRunId of coreRunIds) {
      const trialsDir = join(this.options.runRootDir, coreRunId, "trials");
      for (const trialName of await listDirectories(trialsDir)) {
        try {
          await this.pumpTrial(coreRunId, join(trialsDir, trialName), trialName);
        } catch (error) {
          this.io.onError("pump_trial", {
            core_run_id: coreRunId,
            trial: trialName,
            error: errorMessage(error),
          });
        }
      }
    }
  }

  private async pumpTrial(coreRunId: string, trialDir: string, trialName: string): Promise<void> {
    const eventsPath = join(trialDir, "agent", "events.jsonl");
    let cursor = this.cursors.get(eventsPath) ?? freshCursor(trialName);

    // Trial ids (variant, task, schedule slot, attempt) only exist once the
    // runner writes the contract trace or summary. When they appear, rewind
    // and re-send the full file so previously flushed rows are upgraded in
    // place — the upsert refreshes attribution on conflict.
    if (!cursor.identity.enriched) {
      const identity = await readTrialIdentity(trialDir, trialName);
      if (identity) {
        cursor = freshCursor(trialName, identity);
      }
    }

    let size: number;
    try {
      const fileStat = await stat(eventsPath);
      if (!fileStat.isFile()) {
        return;
      }
      size = fileStat.size;
    } catch (error) {
      if (isNodeError(error) && error.code === "ENOENT") {
        return;
      }
      throw error;
    }

    if (size < cursor.offset) {
      // The file shrank: the attempt restarted and rewrote its trajectory.
      // Start over; re-sent rows collapse onto the same keys.
      cursor = freshCursor(trialName, cursor.identity);
    }
    if (size === cursor.offset) {
      this.cursors.set(eventsPath, cursor);
      return;
    }

    const next = await this.readRows(coreRunId, eventsPath, cursor, size);
    if (next.rows.length > 0) {
      for (let start = 0; start < next.rows.length; start += this.options.maxBatchRows) {
        const batch = next.rows.slice(start, start + this.options.maxBatchRows);
        try {
          await this.io.postEventRows(batch);
          this.stats.batches_posted += 1;
          this.stats.rows_posted += batch.length;
        } catch (error) {
          // Leave the cursor unadvanced: the next tick re-reads the same
          // bytes and the idempotent upsert absorbs the replay.
          this.stats.post_failures += 1;
          this.io.onError("post_event_rows", {
            core_run_id: coreRunId,
            events_path: eventsPath,
            batch_rows: batch.length,
            error: errorMessage(error),
          });
          return;
        }
      }
    }
    this.cursors.set(eventsPath, next.cursor);
  }

  private async readRows(
    coreRunId: string,
    eventsPath: string,
    cursor: TrialCursor,
    size: number,
  ): Promise<{ rows: RuntimeEventRowInsert[]; cursor: TrialCursor }> {
    const readBytes = Math.min(size - cursor.offset, this.options.maxBytesPerRead);
    const chunk = await readChunk(eventsPath, cursor.offset, readBytes);
    let buffer = Buffer.concat([cursor.remainder, chunk]);
    let rowSeq = cursor.rowSeq;
    let skipping = cursor.skippingOversizedLine;
    const rows: RuntimeEventRowInsert[] = [];

    if (skipping) {
      const newline = buffer.indexOf(0x0a);
      if (newline === -1) {
        return {
          rows,
          cursor: { ...cursor, offset: cursor.offset + chunk.length, remainder: Buffer.alloc(0) },
        };
      }
      buffer = buffer.subarray(newline + 1);
      skipping = false;
    }

    const lastNewline = buffer.lastIndexOf(0x0a);
    let remainder: Buffer;
    let complete: string;
    if (lastNewline === -1) {
      complete = "";
      remainder = buffer;
    } else {
      // Split on the final newline byte so multi-byte UTF-8 sequences never
      // straddle a decode boundary.
      complete = buffer.subarray(0, lastNewline).toString("utf8");
      remainder = Buffer.from(buffer.subarray(lastNewline + 1));
    }

    for (const line of complete.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed) {
        continue;
      }
      rows.push(this.rowFromLine(coreRunId, cursor.identity, trimmed, rowSeq));
      rowSeq += 1;
    }

    if (remainder.length > this.options.maxLineBytes) {
      // A single line larger than the cap would otherwise pin the cursor
      // forever. Record a stub row for it and skip ahead to the next line.
      rows.push(this.stubRow(coreRunId, cursor.identity, rowSeq, remainder.length));
      rowSeq += 1;
      remainder = Buffer.alloc(0);
      skipping = true;
    }

    return {
      rows,
      cursor: {
        offset: cursor.offset + chunk.length,
        rowSeq,
        remainder,
        skippingOversizedLine: skipping,
        identity: cursor.identity,
      },
    };
  }

  private rowFromLine(
    coreRunId: string,
    identity: TrialIdentity,
    line: string,
    rowSeq: number,
  ): RuntimeEventRowInsert {
    let payload: JsonObject;
    try {
      const parsed = JSON.parse(line) as unknown;
      if (isRecord(parsed)) {
        payload = redactSensitiveJsonObject(parsed);
      } else {
        this.stats.parse_errors += 1;
        payload = { event_type: "trajectory_parse_error", error: "event line is not a JSON object" };
      }
    } catch (error) {
      this.stats.parse_errors += 1;
      payload = {
        event_type: "trajectory_parse_error",
        error: errorMessage(error),
        raw_line: line.slice(0, 512),
      };
    }
    return this.eventRow(coreRunId, identity, rowSeq, payload);
  }

  private stubRow(
    coreRunId: string,
    identity: TrialIdentity,
    rowSeq: number,
    observedBytes: number,
  ): RuntimeEventRowInsert {
    return this.eventRow(coreRunId, identity, rowSeq, {
      event_type: "trajectory_event_truncated",
      observed_bytes: observedBytes,
      limit_bytes: this.options.maxLineBytes,
    });
  }

  private eventRow(
    coreRunId: string,
    identity: TrialIdentity,
    rowSeq: number,
    payload: JsonObject,
  ): RuntimeEventRowInsert {
    return {
      core_run_id: coreRunId,
      trial_id: identity.trial_id,
      schedule_idx: identity.schedule_idx,
      attempt: identity.attempt,
      row_seq: rowSeq,
      slot_commit_id: "",
      variant_id: identity.variant_id,
      task_id: identity.task_id,
      repl_idx: identity.repl_idx,
      seq: numberField(payload.seq) ?? rowSeq,
      event_type: stringField(payload.event_type) ?? stringField(payload.type) ?? "unknown",
      ts: stringField(payload.ts) ?? stringField(payload.timestamp) ?? null,
      // Parsed straight from JSON text, so the unknown-valued record is a
      // plain JSON object by construction.
      payload: payload as WireJsonObject,
      row: {
        source: "worker_live_ingest",
        core_run_id: coreRunId,
        trial_id: identity.trial_id,
      },
    };
  }
}

export function startEvidencePump(io: EvidencePumpIo, options: EvidencePumpOptions): EvidencePump {
  return new EvidencePump(io, {
    runRootDir: options.runRootDir,
    intervalMs: options.intervalMs ?? DEFAULT_INTERVAL_MS,
    maxBatchRows: options.maxBatchRows ?? DEFAULT_MAX_BATCH_ROWS,
    maxBytesPerRead: options.maxBytesPerRead ?? DEFAULT_MAX_BYTES_PER_READ,
    maxLineBytes: options.maxLineBytes ?? DEFAULT_MAX_LINE_BYTES,
  });
}

export async function discoverCoreRunIdsFromRunRoot(runRootDir: string): Promise<string[]> {
  return (await listDirectories(runRootDir))
    .filter((name) => name.startsWith("run_"))
    .sort();
}

function freshCursor(trialName: string, identity?: TrialIdentity): TrialCursor {
  return {
    offset: 0,
    rowSeq: 0,
    remainder: Buffer.alloc(0),
    skippingOversizedLine: false,
    identity: identity ?? {
      trial_id: trialName,
      schedule_idx: 0,
      attempt: 0,
      variant_id: "unknown",
      task_id: "unknown",
      repl_idx: 0,
      enriched: false,
    },
  };
}

/** Mirrors the id selection used by snapshot-derived rows so the logical row
 * keys produced live and post-hoc are identical. */
async function readTrialIdentity(trialDir: string, trialName: string): Promise<TrialIdentity | null> {
  const contractTrace = await readBoundedJson(join(trialDir, "runner", "contract_trace.json"));
  const summary = contractTrace ? null : await readBoundedJson(join(trialDir, "summary.json"));
  const source = contractTrace ?? summary;
  if (!source || !isRecord(source.ids)) {
    return null;
  }
  const ids = source.ids;
  return {
    trial_id: stringField(ids.trial_id) ?? trialName,
    schedule_idx: numberField(ids.schedule_idx) ?? 0,
    attempt: numberField(ids.attempt) ?? 0,
    variant_id: stringField(ids.variant_id) ?? "unknown",
    task_id: stringField(ids.task_id) ?? "unknown",
    repl_idx: numberField(ids.repl_idx) ?? 0,
    enriched: true,
  };
}

async function listDirectories(dir: string): Promise<string[]> {
  let entries: Dirent[];
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return [];
    }
    throw error;
  }
  return entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
}

async function readChunk(path: string, offset: number, length: number): Promise<Buffer> {
  const handle = await open(path, "r");
  try {
    const buffer = Buffer.alloc(length);
    const { bytesRead } = await handle.read(buffer, 0, length, offset);
    return buffer.subarray(0, bytesRead);
  } finally {
    await handle.close();
  }
}

async function readBoundedJson(path: string): Promise<JsonObject | null> {
  try {
    const fileStat = await stat(path);
    if (!fileStat.isFile() || fileStat.size > IDENTITY_FILE_MAX_BYTES) {
      return null;
    }
    const parsed = JSON.parse(await readFile(path, "utf8")) as unknown;
    return isRecord(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function stringField(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function numberField(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? Math.trunc(value) : null;
}

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNodeError(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && "code" in error;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

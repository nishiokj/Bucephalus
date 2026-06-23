#!/usr/bin/env bun
import { chmod, mkdir, readdir, readFile, rm, stat, statfs, writeFile } from "node:fs/promises";
import type { Dirent } from "node:fs";
import { randomUUID } from "node:crypto";
import { join, resolve } from "node:path";
import os from "node:os";
import { spawn, type ChildProcess } from "node:child_process";
import { request as httpRequest } from "node:http";
import { setTimeout as sleep } from "node:timers/promises";
import { inspectSealedPackageArchive } from "./imports/sealedPackage";
import { redactSensitiveJsonObject } from "./jsonRedaction";
import { releaseIdentity } from "./release";
import { SECRET_FILE_MODE } from "./secretFiles";
import {
  discoverCoreRunIdsFromRunRoot,
  startEvidencePump,
  type EvidencePump,
} from "./workerEvidence";
export { discoverCoreRunIdsFromRunRoot } from "./workerEvidence";
import {
  childTraceContext,
  initTelemetry,
  logError,
  logInfo,
  newTraceContext,
  type TraceContext,
} from "./logging";

interface WorkerConfig {
  apiUrl: string;
  workerId: string;
  runnerPoolId: string;
  runnerInstanceId: string | null;
  leaseSeconds: number;
  pollMs: number;
  heartbeatMs: number;
  sweeperMs: number;
  dataDir: string;
  coreRunnerCommand: string;
  workerToken: string;
  secretResolverCommand: string[] | null;
  networkPolicyCommand: string[] | null;
  portForwardCommand: string[] | null;
  execCommand: string[] | null;
  capabilities: WorkerCapabilities;
  minFreeBytes: number;
  retainAttemptWorkspaces: boolean;
  provisionRequestId: string | null;
  providerInstanceId: string | null;
  workerImageRef: string | null;
  liveEvidence: boolean;
  evidenceIntervalMs: number;
  coreCompletionGraceMs: number;
  apiRequestTimeoutMs: number;
}

type JsonObject = Record<string, unknown>;

const RUNTIME_SNAPSHOT_EVENT_TYPE = "worker.runtime.snapshot";
const RUNTIME_SNAPSHOT_MAX_TRIALS = 200;
const RUNTIME_SNAPSHOT_MAX_EVENTS_PER_TRIAL = 200;
const RUNTIME_SNAPSHOT_MAX_EVIDENCE_RECORDS = 500;
const RUNTIME_SNAPSHOT_MAX_JSON_BYTES = 2 * 1024 * 1024;
const RUNTIME_SNAPSHOT_MAX_PAYLOAD_BYTES = 4 * 1024 * 1024;
const RUNTIME_SNAPSHOT_PAYLOAD_ENVELOPE_BYTES = 128 * 1024;
const RUNTIME_ARTIFACT_MAX_BYTES = 16 * 1024 * 1024;
const WORKER_JSON_COMMAND_MAX_STDOUT_BYTES = 1024 * 1024;
const WORKER_JSON_COMMAND_MAX_STDERR_BYTES = 128 * 1024;
const RUNTIME_EXEC_OUTPUT_TAIL_BYTES = 16_000;
const DOCKER_SOCKET_PATH = "/var/run/docker.sock";
const DOCKER_API_VERSION = "v1.41";
const RUNTIME_DOCKER_EXEC_HELPER_MODE = "runtime-docker-exec";
const RUNTIME_GCE_IAP_PORT_FORWARD_HELPER_MODE = "runtime-gce-iap-port-forward";
const RUNTIME_ARTIFACT_SPECS = [
  { role: "agent_result", relativePath: "agent/result.json", mediaType: "application/json; charset=utf-8" },
  { role: "agent_stdout", relativePath: "agent/stdout.log", mediaType: "text/plain; charset=utf-8" },
  { role: "agent_stderr", relativePath: "agent/stderr.log", mediaType: "text/plain; charset=utf-8" },
  { role: "agent_events", relativePath: "agent/events.jsonl", mediaType: "application/x-ndjson; charset=utf-8" },
  { role: "candidate_patch", relativePath: "candidate.patch", mediaType: "text/x-diff; charset=utf-8" },
  { role: "contract_trace", relativePath: "runner/contract_trace.json", mediaType: "application/json; charset=utf-8" },
  { role: "trial_summary", relativePath: "summary.json", mediaType: "application/json; charset=utf-8" },
] as const;

class WorkerError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WorkerError";
  }
}

class CloudApiError extends WorkerError {
  constructor(
    public readonly status: number,
    public readonly code: string | null,
    message: string,
    public readonly detail: JsonObject | null,
  ) {
    super(message);
    this.name = "CloudApiError";
  }
}

let shuttingDown = false;
let activeChild: ChildProcess | null = null;
let runnerInstancePoisoned = false;
let workerContext: TraceContext = newTraceContext({ component: "worker" });

async function main(): Promise<void> {
  if (process.argv[2] === RUNTIME_DOCKER_EXEC_HELPER_MODE) {
    await runRuntimeDockerExecCommand();
    return;
  }
  if (process.argv[2] === RUNTIME_GCE_IAP_PORT_FORWARD_HELPER_MODE) {
    await runRuntimeGceIapPortForwardCommand();
    return;
  }
  await initTelemetry();
  const config = loadWorkerConfig();
  workerContext = newTraceContext({ component: "worker", requestId: `worker-${config.workerId}` });
  const instance = await registerRunnerInstance(config);
  config.runnerInstanceId = instance.runner_instance_id;
  logInfo("worker.instance_registered", workerContext, {
    runner_instance_id: config.runnerInstanceId,
    worker_id: config.workerId,
  });
  try {
    await validateWorkerHost(config);
    await cleanupStartupResidue(config);
  } catch (error) {
    await poisonRunnerInstance(config, "startup_cleanup_failed", {
      error: errorMessage(error),
    }).catch((poisonError) => {
      logError("worker.poison_failed", workerContext, { error: errorMessage(poisonError) });
    });
    throw error;
  }
  process.on("SIGINT", () => requestShutdown("SIGINT"));
  process.on("SIGTERM", () => requestShutdown("SIGTERM"));

  const sweeper = runSweeper(config).catch((error) => {
    logError("worker.sweeper_stopped", workerContext, { error: errorMessage(error) });
    shuttingDown = true;
  });
  const instanceHeartbeat = runInstanceHeartbeat(config).catch((error) => {
    logError("worker.instance_heartbeat_stopped", workerContext, { error: errorMessage(error) });
    shuttingDown = true;
  });

  try {
    while (!shuttingDown) {
      const claim = await claimRun(config);
      if (claim.claimed) {
        await executeClaimedRun(config, claim);
        continue;
      }
      await sleep(config.pollMs);
    }
  } finally {
    shuttingDown = true;
    await sweeper.catch(() => undefined);
    await instanceHeartbeat.catch(() => undefined);
    if (config.runnerInstanceId && !runnerInstancePoisoned) {
      await markRunnerInstanceOffline(config, "worker_shutdown").catch((error) => {
        logError("worker.offline_failed", workerContext, { error: errorMessage(error) });
      });
    }
  }
}

async function runSweeper(config: WorkerConfig): Promise<void> {
  while (!shuttingDown) {
    await sleep(config.sweeperMs);
    if (shuttingDown) {
      return;
    }
    const result = await cloudFetch(config, "/v1/worker/runs/expire-leases", {
      method: "POST",
      body: {},
    });
    if (isRecord(result) && Array.isArray(result.expired) && result.expired.length > 0) {
      logInfo("worker.sweeper_expired", workerContext, { expired_count: result.expired.length });
    }
  }
}

async function runInstanceHeartbeat(config: WorkerConfig): Promise<void> {
  while (!shuttingDown) {
    await sleep(config.heartbeatMs);
    if (shuttingDown) {
      return;
    }
    await heartbeatRunnerInstance(config);
  }
}

async function executeClaimedRun(config: WorkerConfig, claim: RunClaim): Promise<void> {
  const attemptId = claim.attempt.attempt_id;
  const runId = claim.run.run_id;
  const workspaceDir = attemptWorkspaceDir(config, claim);
  const runContext = newTraceContext({ component: "worker-run", runId, attemptId });
  logInfo("worker.claimed_run", runContext, {
    run_id: runId,
    attempt_id: attemptId,
    worker_id: config.workerId,
  });

  let heartbeatStop = false;
  let materialized: MaterializedPackage | null = null;
  const heartbeatLoop = (async () => {
    while (!heartbeatStop && !shuttingDown) {
      await sleep(config.heartbeatMs);
      if (heartbeatStop || shuttingDown) {
        return;
      }
      await heartbeat(config, claim);
      if (materialized && config.portForwardCommand) {
        await processPortForwardRequests(config, claim, materialized).catch((error) => {
          logError("worker.runtime_port_forward_poll_failed", runContext, { error: errorMessage(error) });
        });
      }
      if (materialized && config.execCommand) {
        await processExecRequests(config, claim, materialized).catch((error) => {
          logError("worker.runtime_exec_poll_failed", runContext, { error: errorMessage(error) });
        });
      }
    }
  })();

  let evidencePump: EvidencePump | null = null;
  let coreError: unknown = null;
  let cleanupError: unknown = null;

  try {
    try {
      await appendEvent(config, claim, "worker.materializing", {
        package_digest: claim.run.package_digest,
        has_env: Object.keys(claim.run.env).length > 0,
        secret_ref_names: Object.keys(claim.run.secret_refs),
      });
      materialized = await materializePackage(config, claim);
      await appendEvent(config, claim, "worker.materialized", {
        workspace_dir: materialized.workspaceDir,
        package_archive_path: materialized.packageArchivePath,
        extracted_dir: materialized.extractedDir,
        run_root_dir: materialized.runRootDir,
        manifest_experiment_id: stringAt(materialized.manifestJson, "/resolved_experiment/experiment/id"),
      });
      for (const secretId of Object.keys(claim.run.secret_refs).sort()) {
        await appendEvent(config, claim, "worker.runtime.secret_binding.materialized", {
          resource_kind: "SecretBinding",
          resource_name: runtimeDeclaredResourceName(secretId),
          secret_id: secretId,
          status: "materialized",
          attempt_id: attemptId,
          run_id: runId,
          runner_instance_id: requireRunnerInstanceId(config),
          worker_id: config.workerId,
        });
      }
      evidencePump = startLiveEvidencePump(config, claim, materialized, runContext);
      await validateSidecarRequirementsWithAudit(config, claim, runContext);
      await validateAcceleratorRequirementsWithAudit(config, claim, runContext);
      await prePullRunImagesWithAudit(config, claim, runContext);
      await applyRuntimeNetworkPolicyWithAudit(config, claim, materialized, runContext);
      if (config.portForwardCommand) {
        await processPortForwardRequests(config, claim, materialized);
      }
      if (config.execCommand) {
        await processExecRequests(config, claim, materialized);
      }
      await executeCoreRun(config, claim, materialized, runContext);
    } catch (error) {
      coreError = error;
    }

    if (evidencePump) {
      // Drain before the snapshot pass so the stream is complete first and
      // the snapshot is reduced to its repair role. Pump trouble never
      // outranks the core outcome.
      try {
        const stats = await evidencePump.stop();
        await appendEvent(config, claim, "worker.runtime.live_evidence_summary", { ...stats });
      } catch (error) {
        logError("worker.evidence_pump_stop_failed", runContext, { error: errorMessage(error) });
      }
      evidencePump = null;
    }

    if (materialized) {
      try {
        const uploadSummary = await uploadRuntimeArtifacts(config, claim, materialized);
        await appendEvent(config, claim, "worker.runtime.artifacts_uploaded", uploadSummary);
      } catch (error) {
        await appendEvent(config, claim, "worker.runtime.artifact_upload_failed", {
          error: errorMessage(error),
        }).catch((eventError) => {
          logError("worker.artifact_upload_failure_event_failed", runContext, { error: errorMessage(eventError) });
        });
        if (!coreError) {
          coreError = error;
        } else {
          logError("worker.runtime_artifact_upload_failed", runContext, { error: errorMessage(error) });
        }
      }
      try {
        await uploadRuntimeSnapshots(config, claim, materialized);
      } catch (error) {
        await appendEvent(config, claim, "worker.runtime.snapshot_failed", {
          error: errorMessage(error),
        }).catch((eventError) => {
          logError("worker.snapshot_failure_event_failed", runContext, { error: errorMessage(eventError) });
        });
        if (!coreError) {
          coreError = error;
        } else {
          logError("worker.snapshot_upload_failed", runContext, { error: errorMessage(error) });
        }
      }
    }

    try {
      await cleanupClaimWorkspace(config, claim, materialized ?? {
        workspaceDir,
        packageArchivePath: join(workspaceDir, "package.tgz"),
        extractedDir: join(workspaceDir, "package"),
        runRootDir: join(workspaceDir, "run-root"),
        manifestJson: {},
        secretFiles: {},
        secretEnvFile: null,
      });
    } catch (error) {
      cleanupError = error;
    }

    if (cleanupError) {
      const message = `runner cleanup failed after run ${runId} attempt ${attemptId}: ${errorMessage(cleanupError)}`;
      await fail(config, claim, message).catch((failError) => {
        logError("worker.fail_run_failed", runContext, { error: errorMessage(failError) });
      });
      await poisonRunnerInstance(config, "attempt_cleanup_failed", {
        run_id: runId,
        attempt_id: attemptId,
        error: errorMessage(cleanupError),
      }).catch((poisonError) => {
        logError("worker.poison_failed", runContext, { error: errorMessage(poisonError) });
      });
      shuttingDown = true;
    } else if (coreError) {
      await fail(config, claim, errorMessage(coreError)).catch((failError) => {
        logError("worker.fail_run_failed", runContext, { error: errorMessage(failError) });
      });
    } else {
      await complete(config, claim);
      logInfo("worker.run_completed", runContext, { run_id: runId, worker_id: config.workerId });
    }
  } finally {
    heartbeatStop = true;
    await heartbeatLoop.catch(() => undefined);
  }
}

async function executeCoreRun(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
  context: TraceContext,
): Promise<void> {
  const command = coreRunnerCommand(config, claim, materialized);
  await appendEvent(config, claim, "worker.core.starting", {
    command: command.redactedArgs,
    workspace_dir: materialized.workspaceDir,
    run_root_dir: materialized.runRootDir,
  });
  // Hand the Rust core runner the trace identity so its structured logs join
  // this run's trace, and request JSON logs so log pipelines that parse
  // container output can recover severity + trace fields. Project id flows
  // through from the worker env.
  const runnerContext = childTraceContext(context, { component: "core-runner" });
  const env = coreRunnerEnv();
  env.BUCEPHALUS_LOG_FORMAT = "json";
  env.BUCEPHALUS_TRACE_ID = runnerContext.traceId;
  env.BUCEPHALUS_SPAN_ID = runnerContext.spanId;
  env.BUCEPHALUS_RUN_ID = claim.run.run_id;
  env.BUCEPHALUS_ATTEMPT_ID = claim.attempt.attempt_id;
  const timeoutMs = coreRunTimeoutMs(claim);
  const result = await runProcess(command.executable, command.args, {
    cwd: materialized.workspaceDir,
    env,
    timeoutMs,
    completionGraceMs: config.coreCompletionGraceMs,
  });
  const eventPayload = {
    exit_code: result.exitCode,
    timed_out: result.timedOut,
    completed_by_progress_watchdog: result.completedByProgressWatchdog,
    stdout_tail: tail(result.stdout, 16_000),
    stderr_tail: tail(result.stderr, 16_000),
  };
  if (result.completedByProgressWatchdog) {
    await appendEvent(config, claim, "worker.core.completed_after_progress_watchdog", eventPayload);
    return;
  }
  if (result.timedOut) {
    await appendEvent(config, claim, "worker.core.timed_out", eventPayload);
    throw new WorkerError(`Core runner timed out after ${timeoutMs} ms`);
  }
  if (result.exitCode !== 0) {
    await appendEvent(config, claim, "worker.core.failed", eventPayload);
    throw new WorkerError(
      coreRunnerFailureMessage(result.exitCode, result.stdout, result.stderr),
    );
  }
  await appendEvent(config, claim, "worker.core.completed", eventPayload);
}

export function coreRunnerFailureMessage(exitCode: number, stdout: string, stderr: string): string {
  const stdoutError = coreRunnerStdoutErrorMessage(stdout);
  const stderrTail = tail(stderr, 1000);
  const stdoutTail = tail(stdout, 1000);
  if (stdoutError && stderrTail) {
    return `Core runner exited with ${exitCode}: ${stdoutError}; stderr tail: ${stderrTail}`;
  }
  if (stdoutError) {
    return `Core runner exited with ${exitCode}: ${stdoutError}`;
  }
  return `Core runner exited with ${exitCode}: ${stderrTail || stdoutTail}`;
}

function coreRunnerStdoutErrorMessage(stdout: string): string | null {
  const fullStdoutError = coreRunnerErrorMessageFromJson(stdout.trim());
  if (fullStdoutError) {
    return fullStdoutError;
  }
  for (const line of stdout.split(/\r?\n/).reverse()) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("{")) {
      continue;
    }
    const lineError = coreRunnerErrorMessageFromJson(trimmed);
    if (lineError) {
      return lineError;
    }
  }
  return null;
}

function coreRunnerErrorMessageFromJson(raw: string): string | null {
  if (!raw.startsWith("{")) {
    return null;
  }
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!isRecord(parsed) || !isRecord(parsed.error)) {
      return null;
    }
    const message = parsed.error.message;
    const code = parsed.error.code;
    if (typeof message === "string" && message.length > 0) {
      return typeof code === "string" && code.length > 0
        ? `${code}: ${message}`
        : message;
    }
  } catch {
    return null;
  }
  return null;
}

async function uploadRuntimeSnapshots(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): Promise<void> {
  const coreRunIds = await discoverCoreRunIdsFromRunRoot(materialized.runRootDir);
  if (coreRunIds.length === 0) {
    throw new WorkerError("Core runner completed without producing a Core run directory");
  }
  for (const coreRunId of coreRunIds) {
    const snapshot = await collectRuntimeSnapshot(materialized.runRootDir, coreRunId);
    await appendEvent(config, claim, RUNTIME_SNAPSHOT_EVENT_TYPE, snapshot);
  }
}

async function uploadRuntimeArtifacts(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): Promise<JsonObject> {
  const coreRunIds = await discoverCoreRunIdsFromRunRoot(materialized.runRootDir);
  let uploaded = 0;
  const omitted: string[] = [];
  for (const coreRunId of coreRunIds) {
    const collected = await collectRuntimeArtifactUploads(materialized.runRootDir, coreRunId);
    omitted.push(...collected.omitted);
    for (const artifact of collected.items) {
      const bytes = await readFile(artifact.absolutePath);
      await uploadRuntimeArtifact(config, claim, artifact, bytes);
      uploaded += 1;
    }
  }
  return {
    core_run_ids: coreRunIds,
    uploaded_artifacts: uploaded,
    omitted_artifacts: omitted,
  };
}

export async function collectRuntimeArtifactUploads(
  runRootDir: string,
  coreRunId: string,
): Promise<{ items: RuntimeArtifactUpload[]; omitted: string[] }> {
  assertCoreRunId(coreRunId);
  const runDir = join(runRootDir, coreRunId);
  const trialsDir = join(runDir, "trials");
  let entries: Dirent[];
  try {
    entries = await readdir(trialsDir, { withFileTypes: true });
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { items: [], omitted: [] };
    }
    throw error;
  }
  const trialDirs = entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
  const items: RuntimeArtifactUpload[] = [];
  const omitted: string[] = [];
  for (const [index, trialId] of trialDirs.entries()) {
    const trialDir = join(trialsDir, trialId);
    const identity = await runtimeTrialIdentity(trialDir, trialId, index);
    for (const spec of RUNTIME_ARTIFACT_SPECS) {
      const relativePath = spec.relativePath;
      const absolutePath = join(trialDir, ...relativePath.split("/"));
      let fileStat;
      try {
        fileStat = await stat(absolutePath);
      } catch (error) {
        if (isNodeError(error) && error.code === "ENOENT") {
          continue;
        }
        throw error;
      }
      if (!fileStat.isFile()) {
        continue;
      }
      if (fileStat.size > RUNTIME_ARTIFACT_MAX_BYTES) {
        omitted.push(`trials/${trialId}/${relativePath}`);
        continue;
      }
      items.push({
        coreRunId,
        trialId: identity.trialId,
        scheduleIdx: identity.scheduleIdx,
        attempt: identity.attempt,
        role: spec.role,
        relativePath,
        mediaType: spec.mediaType,
        absolutePath,
        byteSize: fileStat.size,
      });
    }
  }
  return { items, omitted };
}

async function runtimeTrialIdentity(
  trialDir: string,
  fallbackTrialId: string,
  fallbackScheduleIdx: number,
): Promise<{ trialId: string; scheduleIdx: number; attempt: number }> {
  const summary = await readBoundedJsonObject(join(trialDir, "summary.json"));
  const contractTrace = await readBoundedJsonObject(join(trialDir, "runner", "contract_trace.json"));
  const summaryObject = summary.status === "read" ? summary.object : {};
  const contractObject = contractTrace.status === "read" ? contractTrace.object : {};
  const summaryIds = isRecord(summaryObject.ids) ? summaryObject.ids : {};
  const contractIds = isRecord(contractObject.ids) ? contractObject.ids : {};
  return {
    trialId: stringField(contractIds.trial_id) ?? stringField(summaryIds.trial_id) ?? fallbackTrialId,
    scheduleIdx: numberField(contractIds.schedule_idx) ?? numberField(summaryIds.schedule_idx) ?? fallbackScheduleIdx,
    attempt: numberField(contractIds.attempt) ?? numberField(summaryIds.attempt) ?? 0,
  };
}

async function uploadRuntimeArtifact(
  config: WorkerConfig,
  claim: RunClaim,
  artifact: RuntimeArtifactUpload,
  bytes: Uint8Array,
): Promise<void> {
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/runtime/artifacts`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    rawBody: bytes,
    headers: {
      "content-type": artifact.mediaType,
      "x-bucephalus-runner-instance-id": requireRunnerInstanceId(config),
      "x-bucephalus-core-run-id": artifact.coreRunId,
      "x-bucephalus-trial-id": artifact.trialId,
      "x-bucephalus-schedule-idx": String(artifact.scheduleIdx),
      "x-bucephalus-trial-attempt": String(artifact.attempt),
      "x-bucephalus-artifact-role": artifact.role,
      "x-bucephalus-artifact-relative-path": artifact.relativePath,
    },
  });
}

export async function collectRuntimeSnapshot(runRootDir: string, coreRunId: string): Promise<RuntimeSnapshotPayload> {
  assertCoreRunId(coreRunId);
  const runDir = join(runRootDir, coreRunId);
  const runtimeDir = join(runDir, "runtime");
  const budget = new RuntimeSnapshotBudget(
    RUNTIME_SNAPSHOT_MAX_PAYLOAD_BYTES - RUNTIME_SNAPSHOT_PAYLOAD_ENVELOPE_BYTES,
  );
  const runtimeValues: Record<string, JsonObject> = {};
  const omitted: string[] = [];
  const omit = (path: string) => {
    if (budget.tryAdd(path)) {
      omitted.push(path);
    }
  };
  for (const [key, relativePath] of [
    ["run_control_v2", "run_control.json"],
    ["schedule_progress_v2", "schedule_progress.json"],
    ["run_session_state_v1", "run_session_state.json"],
  ] as const) {
    const value = await readBoundedJsonObject(join(runtimeDir, relativePath));
    if (value.status === "read") {
      if (budget.tryAdd(value.object)) {
        runtimeValues[key] = value.object;
      } else {
        omit(`runtime/${relativePath}`);
      }
    } else if (value.status === "omitted") {
      omit(`runtime/${relativePath}`);
    }
  }

  const trialSummaries = await collectTrialSummaries(runDir, budget);
  if (trialSummaries.truncated) {
    omit("trials");
  }
  for (const path of trialSummaries.omitted) {
    omit(path);
  }
  const evidenceRecords = await readBoundedJsonLines(
    join(runDir, "evidence", "evidence_records.jsonl"),
    RUNTIME_SNAPSHOT_MAX_EVIDENCE_RECORDS,
  );
  let evidenceRecordItems: JsonObject[] = [];
  if (evidenceRecords.status === "read") {
    for (const record of evidenceRecords.items) {
      if (!budget.tryAdd(record)) {
        omit("evidence/evidence_records.jsonl");
        break;
      }
      evidenceRecordItems.push(record);
    }
    if (evidenceRecords.truncated) {
      omit("evidence/evidence_records.jsonl");
    }
  } else if (evidenceRecords.status === "omitted") {
    omit("evidence/evidence_records.jsonl");
  }
  return {
    core_run_id: coreRunId,
    run_dir_name: coreRunId,
    runtime_values: runtimeValues,
    trial_summaries: trialSummaries.items,
    evidence_records: evidenceRecordItems,
    omitted,
    snapshot_budget: {
      max_payload_bytes: RUNTIME_SNAPSHOT_MAX_PAYLOAD_BYTES,
      estimated_payload_bytes: budget.usedBytes,
      envelope_reserve_bytes: RUNTIME_SNAPSHOT_PAYLOAD_ENVELOPE_BYTES,
    },
  };
}

async function collectTrialSummaries(runDir: string, budget: RuntimeSnapshotBudget): Promise<{
  items: RuntimeTrialSummaryPayload[];
  truncated: boolean;
  omitted: string[];
}> {
  const trialsDir = join(runDir, "trials");
  let entries: Dirent[];
  try {
    entries = await readdir(trialsDir, { withFileTypes: true });
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { items: [], truncated: false, omitted: [] };
    }
    throw error;
  }
  const trialDirs = entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
  const items: RuntimeTrialSummaryPayload[] = [];
  const omitted: string[] = [];
  const omit = (path: string) => {
    if (budget.tryAdd(path)) {
      omitted.push(path);
    }
  };
  for (const trialId of trialDirs.slice(0, RUNTIME_SNAPSHOT_MAX_TRIALS)) {
    const value = await readBoundedJsonObject(join(trialsDir, trialId, "summary.json"));
    if (value.status === "read") {
      const item: RuntimeTrialSummaryPayload = {
        trial_id: trialId,
        summary: value.object,
      };
      const contractTrace = await readBoundedJsonObject(join(trialsDir, trialId, "runner", "contract_trace.json"));
      if (contractTrace.status === "read") {
        const candidate = {
          ...item,
          contract_trace: contractTrace.object,
        };
        if (budget.fits(candidate)) {
          item.contract_trace = contractTrace.object;
        } else {
          omit(`trials/${trialId}/runner/contract_trace.json`);
        }
      } else if (contractTrace.status === "omitted") {
        omit(`trials/${trialId}/runner/contract_trace.json`);
      }
      const trialEvents = await readBoundedJsonLines(
        join(trialsDir, trialId, "agent", "events.jsonl"),
        RUNTIME_SNAPSHOT_MAX_EVENTS_PER_TRIAL,
      );
      if (trialEvents.status === "read") {
        const candidate = {
          ...item,
          trial_events: trialEvents.items,
        };
        if (budget.fits(candidate)) {
          item.trial_events = trialEvents.items;
        } else {
          omit(`trials/${trialId}/agent/events.jsonl`);
        }
        if (trialEvents.truncated) {
          omit(`trials/${trialId}/agent/events.jsonl`);
        }
      } else if (trialEvents.status === "omitted") {
        omit(`trials/${trialId}/agent/events.jsonl`);
      }
      if (budget.tryAdd(item)) {
        items.push(item);
      } else {
        omit(`trials/${trialId}/summary.json`);
      }
    } else if (value.status === "omitted") {
      omit(`trials/${trialId}/summary.json`);
    }
  }
  return {
    items,
    truncated: trialDirs.length > RUNTIME_SNAPSHOT_MAX_TRIALS,
    omitted,
  };
}

class RuntimeSnapshotBudget {
  usedBytes = 0;

  constructor(private readonly maxBytes: number) {}

  fits(value: unknown): boolean {
    return this.usedBytes + estimatedJsonBytes(value) <= this.maxBytes;
  }

  tryAdd(value: unknown): boolean {
    const bytes = estimatedJsonBytes(value);
    if (this.usedBytes + bytes > this.maxBytes) {
      return false;
    }
    this.usedBytes += bytes;
    return true;
  }
}

function estimatedJsonBytes(value: unknown): number {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

async function readBoundedJsonLines(path: string, maxLines: number): Promise<
  | { status: "missing" }
  | { status: "omitted" }
  | { status: "read"; items: JsonObject[]; truncated: boolean }
> {
  let fileStat;
  try {
    fileStat = await stat(path);
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { status: "missing" };
    }
    throw error;
  }
  if (!fileStat.isFile() || fileStat.size > RUNTIME_SNAPSHOT_MAX_JSON_BYTES) {
    return { status: "omitted" };
  }
  const lines = (await readFile(path, "utf8")).split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const items = lines.slice(0, maxLines).map((line) => {
    try {
      const parsed = JSON.parse(line);
      return isRecord(parsed)
        ? redactSensitiveJsonObject(parsed)
        : { event_type: "trajectory_parse_error", error: "event line is not a JSON object" };
    } catch (error) {
      return {
        event_type: "trajectory_parse_error",
        error: errorMessage(error),
        raw_line: line,
      };
    }
  });
  return {
    status: "read",
    items,
    truncated: lines.length > maxLines,
  };
}

async function readBoundedJsonObject(path: string): Promise<
  | { status: "missing" }
  | { status: "omitted" }
  | { status: "read"; object: JsonObject }
> {
  let fileStat;
  try {
    fileStat = await stat(path);
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return { status: "missing" };
    }
    throw error;
  }
  if (!fileStat.isFile() || fileStat.size > RUNTIME_SNAPSHOT_MAX_JSON_BYTES) {
    return { status: "omitted" };
  }
  const parsed = JSON.parse(await readFile(path, "utf8"));
  if (!isRecord(parsed)) {
    return { status: "omitted" };
  }
  return { status: "read", object: redactSensitiveJsonObject(parsed) };
}

function assertCoreRunId(coreRunId: string): void {
  if (!/^run_[A-Za-z0-9_.-]+$/.test(coreRunId)) {
    throw new WorkerError(`Invalid Core run id '${coreRunId}'`);
  }
}

export function coreRunnerEnv(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {
    ...process.env,
  };
  delete env.BUCEPHALUS_CLOUD_API_URL;
  delete env.BUCEPHALUS_CLOUD_WORKER_TOKEN;
  delete env.BUCEPHALUS_CLOUD_ALLOW_CONTROL_PLANE_SECRET_REFS;
  delete env.BUCEPHALUS_CLOUD_ALLOW_LOCAL_IMAGE_REFS;
  delete env.BUCEPHALUS_RUNNER_POOL_ID;
  delete env.BUCEPHALUS_RUNNER_INSTANCE_ID;
  delete env.BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID;
  delete env.BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID;
  delete env.DATABASE_URL;
  delete env.BUCEPHALUS_WORKER_DATABASE_URL;
  delete env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON;
  delete env.BUCEPHALUS_RUN_STORE;
  delete env.BUCEPHALUS_RUN_STORE_URL;
  delete env.BUCEPHALUS_RUN_STORE_SCHEMA;
  delete env.BUCEPHALUS_SECRET_RESOLVER_AWS_CMD;
  delete env.BUCEPHALUS_SECRET_RESOLVER_GCLOUD_CMD;
  delete env.BUCEPHALUS_SECRET_RESOLVER_GCP_AUTH;
  delete env.BUCEPHALUS_SECRET_RESOLVER_ALLOW_CONTROL_PLANE_REFS;
  delete env.BUCEPHALUS_SECRET_RESOLVER_ALLOW_ENV;
  delete env.AWS_ACCESS_KEY_ID;
  delete env.AWS_SECRET_ACCESS_KEY;
  delete env.AWS_SESSION_TOKEN;
  delete env.GOOGLE_APPLICATION_CREDENTIALS;
  return env;
}

function coreRunnerCommand(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): { executable: string; args: string[]; redactedArgs: string[] } {
  const args = [
    "run",
    materialized.extractedDir,
    "--json",
    "--run-root",
    materialized.runRootDir,
  ];
  const runtimeOptions = claim.run.runtime_options;
  const smokeTest = runtimeOptions.smoke_test === true;
  if (smokeTest) {
    args.push("--smoke-test");
  } else {
    args.push("--run-dangerously");
  }

  const executor = optionalRuntimeString(runtimeOptions.executor);
  const cloudExecutor = claim.run.run_requirements.executor;
  const coreExecutor = executor ?? coreExecutorForCloudExecutor(cloudExecutor);
  if (coreExecutor) {
    args.push("--executor", coreExecutor);
  }
  const materialize = optionalRuntimeString(runtimeOptions.materialize);
  if (materialize) {
    args.push("--materialize", materialize);
  }

  for (const [key, value] of Object.entries(claim.run.env)) {
    assertRuntimeEnvKey(key);
    args.push("--env", `${key}=${value}`);
  }

  const redactedArgs = [...args];
  if (materialized.secretEnvFile) {
    args.push("--env-file", materialized.secretEnvFile);
    redactedArgs.push("--env-file", "<secret-env-file>");
  }
  for (const [id, secretPath] of Object.entries(materialized.secretFiles)) {
    args.push("--secret-file", `${id}=${secretPath}`);
    redactedArgs.push("--secret-file", `${id}=<secret:${claim.run.secret_refs[id] ?? "redacted"}>`);
  }

  return {
    executable: config.coreRunnerCommand,
    args,
    redactedArgs,
  };
}

function coreExecutorForCloudExecutor(executor: string): string | null {
  switch (executor) {
    case "runner-docker":
      return "local_docker";
    case "modal":
      return "modal";
    default:
      throw new WorkerError(`Unsupported Cloud runner executor '${executor}'`);
  }
}

function optionalRuntimeString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function assertRuntimeEnvKey(key: string): void {
  if (!/^[A-Z_][A-Z0-9_]*$/.test(key)) {
    throw new WorkerError(`Invalid runtime env key '${key}'`);
  }
}

function assertSecretId(id: string): void {
  if (!/^[A-Za-z0-9_.-]+$/.test(id)) {
    throw new WorkerError(`Invalid secret id '${id}'`);
  }
}

async function runProcess(
  executable: string,
  args: string[],
  options: { cwd: string; env: NodeJS.ProcessEnv; timeoutMs?: number; completionGraceMs?: number },
): Promise<{ exitCode: number; stdout: string; stderr: string; timedOut: boolean; completedByProgressWatchdog: boolean }> {
  return await new Promise((resolve, reject) => {
    const child = spawn(executable, args, {
      cwd: options.cwd,
      env: options.env,
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    activeChild = child;
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let timedOut = false;
    let completedByProgressWatchdog = false;
    let completionGraceTimer: ReturnType<typeof setTimeout> | null = null;
    const clearCompletionGraceTimer = () => {
      if (completionGraceTimer) {
        clearTimeout(completionGraceTimer);
        completionGraceTimer = null;
      }
    };
    const terminateChildProcessGroup = (signal: NodeJS.Signals) => {
      if (!child.pid) {
        return;
      }
      try {
        process.kill(-child.pid, signal);
      } catch {
        child.kill(signal);
      }
    };
    const startCompletionGraceTimer = () => {
      const graceMs = options.completionGraceMs ?? 0;
      if (completionGraceTimer || graceMs <= 0 || child.exitCode !== null) {
        return;
      }
      completionGraceTimer = setTimeout(() => {
        completedByProgressWatchdog = true;
        terminateChildProcessGroup("SIGTERM");
        setTimeout(() => {
          if (child.exitCode === null) {
            terminateChildProcessGroup("SIGKILL");
          }
        }, 5000).unref();
      }, graceMs);
      completionGraceTimer.unref();
    };
    const timeout = options.timeoutMs && options.timeoutMs > 0
      ? setTimeout(() => {
          timedOut = true;
          terminateChildProcessGroup("SIGTERM");
          setTimeout(() => {
            if (child.exitCode === null) {
              terminateChildProcessGroup("SIGKILL");
            }
          }, 5000).unref();
        }, options.timeoutMs)
      : null;
    timeout?.unref();
    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk));
    // The core runner emits its structured JSON logs on stderr (stdout carries
    // the --json result payload). Tee stderr through to the worker's own stderr
    // so the lines reach the container stream and the host's Cloud Logging
    // agent, while still buffering for the redacted failure-event tail. Stdout
    // is not forwarded: it is the result protocol, not logs.
    child.stderr.on("data", (chunk: Buffer) => {
      stderr.push(chunk);
      process.stderr.write(chunk);
      if (coreProgressCompleted(chunk.toString("utf8"))) {
        startCompletionGraceTimer();
      }
    });
    child.on("error", (error) => {
      if (timeout) {
        clearTimeout(timeout);
      }
      clearCompletionGraceTimer();
      if (activeChild === child) {
        activeChild = null;
      }
      reject(error);
    });
    child.on("close", (code) => {
      if (timeout) {
        clearTimeout(timeout);
      }
      clearCompletionGraceTimer();
      if (activeChild === child) {
        activeChild = null;
      }
      resolve({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
        timedOut,
        completedByProgressWatchdog,
      });
    });
  });
}

function coreRunTimeoutMs(claim: RunClaim): number {
  const requirementTimeout = claim.run.run_requirements.timeout_ms;
  if (typeof requirementTimeout !== "number" || requirementTimeout <= 0) {
    throw new WorkerError(
      `Run ${claim.run.run_id} has no usable run_requirements.timeout_ms; ` +
      `the run was created without a timeout and cannot be executed. ` +
      `Recreate the run with an explicit timeout_ms.`,
    );
  }
  return requirementTimeout;
}

export function coreProgressCompleted(text: string): boolean {
  return /(?:^|\n)\[run\]\s+run_[^\s:]+:\s+progress\s+(\d+)\/\1\s+\(100\.0%\)\s+slot=\d+\s+trial=\S+\s+status=completed(?:\r?\n|$)/.test(text);
}

async function claimRun(config: WorkerConfig): Promise<RunClaim | EmptyClaim> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  return await cloudFetch(config, "/v1/worker/runs/claim", {
    method: "POST",
    body: {
      runner_instance_id: runnerInstanceId,
      lease_seconds: config.leaseSeconds,
    },
  }) as RunClaim | EmptyClaim;
}

async function heartbeat(config: WorkerConfig, claim: RunClaim): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/heartbeat`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: runnerInstanceId,
      lease_seconds: config.leaseSeconds,
    },
  });
}

async function registerRunnerInstance(config: WorkerConfig): Promise<RunnerInstance> {
  return await cloudFetch(config, "/v1/runner-instances/register", {
    method: "POST",
    body: {
      runner_pool_id: config.runnerPoolId,
      instance_name: config.workerId,
      capabilities: config.capabilities,
      metadata: await runnerMetadata(config),
    },
  }) as RunnerInstance;
}

async function heartbeatRunnerInstance(config: WorkerConfig): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/runner-instances/${runnerInstanceId}/heartbeat`, {
    method: "POST",
    body: {
      capabilities: config.capabilities,
      metadata: await runnerMetadata(config),
    },
  });
}

async function poisonRunnerInstance(
  config: WorkerConfig,
  reason: string,
  details: JsonObject,
): Promise<void> {
  runnerInstancePoisoned = true;
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/runner-instances/${runnerInstanceId}/unhealthy`, {
    method: "POST",
    body: {
      reason,
      details,
    },
  });
}

async function markRunnerInstanceOffline(config: WorkerConfig, reason: string): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/runner-instances/${runnerInstanceId}/offline`, {
    method: "POST",
    body: { reason },
  });
}

export async function materializePackage(
  config: WorkerConfig,
  claim: RunClaim,
): Promise<MaterializedPackage> {
  const workspaceDir = attemptWorkspaceDir(config, claim);
  const packageArchivePath = join(workspaceDir, "package.tgz");
  const extractedDir = join(workspaceDir, "package");
  const runRootDir = join(workspaceDir, "run-root");
  await rm(workspaceDir, { recursive: true, force: true });
  await mkdir(extractedDir, { recursive: true });
  await mkdir(runRootDir, { recursive: true });
  const packageBytes = await cloudFetchBytes(config, `/v1/packages/${encodeURIComponent(claim.run.package_digest)}/content`, {
    authToken: claim.attempt.attempt_token,
    attemptId: claim.attempt.attempt_id,
  });
  await writeFile(packageArchivePath, packageBytes);
  const inspection = await inspectSealedPackageArchive({
    archivePath: packageArchivePath,
    workDir: extractedDir,
  });
  if (inspection.packageDigest !== claim.run.package_digest) {
    throw new WorkerError(
      `Downloaded package digest mismatch: claim expected ${claim.run.package_digest}, package declares ${inspection.packageDigest ?? "<missing>"}`,
    );
  }

  await writeFile(
    join(workspaceDir, "run-env.json"),
    `${JSON.stringify({
      env: claim.run.env,
      secret_refs: claim.run.secret_refs,
      runtime_options: claim.run.runtime_options,
    }, null, 2)}\n`,
  );
  const secretFiles = await materializeAttemptSecrets(config, claim, workspaceDir);
  const secretFromTypes = runtimeSecretFromTypes(inspection.manifestJson);
  const secretEnvFile = await buildSecretEnvFile(workspaceDir, secretFiles, secretFromTypes);
  const fileOnlySecretFiles = Object.fromEntries(
    Object.entries(secretFiles).filter(([id]) => secretFromTypes.get(id) === "file"),
  );

  return {
    workspaceDir,
    packageArchivePath,
    extractedDir,
    runRootDir,
    manifestJson: inspection.manifestJson,
    secretFiles: fileOnlySecretFiles,
    secretEnvFile,
  };
}

export async function materializeAttemptSecrets(
  config: Pick<WorkerConfig, "secretResolverCommand">,
  claim: Pick<RunClaim, "run" | "attempt">,
  workspaceDir: string,
): Promise<Record<string, string>> {
  const secretEntries = Object.entries(claim.run.secret_refs);
  if (secretEntries.length === 0) {
    return {};
  }
  for (const [id, ref] of secretEntries) {
    assertSecretId(id);
    assertSecretRef(ref);
  }
  if (!config.secretResolverCommand) {
    throw new WorkerError(
      "Run declares secret_refs, but no attempt-scoped secret resolver is configured. "
        + "Set BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON to a provider-managed resolver; "
        + "local persistent secret directories are not a Cloud runtime boundary.",
    );
  }

  const secretDir = join(workspaceDir, "secrets");
  await mkdir(secretDir, { recursive: true, mode: 0o700 });
  const result = await runJsonCommand(config.secretResolverCommand, {
    attempt_id: claim.attempt.attempt_id,
    run_id: claim.run.run_id,
    output_dir: secretDir,
    secrets: secretEntries.map(([id, ref]) => ({ id, ref })),
  });
  if (!isRecord(result) || !isRecord(result.files)) {
    throw new WorkerError("Secret resolver must return a JSON object with a files object");
  }

  const files: Record<string, string> = {};
  for (const [id, value] of Object.entries(result.files)) {
    assertSecretId(id);
    if (typeof value !== "string" || value.trim().length === 0) {
      throw new WorkerError(`Secret resolver returned an invalid file for '${id}'`);
    }
    if (!Object.prototype.hasOwnProperty.call(claim.run.secret_refs, id)) {
      throw new WorkerError(`Secret resolver returned undeclared secret id '${id}'`);
    }
    const outputPath = resolvedSecretOutputPath(secretDir, value);
    const fileStat = await stat(outputPath);
    if (!fileStat.isFile()) {
      throw new WorkerError(`Secret resolver output for '${id}' is not a file`);
    }
    await chmod(outputPath, SECRET_FILE_MODE);
    files[id] = outputPath;
  }
  const missing = secretEntries.map(([id]) => id).filter((id) => !files[id]);
  if (missing.length > 0) {
    throw new WorkerError(`Secret resolver did not materialize required secret id(s): ${missing.join(", ")}`);
  }
  return files;
}

function runtimeSecretFromTypes(manifestJson: JsonObject): Map<string, "env" | "file"> {
  const out = new Map<string, "env" | "file">();
  const resolved = manifestJson.resolved_experiment;
  if (!isRecord(resolved)) {
    return out;
  }
  const runtime = resolved.runtime;
  if (!isRecord(runtime)) {
    return out;
  }
  const secrets = runtime.secrets;
  if (!Array.isArray(secrets)) {
    return out;
  }
  for (const item of secrets) {
    if (!isRecord(item)) {
      continue;
    }
    const id = typeof item.name === "string" ? item.name.trim() : "";
    const from = typeof item.from === "string" ? item.from.trim() : "";
    if (!id || (from !== "env" && from !== "file")) {
      continue;
    }
    out.set(id, from);
  }
  return out;
}

async function buildSecretEnvFile(
  workspaceDir: string,
  secretFiles: Record<string, string>,
  fromTypes: Map<string, "env" | "file">,
): Promise<string | null> {
  const envIds = Object.keys(secretFiles).filter((id) => fromTypes.get(id) === "env");
  if (envIds.length === 0) {
    return null;
  }
  const envFilePath = join(workspaceDir, "run-secrets.env");
  const lines: string[] = [];
  for (const id of envIds.sort()) {
    const secretPath = secretFiles[id];
    if (!secretPath) {
      continue;
    }
    const content = await readFile(secretPath, "utf8");
    const value = content.replace(/\r?\n$/, "");
    lines.push(`${id}=${value}`);
  }
  if (lines.length === 0) {
    return null;
  }
  await writeFile(envFilePath, `${lines.join("\n")}\n`, { mode: 0o600 });
  return envFilePath;
}

export async function applyRuntimeNetworkPolicy(
  config: Pick<WorkerConfig, "networkPolicyCommand" | "workerId" | "runnerInstanceId">,
  claim: Pick<RunClaim, "run" | "attempt">,
  materialized: Pick<MaterializedPackage, "workspaceDir" | "runRootDir">,
): Promise<void> {
  const networkPerimeter = runtimeNetworkPerimeter(claim.run.run_requirements);
  if (networkPerimeter.egress_hosts.length === 0) {
    return;
  }
  if (!config.networkPolicyCommand) {
    throw new WorkerError(
      "Run declares runtime network egress requirements, but no network policy enforcer is configured. "
        + "Set BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON to a provider-managed enforcer; "
        + "ambient VM network access is not a Cloud runtime boundary.",
    );
  }
  await runJsonCommand(config.networkPolicyCommand, {
    attempt_id: claim.attempt.attempt_id,
    run_id: claim.run.run_id,
    runner_instance_id: config.runnerInstanceId,
    worker_id: config.workerId,
    workspace_dir: materialized.workspaceDir,
    run_root_dir: materialized.runRootDir,
    network_perimeter: networkPerimeter,
    egress_hosts: networkPerimeter.egress_hosts,
  });
}

export async function applyRuntimeNetworkPolicyWithAudit(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  materialized: Pick<MaterializedPackage, "workspaceDir" | "runRootDir">,
  context: TraceContext = newTraceContext({
    component: "worker-network-policy",
    runId: claim.run.run_id,
    attemptId: claim.attempt.attempt_id,
  }),
): Promise<void> {
  const networkPerimeter = runtimeNetworkPerimeter(claim.run.run_requirements);
  if (networkPerimeter.egress_hosts.length === 0) {
    return;
  }
  const eventPayload = () => ({
    resource_kind: "NetworkPerimeter",
    resource_name: "declared",
    status: "applied",
    attempt_id: claim.attempt.attempt_id,
    run_id: claim.run.run_id,
    runner_instance_id: requireRunnerInstanceId(config),
    worker_id: config.workerId,
    default: networkPerimeter.default,
    task_sandbox: networkPerimeter.task_sandbox,
    agent: networkPerimeter.agent,
    egress_hosts: networkPerimeter.egress_hosts,
  });
  await appendEvent(config, claim, "worker.runtime.network_perimeter.applying", {
    ...eventPayload(),
    status: "applying",
  });
  try {
    await applyRuntimeNetworkPolicy(config, claim, materialized);
  } catch (error) {
    await appendEvent(config, claim, "worker.runtime.network_perimeter.failed", {
      ...eventPayload(),
      status: "failed",
      error: errorMessage(error),
    }).catch((eventError) => {
      logError("worker.runtime_network_policy_failure_event_failed", context, { error: errorMessage(eventError) });
    });
    throw error;
  }
  await appendEvent(config, claim, "worker.runtime.network_perimeter.applied", eventPayload());
}

export async function processPortForwardRequests(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  materialized: Pick<MaterializedPackage, "workspaceDir" | "extractedDir" | "runRootDir">,
): Promise<void> {
  if (!config.portForwardCommand) {
    return;
  }
  const runnerInstanceId = requireRunnerInstanceId(config);
  const response = await cloudFetch(
    config,
    `/v1/worker/run-attempts/${claim.attempt.attempt_id}/runtime/resources/PortForward?runner_instance_id=${encodeURIComponent(runnerInstanceId)}`,
    { authToken: claim.attempt.attempt_token },
  );
  const requests = isRecord(response) && Array.isArray(response.resources)
    ? response.resources.filter(isRecord).map(runtimePortForwardRequestFromResponse).filter(isRuntimePortForwardRequest)
    : [];
  for (const request of requests) {
    if (request.status !== "requested" && request.status !== "accepted") {
      continue;
    }
    await fulfillPortForwardRequest(config, claim, materialized, request);
  }
}

export async function processExecRequests(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  materialized: Pick<MaterializedPackage, "workspaceDir" | "extractedDir" | "runRootDir">,
): Promise<void> {
  if (!config.execCommand) {
    return;
  }
  const runnerInstanceId = requireRunnerInstanceId(config);
  const response = await cloudFetch(
    config,
    `/v1/worker/run-attempts/${claim.attempt.attempt_id}/runtime/resources/Exec?runner_instance_id=${encodeURIComponent(runnerInstanceId)}`,
    { authToken: claim.attempt.attempt_token },
  );
  const requests = isRecord(response) && Array.isArray(response.resources)
    ? response.resources.filter(isRecord).map(runtimeExecRequestFromResponse).filter(isRuntimeExecRequest)
    : [];
  for (const request of requests) {
    if (request.status !== "requested" && request.status !== "accepted" && request.status !== "active") {
      continue;
    }
    await fulfillExecRequest(config, claim, materialized, request);
  }
}

function runtimePortForwardRequestFromResponse(value: Record<string, unknown>): RuntimePortForwardRequest | null {
  if (stringAtRecord(value, "kind") !== "PortForward") {
    return null;
  }
  const spec = objectAtRecord(value, "spec");
  const status = objectAtRecord(value, "status");
  const metadata = objectAtRecord(value, "metadata");
  const labels = objectAtRecord(metadata, "labels");
  const audit = objectAtRecord(value, "audit");
  const targetRef = objectAtRecord(spec, "target_ref");
  const runnerBinding = objectAtRecord(status, "runner_binding");
  const accessRequestId = stringAtRecord(spec, "access_request_id")
    ?? stringAtRecord(metadata, "uid")
    ?? stringAtRecord(metadata, "name");
  const targetPort = numberAtRecord(spec, "target_port");
  const resourceKind = stringAtRecord(spec, "resource_kind") ?? stringAtRecord(targetRef, "kind");
  const resourceName = stringAtRecord(spec, "resource_name") ?? stringAtRecord(targetRef, "name");
  if (!accessRequestId || !validPortForwardConnectionPort(targetPort) || !resourceKind || !resourceName) {
    return null;
  }
  return {
    access_request_id: accessRequestId,
    run_id: stringAtRecord(labels, "bucephalus.dev/run-id") ?? "",
    kind: "port_forward",
    status: stringAtRecord(status, "phase") ?? "unknown",
    resource_kind: resourceKind,
    resource_name: resourceName,
    protocol: stringAtRecord(spec, "protocol") ?? "tcp",
    target_port: targetPort,
    local_port: numberAtRecord(spec, "local_port"),
    runner_instance_id: stringAtRecord(status, "runner_instance_id") ?? stringAtRecord(runnerBinding, "runner_instance_id"),
    attempt_id: stringAtRecord(status, "attempt_id") ?? stringAtRecord(runnerBinding, "attempt_id"),
    requester: stringAtRecord(audit, "requester"),
    reason: stringAtRecord(spec, "reason"),
    connection: objectAtRecord(status, "connection"),
    error_message: stringAtRecord(status, "error_message"),
    created_at: stringAtRecord(metadata, "created_at") ?? "",
    updated_at: stringAtRecord(metadata, "updated_at") ?? stringAtRecord(metadata, "created_at") ?? "",
  };
}

function runtimeExecRequestFromResponse(value: Record<string, unknown>): RuntimeExecRequest | null {
  if (stringAtRecord(value, "kind") !== "Exec") {
    return null;
  }
  const spec = objectAtRecord(value, "spec");
  const status = objectAtRecord(value, "status");
  const metadata = objectAtRecord(value, "metadata");
  const labels = objectAtRecord(metadata, "labels");
  const audit = objectAtRecord(value, "audit");
  const targetRef = objectAtRecord(spec, "target_ref");
  const runnerBinding = objectAtRecord(status, "runner_binding");
  const accessRequestId = stringAtRecord(spec, "access_request_id")
    ?? stringAtRecord(metadata, "uid")
    ?? stringAtRecord(metadata, "name");
  const resourceKind = stringAtRecord(spec, "resource_kind") ?? stringAtRecord(targetRef, "kind");
  const resourceName = stringAtRecord(spec, "resource_name") ?? stringAtRecord(targetRef, "name");
  const command = stringArrayAtRecord(spec, "command");
  if (!accessRequestId || !resourceKind || !resourceName || command.length === 0) {
    return null;
  }
  return {
    access_request_id: accessRequestId,
    run_id: stringAtRecord(labels, "bucephalus.dev/run-id") ?? "",
    kind: "exec",
    status: stringAtRecord(status, "phase") ?? "unknown",
    resource_kind: resourceKind,
    resource_name: resourceName,
    protocol: stringAtRecord(spec, "protocol") ?? "exec",
    target_port: null,
    local_port: null,
    command,
    runner_instance_id: stringAtRecord(status, "runner_instance_id") ?? stringAtRecord(runnerBinding, "runner_instance_id"),
    attempt_id: stringAtRecord(status, "attempt_id") ?? stringAtRecord(runnerBinding, "attempt_id"),
    requester: stringAtRecord(audit, "requester"),
    reason: stringAtRecord(spec, "reason"),
    connection: objectAtRecord(status, "connection"),
    error_message: stringAtRecord(status, "error_message"),
    created_at: stringAtRecord(metadata, "created_at") ?? "",
    updated_at: stringAtRecord(metadata, "updated_at") ?? stringAtRecord(metadata, "created_at") ?? "",
  };
}

function isRuntimePortForwardRequest(value: RuntimePortForwardRequest | null): value is RuntimePortForwardRequest {
  return value !== null;
}

function isRuntimeExecRequest(value: RuntimeExecRequest | null): value is RuntimeExecRequest {
  return value !== null;
}

async function fulfillPortForwardRequest(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  materialized: Pick<MaterializedPackage, "workspaceDir" | "extractedDir" | "runRootDir">,
  request: RuntimePortForwardRequest,
): Promise<void> {
  if (!config.portForwardCommand) {
    return;
  }
  if (request.status === "requested") {
    const accepted = await tryUpdatePortForwardRequest(config, claim, request, "accept", {
      connection: {
        mode: "worker_command",
        worker_id: config.workerId,
      },
    });
    if (!accepted) {
      return;
    }
  }
  let output: JsonObject;
  try {
    const result = await runJsonCommand(config.portForwardCommand, {
      schema_version: "runtime_port_forward_command_v1",
      worker_id: config.workerId,
      runner_instance_id: requireRunnerInstanceId(config),
      provider_instance_id: config.providerInstanceId,
      run_id: claim.run.run_id,
      attempt_id: claim.attempt.attempt_id,
      workspace_dir: materialized.workspaceDir,
      package_dir: materialized.extractedDir,
      run_root_dir: materialized.runRootDir,
      port_forward: request,
    });
    output = isRecord(result) ? result : {};
  } catch (error) {
    await tryUpdatePortForwardRequest(config, claim, request, "fail", {
      error_message: errorMessage(error),
    });
    return;
  }
  try {
    const status = stringAtRecord(output, "status");
    const connection = isRecord(output.connection) ? output.connection as JsonObject : {};
    const action = status === "accepted"
      ? "accept"
      : status === "failed"
        ? "fail"
        : status === "expired"
          ? "expire"
          : portForwardCommandConnectionHasHandle(connection)
            ? "active"
            : "fail";
    await tryUpdatePortForwardRequest(config, claim, request, action, {
      connection,
      error_message: action === "fail"
        ? stringAtRecord(output, "error_message") ?? "port-forward command did not report a usable connection handle"
        : stringAtRecord(output, "error_message") ?? null,
    });
  } catch (error) {
    if (isStaleRuntimeAccessUpdate(error)) {
      return;
    }
    throw error;
  }
}

function portForwardCommandConnectionHasHandle(connection: JsonObject): boolean {
  if (validPortForwardConnectionPort(connection.local_port)) {
    return true;
  }
  return [
    "local_probe",
    "endpoint",
    "url",
    "listen",
    "listen_address",
    "local_address",
    "address",
    "client_endpoint",
    "client_url",
    "client_listen",
    "provider_tunnel_url",
    "tunnel",
  ].some((key) => Boolean(stringAtRecord(connection, key)));
}

function validPortForwardConnectionPort(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= 65535;
}

async function fulfillExecRequest(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  materialized: Pick<MaterializedPackage, "workspaceDir" | "extractedDir" | "runRootDir">,
  request: RuntimeExecRequest,
): Promise<void> {
  if (!config.execCommand) {
    return;
  }
  if (request.status === "requested") {
    const accepted = await tryUpdateExecRequest(config, claim, request, "accept", {
      connection: {
        mode: "worker_command",
        worker_id: config.workerId,
      },
    });
    if (!accepted) {
      return;
    }
  }
  let output: JsonObject;
  try {
    const result = await runJsonCommand(config.execCommand, {
      schema_version: "runtime_exec_command_v1",
      worker_id: config.workerId,
      runner_instance_id: requireRunnerInstanceId(config),
      run_id: claim.run.run_id,
      attempt_id: claim.attempt.attempt_id,
      workspace_dir: materialized.workspaceDir,
      package_dir: materialized.extractedDir,
      run_root_dir: materialized.runRootDir,
      exec: request,
    });
    output = isRecord(result) ? result : {};
  } catch (error) {
    await tryUpdateExecRequest(config, claim, request, "fail", {
      error_message: errorMessage(error),
    });
    return;
  }
  try {
    const status = stringAtRecord(output, "status");
    const connection = execCommandConnection(output, config.workerId);
    const action = execCommandAction(status, connection);
    await tryUpdateExecRequest(config, claim, request, action, {
      connection,
      error_message: action === "fail"
        ? stringAtRecord(output, "error_message") ?? execCommandFailureMessage(status, connection)
        : stringAtRecord(output, "error_message") ?? null,
    });
  } catch (error) {
    if (isStaleRuntimeAccessUpdate(error)) {
      return;
    }
    throw error;
  }
}

function execCommandAction(
  status: string | null,
  connection: JsonObject,
): "accept" | "active" | "fail" | "expire" | "complete" {
  if (status === "accepted") {
    return "accept";
  }
  if (status === "active") {
    return "active";
  }
  if (status === "failed") {
    return "fail";
  }
  if (status === "expired") {
    return "expire";
  }
  return execCommandConnectionHasExitCode(connection) ? "complete" : "fail";
}

function execCommandConnectionHasExitCode(connection: JsonObject): boolean {
  return validRuntimeExecExitCode(connection.exit_code);
}

function execCommandFailureMessage(status: string | null, connection: JsonObject): string | null {
  if (status === "failed") {
    return null;
  }
  return execCommandConnectionHasExitCode(connection)
    ? null
    : "runtime exec command did not report an exit_code";
}

function validRuntimeExecExitCode(value: unknown): boolean {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 255;
}

async function updatePortForwardRequest(
  config: WorkerConfig,
  claim: Pick<RunClaim, "attempt">,
  request: RuntimePortForwardRequest,
  action: "accept" | "active" | "fail" | "expire",
  body: { connection?: JsonObject; error_message?: string | null },
): Promise<void> {
  await cloudFetch(
    config,
    `/v1/worker/run-attempts/${claim.attempt.attempt_id}/runtime/resources/PortForward/${encodeURIComponent(request.access_request_id)}/${action}`,
    {
      method: "POST",
      authToken: claim.attempt.attempt_token,
      body: {
        runner_instance_id: requireRunnerInstanceId(config),
        ...body,
      },
    },
  );
}

async function tryUpdatePortForwardRequest(
  config: WorkerConfig,
  claim: Pick<RunClaim, "attempt">,
  request: RuntimePortForwardRequest,
  action: "accept" | "active" | "fail" | "expire",
  body: { connection?: JsonObject; error_message?: string | null },
): Promise<boolean> {
  try {
    await updatePortForwardRequest(config, claim, request, action, body);
    return true;
  } catch (error) {
    if (isStaleRuntimeAccessUpdate(error)) {
      return false;
    }
    throw error;
  }
}

async function updateExecRequest(
  config: WorkerConfig,
  claim: Pick<RunClaim, "attempt">,
  request: RuntimeExecRequest,
  action: "accept" | "active" | "complete" | "fail" | "expire",
  body: { connection?: JsonObject; error_message?: string | null },
): Promise<void> {
  await cloudFetch(
    config,
    `/v1/worker/run-attempts/${claim.attempt.attempt_id}/runtime/resources/Exec/${encodeURIComponent(request.access_request_id)}/${action}`,
    {
      method: "POST",
      authToken: claim.attempt.attempt_token,
      body: {
        runner_instance_id: requireRunnerInstanceId(config),
        ...body,
      },
    },
  );
}

async function tryUpdateExecRequest(
  config: WorkerConfig,
  claim: Pick<RunClaim, "attempt">,
  request: RuntimeExecRequest,
  action: "accept" | "active" | "complete" | "fail" | "expire",
  body: { connection?: JsonObject; error_message?: string | null },
): Promise<boolean> {
  try {
    await updateExecRequest(config, claim, request, action, body);
    return true;
  } catch (error) {
    if (isStaleRuntimeAccessUpdate(error)) {
      return false;
    }
    throw error;
  }
}

function isStaleRuntimeAccessUpdate(error: unknown): boolean {
  return error instanceof CloudApiError
    && error.status === 409
    && (
      error.code === "runtime_access_request_not_active"
      || error.code === "runtime_access_transition_invalid"
    );
}

function execCommandConnection(output: JsonObject, workerId: string): JsonObject {
  const connection: JsonObject = isRecord(output.connection) ? { ...output.connection as JsonObject } : {};
  if (!connection.mode) {
    connection.mode = "worker_command";
  }
  if (!connection.worker_id) {
    connection.worker_id = workerId;
  }
  const exitCode = output.exit_code;
  if (typeof exitCode === "number" && Number.isInteger(exitCode)) {
    connection.exit_code = exitCode;
  }
  const stdout = output.stdout;
  if (typeof stdout === "string") {
    addExecOutputConnectionEvidence(connection, "stdout", stdout);
  }
  const stderr = output.stderr;
  if (typeof stderr === "string") {
    addExecOutputConnectionEvidence(connection, "stderr", stderr);
  }
  return connection;
}

function addExecOutputConnectionEvidence(
  connection: JsonObject,
  stream: "stdout" | "stderr",
  value: string,
): void {
  const bytes = Buffer.byteLength(value, "utf8");
  connection[`${stream}_tail`] = tail(value, RUNTIME_EXEC_OUTPUT_TAIL_BYTES);
  connection[`${stream}_bytes`] = bytes;
  connection[`${stream}_tail_bytes`] = Math.min(bytes, RUNTIME_EXEC_OUTPUT_TAIL_BYTES);
  connection[`${stream}_tail_truncated`] = bytes > RUNTIME_EXEC_OUTPUT_TAIL_BYTES;
}

export type RuntimeExecCommandTarget =
  | {
    mode: "docker_container";
    coreRunId: string;
    trialId: string;
    scheduleIdx: number | null;
    role: string;
    containerId: string;
    workdir: string | null;
    resourceName: string;
  }
  | {
    mode: "worker_process";
    resourceKind: string;
    resourceName: string;
  };

type RuntimePortForwardCommandTarget =
  | {
    mode: "docker_container";
    coreRunId: string;
    trialId: string;
    role: string;
    containerId: string;
    targetHost: string;
    targetPort: number;
  }
  | {
    mode: "worker_host";
    resourceKind: string;
    resourceName: string;
    targetHost: string;
    targetPort: number;
  };

async function runRuntimeGceIapPortForwardCommand(): Promise<void> {
  const input = await readRuntimePortForwardCommandInput();
  const portForward = objectAtRecord(input, "port_forward");
  const targetPort = numberAtRecord(portForward, "target_port");
  if (!validPortForwardConnectionPort(targetPort)) {
    throw new WorkerError("runtime port-forward helper input must include port_forward.target_port");
  }
  const targetPortValue = targetPort as number;
  const resourceKind = stringAtRecord(portForward, "resource_kind");
  const resourceName = stringAtRecord(portForward, "resource_name");
  if (!resourceKind || !resourceName) {
    throw new WorkerError("runtime port-forward helper input must include port_forward.resource_kind and port_forward.resource_name");
  }
  const runRootDir = stringAtRecord(input, "run_root_dir");
  if (!runRootDir) {
    throw new WorkerError("runtime port-forward helper input must include run_root_dir");
  }
  const provider = parseGceProviderInstanceId(
    stringAtRecord(input, "provider_instance_id")
      ?? process.env.BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID
      ?? "",
  );
  if (!provider) {
    throw new WorkerError("runtime GCE IAP port-forward helper requires a gce:// provider_instance_id");
  }
  const target = await resolveRuntimePortForwardCommandTarget(runRootDir, resourceKind, resourceName, targetPortValue);
  const localPort = numberAtRecord(portForward, "local_port");
  const connection = compactWorkerJson({
    mode: "gcp_iap_ssh",
    provider: "gce-iap-ssh-local-forward",
    project_id: provider.projectId,
    zone: provider.zone,
    instance_name: provider.instanceName,
    target_host: target.targetHost,
    target_port: target.targetPort,
    requested_target_port: targetPortValue,
    local_port: validPortForwardConnectionPort(localPort) ? localPort : undefined,
    target_mode: target.mode,
    client_reachable: false,
    provider_tunnel_url: `gcp-iap-ssh://projects/${encodeURIComponent(provider.projectId)}/zones/${encodeURIComponent(provider.zone)}/instances/${encodeURIComponent(provider.instanceName)}?target_host=${encodeURIComponent(target.targetHost)}&target_port=${encodeURIComponent(String(target.targetPort))}`,
    tunnel: `gcp-iap-ssh ${provider.instanceName} ${target.targetHost}:${target.targetPort}`,
    worker_id: stringAtRecord(input, "worker_id"),
    ...(target.mode === "docker_container"
      ? {
        core_run_id: target.coreRunId,
        trial_id: target.trialId,
        role: target.role,
        container_id: target.containerId,
      }
      : {
        resource_kind: target.resourceKind,
        resource_name: target.resourceName,
      }),
  });
  console.log(JSON.stringify({
    status: "active",
    connection,
  }));
}

async function readRuntimePortForwardCommandInput(): Promise<JsonObject> {
  const text = await new Response(Bun.stdin.stream()).text();
  if (text.trim().length === 0) {
    throw new WorkerError("runtime port-forward helper requires JSON on stdin");
  }
  const parsed = JSON.parse(text);
  if (!isRecord(parsed)) {
    throw new WorkerError("runtime port-forward helper input must be a JSON object");
  }
  if (stringAtRecord(parsed, "schema_version") !== "runtime_port_forward_command_v1") {
    throw new WorkerError("runtime port-forward helper input schema_version must be runtime_port_forward_command_v1");
  }
  return parsed as JsonObject;
}

async function resolveRuntimePortForwardCommandTarget(
  runRootDir: string,
  resourceKind: string,
  resourceName: string,
  targetPort: number,
): Promise<RuntimePortForwardCommandTarget> {
  const execTarget = await resolveRuntimeExecCommandTarget(runRootDir, resourceKind, resourceName);
  if (execTarget.mode === "docker_container") {
    return {
      mode: "docker_container",
      coreRunId: execTarget.coreRunId,
      trialId: execTarget.trialId,
      role: execTarget.role,
      containerId: execTarget.containerId,
      targetHost: await dockerContainerIpAddress(execTarget.containerId),
      targetPort,
    };
  }
  return {
    mode: "worker_host",
    resourceKind: execTarget.resourceKind,
    resourceName: execTarget.resourceName,
    targetHost: "127.0.0.1",
    targetPort,
  };
}

async function dockerContainerIpAddress(containerId: string): Promise<string> {
  const inspect = await dockerRequest<{ NetworkSettings?: unknown }>(
    "GET",
    `/containers/${encodeURIComponent(containerId)}/json`,
  );
  const networkSettings = isRecord(inspect.NetworkSettings) ? inspect.NetworkSettings : {};
  const bridgeAddress = stringAtRecord(networkSettings, "IPAddress");
  if (bridgeAddress) {
    return bridgeAddress;
  }
  const networks = isRecord(networkSettings.Networks) ? networkSettings.Networks : {};
  for (const network of Object.values(networks)) {
    if (!isRecord(network)) {
      continue;
    }
    const address = stringAtRecord(network, "IPAddress");
    if (address) {
      return address;
    }
  }
  throw new WorkerError(`Docker container ${containerId} does not have an inspectable IP address for port-forward`);
}

function parseGceProviderInstanceId(value: string): { projectId: string; zone: string; instanceName: string } | null {
  const match = /^gce:\/\/projects\/([^/]+)\/zones\/([^/]+)\/instances\/([^/]+)$/.exec(value);
  if (!match) {
    return null;
  }
  return {
    projectId: decodeURIComponent(match[1] ?? ""),
    zone: decodeURIComponent(match[2] ?? ""),
    instanceName: decodeURIComponent(match[3] ?? ""),
  };
}

function compactWorkerJson(input: Record<string, unknown>): JsonObject {
  const out: JsonObject = {};
  for (const [key, value] of Object.entries(input)) {
    if (value !== undefined && value !== null && value !== "") {
      out[key] = value;
    }
  }
  return out;
}

async function runRuntimeDockerExecCommand(): Promise<void> {
  const input = await readRuntimeExecCommandInput();
  const exec = objectAtRecord(input, "exec");
  const command = Array.isArray(exec.command)
    ? exec.command.filter((item): item is string => typeof item === "string" && item.length > 0)
    : [];
  if (command.length === 0) {
    throw new WorkerError("runtime exec command input must include exec.command");
  }
  const resourceKind = stringAtRecord(exec, "resource_kind");
  const resourceName = stringAtRecord(exec, "resource_name");
  if (!resourceKind || !resourceName) {
    throw new WorkerError("runtime exec command input must include exec.resource_kind and exec.resource_name");
  }
  const runRootDir = stringAtRecord(input, "run_root_dir");
  if (!runRootDir) {
    throw new WorkerError("runtime exec command input must include run_root_dir");
  }
  const target = await resolveRuntimeExecCommandTarget(runRootDir, resourceKind, resourceName);
  const result = target.mode === "docker_container"
    ? await executeDockerContainerExec(target, command)
    : await executeWorkerProcessExec(command, stringAtRecord(input, "workspace_dir") ?? process.cwd());
  const stdout = tail(result.stdout, 16_000);
  const stderr = tail(result.stderr, 16_000);
  const connection: JsonObject = target.mode === "docker_container"
    ? {
      mode: "docker_exec",
      provider: "gce-docker-socket",
      core_run_id: target.coreRunId,
      trial_id: target.trialId,
      role: target.role,
      container_id: target.containerId,
      workdir: target.workdir,
      provider_exec_id: result.providerExecId,
    }
    : {
      mode: "worker_process",
      provider: "gce-worker-container",
      resource_kind: target.resourceKind,
      resource_name: target.resourceName,
    };
  console.log(JSON.stringify({
    status: result.exitCode === 0 ? "completed" : "failed",
    exit_code: result.exitCode,
    stdout,
    stderr,
    error_message: result.exitCode === 0 ? null : `runtime exec exited ${result.exitCode}`,
    connection,
  }));
}

async function readRuntimeExecCommandInput(): Promise<JsonObject> {
  const text = await new Response(Bun.stdin.stream()).text();
  if (text.trim().length === 0) {
    throw new WorkerError("runtime exec helper requires JSON on stdin");
  }
  const parsed = JSON.parse(text);
  if (!isRecord(parsed)) {
    throw new WorkerError("runtime exec helper input must be a JSON object");
  }
  if (stringAtRecord(parsed, "schema_version") !== "runtime_exec_command_v1") {
    throw new WorkerError("runtime exec helper input schema_version must be runtime_exec_command_v1");
  }
  return parsed as JsonObject;
}

export async function resolveRuntimeExecCommandTarget(
  runRootDir: string,
  resourceKind: string,
  resourceName: string,
): Promise<RuntimeExecCommandTarget> {
  const kind = canonicalRuntimeExecTargetKind(resourceKind);
  if (kind === "Run" || kind === "RunnerInstance" || kind === "RunnerAttempt") {
    return { mode: "worker_process", resourceKind: kind, resourceName };
  }
  const targets = await runtimeExecContainerTargets(runRootDir);
  if (kind === "TrialContainer") {
    const selected = targets.find((target) =>
      target.resourceName === resourceName
      || target.containerId === resourceName
      || shortRuntimeResourceName(target.containerId) === resourceName
    );
    if (!selected) {
      throw new WorkerError(`No runtime container matches TrialContainer/${resourceName}`);
    }
    return selected;
  }
  if (kind === "Trial") {
    const selected = preferredRuntimeExecContainer(targets.filter((target) =>
      runtimeResourceName(target.trialId) === resourceName || target.trialId === resourceName
    ));
    if (!selected) {
      throw new WorkerError(`No live runtime container matches Trial/${resourceName}`);
    }
    return selected;
  }
  if (kind === "ScheduleSlot") {
    const parsed = parseScheduleSlotResourceName(resourceName);
    const selected = parsed
      ? preferredRuntimeExecContainer(targets.filter((target) =>
        runtimeResourceName(target.coreRunId) === parsed.coreRunName
        && target.scheduleIdx === parsed.scheduleIdx
      ))
      : null;
    if (!selected) {
      throw new WorkerError(`No live runtime container matches ScheduleSlot/${resourceName}`);
    }
    return selected;
  }
  throw new WorkerError(`Runtime exec helper does not support ${resourceKind}/${resourceName}`);
}

async function executeDockerContainerExec(
  target: Extract<RuntimeExecCommandTarget, { mode: "docker_container" }>,
  command: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string; providerExecId: string }> {
  const create = await dockerRequest<{ Id?: unknown }>(
    "POST",
    `/containers/${encodeURIComponent(target.containerId)}/exec`,
    {
      AttachStdout: true,
      AttachStderr: true,
      Cmd: command,
      Tty: false,
      ...(target.workdir ? { WorkingDir: target.workdir } : {}),
    },
  );
  const execId = typeof create.Id === "string" && create.Id.length > 0 ? create.Id : null;
  if (!execId) {
    throw new WorkerError("Docker exec create did not return an exec id");
  }
  const output = await dockerRawRequest("POST", `/exec/${encodeURIComponent(execId)}/start`, {
    body: {
      Detach: false,
      Tty: false,
    },
  });
  const inspect = await dockerRequest<{ ExitCode?: unknown }>("GET", `/exec/${encodeURIComponent(execId)}/json`);
  const exitCode = typeof inspect.ExitCode === "number" && Number.isInteger(inspect.ExitCode)
    ? inspect.ExitCode
    : 1;
  const streams = demuxDockerExecOutput(output.body);
  return {
    exitCode,
    stdout: streams.stdout,
    stderr: streams.stderr,
    providerExecId: execId,
  };
}

async function executeWorkerProcessExec(
  command: string[],
  cwd: string,
): Promise<{ exitCode: number; stdout: string; stderr: string; providerExecId: string }> {
  const [executable, ...args] = command;
  if (!executable) {
    throw new WorkerError("runtime exec command is empty");
  }
  const result = await runProcess(executable, args, {
    cwd,
    env: process.env,
  });
  return {
    ...result,
    providerExecId: `worker-process:${Date.now()}`,
  };
}

export function demuxDockerExecOutput(buffer: Buffer): { stdout: string; stderr: string } {
  let offset = 0;
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];
  let frames = 0;
  while (offset + 8 <= buffer.length) {
    const stream = buffer[offset];
    const length = buffer.readUInt32BE(offset + 4);
    const next = offset + 8 + length;
    if (next > buffer.length) {
      break;
    }
    const payload = buffer.subarray(offset + 8, next);
    if (stream === 2) {
      stderr.push(payload);
    } else {
      stdout.push(payload);
    }
    frames += 1;
    offset = next;
  }
  if (frames > 0 && offset === buffer.length) {
    return {
      stdout: Buffer.concat(stdout).toString("utf8"),
      stderr: Buffer.concat(stderr).toString("utf8"),
    };
  }
  return {
    stdout: buffer.toString("utf8"),
    stderr: "",
  };
}

async function runtimeExecContainerTargets(runRootDir: string): Promise<Array<Extract<RuntimeExecCommandTarget, { mode: "docker_container" }>>> {
  const targets: Array<Extract<RuntimeExecCommandTarget, { mode: "docker_container" }>> = [];
  const coreRunIds = await discoverCoreRunIdsFromRunRoot(runRootDir);
  for (const coreRunId of coreRunIds) {
    const trialsDir = join(runRootDir, coreRunId, "trials");
    const trialEntries = await readdir(trialsDir, { withFileTypes: true }).catch((error) => {
      if (isNodeError(error) && error.code === "ENOENT") {
        return [];
      }
      throw error;
    });
    for (const trialEntry of trialEntries) {
      if (!trialEntry.isDirectory()) {
        continue;
      }
      const trialId = trialEntry.name;
      const statePayload = await readJsonObjectIfExists(join(trialsDir, trialId, "runner", "trial_runtime_state.json"));
      const state = isRecord(statePayload?.state) ? statePayload.state : null;
      if (!state) {
        continue;
      }
      const key = objectAtRecord(state, "key");
      const scheduleIdx = numberAtRecord(key, "schedule_idx");
      for (const container of runtimeExecContainersFromState(state)) {
        targets.push({
          mode: "docker_container",
          coreRunId,
          trialId,
          scheduleIdx,
          role: container.role,
          containerId: container.containerId,
          workdir: container.workdir,
          resourceName: [
            runtimeResourceName(trialId),
            runtimeResourceName(container.role),
            shortRuntimeResourceName(container.containerId),
          ].join("."),
        });
      }
    }
  }
  return targets;
}

function runtimeExecContainersFromState(state: Record<string, unknown>): Array<{ role: string; containerId: string; workdir: string | null }> {
  const containers: Array<{ role: string; containerId: string; workdir: string | null }> = [];
  const task = isRecord(state.task_sandbox) ? state.task_sandbox : null;
  const taskContainerId = task ? stringAtRecord(task, "container_id") : null;
  if (taskContainerId) {
    containers.push({
      role: "task",
      containerId: taskContainerId,
      workdir: task ? stringAtRecord(task, "workdir") : null,
    });
  }
  const grading = isRecord(state.grading_sandbox) ? state.grading_sandbox : null;
  const gradingContainerId = grading ? stringAtRecord(grading, "container_id") : null;
  if (gradingContainerId && gradingContainerId !== "host" && !containers.some((container) => container.containerId === gradingContainerId)) {
    containers.push({
      role: "grading",
      containerId: gradingContainerId,
      workdir: grading ? stringAtRecord(grading, "workdir") : null,
    });
  }
  const ephemerals = Array.isArray(state.ephemerals) ? state.ephemerals.filter(isRecord) : [];
  for (const ephemeral of ephemerals) {
    const containerId = stringAtRecord(ephemeral, "container_id");
    if (!containerId || containers.some((container) => container.containerId === containerId)) {
      continue;
    }
    const id = stringAtRecord(ephemeral, "id") ?? "ephemeral";
    containers.push({
      role: `ephemeral-${id}`,
      containerId,
      workdir: null,
    });
  }
  return containers;
}

async function readJsonObjectIfExists(path: string): Promise<JsonObject | null> {
  try {
    const parsed = JSON.parse(await readFile(path, "utf8"));
    return isRecord(parsed) ? parsed as JsonObject : null;
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return null;
    }
    throw error;
  }
}

function preferredRuntimeExecContainer(
  targets: Array<Extract<RuntimeExecCommandTarget, { mode: "docker_container" }>>,
): Extract<RuntimeExecCommandTarget, { mode: "docker_container" }> | null {
  return targets.find((target) => target.role === "task")
    ?? targets.find((target) => target.role === "agent")
    ?? targets[0]
    ?? null;
}

function canonicalRuntimeExecTargetKind(kind: string): string {
  const normalized = kind.trim().toLowerCase().replaceAll(/[^a-z0-9]/g, "");
  switch (normalized) {
    case "run":
    case "runs":
      return "Run";
    case "runnerinstance":
    case "runnerinstances":
    case "runner":
    case "runners":
      return "RunnerInstance";
    case "runnerattempt":
    case "runnerattempts":
    case "attempt":
    case "attempts":
      return "RunnerAttempt";
    case "trial":
    case "trials":
      return "Trial";
    case "scheduleslot":
    case "scheduleslots":
    case "slot":
    case "slots":
      return "ScheduleSlot";
    case "trialcontainer":
    case "trialcontainers":
    case "container":
    case "containers":
      return "TrialContainer";
    default:
      return kind.trim();
  }
}

function parseScheduleSlotResourceName(resourceName: string): { coreRunName: string; scheduleIdx: number } | null {
  const index = resourceName.lastIndexOf(".");
  if (index <= 0 || index === resourceName.length - 1) {
    return null;
  }
  const scheduleIdx = Number.parseInt(resourceName.slice(index + 1), 10);
  if (!Number.isInteger(scheduleIdx) || scheduleIdx < 0) {
    return null;
  }
  return {
    coreRunName: resourceName.slice(0, index),
    scheduleIdx,
  };
}

function runtimeResourceName(value: string): string {
  const normalized = value
    .toLowerCase()
    .replace(/^sha256:/, "sha256-")
    .replace(/[^a-z0-9_.-]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return normalized.slice(0, 96) || "resource";
}

function shortRuntimeResourceName(value: string): string {
  return runtimeResourceName(value).slice(0, 32) || "resource";
}

export async function prePullRunImages(
  config: Pick<WorkerConfig, "capabilities">,
  claim: Pick<RunClaim, "run" | "attempt">,
  pullImage: (imageRef: string) => Promise<void> = dockerPullImage,
): Promise<void> {
  if (!config.capabilities.resources.includes("docker_daemon")
    || !config.capabilities.resources.includes("registry_pull")) {
    return;
  }
  const imageRefs = Array.isArray(claim.run.run_requirements.image_refs)
    ? claim.run.run_requirements.image_refs.filter((item): item is string => typeof item === "string")
    : [];
  for (const imageRef of [...new Set(imageRefs)]) {
    await pullImage(imageRef);
  }
}

export async function prePullRunImagesWithAudit(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  context: TraceContext = newTraceContext({
    component: "worker-image-pull",
    runId: claim.run.run_id,
    attemptId: claim.attempt.attempt_id,
  }),
  pullImage: (imageRef: string) => Promise<void> = dockerPullImage,
): Promise<void> {
  if (!config.capabilities.resources.includes("docker_daemon")
    || !config.capabilities.resources.includes("registry_pull")) {
    return;
  }
  const imageRefs = Array.isArray(claim.run.run_requirements.image_refs)
    ? claim.run.run_requirements.image_refs.filter((item): item is string => typeof item === "string")
    : [];
  for (const imageRef of [...new Set(imageRefs)]) {
    const eventPayload = () => ({
      resource_kind: "ImagePull",
      resource_name: runtimeDeclaredResourceName(imageRef),
      image_ref: imageRef,
      status: "pulled",
      attempt_id: claim.attempt.attempt_id,
      run_id: claim.run.run_id,
      runner_instance_id: requireRunnerInstanceId(config),
      worker_id: config.workerId,
    });
    await appendEvent(config, claim, "worker.runtime.image_pull.pulling", {
      ...eventPayload(),
      status: "pulling",
    });
    try {
      await pullImage(imageRef);
    } catch (error) {
      await appendEvent(config, claim, "worker.runtime.image_pull.failed", {
        ...eventPayload(),
        status: "failed",
        error: errorMessage(error),
      }).catch((eventError) => {
        logError("worker.runtime_image_pull_failure_event_failed", context, { error: errorMessage(eventError) });
      });
      throw error;
    }
    await appendEvent(config, claim, "worker.runtime.image_pull.pulled", eventPayload());
  }
}

export async function validateSidecarRequirementsWithAudit(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  context: TraceContext = newTraceContext({
    component: "worker-sidecar-requirement",
    runId: claim.run.run_id,
    attemptId: claim.attempt.attempt_id,
  }),
): Promise<void> {
  const sidecars = Array.isArray(claim.run.run_requirements.sidecars)
    ? claim.run.run_requirements.sidecars.filter((item): item is string => typeof item === "string" && item.trim().length > 0)
    : [];
  for (const sidecar of [...new Set(sidecars)].sort()) {
    const requiredCapability = `sidecar:${sidecar}`;
    const eventPayload = () => ({
      resource_kind: "SidecarRequirement",
      resource_name: runtimeDeclaredResourceName(sidecar),
      sidecar,
      required_capability: requiredCapability,
      status: "available",
      attempt_id: claim.attempt.attempt_id,
      run_id: claim.run.run_id,
      runner_instance_id: requireRunnerInstanceId(config),
      worker_id: config.workerId,
    });
    await appendEvent(config, claim, "worker.runtime.sidecar_requirement.checking", {
      ...eventPayload(),
      status: "checking",
    });
    if (!config.capabilities.resources.includes(requiredCapability)) {
      const message = `Runner worker does not advertise required ${requiredCapability}`;
      await appendEvent(config, claim, "worker.runtime.sidecar_requirement.failed", {
        ...eventPayload(),
        status: "failed",
        error: message,
      }).catch((eventError) => {
        logError("worker.runtime_sidecar_requirement_failure_event_failed", context, { error: errorMessage(eventError) });
      });
      throw new WorkerError(message);
    }
    await appendEvent(config, claim, "worker.runtime.sidecar_requirement.available", eventPayload());
  }
}

export async function validateAcceleratorRequirementsWithAudit(
  config: WorkerConfig,
  claim: Pick<RunClaim, "run" | "attempt">,
  context: TraceContext = newTraceContext({
    component: "worker-accelerator-requirement",
    runId: claim.run.run_id,
    attemptId: claim.attempt.attempt_id,
  }),
): Promise<void> {
  const accelerators = Array.isArray(claim.run.run_requirements.accelerators)
    ? claim.run.run_requirements.accelerators.filter((item): item is string => typeof item === "string" && item.trim().length > 0)
    : [];
  for (const accelerator of [...new Set(accelerators)].sort()) {
    const requiredCapability = `accelerator:${accelerator}`;
    const eventPayload = () => ({
      resource_kind: "AcceleratorRequirement",
      resource_name: runtimeDeclaredResourceName(accelerator),
      accelerator,
      required_capability: requiredCapability,
      status: "available",
      attempt_id: claim.attempt.attempt_id,
      run_id: claim.run.run_id,
      runner_instance_id: requireRunnerInstanceId(config),
      worker_id: config.workerId,
    });
    await appendEvent(config, claim, "worker.runtime.accelerator_requirement.checking", {
      ...eventPayload(),
      status: "checking",
    });
    if (!config.capabilities.resources.includes(requiredCapability)) {
      const message = `Runner worker does not advertise required ${requiredCapability}`;
      await appendEvent(config, claim, "worker.runtime.accelerator_requirement.failed", {
        ...eventPayload(),
        status: "failed",
        error: message,
      }).catch((eventError) => {
        logError("worker.runtime_accelerator_requirement_failure_event_failed", context, { error: errorMessage(eventError) });
      });
      throw new WorkerError(message);
    }
    await appendEvent(config, claim, "worker.runtime.accelerator_requirement.available", eventPayload());
  }
}

function runtimeNetworkPerimeter(requirements: RunRequirements): RuntimeNetworkPerimeter {
  const raw = requirements.network_perimeter;
  if (!isRecord(raw)) {
    return {
      default: "none",
      task_sandbox: "none",
      agent: "none",
      egress_hosts: [],
    };
  }
  const defaultMode = runtimeNetworkMode(raw.default);
  return {
    default: defaultMode,
    task_sandbox: runtimeNetworkMode(raw.task_sandbox ?? defaultMode),
    agent: runtimeNetworkMode(raw.agent ?? defaultMode),
    egress_hosts: Array.isArray(raw.egress_hosts)
      ? raw.egress_hosts.filter((item): item is string => typeof item === "string")
      : [],
  };
}

function runtimeNetworkMode(value: unknown): RuntimeNetworkMode {
  return value === "allowlist_enforced" ? "allowlist_enforced" : "none";
}

function assertSecretRef(ref: string): void {
  if (typeof ref !== "string" || ref.trim().length === 0) {
    throw new WorkerError("Secret ref must be a non-empty string");
  }
  if (ref.includes("\n") || ref.includes("\r")) {
    throw new WorkerError(`Invalid secret ref '${ref}'`);
  }
}

function runtimeDeclaredResourceName(value: string): string {
  const normalized = value
    .toLowerCase()
    .replace(/^sha256:/, "sha256-")
    .replace(/[^a-z0-9_.-]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return normalized.slice(0, 96) || "resource";
}

function resolvedSecretOutputPath(secretDir: string, resolverPath: string): string {
  if (resolverPath.startsWith("/")) {
    throw new WorkerError("Secret resolver returned an absolute path; expected a path relative to output_dir");
  }
  const outputPath = resolve(secretDir, resolverPath);
  const outputRoot = resolve(secretDir);
  if (outputPath !== outputRoot && !outputPath.startsWith(`${outputRoot}/`)) {
    throw new WorkerError("Secret resolver returned a path outside output_dir");
  }
  return outputPath;
}

function attemptWorkspaceDir(config: WorkerConfig, claim: RunClaim): string {
  return join(config.dataDir, "worker-runs", claim.run.run_id, claim.attempt.attempt_id);
}

async function cleanupClaimWorkspace(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
): Promise<void> {
  await appendEvent(config, claim, "worker.cleanup.starting", {
    workspace_dir: materialized.workspaceDir,
    run_root_dir: materialized.runRootDir,
    retain_attempt_workspace: config.retainAttemptWorkspaces,
  }).catch((error) => {
    logError("worker.cleanup_start_event_failed", workerContext, { error: errorMessage(error) });
  });

  const cleanup = await cleanupAttemptWorkspace(config, materialized);

  await appendEvent(config, claim, "worker.cleanup.completed", {
    workspace_dir: materialized.workspaceDir,
    core_run_ids: cleanup.coreRunIds,
    docker_resources_removed: cleanup.dockerResourcesRemoved,
    workspace_removed: cleanup.workspaceRemoved,
  }).catch((error) => {
    logError("worker.cleanup_completed_event_failed", workerContext, { error: errorMessage(error) });
  });
}

async function cleanupAttemptWorkspace(
  config: WorkerConfig,
  materialized: MaterializedPackage,
): Promise<AttemptCleanupResult> {
  const coreRunIds = await discoverCoreRunIdsFromRunRoot(materialized.runRootDir);
  const dockerResourcesRemoved = await cleanupDockerRuntimeResources(config, coreRunIds);
  let workspaceRemoved = false;
  if (!config.retainAttemptWorkspaces) {
    await rm(materialized.workspaceDir, { recursive: true, force: true });
    workspaceRemoved = true;
  }
  return {
    coreRunIds,
    dockerResourcesRemoved,
    workspaceRemoved,
  };
}

async function cleanupDockerRuntimeResources(
  config: WorkerConfig,
  coreRunIds: string[],
): Promise<DockerCleanupSummary> {
  if (!config.capabilities.resources.includes("docker_daemon")) {
    return { containers: 0, networks: 0, volumes: 0 };
  }
  const summary: DockerCleanupSummary = { containers: 0, networks: 0, volumes: 0 };
  for (const coreRunId of coreRunIds) {
    const labels = [`label=bucephalus.run_id=${coreRunId}`];
    summary.containers += await removeDockerResources("container", labels);
    summary.networks += await removeDockerResources("network", labels);
    summary.volumes += await removeDockerResources("volume", labels);
  }
  return summary;
}

async function cleanupStartupResidue(config: WorkerConfig): Promise<void> {
  const workerRunsDir = join(config.dataDir, "worker-runs");
  await mkdir(workerRunsDir, { recursive: true });
  if (config.capabilities.resources.includes("docker_daemon")) {
    await cleanupAllBucephalusDockerResources();
  }

  const entries = await readdir(workerRunsDir, { withFileTypes: true });
  for (const runEntry of entries) {
    if (!runEntry.isDirectory()) {
      continue;
    }
    const runDir = join(workerRunsDir, runEntry.name);
    const attemptEntries = await readdir(runDir, { withFileTypes: true }).catch((error) => {
      if (isNodeError(error) && error.code === "ENOENT") {
        return [];
      }
      throw error;
    });
    for (const attemptEntry of attemptEntries) {
      if (!attemptEntry.isDirectory()) {
        continue;
      }
      const workspaceDir = join(runDir, attemptEntry.name);
      const runRootDir = join(workspaceDir, "run-root");
      const coreRunIds = await discoverCoreRunIdsFromRunRoot(runRootDir);
      await cleanupDockerRuntimeResources(config, coreRunIds);
      await rm(workspaceDir, { recursive: true, force: true });
    }
    await rm(runDir, { recursive: true, force: true });
  }
}

async function cleanupAllBucephalusDockerResources(): Promise<DockerCleanupSummary> {
  const labels = ["label=bucephalus.run_id"];
  return {
    containers: await removeDockerResources("container", labels),
    networks: await removeDockerResources("network", labels),
    volumes: await removeDockerResources("volume", labels),
  };
}

async function removeDockerResources(
  kind: "container" | "network" | "volume",
  filters: string[],
): Promise<number> {
  const ids = await listDockerResourceIds(kind, filters);
  if (ids.length === 0) {
    return 0;
  }
  for (const id of ids) {
    await removeDockerResource(kind, id);
  }
  return ids.length;
}

async function listDockerResourceIds(
  kind: "container" | "network" | "volume",
  filters: string[],
): Promise<string[]> {
  const query = `filters=${encodeURIComponent(JSON.stringify(dockerLabelFilters(filters)))}`;
  if (kind === "container") {
    const containers = await dockerRequest<Array<{ Id?: unknown }>>("GET", `/containers/json?all=1&${query}`);
    return containers.map((item) => typeof item.Id === "string" ? item.Id : "").filter(Boolean);
  }
  if (kind === "network") {
    const networks = await dockerRequest<Array<{ Id?: unknown }>>("GET", `/networks?${query}`);
    return networks.map((item) => typeof item.Id === "string" ? item.Id : "").filter(Boolean);
  }
  const volumes = await dockerRequest<{ Volumes?: Array<{ Name?: unknown }> }>("GET", `/volumes?${query}`);
  return (volumes.Volumes ?? []).map((item) => typeof item.Name === "string" ? item.Name : "").filter(Boolean);
}

async function removeDockerResource(kind: "container" | "network" | "volume", id: string): Promise<void> {
  const encoded = encodeURIComponent(id);
  if (kind === "container") {
    await dockerRequest("DELETE", `/containers/${encoded}?force=true`);
  } else if (kind === "network") {
    await dockerRequest("DELETE", `/networks/${encoded}`);
  } else {
    await dockerRequest("DELETE", `/volumes/${encoded}`);
  }
}

function dockerLabelFilters(filters: string[]): { label: string[] } {
  return {
    label: filters.map((filter) => filter.startsWith("label=") ? filter.slice("label=".length) : filter),
  };
}

async function dockerPullImage(imageRef: string): Promise<void> {
  const response = await dockerRequestText(
    "POST",
    `/images/create?fromImage=${encodeURIComponent(imageRef)}`,
    {
      headers: await dockerRegistryAuthHeaders(imageRef),
    },
  );
  const errors: string[] = [];
  for (const line of response.body.split(/\r?\n/).map((item) => item.trim()).filter(Boolean)) {
    try {
      const parsed = JSON.parse(line);
      if (isRecord(parsed) && typeof parsed.error === "string" && parsed.error.trim().length > 0) {
        errors.push(parsed.error);
      }
    } catch {
      // Docker streams JSON objects; ignore non-JSON progress fragments defensively.
    }
  }
  if (errors.length > 0) {
    throw new WorkerError(`Docker image pull failed for ${imageRef}: ${tail(errors.join("\n"), 1000)}`);
  }
}

export async function dockerRegistryAuthHeaders(imageRef: string): Promise<Record<string, string>> {
  const registry = registryHostFromImageRef(imageRef);
  const auth = await dockerRegistryAuth(registry);
  if (!auth) {
    return {};
  }
  return {
    "X-Registry-Auth": Buffer.from(JSON.stringify(auth)).toString("base64"),
  };
}

async function dockerRegistryAuth(registry: string): Promise<JsonObject | null> {
  const dockerConfigDir = process.env.DOCKER_CONFIG
    ?? (process.env.HOME ? join(process.env.HOME, ".docker") : null);
  if (!dockerConfigDir) {
    return null;
  }
  try {
    const parsed = JSON.parse(await readFile(join(dockerConfigDir, "config.json"), "utf8"));
    if (!isRecord(parsed) || !isRecord(parsed.auths)) {
      return null;
    }
    const direct = parsed.auths[registry];
    const httpsRegistry = `https://${registry}`;
    const https = parsed.auths[httpsRegistry];
    const entry = isRecord(direct) ? direct : isRecord(https) ? https : null;
    if (!entry) {
      return null;
    }
    const serveraddress = isRecord(direct) ? registry : httpsRegistry;
    const decodedAuth = dockerAuthCredential(entry);
    const username = typeof entry.username === "string" ? entry.username : decodedAuth?.username;
    const password = typeof entry.password === "string" ? entry.password : decodedAuth?.password;
    return {
      username,
      password,
      auth: typeof entry.auth === "string" ? entry.auth : undefined,
      serveraddress,
    };
  } catch (error) {
    if (isNodeError(error) && error.code === "ENOENT") {
      return null;
    }
    throw error;
  }
}

function dockerAuthCredential(entry: JsonObject): { username: string; password: string } | null {
  if (typeof entry.auth !== "string" || entry.auth.trim().length === 0) {
    return null;
  }
  let decoded: string;
  try {
    decoded = Buffer.from(entry.auth, "base64").toString("utf8");
  } catch {
    return null;
  }
  const separator = decoded.indexOf(":");
  if (separator <= 0) {
    return null;
  }
  return {
    username: decoded.slice(0, separator),
    password: decoded.slice(separator + 1),
  };
}

function registryHostFromImageRef(imageRef: string): string {
  const name = imageRef.split("@", 1)[0]?.split(":", 1)[0] ?? imageRef;
  const first = name.split("/", 1)[0] ?? "";
  if (first.includes(".") || first.includes(":") || first === "localhost") {
    return first;
  }
  return "index.docker.io";
}

type DockerHttpMethod = "GET" | "POST" | "DELETE";

async function dockerRequest<T = unknown>(method: DockerHttpMethod, apiPath: string, body?: unknown): Promise<T> {
  const response = await dockerRawRequest(method, apiPath, { body });
  if (response.body.length === 0 || response.body.toString("utf8").trim() === "") {
    return undefined as T;
  }
  return JSON.parse(response.body.toString("utf8")) as T;
}

async function dockerRequestText(
  method: DockerHttpMethod,
  apiPath: string,
  options: { headers?: Record<string, string>; body?: unknown } = {},
): Promise<{ statusCode: number; body: string }> {
  const response = await dockerRawRequest(method, apiPath, options);
  return { statusCode: response.statusCode, body: response.body.toString("utf8") };
}

async function dockerRawRequest(
  method: DockerHttpMethod,
  apiPath: string,
  options: { headers?: Record<string, string>; body?: unknown } = {},
): Promise<{ statusCode: number; body: Buffer }> {
  const path = `/${DOCKER_API_VERSION}${apiPath}`;
  const encodedBody = options.body === undefined ? null : Buffer.from(JSON.stringify(options.body), "utf8");
  const headers = {
    ...(options.headers ?? {}),
    ...(encodedBody
      ? {
        "content-type": "application/json",
        "content-length": String(encodedBody.byteLength),
      }
      : {}),
  };
  const { statusCode, responseBody } = await new Promise<{ statusCode: number; responseBody: Buffer }>((resolve, reject) => {
    const request = httpRequest({
      socketPath: DOCKER_SOCKET_PATH,
      path,
      method,
      headers,
    }, (response) => {
      const chunks: Buffer[] = [];
      response.on("data", (chunk: Buffer) => chunks.push(chunk));
      response.on("end", () => {
        resolve({
          statusCode: response.statusCode ?? 0,
          responseBody: Buffer.concat(chunks),
        });
      });
    });
    request.on("error", reject);
    if (encodedBody) {
      request.write(encodedBody);
    }
    request.end();
  });
  if (statusCode < 200 || statusCode >= 300) {
    throw new WorkerError(`Docker API ${method} ${apiPath} returned ${statusCode}: ${tail(responseBody.toString("utf8"), 1000)}`);
  }
  return { statusCode, body: responseBody };
}

async function validateWorkerHost(config: WorkerConfig): Promise<void> {
  await mkdir(config.dataDir, { recursive: true });
  const resources = await workerResourceSnapshot(config);
  if (resources.data_dir_free_bytes < config.minFreeBytes) {
    throw new WorkerError(
      `runner data dir free bytes below floor: required=${config.minFreeBytes} available=${resources.data_dir_free_bytes}`,
    );
  }
  if (config.capabilities.resources.includes("docker_daemon")) {
    await dockerRequest("GET", "/version");
  }
}

async function runnerMetadata(config: WorkerConfig): Promise<JsonObject> {
  const resources = await workerResourceSnapshot(config).catch((error) => ({
    error: errorMessage(error),
  }));
  return {
    daemon: "bucephalus-cloud-worker",
    release: releaseIdentity(),
    ...(config.provisionRequestId ? { provision_request_id: config.provisionRequestId } : {}),
    ...(config.providerInstanceId ? { provider_instance_id: config.providerInstanceId } : {}),
    ...(config.workerImageRef ? { worker_image_ref: config.workerImageRef } : {}),
    cleanup_policy: {
      mode: "reuse_vm_mandatory_cleanup_poison_on_failure",
      retain_attempt_workspaces: config.retainAttemptWorkspaces,
    },
    resources,
  };
}

async function workerResourceSnapshot(config: WorkerConfig): Promise<JsonObject & { data_dir_free_bytes: number }> {
  await mkdir(config.dataDir, { recursive: true });
  const fsStats = await statfs(config.dataDir);
  const freeBytes = Number(fsStats.bavail) * Number(fsStats.bsize);
  return {
    cpu_count: os.cpus().length,
    total_memory_bytes: os.totalmem(),
    free_memory_bytes: os.freemem(),
    data_dir: config.dataDir,
    data_dir_free_bytes: freeBytes,
    min_free_bytes: config.minFreeBytes,
  };
}

function startLiveEvidencePump(
  config: WorkerConfig,
  claim: RunClaim,
  materialized: MaterializedPackage,
  context: TraceContext,
): EvidencePump | null {
  if (!config.liveEvidence) {
    return null;
  }
  return startEvidencePump(
    {
      postEventRows: async (rows) => {
        await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/runtime/event-rows`, {
          method: "POST",
          authToken: claim.attempt.attempt_token,
          body: {
            runner_instance_id: requireRunnerInstanceId(config),
            rows,
          },
        });
      },
      announceCoreRuns: async (coreRunIds) => {
        await appendEvent(config, claim, "worker.runtime.core_runs_discovered", {
          core_run_ids: coreRunIds,
        });
      },
      onError: (stage, fields) => {
        logError("worker.evidence_pump_error", context, { stage, ...fields });
      },
    },
    {
      runRootDir: materialized.runRootDir,
      intervalMs: config.evidenceIntervalMs,
    },
  );
}

async function appendEvent(
  config: WorkerConfig,
  claim: Pick<RunClaim, "attempt">,
  eventType: string,
  payload: JsonObject,
): Promise<void> {
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/events`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: requireRunnerInstanceId(config),
      event_type: eventType,
      payload,
    },
  });
}

async function complete(config: WorkerConfig, claim: RunClaim): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/complete`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: runnerInstanceId,
    },
  });
}

async function fail(config: WorkerConfig, claim: RunClaim, message: string): Promise<void> {
  const runnerInstanceId = requireRunnerInstanceId(config);
  await cloudFetch(config, `/v1/worker/run-attempts/${claim.attempt.attempt_id}/fail`, {
    method: "POST",
    authToken: claim.attempt.attempt_token,
    body: {
      runner_instance_id: runnerInstanceId,
      message,
    },
  });
}

async function cloudFetch(
  config: WorkerConfig,
  path: string,
  options: { method?: string; body?: unknown; rawBody?: Uint8Array; headers?: Record<string, string>; authToken?: string } = {},
): Promise<unknown> {
  const init: RequestInit = {
    method: options.method ?? "GET",
    headers: {
      ...workerAuthHeaders(config, options.authToken),
      ...(options.headers ?? {}),
    },
  };
  if (options.body !== undefined) {
    init.headers = { ...workerAuthHeaders(config, options.authToken), ...(options.headers ?? {}), "content-type": "application/json" };
    init.body = JSON.stringify(options.body);
  } else if (options.rawBody !== undefined) {
    init.body = options.rawBody.buffer.slice(
      options.rawBody.byteOffset,
      options.rawBody.byteOffset + options.rawBody.byteLength,
    ) as ArrayBuffer;
  }
  init.signal = AbortSignal.timeout(workerApiRequestTimeoutMs(config));
  const response = await fetch(`${config.apiUrl}${path}`, init);
  const text = await response.text();
  const payload = text.trim().length > 0 ? JSON.parse(text) : null;
  if (!response.ok) {
    const code = isRecord(payload) && typeof payload.code === "string" ? payload.code : null;
    const detail = isRecord(payload) && isRecord(payload.detail) ? payload.detail as JsonObject : null;
    throw new CloudApiError(
      response.status,
      code,
      isRecord(payload) && typeof payload.message === "string"
        ? payload.message
        : `Cloud API request failed: ${response.status}`,
      detail,
    );
  }
  return payload;
}

async function cloudFetchBytes(
  config: WorkerConfig,
  path: string,
  options: { authToken?: string; attemptId?: string } = {},
): Promise<Uint8Array> {
  const response = await fetch(`${config.apiUrl}${path}`, {
    headers: {
      ...workerAuthHeaders(config, options.authToken),
      ...(options.attemptId ? { "x-bucephalus-attempt-id": options.attemptId } : {}),
    },
    signal: AbortSignal.timeout(workerApiRequestTimeoutMs(config)),
  });
  if (!response.ok) {
    const text = await response.text();
    let message = `Cloud API request failed: ${response.status}`;
    try {
      const payload = text.trim().length > 0 ? JSON.parse(text) : null;
      if (isRecord(payload) && typeof payload.message === "string") {
        message = payload.message;
      }
    } catch {
      if (text.trim().length > 0) {
        message = text;
      }
    }
    throw new WorkerError(message);
  }
  return new Uint8Array(await response.arrayBuffer());
}

function workerApiRequestTimeoutMs(config: WorkerConfig): number {
  return Number.isFinite(config.apiRequestTimeoutMs) && config.apiRequestTimeoutMs > 0
    ? config.apiRequestTimeoutMs
    : 30_000;
}

export function loadWorkerConfig(env: NodeJS.ProcessEnv = process.env): WorkerConfig {
  const apiUrl = requiredEnv(env.BUCEPHALUS_CLOUD_API_URL, "BUCEPHALUS_CLOUD_API_URL");
  const leaseSeconds = numberEnv(env.BUCEPHALUS_WORKER_LEASE_SECONDS, 30);
  const secretResolverCommand = optionalCommandJson(
    env.BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON,
    "BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON",
  );
  const networkPolicyCommand = optionalCommandJson(
    env.BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON,
    "BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON",
  );
  const portForwardCommand = optionalCommandJson(
    env.BUCEPHALUS_WORKER_PORT_FORWARD_CMD_JSON,
    "BUCEPHALUS_WORKER_PORT_FORWARD_CMD_JSON",
  );
  const execCommand = optionalCommandJson(
    env.BUCEPHALUS_WORKER_EXEC_CMD_JSON,
    "BUCEPHALUS_WORKER_EXEC_CMD_JSON",
  );
  return {
    apiUrl: apiUrl.replace(/\/+$/, ""),
    workerId: env.BUCEPHALUS_WORKER_ID ?? `worker-${randomUUID()}`,
    runnerPoolId: requiredEnv(env.BUCEPHALUS_RUNNER_POOL_ID, "BUCEPHALUS_RUNNER_POOL_ID"),
    runnerInstanceId: null,
    leaseSeconds,
    pollMs: numberEnv(env.BUCEPHALUS_WORKER_POLL_MS, 2000),
    heartbeatMs: numberEnv(env.BUCEPHALUS_WORKER_HEARTBEAT_MS, Math.max(1000, Math.floor((leaseSeconds * 1000) / 3))),
    sweeperMs: numberEnv(env.BUCEPHALUS_WORKER_SWEEPER_MS, 5000),
    dataDir: resolve(env.BUCEPHALUS_CLOUD_DATA_DIR ?? ".data"),
    coreRunnerCommand: env.BUCEPHALUS_CORE_RUNNER_CMD ?? "bucephalus",
    workerToken: requiredEnv(env.BUCEPHALUS_CLOUD_WORKER_TOKEN, "BUCEPHALUS_CLOUD_WORKER_TOKEN"),
    secretResolverCommand,
    networkPolicyCommand,
    portForwardCommand,
    execCommand,
    capabilities: workerCapabilities(env, {
      secretResolverCommand,
      networkPolicyCommand,
      portForwardCommand,
      execCommand,
    }),
    minFreeBytes: numberEnv(
      env.BUCEPHALUS_WORKER_MIN_FREE_BYTES ?? env.BUCEPHALUS_MIN_FREE_BYTES,
      20 * 1024 * 1024 * 1024,
    ),
    retainAttemptWorkspaces: booleanEnv(env.BUCEPHALUS_WORKER_RETAIN_ATTEMPT_WORKSPACES, false),
    provisionRequestId: env.BUCEPHALUS_RUNNER_PROVISION_REQUEST_ID?.trim() || null,
    providerInstanceId: env.BUCEPHALUS_RUNNER_PROVIDER_INSTANCE_ID?.trim() || null,
    workerImageRef: env.BUCEPHALUS_WORKER_IMAGE_REF?.trim() || null,
    liveEvidence: booleanEnv(env.BUCEPHALUS_WORKER_LIVE_EVIDENCE, true),
    evidenceIntervalMs: numberEnv(env.BUCEPHALUS_WORKER_EVIDENCE_INTERVAL_MS, 2000),
    coreCompletionGraceMs: numberEnv(env.BUCEPHALUS_WORKER_CORE_COMPLETION_GRACE_MS, 120_000),
    apiRequestTimeoutMs: numberEnv(env.BUCEPHALUS_WORKER_API_REQUEST_TIMEOUT_MS, 30_000),
  };
}

function workerAuthHeaders(config: WorkerConfig, token = config.workerToken): Record<string, string> {
  return {
    authorization: `Bearer ${token}`,
  };
}

function optionalCommandJson(raw: string | undefined, name: string): string[] | null {
  if (!raw || raw.trim().length === 0) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new WorkerError(`invalid ${name}: ${errorMessage(error)}`);
  }
  if (!Array.isArray(parsed) || parsed.length === 0 || parsed.some((item) => typeof item !== "string" || item.trim().length === 0)) {
    throw new WorkerError(`${name} must be a non-empty JSON string array`);
  }
  return parsed.map((item) => item.trim());
}

async function runJsonCommand(command: string[], input: JsonObject): Promise<unknown> {
  const [executable, ...args] = command;
  if (!executable) {
    throw new WorkerError("command is empty");
  }
  const result = await new Promise<{
    exitCode: number;
    stdout: string;
    stderr: string;
    stdoutExceeded: boolean;
    stderrExceeded: boolean;
  }>((resolvePromise, reject) => {
    const child = spawn(executable, args, {
      stdio: ["pipe", "pipe", "pipe"],
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let stdoutExceeded = false;
    let stderrExceeded = false;
    let killedForOutputLimit = false;
    const killForOutputLimit = () => {
      if (!killedForOutputLimit) {
        killedForOutputLimit = true;
        child.kill("SIGKILL");
      }
    };
    child.stdout.on("data", (chunk: Buffer) => {
      stdoutBytes += chunk.byteLength;
      if (stdoutBytes <= WORKER_JSON_COMMAND_MAX_STDOUT_BYTES) {
        stdout.push(chunk);
        return;
      }
      stdoutExceeded = true;
      killForOutputLimit();
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderrBytes += chunk.byteLength;
      if (stderrBytes <= WORKER_JSON_COMMAND_MAX_STDERR_BYTES) {
        stderr.push(chunk);
        return;
      }
      stderrExceeded = true;
      killForOutputLimit();
    });
    child.on("error", reject);
    child.on("close", (code) => {
      resolvePromise({
        exitCode: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
        stdoutExceeded,
        stderrExceeded,
      });
    });
    child.stdin.end(`${JSON.stringify(input)}\n`);
  });
  if (result.stdoutExceeded) {
    throw new WorkerError(`${executable} command stdout exceeded ${WORKER_JSON_COMMAND_MAX_STDOUT_BYTES} bytes; worker control commands must return compact JSON`);
  }
  if (result.stderrExceeded) {
    throw new WorkerError(`${executable} command stderr exceeded ${WORKER_JSON_COMMAND_MAX_STDERR_BYTES} bytes`);
  }
  if (result.exitCode !== 0) {
    throw new WorkerError(`${executable} exited ${result.exitCode}: ${tail(result.stderr || result.stdout, 1000)}`);
  }
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    throw new WorkerError(`command returned invalid JSON: ${errorMessage(error)}`);
  }
}

function requestShutdown(signal: NodeJS.Signals): void {
  shuttingDown = true;
  const child = activeChild;
  if (!child?.pid) {
    return;
  }
  signalChildProcessGroup(child, signal);
  const killTimer = globalThis.setTimeout(() => {
    if (activeChild === child) {
      signalChildProcessGroup(child, "SIGKILL");
    }
  }, 5000);
  killTimer.unref?.();
}

function signalChildProcessGroup(child: ChildProcess, signal: NodeJS.Signals): void {
  if (!child.pid) {
    return;
  }
  try {
    process.kill(-child.pid, signal);
  } catch {
    child.kill(signal);
  }
}

function requireRunnerInstanceId(config: WorkerConfig): string {
  if (!config.runnerInstanceId) {
    throw new WorkerError("Runner instance is not registered");
  }
  return config.runnerInstanceId;
}

function requiredEnv(value: string | undefined, name: string): string {
  if (!value || value.trim().length === 0) {
    throw new WorkerError(`${name} is required`);
  }
  return value.trim();
}

function workerCapabilities(
  env: NodeJS.ProcessEnv,
  commands: Pick<WorkerConfig, "secretResolverCommand" | "networkPolicyCommand" | "portForwardCommand" | "execCommand">,
): WorkerCapabilities {
  const resources = csvEnv(env.BUCEPHALUS_WORKER_RESOURCES, ["core_runner", "docker_daemon", "registry_pull"]);
  addCommandBackedCapability(
    resources,
    "secret_resolver",
    commands.secretResolverCommand,
    "BUCEPHALUS_WORKER_SECRET_RESOLVER_CMD_JSON",
  );
  addCommandBackedCapability(
    resources,
    "network_perimeter",
    commands.networkPolicyCommand,
    "BUCEPHALUS_WORKER_NETWORK_POLICY_CMD_JSON",
  );
  addCommandBackedCapability(
    resources,
    "runtime_port_forward",
    commands.portForwardCommand,
    "BUCEPHALUS_WORKER_PORT_FORWARD_CMD_JSON",
  );
  addCommandBackedCapability(
    resources,
    "runtime_exec",
    commands.execCommand,
    "BUCEPHALUS_WORKER_EXEC_CMD_JSON",
  );
  return {
    executors: csvEnv(env.BUCEPHALUS_WORKER_EXECUTORS, ["runner-docker"]),
    resources,
    arch: normalizeArch(env.BUCEPHALUS_WORKER_ARCH ?? os.arch()),
    cpu_count: numberEnv(env.BUCEPHALUS_WORKER_CPU_COUNT, os.cpus().length),
    memory_mb: numberEnv(env.BUCEPHALUS_WORKER_MEMORY_MB, Math.floor(os.totalmem() / 1024 / 1024)),
    disk_mb: numberEnv(env.BUCEPHALUS_WORKER_DISK_MB, Math.floor(numberEnv(env.BUCEPHALUS_WORKER_MIN_FREE_BYTES ?? env.BUCEPHALUS_MIN_FREE_BYTES, 20 * 1024 * 1024 * 1024) / 1024 / 1024)),
    isolation: csvEnv(env.BUCEPHALUS_WORKER_ISOLATION, ["reusable_vm"]),
  };
}

function addCommandBackedCapability(
  resources: string[],
  resource: string,
  command: string[] | null,
  commandEnvName: string,
): void {
  const declared = resources.includes(resource);
  if (command) {
    if (!declared) {
      resources.push(resource);
    }
    return;
  }
  if (declared) {
    throw new WorkerError(`${resource} requires ${commandEnvName} to be configured`);
  }
}

function normalizeArch(value: string): string {
  switch (value.trim().toLowerCase()) {
    case "x64":
    case "amd64":
    case "x86_64":
      return "x86_64";
    case "arm64":
    case "aarch64":
      return "arm64";
    default:
      return value.trim();
  }
}

function csvEnv(value: string | undefined, fallback: string[]): string[] {
  if (!value) {
    return fallback;
  }
  return value
    .split(",")
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

function tail(value: string, maxBytes: number): string {
  const buffer = Buffer.from(value, "utf8");
  if (buffer.byteLength <= maxBytes) {
    return value;
  }
  return buffer.subarray(buffer.byteLength - maxBytes).toString("utf8");
}

function numberEnv(value: string | undefined, fallback: number): number {
  if (!value) {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function booleanEnv(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) {
    return fallback;
  }
  const normalized = value.trim().toLowerCase();
  if (["1", "true", "yes", "on"].includes(normalized)) {
    return true;
  }
  if (["0", "false", "no", "off"].includes(normalized)) {
    return false;
  }
  return fallback;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function objectAtRecord(value: Record<string, unknown>, key: string): JsonObject {
  const child = value[key];
  return isRecord(child) ? child as JsonObject : {};
}

function stringAtRecord(value: Record<string, unknown>, key: string): string | null {
  const child = value[key];
  return typeof child === "string" && child.length > 0 ? child : null;
}

function numberAtRecord(value: Record<string, unknown>, key: string): number | null {
  const child = value[key];
  if (typeof child === "number" && Number.isFinite(child)) {
    return child;
  }
  if (typeof child === "string" && child.trim().length > 0) {
    const parsed = Number.parseInt(child, 10);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function stringArrayAtRecord(value: Record<string, unknown>, key: string): string[] {
  const child = value[key];
  if (!Array.isArray(child)) {
    return [];
  }
  return child.filter((item): item is string => typeof item === "string" && item.length > 0);
}

function isNodeError(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && "code" in error;
}

interface EmptyClaim {
  claimed: false;
}

interface RunClaim {
  claimed: true;
  run: {
    run_id: string;
    package_digest: string;
    env: Record<string, string>;
    secret_refs: Record<string, string>;
    runtime_options: JsonObject;
    run_requirements: RunRequirements;
  };
  attempt: {
    attempt_id: string;
    attempt_token: string;
  };
}

interface RunnerInstance {
  runner_instance_id: string;
}

interface RunRequirements {
  executor: string;
  requires: string[];
  image_refs: string[];
  secret_ids?: string[];
  network_perimeter?: JsonObject;
  sidecars?: string[];
  accelerators?: string[];
  arch?: string;
  cpu_count?: number;
  memory_mb?: number;
  disk_mb?: number;
  isolation?: string;
  timeout_ms?: number | null;
  max_parallel_trials?: number;
}

type RuntimeNetworkMode = "none" | "allowlist_enforced";

interface RuntimeNetworkPerimeter extends JsonObject {
  default: RuntimeNetworkMode;
  task_sandbox: RuntimeNetworkMode;
  agent: RuntimeNetworkMode;
  egress_hosts: string[];
}

interface RuntimePortForwardRequest extends JsonObject {
  access_request_id: string;
  run_id: string;
  kind: "port_forward";
  status: string;
  resource_kind: string;
  resource_name: string;
  protocol: string;
  target_port: number;
  local_port: number | null;
  runner_instance_id: string | null;
  attempt_id: string | null;
  requester: string | null;
  reason: string | null;
  connection: JsonObject;
  error_message: string | null;
  created_at: string;
  updated_at: string;
}

interface RuntimeExecRequest extends JsonObject {
  access_request_id: string;
  run_id: string;
  kind: "exec";
  status: string;
  resource_kind: string;
  resource_name: string;
  protocol: string;
  target_port: null;
  local_port: null;
  command: string[];
  runner_instance_id: string | null;
  attempt_id: string | null;
  requester: string | null;
  reason: string | null;
  connection: JsonObject;
  error_message: string | null;
  created_at: string;
  updated_at: string;
}

interface WorkerCapabilities {
  executors: string[];
  resources: string[];
  arch?: string;
  cpu_count?: number;
  memory_mb?: number;
  disk_mb?: number;
  isolation?: string[];
}

interface MaterializedPackage {
  workspaceDir: string;
  packageArchivePath: string;
  extractedDir: string;
  runRootDir: string;
  manifestJson: JsonObject;
  secretFiles: Record<string, string>;
  secretEnvFile: string | null;
}

interface DockerCleanupSummary {
  containers: number;
  networks: number;
  volumes: number;
}

interface AttemptCleanupResult {
  coreRunIds: string[];
  dockerResourcesRemoved: DockerCleanupSummary;
  workspaceRemoved: boolean;
}

interface RuntimeSnapshotPayload extends JsonObject {
  core_run_id: string;
  run_dir_name: string;
  runtime_values: Record<string, JsonObject>;
  trial_summaries: RuntimeTrialSummaryPayload[];
  evidence_records: JsonObject[];
  omitted: string[];
  snapshot_budget: JsonObject;
}

interface RuntimeTrialSummaryPayload extends JsonObject {
  trial_id: string;
  summary: JsonObject;
  contract_trace?: JsonObject;
  trial_events?: JsonObject[];
}

interface RuntimeArtifactUpload {
  coreRunId: string;
  trialId: string;
  scheduleIdx: number;
  attempt: number;
  role: string;
  relativePath: string;
  mediaType: string;
  absolutePath: string;
  byteSize: number;
}

function stringField(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function numberField(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    return Math.trunc(value);
  }
  if (typeof value === "string" && /^[0-9]+$/.test(value)) {
    const parsed = Number.parseInt(value, 10);
    return Number.isSafeInteger(parsed) ? parsed : null;
  }
  return null;
}

function stringAt(root: JsonObject, pointer: string): string | null {
  let current: unknown = root;
  for (const rawSegment of pointer.split("/").slice(1)) {
    const segment = rawSegment.replaceAll("~1", "/").replaceAll("~0", "~");
    if (!isRecord(current)) {
      return null;
    }
    current = current[segment];
  }
  return typeof current === "string" ? current : null;
}

if (import.meta.main) {
  main().catch((error) => {
    logError("worker.fatal", workerContext, { error: errorMessage(error) });
    process.exit(1);
  });
}

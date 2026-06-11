import { randomBytes, randomUUID } from "node:crypto";

type JsonValue = string | number | boolean | null | undefined | JsonObject | JsonValue[];

export interface JsonObject {
  [key: string]: JsonValue;
}

export interface TraceContext {
  component: string;
  requestId: string;
  traceId: string;
  spanId: string;
  parentSpanId?: string;
  runId?: string;
  attemptId?: string;
}

export interface LogFields {
  [key: string]: JsonValue;
}

export interface TraceMetadata {
  request_id: string;
  trace_id: string;
  span_id: string;
  parent_span_id?: string;
  run_id?: string;
  attempt_id?: string;
}

const HEADER_REQUEST_ID = "x-request-id";
const HEADER_TRACE_ID = "x-bucephalus-trace-id";
const HEADER_REQUEST_ID_ALIAS = "x-bucephalus-request-id";
const HEADER_SPAN_ID = "x-bucephalus-span-id";
const HEADER_PARENT_SPAN_ID = "x-bucephalus-parent-span-id";

// Cloud Logging parses these JSON keys specially when a structured line is
// written to stdout/stderr. Matching them turns our logs into first-class
// entries (severity, timestamp) and correlates them into traces.
const GCP_TRACE_FIELD = "logging.googleapis.com/trace";
const GCP_SPAN_FIELD = "logging.googleapis.com/spanId";

const SEVERITY: Record<"info" | "warn" | "error", string> = {
  info: "INFO",
  warn: "WARNING",
  error: "ERROR",
};

const METADATA_PROJECT_URL =
  "http://metadata.google.internal/computeMetadata/v1/project/project-id";

let resolvedProjectId: string | undefined = sanitizeId(
  process.env.BUCEPHALUS_GCP_PROJECT_ID ??
    process.env.GOOGLE_CLOUD_PROJECT ??
    process.env.GCP_PROJECT,
);

export function setLoggingProjectId(projectId: string | undefined): void {
  const cleaned = sanitizeId(projectId);
  if (cleaned) {
    resolvedProjectId = cleaned;
  }
}

/**
 * Resolve the GCP project id so logs carry the trace-correlation field. The
 * env vars are set on Cloud Run; the metadata server is the fallback for GCE
 * worker VMs. Best-effort: logging still works without it, just without
 * cross-service trace grouping.
 */
export async function initTelemetry(timeoutMs = 750): Promise<void> {
  if (resolvedProjectId) {
    return;
  }
  try {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      // The GCE/Cloud Run metadata service is link-local and HTTP-only by
      // design; there is no HTTPS variant of this endpoint.
      // nosemgrep: typescript.react.security.react-insecure-request.react-insecure-request
      const response = await fetch(METADATA_PROJECT_URL, {
        headers: { "metadata-flavor": "Google" },
        signal: controller.signal,
      });
      if (response.ok) {
        setLoggingProjectId((await response.text()).trim());
      }
    } finally {
      clearTimeout(timer);
    }
  } catch {
    // No metadata server (local/dev) — leave trace correlation off.
  }
}

export function newTraceContext(input: {
  component: string;
  requestId?: string | undefined;
  traceId?: string | undefined;
  spanId?: string | undefined;
  parentSpanId?: string | undefined;
  runId?: string | undefined;
  attemptId?: string | undefined;
}): TraceContext {
  return {
    component: input.component,
    requestId: sanitizeId(input.requestId) ?? randomUUID(),
    traceId: sanitizeId(input.traceId) ?? randomUUID(),
    spanId: sanitizeSpanId(input.spanId),
    ...(input.parentSpanId ? { parentSpanId: sanitizeSpanId(input.parentSpanId) } : {}),
    ...(input.runId ? { runId: input.runId } : {}),
    ...(input.attemptId ? { attemptId: input.attemptId } : {}),
  };
}

export function childTraceContext(parent: TraceContext, input: {
  component?: string | undefined;
  runId?: string | undefined;
  attemptId?: string | undefined;
  parentSpanId?: string | undefined;
} = {}): TraceContext {
  return newTraceContext({
    component: input.component ?? parent.component,
    requestId: parent.requestId,
    traceId: parent.traceId,
    parentSpanId: input.parentSpanId ?? parent.spanId,
    ...(input.runId !== undefined ? { runId: input.runId } : parent.runId !== undefined ? { runId: parent.runId } : {}),
    ...(input.attemptId !== undefined
      ? { attemptId: input.attemptId }
      : parent.attemptId !== undefined ? { attemptId: parent.attemptId } : {}),
  });
}

export function parseTraceContextFromHeaders(
  headers: Headers,
  component: string,
): TraceContext {
  return newTraceContext({
    component,
    requestId: firstHeader(headers, [HEADER_REQUEST_ID_ALIAS, HEADER_REQUEST_ID])?.trim(),
    traceId: headers.get(HEADER_TRACE_ID)?.trim(),
    spanId: headers.get(HEADER_SPAN_ID)?.trim(),
    parentSpanId: headers.get(HEADER_PARENT_SPAN_ID)?.trim(),
  });
}

export function attachTraceHeaders(headers: HeadersInit, context: TraceContext): Headers {
  const out = new Headers(headers);
  out.set(HEADER_REQUEST_ID_ALIAS, context.requestId);
  out.set(HEADER_REQUEST_ID, context.requestId);
  out.set(HEADER_TRACE_ID, context.traceId);
  out.set(HEADER_SPAN_ID, context.spanId);
  if (context.parentSpanId) {
    out.set(HEADER_PARENT_SPAN_ID, context.parentSpanId);
  }
  return out;
}

export function headersForTrace(context: TraceContext, base: HeadersInit = {}): Record<string, string> {
  const out = Object.fromEntries(attachTraceHeaders(base, context).entries()) as Record<string, string>;
  return out;
}

export function traceMetadata(context: TraceContext): TraceMetadata {
  return {
    request_id: context.requestId,
    trace_id: context.traceId,
    span_id: context.spanId,
    ...(context.parentSpanId ? { parent_span_id: context.parentSpanId } : {}),
    ...(context.runId ? { run_id: context.runId } : {}),
    ...(context.attemptId ? { attempt_id: context.attemptId } : {}),
  };
}

export function logInfo(message: string, context: TraceContext, fields: LogFields = {}): void {
  emitLog("info", message, context, fields);
}

export function logWarn(message: string, context: TraceContext, fields: LogFields = {}): void {
  emitLog("warn", message, context, fields);
}

export function logError(message: string, context: TraceContext, fields: LogFields = {}): void {
  emitLog("error", message, context, fields);
}

function emitLog(level: "info" | "warn" | "error", message: string, context: TraceContext, fields: LogFields): void {
  const payload: JsonObject = {
    time: new Date().toISOString(),
    severity: SEVERITY[level],
    component: context.component,
    message,
    ...traceMetadata(context),
    ...fields,
  };
  if (resolvedProjectId) {
    payload[GCP_TRACE_FIELD] = `projects/${resolvedProjectId}/traces/${context.traceId}`;
    payload[GCP_SPAN_FIELD] = context.spanId;
  }
  const line = JSON.stringify(payload);
  if (level === "error") {
    console.error(line);
    return;
  }
  if (level === "warn") {
    console.warn(line);
    return;
  }
  console.log(line);
}

function firstHeader(headers: Headers, names: string[]): string | undefined {
  for (const name of names) {
    const value = headers.get(name);
    if (value && value.trim().length > 0) {
      return value.trim();
    }
  }
  return undefined;
}

function sanitizeId(value?: string): string | undefined {
  if (!value) {
    return undefined;
  }
  const trimmed = value.trim();
  if (trimmed.length === 0) {
    return undefined;
  }
  if (trimmed.length > 2048) {
    return trimmed.slice(0, 2048);
  }
  return trimmed;
}

function sanitizeSpanId(value?: string): string {
  if (!value) {
    return randomBytes(8).toString("hex");
  }
  const trimmed = value.trim();
  if (trimmed.length === 0) {
    return randomBytes(8).toString("hex");
  }
  if (trimmed.length > 128) {
    return trimmed.slice(0, 128);
  }
  return trimmed;
}

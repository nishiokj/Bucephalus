#!/usr/bin/env bun
import { chmodSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const NOVA_HOST = "127.0.0.1";
const NOVA_PORT = 9555;
const BRIDGE_COMMAND_CHANNEL = "bridge_command";
const DEFAULT_PG_DATA_API_URL = "http://pg-data-api:9757";


function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function writeJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

function trialInputPath() {
  const path = process.env.BUCEPHALUS_TRIAL_INPUT_PATH;
  if (!path) throw new Error("BUCEPHALUS_TRIAL_INPUT_PATH not set");
  return path;
}

function resultPath() {
  return process.env.BUCEPHALUS_RESULT_PATH ?? "/bucephalus/out/result.json";
}

function providerEnvName(provider) {
  return `${provider.toUpperCase().replaceAll("-", "_")}_API_KEY`;
}


function positiveIntegerEnv(name, fallback) {
  const raw = process.env[name];
  if (raw === undefined || raw === "") return fallback;
  const value = Number(raw);
  if (!Number.isInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive integer number of milliseconds`);
  }
  return value;
}

function pgDataApiUrl() {
  return process.env.PG_DATA_API_URL ?? DEFAULT_PG_DATA_API_URL;
}

function materializeWorkspace({ caseId, inputs }) {
  const dest = join("/bucephalus/workspace", caseId);
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(join(dest, "output"), { recursive: true });
  mkdirSync(join(dest, "events"), { recursive: true });
  mkdirSync(join(dest, "tools"), { recursive: true });

  cpSync("/opt/peter-gregory/agent/scan_schema.json", join(dest, "output", "scan_schema.json"));
  cpSync("/opt/peter-gregory/agent/pg_query.py", join(dest, "tools", "pg_query"));
  chmodSync(join(dest, "tools", "pg_query"), 0o755);
  writeFileSync(join(dest, "events", "event-stream.md"), String(inputs.event_stream ?? ""));
  writeFileSync(
    join(dest, "README.md"),
    [
      "# Peter Gregory v2 Latent Exposure Scan",
      "",
      "The external event stream for this run is in `events/event-stream.md` and was also provided in the prompt.",
      "The company data is not present as flat files in the workspace.",
      "Use `tools/pg_query` to search and traverse the read-only company data API.",
      "Return one JSON object matching `output/scan_schema.json`.",
      "",
    ].join("\n"),
  );
  return dest;
}

async function waitForPgDataApi(caseId, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(pgDataApiUrl(), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ case_id: caseId, command: "overview" }),
      });
      if (response.ok) return;
    } catch {
      await Bun.sleep(100);
    }
  }
  throw new Error("Peter Gregory data API did not become reachable");
}




function startNovaDaemon({ caseId }) {
  const logDir = "/bucephalus/out/nova";
  mkdirSync(logDir, { recursive: true });
  return Bun.spawn([
    "bun",
    "/opt/nova/packages/infra/harness-daemon/dist/index.js",
    "--host",
    NOVA_HOST,
    "--port",
    String(NOVA_PORT),
    "--idle-timeout",
    "0",
  ], {
    stdout: Bun.file(join(logDir, "stdout.log")),
    stderr: Bun.file(join(logDir, "stderr.log")),
    env: {
      ...process.env,
      PG_CASE_ID: caseId,
      PG_DATA_API_URL: pgDataApiUrl(),
    },
  });
}

class NovaBus {
  constructor() {
    this.ws = null;
    this.pending = new Map();
    this.waiters = [];
    this.sessionKey = null;
  }

  async connect() {
    for (let attempt = 0; attempt < 80; attempt += 1) {
      try {
        await this.open();
        return;
      } catch {
        await Bun.sleep(250);
      }
    }
    throw new Error("Nova daemon did not become reachable");
  }

  open() {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(`ws://${NOVA_HOST}:${NOVA_PORT}`);
      const timer = setTimeout(() => {
        ws.close();
        reject(new Error("Nova websocket timeout"));
      }, 1_000);
      ws.addEventListener("open", () => {
        clearTimeout(timer);
        this.ws = ws;
        ws.addEventListener("message", (event) => this.onMessage(String(event.data)));
        ws.addEventListener("error", () => this.rejectAll(new Error("Nova websocket error")));
        ws.addEventListener("close", () => this.rejectAll(new Error("Nova websocket closed")));
        resolve();
      });
      ws.addEventListener("error", () => {
        clearTimeout(timer);
        reject(new Error("Nova websocket connection failed"));
      });
    });
  }

  close() {
    this.ws?.close();
  }

  publish(channel, payload) {
    this.ws.send(JSON.stringify({ type: "publish", channel, payload }));
  }

  rpc(method, params = {}, timeoutMs = 120_000) {
    const id = `rpc_${Date.now()}_${Math.random().toString(36).slice(2)}`;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`RPC timeout for ${method}`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timeout });
      this.publish(BRIDGE_COMMAND_CHANNEL, { rpc: 1, id, method, params });
    });
  }

  command(type, data = {}) {
    this.publish(BRIDGE_COMMAND_CHANNEL, { type, data });
  }

  waitForEvent(type, requestId = null, timeoutMs = 600_000) {
    return new Promise((resolve, reject) => {
      const waiter = { type, requestId, resolve, reject };
      waiter.timeout = setTimeout(() => {
        this.waiters = this.waiters.filter((item) => item !== waiter);
        reject(new Error(`Timed out waiting for Nova event ${type}`));
      }, timeoutMs);
      this.waiters.push(waiter);
    });
  }

  onMessage(raw) {
    const message = JSON.parse(raw);
    const payload = message.payload ?? message.message;
    if (!payload) return;
    if (payload.rpc === 1 && payload.id) {
      const pending = this.pending.get(payload.id);
      if (pending) {
        clearTimeout(pending.timeout);
        this.pending.delete(payload.id);
        if (payload.error) pending.reject(new Error(payload.error.message ?? "Nova RPC failed"));
        else pending.resolve(payload.result);
      }
      return;
    }
    if (payload.type === "permission_request") {
      const requestId = payload.data?.request_id;
      if (requestId) {
        this.command("permission_response", { request_id: requestId, decision: "allow" });
      }
    }
    if (payload.type === "ready") {
      this.sessionKey = payload.data?.session_key ?? this.sessionKey;
    }
    for (const waiter of [...this.waiters]) {
      const data = payload.data ?? {};
      const eventRequestId = data.client_request_id ?? data.request_id ?? null;
      if (payload.type === waiter.type && (!waiter.requestId || waiter.requestId === eventRequestId)) {
        clearTimeout(waiter.timeout);
        this.waiters = this.waiters.filter((item) => item !== waiter);
        waiter.resolve(data);
      }
    }
  }

  rejectAll(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.pending.clear();
  }
}

function scanPrompt({ caseId, provider, model, workspace, eventStream, companyProfile }) {
  return `You are evaluating Peter Gregory latent exposure case ${caseId}.

Work in this directory: ${workspace}

Company baseline:
${companyProfile}

External event stream:
${eventStream}

Task:
Triage the event stream for material latent exposure. Most events may be noise. There may be zero exposures. Do not force a connection. Treat "no alert" as the correct answer when the company baseline and company data API do not support a concrete causal path.

Use the company baseline to decide which events are worth deeper inspection. The detailed company data is exposed through a constrained read-only API, not as flat files. Use `tools/pg_query overview`, `tools/pg_query search <query>`, `tools/pg_query get_entity <id>`, `tools/pg_query neighbors <id>`, and `tools/pg_query trace_exposure <id...>` to traverse the ontology and operating data. Return exactly one JSON object matching output/scan_schema.json. Do not include Markdown.

Traversal standard:
- Start broad with the company baseline and event stream.
- Search for plausible materials, commodities, products, customers, suppliers, regulations, brands, or aliases.
- Follow neighbors before claiming an exposure.
- Use trace_exposure to verify downstream products, orders, revenue, and inventory.

Evidence standard:
- Raise an alert only when you can identify a specific causal_event_id and a supported latent_edge through the data API to affected orders or other affected entities.
- Include latent_edge as the causal path you found, and make the nodes/relationship specific enough to audit.
- Include supporting_records for the record collections or entities that substantiate the path.
- If an event is plausible but unsupported, out of scope, or unrelated to the company baseline and API results, put it in no_alert_paths_considered with the reason.

The JSON object must include:
- alerts: only exposures found, including causal_event_id, latent_edge, affected_orders, revenue_at_risk, exposure_kind, business_action, and supporting_records.
- no_alert_paths_considered: events you considered and dismissed, with reasons.

Variant:
  provider: ${provider}
  model: ${model}`;
}

function parseJsonObject(text) {
  const trimmed = String(text ?? "").trim();
  try {
    return JSON.parse(trimmed);
  } catch {
    const start = trimmed.indexOf("{");
    const end = trimmed.lastIndexOf("}");
    if (start >= 0 && end > start) {
      return JSON.parse(trimmed.slice(start, end + 1));
    }
    throw new Error("Nova response did not contain a JSON object");
  }
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function stringArray(value) {
  return Array.isArray(value) ? value.filter((item) => typeof item === "string") : [];
}

function uniqueCount(items) {
  return new Set(items.filter((item) => typeof item === "string" && item.length > 0)).size;
}

function latentNodes(edge) {
  if (typeof edge !== "string") return [];
  return edge.split(/\s*(?:→|->|=>)\s*/u).map((node) => node.trim()).filter((node) => node.length > 0);
}

function validateScan(scan) {
  const errors = [];
  if (!isPlainObject(scan)) {
    return ["scan is not an object"];
  }
  if (!Array.isArray(scan.alerts)) errors.push("alerts must be an array");
  if (!Array.isArray(scan.no_alert_paths_considered)) errors.push("no_alert_paths_considered must be an array");
  for (const [idx, alert] of (Array.isArray(scan.alerts) ? scan.alerts : []).entries()) {
    if (!isPlainObject(alert)) {
      errors.push(`alerts[${idx}] must be an object`);
      continue;
    }
    for (const field of ["causal_event_id", "latent_edge", "exposure_kind", "business_action"]) {
      if (typeof alert[field] !== "string" || alert[field].trim().length === 0) {
        errors.push(`alerts[${idx}].${field} must be a non-empty string`);
      }
    }
    if (!Array.isArray(alert.affected_orders)) errors.push(`alerts[${idx}].affected_orders must be an array`);
    if (typeof alert.revenue_at_risk !== "number" || !Number.isFinite(alert.revenue_at_risk)) {
      errors.push(`alerts[${idx}].revenue_at_risk must be a finite number`);
    }
    if (!Array.isArray(alert.supporting_records)) errors.push(`alerts[${idx}].supporting_records must be an array`);
  }
  for (const [idx, dismissal] of (Array.isArray(scan.no_alert_paths_considered) ? scan.no_alert_paths_considered : []).entries()) {
    if (!isPlainObject(dismissal)) {
      errors.push(`no_alert_paths_considered[${idx}] must be an object`);
      continue;
    }
    if (typeof dismissal.event_id !== "string" || dismissal.event_id.trim().length === 0) {
      errors.push(`no_alert_paths_considered[${idx}].event_id must be a non-empty string`);
    }
    if (typeof dismissal.reason_dismissed !== "string" || dismissal.reason_dismissed.trim().length === 0) {
      errors.push(`no_alert_paths_considered[${idx}].reason_dismissed must be a non-empty string`);
    }
  }
  return errors;
}

function buildObservationMetrics({ submitted, eventStream, responseContent, elapsedSeconds, parseError }) {
  const alerts = isPlainObject(submitted) && Array.isArray(submitted.alerts) ? submitted.alerts : [];
  const dismissals = isPlainObject(submitted) && Array.isArray(submitted.no_alert_paths_considered)
    ? submitted.no_alert_paths_considered
    : [];
  const validationErrors = parseError ? [parseError] : validateScan(submitted);
  const alertObservations = alerts.map((alert) => {
    const nodes = latentNodes(alert?.latent_edge);
    return {
      causal_event_id: alert?.causal_event_id ?? null,
      latent_edge: alert?.latent_edge ?? null,
      latent_edge_nodes: nodes,
      latent_node_count: nodes.length,
      affected_orders: stringArray(alert?.affected_orders),
      affected_products: stringArray(alert?.affected_products),
      supporting_records: stringArray(alert?.supporting_records),
      revenue_at_risk: typeof alert?.revenue_at_risk === "number" ? alert.revenue_at_risk : null,
      exposure_kind: alert?.exposure_kind ?? null,
    };
  });
  const allOrders = alertObservations.flatMap((alert) => alert.affected_orders);
  const allProducts = alertObservations.flatMap((alert) => alert.affected_products);
  const allRecords = alertObservations.flatMap((alert) => alert.supporting_records);
  const nodeCounts = alertObservations.map((alert) => alert.latent_node_count);
  const revenueTotal = alertObservations.reduce((sum, alert) => sum + (alert.revenue_at_risk ?? 0), 0);
  const metrics = {
    submitted_present: isPlainObject(submitted) ? 1 : 0,
    schema_valid: validationErrors.length === 0 ? 1 : 0,
    json_parse_error: parseError ? 1 : 0,
    event_count: (String(eventStream).match(/^##\s+/gm) ?? []).length,
    alert_count: alerts.length,
    dismissal_count: dismissals.length,
    latent_edge_count: alertObservations.filter((alert) => alert.latent_edge_nodes.length > 0).length,
    latent_node_count_total: nodeCounts.reduce((sum, count) => sum + count, 0),
    latent_node_count_max: nodeCounts.length ? Math.max(...nodeCounts) : 0,
    affected_order_count: uniqueCount(allOrders),
    affected_product_count: uniqueCount(allProducts),
    supporting_record_count: uniqueCount(allRecords),
    revenue_at_risk_total: revenueTotal,
    response_text_bytes: Buffer.byteLength(String(responseContent ?? ""), "utf8"),
    elapsed_seconds: elapsedSeconds,
  };
  return {
    metrics,
    observations: {
      validation_errors: validationErrors,
      alerts: alertObservations,
      dismissals,
    },
  };
}

async function main() {
  const started = performance.now();
  const trial = readJson(trialInputPath());
  const inputs = trial.case?.inputs ?? {};
  const caseId = inputs.case_id ?? trial.case?.id ?? trial.ids?.case_id;
  if (!caseId) throw new Error("trial case missing case id");
  const eventStream = String(inputs.event_stream ?? "");
  if (!eventStream) throw new Error("trial case missing inputs.event_stream");
  const companyProfile = String(inputs.company_profile ?? "");
  if (!companyProfile) throw new Error("trial case missing inputs.company_profile");

  const config = trial.variant?.config ?? {};
  const provider = process.env.PROVIDER ?? config.provider ?? "codex";
  const model = process.env.MODEL ?? config.model ?? "gpt-5.5";
  const apiKey = process.env[providerEnvName(provider)];
  if (!apiKey && provider !== "codex") {
    throw new Error(`Missing ${providerEnvName(provider)} for provider ${provider}`);
  }

  const workspace = materializeWorkspace({ caseId, inputs });
  await waitForPgDataApi(caseId);
  const daemon = startNovaDaemon({ caseId });
  const nova = new NovaBus();
  try {
    await nova.connect();
    nova.command("init", { working_dir: workspace });
    await nova.waitForEvent("ready", null, 120_000);
    await nova.rpc("dangerous_mode.set", { enabled: true });
    const modelParams = {
      agent_type: "standard",
      provider,
      model,
    };
    if (apiKey) modelParams.api_key = apiKey;
    await nova.rpc("model.set", modelParams);

    const requestId = `pg_${Date.now()}_${Math.random().toString(36).slice(2)}`;
    const responseTimeoutMs = positiveIntegerEnv("RESPONSE_TIMEOUT_MS", 540_000);
    const responseWaiter = nova.waitForEvent("response", requestId, responseTimeoutMs);
    nova.command("send_text", {
      text: scanPrompt({ caseId, provider, model, workspace, eventStream, companyProfile }),
      client_request_id: requestId,
      working_dir: workspace,
    });
    const response = await responseWaiter;
    const content = response.content ?? response.text ?? response.response ?? "";
    let submitted = null;
    let parseError = null;
    try {
      submitted = parseJsonObject(content);
    } catch (error) {
      parseError = error instanceof Error ? error.message : String(error);
    }
    const elapsedSeconds = Number(((performance.now() - started) / 1000).toFixed(3));
    const { metrics, observations } = buildObservationMetrics({
      submitted,
      eventStream,
      responseContent: content,
      elapsedSeconds,
      parseError,
    });
    writeJson(resultPath(), {
      provider,
      model,
      case_id: caseId,
      feed_type: inputs.feed_type ?? null,
      event_source: inputs.event_source ?? null,
      event_stream: eventStream,
      company_profile: companyProfile,
      workspace,
      submitted,
      observations,
      metrics,
      raw_response_content: content,
      nova_response: response,
    });
  } finally {
    nova.close();
    daemon.kill();
  }
}

main().catch((error) => {
  const message = error instanceof Error ? error.message : String(error);
  writeJson(resultPath(), {
    error: message,
    metrics: {
      submitted_present: 0,
      schema_valid: 0,
      json_parse_error: 0,
      event_count: 0,
      alert_count: 0,
      dismissal_count: 0,
      latent_edge_count: 0,
      latent_node_count_total: 0,
      latent_node_count_max: 0,
      affected_order_count: 0,
      affected_product_count: 0,
      supporting_record_count: 0,
      revenue_at_risk_total: 0,
      response_text_bytes: 0,
      elapsed_seconds: 0,
    },
  });
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  process.exit(1);
});

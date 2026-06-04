type Json = Record<string, unknown>

type QueryResult<T> = { data: T | null; error: Error | null }

declare global {
  interface Window {
    BUCEPHALUS_WEB_CONFIG?: {
      apiBase?: string
      googleOAuthClientId?: string
    }
  }
}

const DEFAULT_API_BASE = "http://100.86.188.117:8099"

export type RegistryItem = {
  id: string
  name: string
  kind: "agent" | "benchmark" | "mcp" | "experiment_package" | "dataset" | "metric" | "runtime_profile" | "variant" | "case" | "grader" | "task_boundary" | "trial_contract"
  version: string
  description: string
  status: "ready" | "building" | "failed"
  size_bytes: number
  tags: string[]
  owner: string
  created_at: string
  secret_requirements: SecretRequirement[]
}

export type SecretRequirement = {
  id: string
  target: string
  required_for_variants: string[]
}

export type Experiment = {
  id: string
  name: string
  description: string
  config: Record<string, unknown>
  tags: string[]
  owner: string
  created_at: string
}

export type Run = {
  id: string
  experiment_id: string | null
  experiment_name: string
  status: "queued" | "running" | "succeeded" | "failed"
  variant: string
  started_at: string | null
  ended_at: string | null
  duration_ms: number
  region: string
  cost_usd: number
  created_at: string
}

export type RunMetric = {
  id: string
  run_id: string
  name: string
  value: number
  unit: string
  step: number
  recorded_at: string
}

export type Trace = {
  id: string
  run_id: string
  level: "info" | "warn" | "error" | "debug"
  span: string
  message: string
  latency_ms: number
  recorded_at: string
}

type TableName = "registry_items" | "experiments" | "runs" | "run_metrics" | "traces"

type Filters = Map<string, unknown>

export type CloudProbeResult = {
  id: "registry" | "packages" | "runs"
  label: string
  path: string
  ok: boolean
  rows: number | null
  latencyMs: number
  message: string
}

class CloudQuery<T> implements PromiseLike<QueryResult<T[]>> {
  private filters: Filters = new Map()
  private sortKey = ""
  private ascending = true
  private maxRows: number | null = null
  private single = false
  private pendingInsert: Json | null = null

  private readonly table: TableName

  constructor(table: TableName) {
    this.table = table
  }

  select(_columns?: string): this {
    return this
  }

  order(key: string, options: { ascending?: boolean } = {}): this {
    this.sortKey = key
    this.ascending = options.ascending ?? true
    return this
  }

  eq(key: string, value: unknown): this {
    this.filters.set(key, value)
    return this
  }

  limit(count: number): this {
    this.maxRows = count
    return this
  }

  maybeSingle(): Promise<QueryResult<T>> {
    this.single = true
    return this.execute().then((result) => ({
      data: (result.data?.[0] as T | undefined) ?? null,
      error: result.error,
    }))
  }

  insert(value: Json): this {
    this.pendingInsert = value
    return this
  }

  then<TResult1 = QueryResult<T[]>, TResult2 = never>(
    onfulfilled?: ((value: QueryResult<T[]>) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: unknown) => TResult2 | PromiseLike<TResult2>) | null,
  ): PromiseLike<TResult1 | TResult2> {
    return this.execute().then(onfulfilled, onrejected)
  }

  private async execute(): Promise<QueryResult<T[]>> {
    try {
      const data = this.pendingInsert
        ? await insertRow(this.table, this.pendingInsert)
        : await selectRows(this.table, this.filters)
      const rows = Array.isArray(data) ? data : [data]
      const sorted = this.sortKey ? sortRows(rows, this.sortKey, this.ascending) : rows
      const limited = this.maxRows == null ? sorted : sorted.slice(0, this.maxRows)
      return { data: limited as T[], error: null }
    } catch (error) {
      return { data: this.single ? ([] as T[]) : [], error: error instanceof Error ? error : new Error(String(error)) }
    }
  }
}

let authTokenProvider: (() => string | null) | null = null

export function setCloudAuthTokenProvider(provider: () => string | null) {
  authTokenProvider = provider
}

export const cloudApi = {
  from<T = any>(table: TableName): CloudQuery<T> {
    return new CloudQuery<T>(table)
  },
}

export async function probeCloudConnection(config: { apiBase?: string; bearerToken?: string } = {}): Promise<CloudProbeResult[]> {
  const targets = [
    {
      id: "registry" as const,
      label: "Registry",
      path: "/v1/registry/search?limit=1",
      rows: (payload: Json) => arrayLength(payload.hits),
    },
    {
      id: "packages" as const,
      label: "Experiment packages",
      path: "/v1/packages?limit=1",
      rows: (payload: Json) => arrayLength(payload.packages),
    },
    {
      id: "runs" as const,
      label: "Runs",
      path: "/v1/runs?limit=1",
      rows: (payload: Json) => arrayLength(payload.runs),
    },
  ]

  return Promise.all(
    targets.map(async (target) => {
      const started = Date.now()
      try {
        const payload = await apiWithConfig<Json>(target.path, {}, config)
        const latencyMs = Date.now() - started
        const rows = target.rows(payload)
        return {
          id: target.id,
          label: target.label,
          path: target.path,
          ok: true,
          rows,
          latencyMs,
          message: rows > 0 ? `${rows} row sample` : "reachable, no rows",
        }
      } catch (error) {
        return {
          id: target.id,
          label: target.label,
          path: target.path,
          ok: false,
          rows: null,
          latencyMs: Date.now() - started,
          message: conciseError(error),
        }
      }
    }),
  )
}

async function selectRows(table: TableName, filters: Filters): Promise<Json[]> {
  switch (table) {
    case "registry_items":
      return registryItems()
    case "experiments":
      return experiments(filters)
    case "runs":
      return runs(filters)
    case "run_metrics":
      return runMetrics(filters)
    case "traces":
      return traces(filters)
  }
}

async function insertRow(table: TableName, row: Json): Promise<Json> {
  if (table !== "experiments") {
    throw new Error(`Insert is not supported for ${table}`)
  }
  const config = isRecord(row.config) ? row.config : {}
  const packageDigest = stringValue(config.benchmark) || stringValue(config.package_digest)
  if (!packageDigest) {
    throw new Error("Choose an accepted experiment package before queueing a run")
  }
  const created = await api<Json>("/v1/runs", {
    method: "POST",
    body: JSON.stringify({
      package_digest: packageDigest,
      run_label: stringValue(row.name),
      env: {},
      secret_refs: isRecord(config.secret_refs) ? stringMap(config.secret_refs) : {},
      runtime_options: {
        backend: "modal",
        region: stringValue(config.region),
        max_parallel_trials: numberValue(config.maxParallel),
      },
    }),
  })
  return experimentFromRun(created)
}

async function registryItems(): Promise<RegistryItem[]> {
  const [packagesResult, registryResult] = await Promise.allSettled([
    api<{ packages?: Json[] }>("/v1/packages?limit=100"),
    api<{ hits?: Json[] }>("/v1/registry/search?limit=100"),
  ])
  if (packagesResult.status === "rejected" || registryResult.status === "rejected") {
    const failures = [
      packagesResult.status === "rejected" ? `packages: ${conciseError(packagesResult.reason)}` : "",
      registryResult.status === "rejected" ? `registry: ${conciseError(registryResult.reason)}` : "",
    ].filter(Boolean)
    throw new Error(failures.join("; "))
  }
  const packages = packagesResult.value
  const registry = registryResult.value
  const packageItems = (packages.packages ?? []).map(packageToRegistryItem)
  const registryItems = (registry.hits ?? []).map(hitToRegistryItem)
  return [...registryItems, ...packageItems]
}

async function experiments(filters: Filters): Promise<Experiment[]> {
  const payload = await api<{ packages?: Json[] }>("/v1/packages?limit=100")
  const rows = (payload.packages ?? []).map(packageToExperiment)
  return applyFilters(rows, filters)
}

async function runs(filters: Filters): Promise<Run[]> {
  const payload = await api<{ runs?: Json[] }>("/v1/runs?limit=100")
  const rows = (payload.runs ?? []).map(runFromCloud)
  return applyFilters(rows, filters)
}

async function runMetrics(filters: Filters): Promise<RunMetric[]> {
  const runId = stringValue(filters.get("run_id"))
  if (!runId) {
    const recent = await runs(new Map())
    const batches = await runtimeEvidenceBatch(recent.slice(0, 24), metricsForRun, "metric observations")
    return applyFilters(batches.flat() as RunMetric[], filters)
  }
  return applyFilters(await metricsForRun(runId), filters)
}

async function metricsForRun(runId: string): Promise<RunMetric[]> {
  const payload = await api<{ trial_results?: Json[]; metric_observations?: Json[] }>(`/v1/runs/${encodeURIComponent(runId)}/runtime/results?limit=1000`)
  const observations = payload.metric_observations ?? []
  const fromObservations = observations.map(metricFromObservation)
  const fromResults = (payload.trial_results ?? []).flatMap(metricsFromResult)
  return [...fromObservations, ...fromResults]
}

async function traces(filters: Filters): Promise<Trace[]> {
  const runId = stringValue(filters.get("run_id"))
  if (!runId) {
    const recent = await runs(new Map())
    const batches = await runtimeEvidenceBatch(recent.slice(0, 16), eventsForRun, "trace events")
    return applyFilters(batches.flat() as Trace[], filters)
  }
  return applyFilters(await eventsForRun(runId), filters)
}

async function eventsForRun(runId: string): Promise<Trace[]> {
  const payload = await api<{ events?: Json[] }>(`/v1/runs/${encodeURIComponent(runId)}/runtime/events?limit=1000`)
  return (payload.events ?? []).map(traceFromEvent)
}

async function runtimeEvidenceBatch<T>(
  recent: Run[],
  fetcher: (runId: string) => Promise<T[]>,
  label: string,
): Promise<T[][]> {
  if (recent.length === 0) return []
  const settled = await Promise.allSettled(recent.map((run) => fetcher(run.id)))
  const fulfilled = settled
    .filter((result): result is PromiseFulfilledResult<T[]> => result.status === "fulfilled")
    .map((result) => result.value)
  if (fulfilled.length > 0) return fulfilled
  const firstError = settled.find((result): result is PromiseRejectedResult => result.status === "rejected")
  throw new Error(`Runtime ${label} unavailable: ${conciseError(firstError?.reason)}`)
}

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  return apiWithConfig(path, init)
}

async function apiWithConfig<T>(path: string, init: RequestInit = {}, config: { apiBase?: string; bearerToken?: string } = {}): Promise<T> {
  const { base, token } = cloudConfig(config)
  const headers = new Headers(init.headers)
  headers.set("accept", "application/json")
  if (init.body && !headers.has("content-type")) headers.set("content-type", "application/json")
  if (token) headers.set("authorization", `Bearer ${token}`)
  const response = await fetch(`${base.replace(/\/+$/, "")}${path}`, { ...init, headers })
  if (!response.ok) throw new Error(conciseError(await response.text()))
  return (await response.json()) as T
}

function cloudConfig(config: { apiBase?: string; bearerToken?: string }) {
  const hasApiBase = Object.prototype.hasOwnProperty.call(config, "apiBase")
  const configuredApiBase = config.apiBase?.trim()
  const configuredToken = config.bearerToken?.trim()
  return {
    base: hasApiBase && configuredApiBase
      ? configuredApiBase
      : hasApiBase
        ? window.BUCEPHALUS_WEB_CONFIG?.apiBase || import.meta.env.VITE_BUCEPHALUS_API_BASE || DEFAULT_API_BASE
        : localStorage.getItem("buc.apiBase") || window.BUCEPHALUS_WEB_CONFIG?.apiBase || import.meta.env.VITE_BUCEPHALUS_API_BASE || DEFAULT_API_BASE,
    token: configuredToken || authTokenProvider?.() || "",
  }
}

function packageToRegistryItem(row: Json): RegistryItem {
  const digest = stringValue(row.package_digest)
  const manifest = isRecord(row.manifest_json) ? row.manifest_json : {}
  const target = stringValue(row.target)
  return {
    id: digest,
    name: packageName(row),
    kind: "experiment_package",
    version: shortDigest(digest),
    description: stringValue(manifest.description) || stringValue(row.target) || "Accepted experiment build artifact",
    status: statusFromPackage(stringValue(row.status)),
    size_bytes: numberValue(row.byte_size ?? row.size_bytes ?? row.canonical_size_bytes),
    tags: uniqueStrings([...stringArray(row.tags), ...stringArray(manifest.tags), target, stringValue(row.status)]),
    owner: packageOwner(row, manifest),
    created_at: stringValue(row.created_at) || stringValue(row.updated_at),
    secret_requirements: secretRequirements(row),
  }
}

function hitToRegistryItem(row: Json): RegistryItem {
  const digest = stringValue(row.content_digest) || stringValue(row.digest) || stringValue(row.id)
  const kind = kindFromCloud(stringValue(row.kind) || stringValue(row.resource_kind))
  return {
    id: digest,
    name: stringValue(row.display_name) || stringValue(row.name) || `resource ${shortDigest(digest)}`,
    kind,
    version: stringValue(row.schema_version) || stringValue(row.version) || "v1",
    description: stringValue(row.summary) || stringValue(row.description) || "Registered reusable experiment resource",
    status: statusFromRegistryHit(stringValue(row.status) || stringValue(row.state)),
    size_bytes: numberValue(row.canonical_size_bytes ?? row.size_bytes ?? row.byte_size),
    tags: uniqueStrings([...stringArray(row.tags), ...stringArray(row.labels), stringValue(row.kind)]),
    owner: registryOwner(row),
    created_at: stringValue(row.created_at) || stringValue(row.updated_at),
    secret_requirements: [],
  }
}

function packageToExperiment(row: Json): Experiment {
  const digest = stringValue(row.package_digest)
  const manifest = isRecord(row.manifest_json) ? row.manifest_json : {}
  return {
    id: digest,
    name: packageName(row),
    description: stringValue(manifest.description) || "Experiment package ready for cloud execution",
    config: {
      package_digest: digest,
      target: row.target,
      diagnostics: row.diagnostics,
      manifest: row.manifest_json,
      resolved_experiment: row.resolved_experiment_json,
      secret_requirements: secretRequirements(row),
    },
    tags: uniqueStrings([...stringArray(row.tags), ...stringArray(manifest.tags), stringValue(row.status), stringValue(row.target)]),
    owner: packageOwner(row, manifest),
    created_at: stringValue(row.created_at) || stringValue(row.updated_at),
  }
}

function secretRequirements(row: Json): SecretRequirement[] {
  const raw = row.secret_requirements
  if (!Array.isArray(raw)) return []
  return raw
    .filter(isRecord)
    .map((item) => ({
      id: stringValue(item.id),
      target: stringValue(item.target),
      required_for_variants: Array.isArray(item.required_for_variants)
        ? item.required_for_variants.map(stringValue).filter(Boolean)
        : [],
    }))
    .filter((item) => item.id)
}

function stringMap(value: Json): Record<string, string> {
  const out: Record<string, string> = {}
  for (const [key, item] of Object.entries(value)) {
    if (typeof item === "string" && item.trim()) out[key] = item.trim()
  }
  return out
}

function experimentFromRun(row: Json): Experiment {
  return {
    id: stringValue(row.run_id),
    name: stringValue(row.run_label) || `run ${shortDigest(stringValue(row.run_id))}`,
    description: "Cloud run queued from the migrated frontend",
    config: row,
    tags: [stringValue(row.status)].filter(Boolean),
    owner: "cloud",
    created_at: stringValue(row.created_at),
  }
}

function runFromCloud(row: Json): Run {
  const started = stringValue(row.started_at)
  const ended = stringValue(row.ended_at) || stringValue(row.completed_at)
  const runtimeOptions = isRecord(row.runtime_options) ? row.runtime_options : {}
  const runRequirements = isRecord(row.run_requirements) ? row.run_requirements : {}
  const duration = numberValue(row.duration_ms) || numberValue(row.duration_seconds) * 1000 || durationMs(started, ended)
  const runId = stringValue(row.run_id) || stringValue(row.id)
  const packageDigest = stringValue(row.package_digest)
  return {
    id: runId,
    experiment_id: stringValue(row.experiment_id) || packageDigest || null,
    experiment_name: stringValue(row.experiment_name) || stringValue(row.run_label) || stringValue(row.name) || labeledDigest("package", packageDigest) || labeledDigest("run", runId),
    status: statusFromRun(stringValue(row.status) || stringValue(row.state)),
    variant: stringValue(row.variant) || stringValue(row.variant_name) || stringValue(runtimeOptions.variant) || stringValue(row.model) || shortDigest(packageDigest) || "default",
    started_at: started || null,
    ended_at: ended || null,
    duration_ms: duration,
    region: stringValue(row.region) || stringValue(runtimeOptions.region) || stringValue(runRequirements.region) || stringValue(runRequirements.executor) || "cloud",
    cost_usd: runCostUsd(row),
    created_at: stringValue(row.created_at) || started,
  }
}

function runCostUsd(row: Json): number {
  const direct = numberValue(row.cost_usd ?? row.estimated_cost_usd ?? row.billing_cost_usd)
  if (direct) return direct
  const cents = numberValue(row.cost_cents ?? row.estimated_cost_cents)
  return cents ? cents / 100 : 0
}

function metricFromObservation(row: Json, index: number): RunMetric {
  const metricId = stringValue(row.metric_observation_id) || stringValue(row.id)
  const trialId = stringValue(row.trial_id) || stringValue(row.trial_result_id)
  return {
    id: metricId || `${trialId || metricRunId(row) || "metric"}:${index}`,
    run_id: metricRunId(row),
    name: stringValue(row.metric_name) || stringValue(row.name) || stringValue(row.key) || "metric",
    value: numberValue(row.metric_value ?? row.value),
    unit: stringValue(row.unit) || "",
    step: numberValue(row.row_seq ?? row.step) || index,
    recorded_at: stringValue(row.recorded_at) || stringValue(row.created_at) || new Date(0).toISOString(),
  }
}

function metricsFromResult(row: Json, index: number): RunMetric[] {
  const trialId = stringValue(row.trial_id) || stringValue(row.id) || `trial:${index}`
  const base = {
    run_id: metricRunId(row),
    step: numberValue(row.row_seq ?? row.trial_index ?? row.step) || index,
    recorded_at: stringValue(row.recorded_at) || stringValue(row.created_at) || new Date(0).toISOString(),
  }
  const primaryName = stringValue(row.primary_metric_name)
  const metrics: RunMetric[] = primaryName ? [{ ...base, id: `${trialId}:primary`, name: primaryName, value: numberValue(row.primary_metric_value), unit: "" }] : []
  if (isRecord(row.metrics)) {
    for (const [name, value] of Object.entries(row.metrics)) {
      metrics.push({ ...base, id: `${trialId}:${name}`, name, value: numberValue(value), unit: "" })
    }
  }
  return metrics
}

function metricRunId(row: Json): string {
  return stringValue(row.run_id) || stringValue(row.cloud_run_id)
}

function traceFromEvent(row: Json): Trace {
  const payload = isRecord(row.payload) ? row.payload : {}
  const eventType = stringValue(row.event_type) || stringValue(row.type)
  const level = traceLevel(stringValue(row.level) || stringValue(payload.level), eventType)
  return {
    id: stringValue(row.event_id) || stringValue(row.id) || String(row.seq ?? ""),
    run_id: stringValue(row.run_id) || stringValue(row.cloud_run_id),
    level,
    span: stringValue(row.span) || stringValue(row.scope) || stringValue(payload.span) || stringValue(payload.scope) || eventType,
    message: stringValue(row.message) || stringValue(row.summary) || stringValue(payload.message) || stringValue(payload.summary) || eventType,
    latency_ms: eventLatencyMs(row, payload),
    recorded_at: stringValue(row.recorded_at) || stringValue(row.created_at),
  }
}

function applyFilters<T extends Record<string, unknown>>(rows: T[], filters: Filters): T[] {
  if (filters.size === 0) return rows
  return rows.filter((row) => {
    for (const [key, value] of filters) {
      if (row[key] !== value) return false
    }
    return true
  })
}

function sortRows<T extends Record<string, unknown>>(rows: T[], key: string, ascending: boolean): T[] {
  return [...rows].sort((a, b) => {
    const left = String(a[key] ?? "")
    const right = String(b[key] ?? "")
    return ascending ? left.localeCompare(right) : right.localeCompare(left)
  })
}

function packageName(row: Json): string {
  const manifest = isRecord(row.manifest_json) ? row.manifest_json : {}
  return stringValue(manifest.name) || stringValue(row.target) || labeledDigest("package", stringValue(row.package_digest)) || "package"
}

function packageOwner(row: Json, manifest: Json): string {
  return stringValue(row.owner) || stringValue(row.created_by) || stringValue(manifest.owner) || stringValue(manifest.author) || "cloud"
}

function registryOwner(row: Json): string {
  return stringValue(row.owner) || stringValue(row.namespace) || stringValue(row.created_by) || "registry"
}

function statusFromPackage(status: string): RegistryItem["status"] {
  if (status === "failed" || status === "rejected") return "failed"
  if (status === "accepted") return "ready"
  return "building"
}

function statusFromRegistryHit(status: string): RegistryItem["status"] {
  if (status === "failed" || status === "error" || status === "rejected") return "failed"
  if (status === "building" || status === "pending" || status === "queued") return "building"
  return "ready"
}

function statusFromRun(status: string): Run["status"] {
  if (status === "running") return "running"
  if (status === "completed" || status === "succeeded") return "succeeded"
  if (status === "failed" || status === "cancelled") return "failed"
  return "queued"
}

function kindFromCloud(kind: string): RegistryItem["kind"] {
  switch (kind) {
    case "agent":
    case "benchmark":
    case "mcp":
    case "experiment_package":
    case "dataset":
    case "metric":
    case "runtime_profile":
    case "variant":
    case "case":
    case "grader":
    case "task_boundary":
    case "trial_contract":
      return kind
    case "agent_app":
      return "agent"
    default:
      return "experiment_package"
  }
}

function levelFromEventType(type: string): Trace["level"] {
  if (type.includes("error") || type.includes("fail")) return "error"
  if (type.includes("warn") || type.includes("retry")) return "warn"
  if (type.includes("debug")) return "debug"
  return "info"
}

function traceLevel(level: string, eventType: string): Trace["level"] {
  if (level === "error" || level === "warn" || level === "debug" || level === "info") return level
  return levelFromEventType(eventType)
}

function eventLatencyMs(row: Json, payload: Json): number {
  const direct = numberValue(row.latency_ms ?? row.duration_ms ?? payload.latency_ms ?? payload.duration_ms)
  if (direct) return direct
  const seconds = numberValue(row.latency_seconds ?? row.duration_seconds ?? payload.latency_seconds ?? payload.duration_seconds)
  return seconds ? seconds * 1000 : 0
}

function durationMs(started: string, ended: string): number {
  if (!started || !ended) return 0
  const startMs = Date.parse(started)
  const endMs = Date.parse(ended)
  return Number.isFinite(startMs) && Number.isFinite(endMs) ? Math.max(0, endMs - startMs) : 0
}

function shortDigest(value: string): string {
  const clean = value.replace(/^sha256:/, "")
  if (!clean) return ""
  if (clean.length <= 12) return clean
  return `${clean.slice(0, 6)}...${clean.slice(-4)}`
}

function labeledDigest(label: string, value: string): string {
  const digest = shortDigest(value)
  return digest ? `${label} ${digest}` : ""
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value : value == null ? "" : String(value)
}

function stringArray(value: unknown): string[] {
  if (Array.isArray(value)) return value.map(stringValue).filter(Boolean)
  if (typeof value === "string") {
    return value
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean)
  }
  return []
}

function uniqueStrings(values: string[]): string[] {
  return Array.from(new Set(values.map((value) => value.trim()).filter(Boolean)))
}

function numberValue(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value
  if (typeof value === "string") {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : 0
  }
  return 0
}

function arrayLength(value: unknown): number {
  return Array.isArray(value) ? value.length : 0
}

function conciseError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  if (/OAuth bearer authentication/i.test(message)) {
    return "Google OAuth sign-in required"
  }
  try {
    const parsed = JSON.parse(message)
    if (isRecord(parsed)) {
      const parsedMessage = stringValue(parsed.message) || stringValue(parsed.error) || stringValue(parsed.detail)
      if (/OAuth bearer authentication/i.test(parsedMessage)) {
        return "Google OAuth sign-in required"
      }
      if (parsedMessage) return parsedMessage.length > 160 ? `${parsedMessage.slice(0, 157)}...` : parsedMessage
    }
  } catch {
    // Non-JSON network and browser errors are already readable.
  }
  return message.length > 160 ? `${message.slice(0, 157)}...` : message
}

function isRecord(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

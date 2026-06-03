type Json = Record<string, unknown>

type QueryResult<T> = { data: T | null; error: Error | null }

declare global {
  interface Window {
    BUCEPHALUS_WEB_CONFIG?: {
      apiBase?: string
      userToken?: string
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

class CloudQuery<T> implements PromiseLike<QueryResult<T[]>> {
  private filters: Filters = new Map()
  private sortKey = ""
  private ascending = true
  private maxRows: number | null = null
  private single = false
  private pendingInsert: Json | null = null

  constructor(private readonly table: TableName) {}

  select(): this {
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

export const supabase = {
  from<T = unknown>(table: TableName): CloudQuery<T> {
    return new CloudQuery<T>(table)
  },
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
      secret_refs: {},
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
  const [packages, registry] = await Promise.all([
    api<{ packages?: Json[] }>("/v1/packages?limit=100").catch(() => ({ packages: [] })),
    api<{ hits?: Json[] }>("/v1/registry/search?q=&limit=100").catch(() => ({ hits: [] })),
  ])
  const packageItems = (packages.packages ?? []).map(packageToRegistryItem)
  const registryItems = (registry.hits ?? []).map(hitToRegistryItem)
  return [...registryItems, ...packageItems]
}

async function experiments(filters: Filters): Promise<Experiment[]> {
  const payload = await api<{ packages?: Json[] }>("/v1/packages?limit=100").catch(() => ({ packages: [] }))
  const rows = (payload.packages ?? []).map(packageToExperiment)
  return applyFilters(rows, filters)
}

async function runs(filters: Filters): Promise<Run[]> {
  const payload = await api<{ runs?: Json[] }>("/v1/runs?limit=100").catch(() => ({ runs: [] }))
  const rows = (payload.runs ?? []).map(runFromCloud)
  return applyFilters(rows, filters)
}

async function runMetrics(filters: Filters): Promise<RunMetric[]> {
  const runId = stringValue(filters.get("run_id"))
  if (!runId) return []
  const payload = await api<{ trial_results?: Json[]; metric_observations?: Json[] }>(`/v1/runs/${encodeURIComponent(runId)}/runtime/results?limit=1000`).catch(() => ({ trial_results: [], metric_observations: [] }))
  const observations = payload.metric_observations ?? []
  const fromObservations = observations.map(metricFromObservation)
  const fromResults = (payload.trial_results ?? []).flatMap(metricsFromResult)
  return applyFilters([...fromObservations, ...fromResults], filters)
}

async function traces(filters: Filters): Promise<Trace[]> {
  const runId = stringValue(filters.get("run_id"))
  if (!runId) return []
  const payload = await api<{ events?: Json[] }>(`/v1/runs/${encodeURIComponent(runId)}/runtime/events?limit=1000`).catch(() => ({ events: [] }))
  return applyFilters((payload.events ?? []).map(traceFromEvent), filters)
}

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const base = localStorage.getItem("buc.apiBase") || window.BUCEPHALUS_WEB_CONFIG?.apiBase || import.meta.env.VITE_BUCEPHALUS_API_BASE || DEFAULT_API_BASE
  const token = localStorage.getItem("buc.userToken") || window.BUCEPHALUS_WEB_CONFIG?.userToken || import.meta.env.VITE_BUCEPHALUS_USER_TOKEN || ""
  const headers = new Headers(init.headers)
  headers.set("accept", "application/json")
  if (init.body && !headers.has("content-type")) headers.set("content-type", "application/json")
  if (token) headers.set("authorization", `Bearer ${token}`)
  const response = await fetch(`${base.replace(/\/+$/, "")}${path}`, { ...init, headers })
  if (!response.ok) throw new Error(await response.text())
  return (await response.json()) as T
}

function packageToRegistryItem(row: Json): RegistryItem {
  const digest = stringValue(row.package_digest)
  return {
    id: digest,
    name: packageName(row),
    kind: "experiment_package",
    version: shortDigest(digest),
    description: stringValue((row.manifest_json as Json | undefined)?.description) || stringValue(row.target) || "Accepted experiment build artifact",
    status: statusFromPackage(stringValue(row.status)),
    size_bytes: numberValue(row.byte_size),
    tags: [stringValue(row.target)].filter(Boolean),
    owner: "cloud",
    created_at: stringValue(row.created_at) || stringValue(row.updated_at),
  }
}

function hitToRegistryItem(row: Json): RegistryItem {
  const digest = stringValue(row.content_digest)
  return {
    id: digest,
    name: stringValue(row.display_name) || shortDigest(digest),
    kind: kindFromCloud(stringValue(row.kind)),
    version: stringValue(row.schema_version) || "v1",
    description: stringValue(row.summary) || "Registered reusable experiment resource",
    status: "ready",
    size_bytes: numberValue(row.canonical_size_bytes),
    tags: [stringValue(row.kind)].filter(Boolean),
    owner: "registry",
    created_at: stringValue(row.created_at),
  }
}

function packageToExperiment(row: Json): Experiment {
  const digest = stringValue(row.package_digest)
  return {
    id: digest,
    name: packageName(row),
    description: stringValue((row.manifest_json as Json | undefined)?.description) || "Experiment package ready for cloud execution",
    config: {
      package_digest: digest,
      target: row.target,
      diagnostics: row.diagnostics,
      manifest: row.manifest_json,
      resolved_experiment: row.resolved_experiment_json,
    },
    tags: [stringValue(row.status), stringValue(row.target)].filter(Boolean),
    owner: "cloud",
    created_at: stringValue(row.created_at) || stringValue(row.updated_at),
  }
}

function experimentFromRun(row: Json): Experiment {
  return {
    id: stringValue(row.run_id),
    name: stringValue(row.run_label) || shortDigest(stringValue(row.run_id)),
    description: "Cloud run queued from the migrated frontend",
    config: row,
    tags: [stringValue(row.status)].filter(Boolean),
    owner: "cloud",
    created_at: stringValue(row.created_at),
  }
}

function runFromCloud(row: Json): Run {
  const started = stringValue(row.started_at)
  const ended = stringValue(row.completed_at)
  return {
    id: stringValue(row.run_id),
    experiment_id: stringValue(row.package_digest) || null,
    experiment_name: stringValue(row.run_label) || shortDigest(stringValue(row.package_digest)) || shortDigest(stringValue(row.run_id)),
    status: statusFromRun(stringValue(row.status)),
    variant: shortDigest(stringValue(row.package_digest)) || "default",
    started_at: started || null,
    ended_at: ended || null,
    duration_ms: durationMs(started, ended),
    region: stringValue((row.runtime_options as Json | undefined)?.region) || stringValue((row.run_requirements as Json | undefined)?.executor) || "cloud",
    cost_usd: 0,
    created_at: stringValue(row.created_at),
  }
}

function metricFromObservation(row: Json, index: number): RunMetric {
  return {
    id: stringValue(row.metric_observation_id) || `${stringValue(row.trial_id)}:${index}`,
    run_id: stringValue(row.cloud_run_id),
    name: stringValue(row.metric_name) || stringValue(row.name) || "metric",
    value: numberValue(row.metric_value ?? row.value),
    unit: stringValue(row.unit) || "",
    step: numberValue(row.row_seq ?? row.step) || index,
    recorded_at: stringValue(row.created_at) || new Date(0).toISOString(),
  }
}

function metricsFromResult(row: Json, index: number): RunMetric[] {
  const base = {
    run_id: stringValue(row.cloud_run_id),
    step: numberValue(row.row_seq ?? row.trial_index) || index,
    recorded_at: stringValue(row.created_at) || new Date(0).toISOString(),
  }
  const primaryName = stringValue(row.primary_metric_name)
  const metrics: RunMetric[] = primaryName ? [{ ...base, id: `${stringValue(row.trial_id)}:primary`, name: primaryName, value: numberValue(row.primary_metric_value), unit: "" }] : []
  if (isRecord(row.metrics)) {
    for (const [name, value] of Object.entries(row.metrics)) {
      metrics.push({ ...base, id: `${stringValue(row.trial_id)}:${name}`, name, value: numberValue(value), unit: "" })
    }
  }
  return metrics
}

function traceFromEvent(row: Json): Trace {
  const payload = isRecord(row.payload) ? row.payload : {}
  const level = stringValue(payload.level) || levelFromEventType(stringValue(row.event_type))
  return {
    id: stringValue(row.event_id) || String(row.seq ?? ""),
    run_id: stringValue(row.run_id) || stringValue(row.cloud_run_id),
    level: level as Trace["level"],
    span: stringValue(payload.span) || stringValue(payload.scope) || stringValue(row.event_type),
    message: stringValue(payload.message) || stringValue(payload.summary) || stringValue(row.event_type),
    latency_ms: numberValue(payload.latency_ms ?? payload.duration_ms),
    recorded_at: stringValue(row.created_at),
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
  return stringValue(manifest.name) || stringValue(row.target) || shortDigest(stringValue(row.package_digest))
}

function statusFromPackage(status: string): RegistryItem["status"] {
  if (status === "failed" || status === "rejected") return "failed"
  if (status === "accepted") return "ready"
  return "building"
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

function durationMs(started: string, ended: string): number {
  if (!started || !ended) return 0
  const startMs = Date.parse(started)
  const endMs = Date.parse(ended)
  return Number.isFinite(startMs) && Number.isFinite(endMs) ? Math.max(0, endMs - startMs) : 0
}

function shortDigest(value: string): string {
  return value ? value.replace(/^sha256:/, "").slice(0, 12) : ""
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value : value == null ? "" : String(value)
}

function numberValue(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value
  if (typeof value === "string") {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : 0
  }
  return 0
}

function isRecord(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

import { useEffect, useMemo, useState } from "react"
import {
  Activity,
  AlertTriangle,
  ArrowUpRight,
  ChevronRight,
  CircleAlert,
  Copy,
  Cpu,
  Download,
  Filter,
  GitCommit,
  Info,
  Plus,
  Search,
} from "lucide-react"
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Line,
  LineChart,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { FilterStrip } from "@/components/filter-strip"
import { ConnectionIssue } from "@/components/connection-issue"
import { StatusPill } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import {
  cloudApi,
  type Run,
  type RunMetric,
  type Trace,
} from "@/lib/cloud-api"
import { formatDuration, formatReadableLabel, formatReadableToken, formatRelative, formatUsd } from "@/lib/format"
import { downloadCsv } from "@/lib/export"
import { PageHeader } from "@/pages/registry"
import { cn } from "@/lib/utils"

export function RunsPage() {
  const { navigate } = useRouter()
  const [runs, setRuns] = useState<Run[]>([])
  const [loaded, setLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [metrics, setMetrics] = useState<RunMetric[]>([])
  const [traces, setTraces] = useState<Trace[]>([])
  const [telemetryLoaded, setTelemetryLoaded] = useState(false)
  const [telemetryError, setTelemetryError] = useState<string | null>(null)
  const [q, setQ] = useState("")
  const [statusFilter, setStatusFilter] = useState<string | null>(null)
  const [regionFilter, setRegionFilter] = useState("all")

  async function loadRuns() {
    setLoaded(false)
    setTelemetryLoaded(false)
    setLoadError(null)
    setTelemetryError(null)
    const { data, error } = await cloudApi
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
    if (error) {
      setRuns([])
      setMetrics([])
      setTraces([])
      setLoadError(error.message)
      setLoaded(true)
      setTelemetryLoaded(true)
      return
    }
    setRuns((data ?? []) as Run[])
    setLoaded(true)
    const [metricRows, traceRows] = await Promise.all([
      cloudApi.from("run_metrics").select("*").order("recorded_at", { ascending: true }),
      cloudApi.from("traces").select("*").order("recorded_at", { ascending: true }),
    ])
    setMetrics(metricRows.error ? [] : ((metricRows.data ?? []) as RunMetric[]))
    setTraces(traceRows.error ? [] : ((traceRows.data ?? []) as Trace[]))
    setTelemetryError(metricRows.error?.message ?? traceRows.error?.message ?? null)
    setTelemetryLoaded(true)
  }

  useEffect(() => {
    void loadRuns()
  }, [])

  const filtered = useMemo(
    () =>
      runs.filter((r) => {
        if (statusFilter && r.status !== statusFilter) return false
        if (regionFilter !== "all" && r.region !== regionFilter) return false
        if (q && !`${r.experiment_name} ${r.variant} ${r.region}`.toLowerCase().includes(q.toLowerCase())) return false
        return true
      }),
    [runs, q, statusFilter, regionFilter],
  )

  const counts = useMemo(() => {
    const c: Record<string, number> = {}
    runs.forEach((r) => (c[r.status] = (c[r.status] ?? 0) + 1))
    return c
  }, [runs])
  const unavailable = Boolean(loadError)
  const totals = useMemo(() => {
    const completed = runs.filter((run) => run.status === "succeeded" || run.status === "failed")
    const totalCost = runs.reduce((acc, run) => acc + Number(run.cost_usd), 0)
    const avgDuration =
      completed.length === 0
        ? 0
        : completed.reduce((acc, run) => acc + run.duration_ms, 0) / completed.length
    return { completed: completed.length, totalCost, avgDuration }
  }, [runs])

  const regions = useMemo(
    () => filterOptions(runs.map((r) => r.region), "all regions"),
    [runs],
  )
  const activityRows = useMemo(() => buildRunActivity(runs), [runs])
  const errorPressureRows = useMemo(() => buildErrorPressure(runs, traces), [runs, traces])
  const latencyRows = useMemo(() => buildLatencyHistogram(runs, metrics, traces), [runs, metrics, traces])
  const attentionRows = useMemo(() => buildAttentionRows(runs, metrics, traces), [runs, metrics, traces])
  const evidenceByRun = useMemo(() => buildRunEvidenceMap(metrics, traces), [metrics, traces])

  function clearFilters() {
    setStatusFilter(null)
    setRegionFilter("all")
    setQ("")
  }

  function exportRunsCsv() {
    downloadCsv(
      `runs-${new Date().toISOString().slice(0, 10)}.csv`,
      filtered.map((run) => ({
        run_id: run.id,
        experiment: run.experiment_name,
        variant: run.variant,
        status: run.status,
        duration_ms: run.duration_ms,
        cost_usd: Number(run.cost_usd),
        region: run.region,
        started_at: run.started_at ?? run.created_at,
        ended_at: run.ended_at ?? "",
      })),
    )
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Runs"
        subtitle="Live executions and trace evidence."
        rightSlot={
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-[12px]"
            onClick={exportRunsCsv}
            disabled={filtered.length === 0}
          >
            <Download className="h-3 w-3" /> Export
          </Button>
        }
        primaryAction={
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-[12px]"
            onClick={() => navigate({ name: "compare" })}
          >
            <GitCommit className="h-3 w-3" /> Compare
          </Button>
        }
      />

      <div className="grid grid-cols-2 gap-px border-b border-border bg-border md:grid-cols-4">
        <Stat
          label="Running"
          value={!loaded ? "loading" : unavailable ? "-" : `${counts.running ?? 0}`}
          detail={unavailable ? "request failed" : undefined}
          dot={unavailable ? "bg-warning" : "bg-info animate-pulse"}
        />
        <Stat
          label="Queued"
          value={!loaded ? "loading" : unavailable ? "-" : `${counts.queued ?? 0}`}
          detail={unavailable ? "unavailable" : undefined}
          dot={unavailable ? "bg-warning" : "bg-muted-foreground"}
        />
        <Stat
          label="Completed"
          value={!loaded ? "loading" : unavailable ? "-" : `${totals.completed}`}
          detail={unavailable ? "connect API" : loaded ? formatDuration(totals.avgDuration) : undefined}
          dot={unavailable ? "bg-warning" : "bg-success"}
        />
        <Stat
          label="Spend"
          value={!loaded ? "loading" : unavailable ? "-" : formatUsd(totals.totalCost)}
          detail={unavailable ? "request failed" : loaded ? `${runs.length} runs` : undefined}
          dot={unavailable ? "bg-warning" : "bg-brand"}
        />
      </div>

      {loaded && !loadError ? (
        <div className="sticky top-11 z-10 flex flex-col gap-2 border-b border-border bg-background/95 px-3 py-1.5 backdrop-blur lg:flex-row lg:items-center lg:justify-between">
          <div className="flex max-w-full items-center gap-1.5 overflow-x-auto scrollbar-thin">
            {(["running", "queued", "succeeded", "failed"] as const).map((s) => {
              const active = statusFilter === s
              return (
                <button
                  key={s}
                  onClick={() => setStatusFilter(active ? null : s)}
                  className={cn(
                    "flex items-center gap-1 rounded-md border px-2 py-1 text-[11.5px]",
                    active
                      ? "border-border bg-secondary text-foreground"
                      : "border-transparent text-muted-foreground hover:bg-secondary",
                  )}
                >
                  <StatusPill status={s} withDot={false} />
                  <span className="font-mono text-[10.5px] text-muted-foreground">
                    {counts[s] ?? 0}
                  </span>
                </button>
              )
            })}
          </div>
          <div className="flex min-w-0 flex-wrap items-center gap-1.5">
            <div className="relative min-w-[180px] flex-1 sm:flex-none">
              <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={q}
                onChange={(e) => setQ(e.target.value)}
                placeholder="Filter runs..."
                className="h-7 w-full pl-7 text-[12px] sm:w-72"
              />
            </div>
            <Filter className="h-3 w-3 text-muted-foreground" />
            <FilterStrip
              label="Region"
              value={regionFilter}
              options={regions}
              onValueChange={setRegionFilter}
              max={5}
              className="w-full max-w-full sm:w-auto"
            />
          </div>
        </div>
      ) : null}

      {!loaded ? <RunsSkeleton /> : null}
      {loaded && loadError ? (
        <ConnectionIssue
          title="Runs request failed"
          detail={loadError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadRuns()}
        />
      ) : null}
      {loaded && !loadError && runs.length === 0 ? (
        <RunsEmptyState
          title="No runs yet"
          detail="Queue an experiment first."
          primary="New experiment"
          onPrimary={() => navigate({ name: "experiment-new" })}
        />
      ) : null}
      {loaded && !loadError && runs.length > 0 && filtered.length === 0 ? (
        <RunsEmptyState
          title="No runs match"
          detail="Clear the current status, region, and search filters to return to the full run history."
          primary="Clear filters"
          onPrimary={clearFilters}
        />
      ) : null}
      {loaded && !loadError && telemetryError ? (
        <ConnectionIssue
          title="Run telemetry request failed"
          detail={telemetryError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadRuns()}
          compact
        />
      ) : null}

      {loaded && runs.length > 0 ? (
        <section className="border-b border-border bg-background">
          <header className="flex h-8 items-center justify-between border-b border-border px-3">
            <div className="flex items-center gap-2">
              <h3 className="text-[12px] font-semibold">Run activity</h3>
              <span className="text-[11px] text-muted-foreground">last 14 days by outcome</span>
            </div>
            <span className="font-mono text-[11px] text-muted-foreground">{runs.length} total</span>
          </header>
          <div className="p-3">
            <ChartContainer config={runActivityCfg} className="h-32 w-full">
              <BarChart data={activityRows} barGap={2}>
                <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
                <XAxis
                  dataKey="day"
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <YAxis hide allowDecimals={false} />
                <Tooltip contentStyle={tooltipStyle} />
                <Bar dataKey="succeeded" stackId="runs" fill="var(--color-succeeded)" isAnimationActive={false} />
                <Bar dataKey="failed" stackId="runs" fill="var(--color-failed)" isAnimationActive={false} />
                <Bar dataKey="running" stackId="runs" fill="var(--color-running)" isAnimationActive={false} />
                <Bar dataKey="queued" stackId="runs" fill="var(--color-queued)" radius={[2, 2, 0, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          </div>
        </section>
      ) : null}

      {loaded && runs.length > 0 ? (
        <section className="grid grid-cols-1 gap-px border-b border-border bg-border xl:grid-cols-[minmax(0,1.25fr)_minmax(0,0.95fr)_minmax(320px,0.8fr)]">
          <HealthPanel
            title="Failure pressure"
            subtitle={telemetryError ? "trace request failed" : telemetryLoaded ? `${traces.length} trace events` : "loading trace events"}
            empty={Boolean(telemetryError) || !errorPressureRows.some((row) => row.failed || row.errors || row.warnings)}
            emptyDetail={telemetryError ? "Trace events are unavailable for this request; retry after fixing API credentials." : "Failures, warnings, and errors will appear here when runtime events are linked to runs."}
          >
            <ChartContainer config={pressureCfg} className="h-44 w-full">
              <BarChart data={errorPressureRows} barGap={2}>
                <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
                <XAxis
                  dataKey="day"
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <YAxis hide allowDecimals={false} />
                <Tooltip contentStyle={tooltipStyle} />
                <Bar dataKey="failed" stackId="pressure" fill="var(--color-failed)" radius={[0, 0, 0, 0]} isAnimationActive={false} />
                <Bar dataKey="errors" stackId="pressure" fill="var(--color-errors)" radius={[0, 0, 0, 0]} isAnimationActive={false} />
                <Bar dataKey="warnings" stackId="pressure" fill="var(--color-warnings)" radius={[2, 2, 0, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          </HealthPanel>

          <HealthPanel
            title="Latency distribution"
            subtitle={telemetryError ? "metric request failed" : telemetryLoaded ? `${latencyRows.reduce((acc, row) => acc + row.runs, 0)} runs with signal` : "loading metric rows"}
            empty={Boolean(telemetryError) || !latencyRows.some((row) => row.runs > 0)}
            emptyDetail={telemetryError ? "Metric and trace latency signals are unavailable for this request." : "Latency bins use latency metrics first, trace latency second, and completed run duration as fallback."}
          >
            <ChartContainer config={latencyCfg} className="h-44 w-full">
              <BarChart data={latencyRows}>
                <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
                <XAxis
                  dataKey="bucket"
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <YAxis hide allowDecimals={false} />
                <Tooltip contentStyle={tooltipStyle} />
                <Bar dataKey="runs" fill="var(--color-runs)" radius={[2, 2, 0, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          </HealthPanel>

          <HealthPanel
            title="Needs attention"
            subtitle={telemetryError ? "telemetry unavailable" : `${attentionRows.length} runs`}
            empty={Boolean(telemetryError) || attentionRows.length === 0}
            emptyDetail={telemetryError ? "Attention scoring needs trace events and metric rows from the runtime endpoints." : "No failed, warning-heavy, or unusually slow runs in the current window."}
          >
            <div className="min-h-44">
              {attentionRows.slice(0, 6).map((row) => (
                <button
                  key={row.run.id}
                  onClick={() => navigate({ name: "run-detail", id: row.run.id })}
                  className="grid w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-2 border-b border-border px-3 py-1.5 text-left hover:bg-accent/40"
                >
                  <span className="min-w-0">
                    <span className="block truncate font-mono text-[11.5px]">{formatReadableLabel(row.run.experiment_name)}</span>
                    <span className="block truncate text-[10.5px] text-muted-foreground">{row.reason}</span>
                  </span>
                  <span className={cn("rounded px-1.5 py-0.5 font-mono text-[10px]", row.tone)}>
                    {row.score}
                  </span>
                </button>
              ))}
            </div>
          </HealthPanel>
        </section>
      ) : null}

      {loaded && filtered.length > 0 ? (
        <div className="overflow-x-auto text-[12px]">
          <div
            className="grid min-w-[1040px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
            style={{
              gridTemplateColumns:
                "minmax(220px,1.4fr) minmax(140px,0.8fr) 100px 130px 90px 80px 100px 90px 24px",
            }}
          >
            <span>Experiment</span>
            <span>Variant</span>
            <span>Status</span>
            <span>Evidence</span>
            <span>Duration</span>
            <span>Cost</span>
            <span>Region</span>
            <span>Started</span>
            <span></span>
          </div>
          {filtered.map((r) => (
            <button
              key={r.id}
              onClick={() => navigate({ name: "run-detail", id: r.id })}
              className="grid min-w-[1040px] w-full items-center gap-2 border-b border-border px-3 py-1.5 text-left hover:bg-accent/40"
              style={{
                gridTemplateColumns:
                  "minmax(220px,1.4fr) minmax(140px,0.8fr) 100px 130px 90px 80px 100px 90px 24px",
              }}
            >
              <div className="flex min-w-0 items-center gap-2">
                <Activity className="h-3 w-3 shrink-0 text-muted-foreground" />
                <span className="truncate font-mono text-[11.5px]">{formatReadableLabel(r.experiment_name)}</span>
              </div>
              <span className="truncate font-mono text-[11px] text-muted-foreground">{formatReadableToken(r.variant)}</span>
              <StatusPill status={r.status} />
              <RunEvidenceCell
                summary={evidenceByRun.get(r.id)}
                status={r.status}
                telemetryError={telemetryError}
              />
              <span className="font-mono text-[11px] text-muted-foreground">{formatDuration(r.duration_ms)}</span>
              <span className="font-mono text-[11px] text-muted-foreground">{formatUsd(Number(r.cost_usd))}</span>
              <span className="font-mono text-[11px] text-muted-foreground">{r.region}</span>
              <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(r.started_at ?? r.created_at)}</span>
              <ArrowUpRight className="h-3 w-3 text-muted-foreground" />
            </button>
          ))}
        </div>
      ) : null}
    </div>
  )
}

function Stat({ label, value, detail, dot }: { label: string; value: string; detail?: string; dot: string }) {
  return (
    <div className="flex items-center justify-between bg-background p-2.5">
      <div className="flex items-center gap-1.5 text-[10.5px] uppercase tracking-wide text-muted-foreground">
        <span className={cn("inline-block h-1.5 w-1.5 rounded-full", dot)} />
        {label}
      </div>
      <div className="text-right">
        <div className="font-mono text-[16px] font-medium">{value}</div>
        {detail ? (
          <div className="font-mono text-[10px] text-muted-foreground">{detail}</div>
        ) : null}
      </div>
    </div>
  )
}

function RunsSkeleton() {
  return (
    <div className="overflow-x-auto border-b border-border bg-background p-3">
      {[0, 1, 2, 3, 4].map((row) => (
        <div key={row} className="mb-2 grid min-w-[700px] grid-cols-[1.5fr_1fr_90px_80px_80px] gap-3">
          <div className="h-3 rounded bg-muted" />
          <div className="h-3 rounded bg-muted/80" />
          <div className="h-3 rounded bg-muted/70" />
          <div className="h-3 rounded bg-muted/60" />
          <div className="h-3 rounded bg-muted/60" />
        </div>
      ))}
    </div>
  )
}

function RunsEmptyState({
  title,
  detail,
  primary,
  onPrimary,
}: {
  title: string
  detail: string
  primary: string
  onPrimary: () => void
}) {
  return (
    <div className="flex min-h-[300px] flex-col items-center justify-center gap-2 border-b border-border bg-background px-4 py-12 text-center">
      <CircleAlert className="h-5 w-5 text-muted-foreground" />
      <div className="text-[14px] font-medium">{title}</div>
      <div className="max-w-[calc(100vw-7rem)] text-[12px] text-muted-foreground sm:max-w-sm">{detail}</div>
      <Button size="sm" className="mt-2 h-7 gap-1 bg-brand text-[12px] text-brand-foreground hover:bg-brand/90" onClick={onPrimary}>
        <Plus className="h-3 w-3" />
        {primary}
      </Button>
    </div>
  )
}

type RunEvidenceSummary = {
  metrics: number
  traces: number
  errors: number
  warnings: number
  latest: string | null
}

function RunEvidenceCell({
  summary,
  status,
  telemetryError,
}: {
  summary?: RunEvidenceSummary
  status: Run["status"]
  telemetryError: string | null
}) {
  if (telemetryError) {
    return (
      <span className="inline-flex h-6 min-w-0 items-center gap-1 rounded border border-warning/30 bg-warning/10 px-1.5 text-[10.5px] text-warning">
        telemetry unavailable
      </span>
    )
  }

  const metrics = summary?.metrics ?? 0
  const traces = summary?.traces ?? 0
  const errors = summary?.errors ?? 0
  const warnings = summary?.warnings ?? 0
  const hasEvidence = metrics > 0 || traces > 0
  const tone = errors > 0 || status === "failed"
    ? "danger"
    : warnings > 0
      ? "warning"
      : hasEvidence
        ? "success"
        : status === "running" || status === "queued"
          ? "info"
          : "muted"
  const label = hasEvidence
    ? `${metrics}m / ${traces}t`
    : status === "running" || status === "queued"
      ? "pending"
      : "no evidence"
  const detail = errors > 0
    ? `${errors} err`
    : warnings > 0
      ? `${warnings} warn`
      : summary?.latest
        ? formatRelative(summary.latest)
        : ""

  return (
    <span
      className={cn(
        "inline-flex h-6 min-w-0 max-w-full items-center justify-between gap-1 rounded border px-1.5 font-mono text-[10.5px]",
        evidenceCellClass(tone),
      )}
      title={`${metrics} metric rows, ${traces} trace events${summary?.latest ? `, latest ${formatRelative(summary.latest)}` : ""}`}
    >
      <span className="truncate">{label}</span>
      {detail ? <span className="shrink-0 opacity-75">{detail}</span> : null}
    </span>
  )
}

function evidenceCellClass(tone: "success" | "warning" | "danger" | "info" | "muted") {
  if (tone === "success") return "border-success/25 bg-success/10 text-success"
  if (tone === "warning") return "border-warning/30 bg-warning/10 text-warning"
  if (tone === "danger") return "border-destructive/30 bg-destructive/10 text-destructive"
  if (tone === "info") return "border-info/25 bg-info/10 text-info"
  return "border-border bg-muted/40 text-muted-foreground"
}

function filterOptions(values: string[], allLabel: string) {
  const counts = new Map<string, number>()
  values.filter(Boolean).forEach((value) => {
    counts.set(value, (counts.get(value) ?? 0) + 1)
  })
  return [
    { value: "all", label: allLabel, count: values.length },
    ...Array.from(counts.entries())
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .map(([value, count]) => ({ value, label: value, count })),
  ]
}

function buildRunEvidenceMap(metrics: RunMetric[], traces: Trace[]) {
  const map = new Map<string, RunEvidenceSummary>()
  metrics.forEach((metric) => {
    const prev = map.get(metric.run_id) ?? { metrics: 0, traces: 0, errors: 0, warnings: 0, latest: null }
    prev.metrics += 1
    prev.latest = newestDate(prev.latest, metric.recorded_at)
    map.set(metric.run_id, prev)
  })
  traces.forEach((trace) => {
    const prev = map.get(trace.run_id) ?? { metrics: 0, traces: 0, errors: 0, warnings: 0, latest: null }
    prev.traces += 1
    if (trace.level === "error") prev.errors += 1
    if (trace.level === "warn") prev.warnings += 1
    prev.latest = newestDate(prev.latest, trace.recorded_at)
    map.set(trace.run_id, prev)
  })
  return map
}

function newestDate(left: string | null, right: string) {
  if (!left) return right
  return (Date.parse(right) || 0) > (Date.parse(left) || 0) ? right : left
}

function buildRunActivity(runs: Run[]) {
  const days = Array.from({ length: 14 }, (_, offset) => {
    const date = new Date()
    date.setHours(0, 0, 0, 0)
    date.setDate(date.getDate() - (13 - offset))
    const key = date.toISOString().slice(0, 10)
    return {
      key,
      day: date.toLocaleDateString(undefined, { month: "numeric", day: "numeric" }),
      succeeded: 0,
      failed: 0,
      running: 0,
      queued: 0,
    }
  })
  const byDay = new Map(days.map((day) => [day.key, day]))
  runs.forEach((run) => {
    const date = new Date(run.started_at ?? run.created_at)
    if (Number.isNaN(date.getTime())) return
    const key = date.toISOString().slice(0, 10)
    const bucket = byDay.get(key)
    if (!bucket) return
    if (run.status === "succeeded") bucket.succeeded += 1
    else if (run.status === "failed") bucket.failed += 1
    else if (run.status === "running") bucket.running += 1
    else if (run.status === "queued") bucket.queued += 1
  })
  return days
}

const eventCfg: ChartConfig = {
  count: { label: "Events", color: "var(--chart-1)" },
}

const runActivityCfg: ChartConfig = {
  succeeded: { label: "Succeeded", color: "var(--success)" },
  failed: { label: "Failed", color: "var(--destructive)" },
  running: { label: "Running", color: "var(--info)" },
  queued: { label: "Queued", color: "var(--muted-foreground)" },
}

const pressureCfg: ChartConfig = {
  failed: { label: "Failed runs", color: "var(--destructive)" },
  errors: { label: "Error events", color: "var(--chart-5)" },
  warnings: { label: "Warnings", color: "var(--warning)" },
}

const latencyCfg: ChartConfig = {
  runs: { label: "Runs", color: "var(--chart-2)" },
}

type RunDetailTab = "traces" | "metrics" | "config" | "logs"
type RunDecisionTone = "success" | "warning" | "danger" | "info" | "muted"
type RunDecisionBrief = {
  tone: RunDecisionTone
  verdict: string
  detail: string
  action: string
  tab: RunDetailTab
  facts: { label: string; value: string; tone?: RunDecisionTone }[]
}

function buildErrorPressure(runs: Run[], traces: Trace[]) {
  const days = Array.from({ length: 14 }, (_, offset) => {
    const date = new Date()
    date.setHours(0, 0, 0, 0)
    date.setDate(date.getDate() - (13 - offset))
    const key = date.toISOString().slice(0, 10)
    return {
      key,
      day: date.toLocaleDateString(undefined, { month: "numeric", day: "numeric" }),
      failed: 0,
      errors: 0,
      warnings: 0,
    }
  })
  const byDay = new Map(days.map((day) => [day.key, day]))
  runs.forEach((run) => {
    if (run.status !== "failed") return
    const key = dayKey(run.ended_at ?? run.started_at ?? run.created_at)
    if (!key) return
    const bucket = byDay.get(key)
    if (bucket) bucket.failed += 1
  })
  traces.forEach((trace) => {
    if (trace.level !== "error" && trace.level !== "warn") return
    const key = dayKey(trace.recorded_at)
    if (!key) return
    const bucket = byDay.get(key)
    if (!bucket) return
    if (trace.level === "error") bucket.errors += 1
    else bucket.warnings += 1
  })
  return days
}

function buildLatencyHistogram(runs: Run[], metrics: RunMetric[], traces: Trace[]) {
  const rows = [
    { bucket: "<1s", runs: 0 },
    { bucket: "1-10s", runs: 0 },
    { bucket: "10-60s", runs: 0 },
    { bucket: "1-5m", runs: 0 },
    { bucket: "5m+", runs: 0 },
  ]
  const latencyByRun = latencySignalByRun(metrics, traces)
  runs.forEach((run) => {
    const latency = latencyByRun.get(run.id) ?? (run.duration_ms > 0 ? run.duration_ms : 0)
    if (!latency) return
    const row = latency < 1_000 ? rows[0] : latency < 10_000 ? rows[1] : latency < 60_000 ? rows[2] : latency < 300_000 ? rows[3] : rows[4]
    row.runs += 1
  })
  return rows
}

function buildAttentionRows(runs: Run[], metrics: RunMetric[], traces: Trace[]) {
  const latencyByRun = latencySignalByRun(metrics, traces)
  const eventsByRun = traces.reduce((acc, trace) => {
    const prev = acc.get(trace.run_id) ?? { errors: 0, warnings: 0 }
    if (trace.level === "error") prev.errors += 1
    if (trace.level === "warn") prev.warnings += 1
    acc.set(trace.run_id, prev)
    return acc
  }, new Map<string, { errors: number; warnings: number }>())

  return runs
    .map((run) => {
      const events = eventsByRun.get(run.id) ?? { errors: 0, warnings: 0 }
      const latency = latencyByRun.get(run.id) ?? run.duration_ms
      const failed = run.status === "failed"
      const score = (failed ? 6 : 0) + events.errors * 3 + events.warnings + (latency > 300_000 ? 2 : latency > 60_000 ? 1 : 0)
      const reason = failed
        ? "Failed run"
        : events.errors
          ? `${events.errors} error event${events.errors === 1 ? "" : "s"}`
          : events.warnings
            ? `${events.warnings} warning event${events.warnings === 1 ? "" : "s"}`
            : latency > 60_000
              ? `Slow signal ${formatDuration(latency)}`
              : ""
      return {
        run,
        reason,
        rawScore: score,
        score: score ? `p${Math.min(99, score * 10)}` : "",
        tone: failed || events.errors ? "bg-destructive/10 text-destructive" : events.warnings ? "bg-warning/10 text-warning" : "bg-info/10 text-info",
      }
    })
    .filter((row) => row.rawScore > 0)
    .sort((a, b) => b.rawScore - a.rawScore || Date.parse(b.run.created_at) - Date.parse(a.run.created_at))
}

function latencySignalByRun(metrics: RunMetric[], traces: Trace[]) {
  const out = new Map<string, number>()
  metrics.forEach((metric) => {
    const name = metric.name.toLowerCase()
    if (!name.includes("latency") && !name.includes("duration")) return
    const value = Number(metric.value)
    if (!Number.isFinite(value) || value <= 0) return
    const ms = metric.unit === "s" || metric.unit === "sec" || metric.unit === "seconds" ? value * 1000 : value
    out.set(metric.run_id, Math.max(out.get(metric.run_id) ?? 0, ms))
  })
  traces.forEach((trace) => {
    if (!trace.latency_ms) return
    out.set(trace.run_id, Math.max(out.get(trace.run_id) ?? 0, trace.latency_ms))
  })
  return out
}

function dayKey(value: string | null | undefined) {
  if (!value) return ""
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? "" : date.toISOString().slice(0, 10)
}

function HealthPanel({
  title,
  subtitle,
  empty,
  emptyDetail,
  children,
}: {
  title: string
  subtitle: string
  empty: boolean
  emptyDetail: string
  children: React.ReactNode
}) {
  return (
    <div className="bg-background">
      <header className="flex h-8 items-center justify-between border-b border-border px-3">
        <h3 className="text-[12px] font-semibold">{title}</h3>
        <span className="truncate text-[11px] text-muted-foreground">{subtitle}</span>
      </header>
      {empty ? (
        <div className="flex h-44 items-center justify-center px-4 text-center text-[12px] text-muted-foreground">
          {emptyDetail}
        </div>
      ) : children}
    </div>
  )
}

function RunDecisionBriefView({
  brief,
  onSelectTab,
}: {
  brief: RunDecisionBrief
  onSelectTab: (tab: RunDetailTab) => void
}) {
  return (
    <section className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(260px,0.95fr)_minmax(0,1.4fr)]">
      <div className="min-w-0 bg-background p-3">
        <div className="flex items-center gap-2">
          <span className={cn("h-2 w-2 rounded-full", decisionDotClass(brief.tone))} />
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Operational brief</div>
            <div className={cn("mt-0.5 truncate text-[14px] font-medium", decisionTextClass(brief.tone))}>
              {brief.verdict}
            </div>
          </div>
        </div>
        <p className="mt-2 line-clamp-2 text-[11.5px] leading-relaxed text-muted-foreground">
          {brief.detail}
        </p>
        <Button
          variant="outline"
          size="sm"
          className="mt-3 h-7 w-full justify-between gap-2 text-[12px] sm:w-auto"
          onClick={() => onSelectTab(brief.tab)}
        >
          {brief.action}
          <ArrowUpRight className="h-3 w-3" />
        </Button>
      </div>
      <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
        {brief.facts.map((fact) => (
          <div key={fact.label} className="min-w-0 bg-background px-3 py-2.5">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
            <div
              className={cn("mt-0.5 truncate font-mono text-[12px]", fact.tone ? decisionTextClass(fact.tone) : "")}
              title={fact.value}
            >
              {fact.value}
            </div>
          </div>
        ))}
      </div>
    </section>
  )
}

export function RunDetailPage() {
  const { route, navigate } = useRouter()
  const id = route.name === "run-detail" ? route.id : ""
  const [run, setRun] = useState<Run | null>(null)
  const [runLoaded, setRunLoaded] = useState(false)
  const [runError, setRunError] = useState<string | null>(null)
  const [metrics, setMetrics] = useState<RunMetric[]>([])
  const [traces, setTraces] = useState<Trace[]>([])
  const [evidenceError, setEvidenceError] = useState<string | null>(null)
  const [tab, setTab] = useState<RunDetailTab>("traces")
  const [actionNotice, setActionNotice] = useState<string | null>(null)

  async function loadRunDetail() {
    if (!id) return
    setRun(null)
    setRunLoaded(false)
    setRunError(null)
    setEvidenceError(null)
    setMetrics([])
    setTraces([])
    const { data, error } = await cloudApi
      .from("runs")
      .select("*")
      .eq("id", id)
      .maybeSingle()
    if (error) {
      setRun(null)
      setRunError(error.message)
      setRunLoaded(true)
      return
    }
    setRun((data as Run) ?? null)
    setRunLoaded(true)
    const [metricRows, traceRows] = await Promise.all([
      cloudApi
        .from("run_metrics")
        .select("*")
        .eq("run_id", id)
        .order("step", { ascending: true }),
      cloudApi
        .from("traces")
        .select("*")
        .eq("run_id", id)
        .order("recorded_at", { ascending: true }),
    ])
    setMetrics(metricRows.error ? [] : ((metricRows.data ?? []) as RunMetric[]))
    setTraces(traceRows.error ? [] : ((traceRows.data ?? []) as Trace[]))
    setEvidenceError(metricRows.error?.message ?? traceRows.error?.message ?? null)
  }

  useEffect(() => {
    void loadRunDetail()
  }, [id])

  const metricPanels = useMemo(() => buildMetricPanels(metrics), [metrics])
  const insight = useMemo(() => runInsight(run, metrics, traces), [run, metrics, traces])
  const decisionBrief = useMemo(
    () => buildRunDecisionBrief(run, metrics, traces, evidenceError),
    [run, metrics, traces, evidenceError],
  )

  function copyReplayCommand() {
    if (!run) return
    copyToClipboard(runReplayCommand(run))
    setActionNotice("Replay command copied.")
  }

  function exportEvidence() {
    if (!run) return
    exportRunEvidence(run, metrics, traces)
    setActionNotice("Runtime evidence CSV downloaded.")
  }

  if (!run && !runLoaded) {
    return <div className="p-6 text-[12px] text-muted-foreground">Loading…</div>
  }

  if (runError) {
    return (
      <ConnectionIssue
        title="Run request failed"
        detail={runError}
        onSettings={() => navigate({ name: "settings" })}
        onRetry={() => void loadRunDetail()}
      />
    )
  }

  if (!run) {
    return (
      <MissingDetail
        title="Run not found"
        detail="This run id is not present in the current cloud API response."
        action="All runs"
        onAction={() => navigate({ name: "runs" })}
      />
    )
  }

  const isRunning = run.status === "running"

  return (
    <div className="flex flex-col">
      <PageHeader
        title={`${formatReadableLabel(run.experiment_name)} · ${formatReadableToken(run.variant)}`}
        subtitle={
          isRunning
            ? "Live run. Traces stream in below."
            : `Completed ${formatRelative(run.ended_at)} · ${formatDuration(run.duration_ms)}`
        }
        rightSlot={<Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]" onClick={() => navigate({ name: "runs" })}>All runs</Button>}
        primaryAction={
          <div className="flex items-center gap-1.5">
            <Button
              variant="outline"
              size="sm"
              className="h-7 gap-1 text-[12px]"
              onClick={copyReplayCommand}
            >
              <Copy className="h-3 w-3" /> Replay command
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="h-7 gap-1 text-[12px]"
              onClick={exportEvidence}
            >
              <Download className="h-3 w-3" /> Evidence
            </Button>
          </div>
        }
      />

      {actionNotice ? (
        <div className="flex items-center justify-between border-b border-border bg-success/10 px-3 py-1.5 text-[12px] text-success">
          <span>{actionNotice}</span>
          <button
            className="rounded px-1 text-[11px] text-success/80 hover:bg-success/10 hover:text-success"
            onClick={() => setActionNotice(null)}
            aria-label="Dismiss run action notice"
          >
            dismiss
          </button>
        </div>
      ) : null}

      <div className="grid grid-cols-2 gap-px border-b border-border bg-border md:grid-cols-6">
        <KV label="Status" value={<StatusPill status={run.status} />} />
        <KV label="Variant" value={formatReadableToken(run.variant)} mono />
        <KV label="Region" value={run.region} mono />
        <KV label="Duration" value={formatDuration(run.duration_ms)} mono />
        <KV label="Cost" value={formatUsd(Number(run.cost_usd))} mono />
        <KV label="Started" value={formatRelative(run.started_at)} mono />
      </div>

      <div className="grid grid-cols-2 gap-px border-b border-border bg-border md:grid-cols-6">
        <MiniKV label="Quality" value={evidenceError ? "unavailable" : insight.pass == null ? "no rows" : `${(insight.pass * 100).toFixed(1)}%`} />
        <MiniKV label="Latency" value={evidenceError ? "unavailable" : insight.latency == null ? "no rows" : formatDuration(insight.latency)} />
        <MiniKV label="Tokens" value={evidenceError ? "unavailable" : insight.tokens == null ? "no rows" : `${Math.round(insight.tokens)}`} />
        <MiniKV label="Metric rows" value={evidenceError ? "-" : `${metrics.length}`} />
        <MiniKV label="Trace events" value={evidenceError ? "-" : `${traces.length}`} />
        <MiniKV label="Slowest span" value={evidenceError ? "request failed" : insight.slowestSpan} />
      </div>

      <RunDecisionBriefView brief={decisionBrief} onSelectTab={setTab} />

      {evidenceError ? (
        <ConnectionIssue
          title="Run evidence request failed"
          detail={evidenceError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadRunDetail()}
          compact
        />
      ) : null}

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(0,1fr)_320px]">
        <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-3">
          {metricPanels.map((panel) => (
            <MetricCard
              key={panel.id}
              panel={panel}
              hasData={!evidenceError && panel.rows.length > 0}
              evidenceError={evidenceError}
            />
          ))}
        </div>

        <Card title="Event mix" badge="trace">
          {!evidenceError && insight.eventMix.some((row) => row.count > 0) ? (
            <ChartContainer config={eventCfg} className="h-40 w-full">
              <BarChart data={insight.eventMix} layout="vertical" margin={{ left: 4, right: 12, top: 8, bottom: 4 }}>
                <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis type="number" hide />
                <YAxis
                  type="category"
                  dataKey="level"
                  width={56}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip contentStyle={tooltipStyle} />
                <Bar dataKey="count" fill="var(--color-count)" radius={[0, 2, 2, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          ) : (
            <EmptyMetric detail={evidenceError ? "Trace events are unavailable for this request." : "Trace events will appear here as the runtime reports worker activity."} />
          )}
        </Card>
      </div>

      <div className="border-b border-border">
        <nav className="flex h-9 items-center gap-1 overflow-x-auto px-2 scrollbar-thin">
          {(["traces", "metrics", "config", "logs"] as const).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              className={cn(
                "rounded-md px-2 py-1 text-[12px] capitalize",
                tab === t
                  ? "bg-secondary text-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {t}
              {t === "traces" && isRunning ? (
                <span className="ml-1.5 inline-block h-1.5 w-1.5 rounded-full bg-info animate-pulse" />
              ) : null}
            </button>
          ))}
        </nav>
      </div>

      {tab === "traces" ? <TracesView traces={traces} evidenceError={evidenceError} /> : null}
      {tab === "metrics" ? <MetricsView metrics={metrics} evidenceError={evidenceError} /> : null}
      {tab === "config" ? (
        <ConfigView run={run} metrics={metrics} traces={traces} />
      ) : null}
      {tab === "logs" ? <LogsView traces={traces} evidenceError={evidenceError} /> : null}
    </div>
  )
}

function MetricsView({ metrics, evidenceError }: { metrics: RunMetric[]; evidenceError: string | null }) {
  if (evidenceError) {
    return (
      <EmptyState
        icon={CircleAlert}
        title="Metric request failed"
        detail={evidenceError}
      />
    )
  }

  if (metrics.length === 0) {
    return (
      <EmptyState
        icon={Activity}
        title="No metric rows"
        detail="This run has not emitted runtime metric observations yet."
      />
    )
  }

  return (
    <div className="overflow-x-auto text-[12px]">
      <div
        className="grid min-w-[560px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
        style={{
          gridTemplateColumns: "minmax(120px,1fr) 80px 80px 80px 100px",
        }}
      >
        <span>Metric</span>
        <span>Step</span>
        <span>Value</span>
        <span>Unit</span>
        <span>Recorded</span>
      </div>
      {metrics.slice(-60).map((m) => (
        <div
          key={m.id}
          className="grid min-w-[560px] items-center gap-2 border-b border-border px-3 py-1 hover:bg-accent/40"
          style={{
            gridTemplateColumns: "minmax(120px,1fr) 80px 80px 80px 100px",
          }}
        >
          <span className="font-mono text-[11.5px]">{m.name}</span>
          <span className="font-mono text-[11px] text-muted-foreground">{m.step}</span>
          <span className="font-mono text-[11.5px]">{Number(m.value).toFixed(3)}</span>
          <span className="font-mono text-[11px] text-muted-foreground">{m.unit}</span>
          <span className="font-mono text-[11px] text-muted-foreground">
            {formatRelative(m.recorded_at)}
          </span>
        </div>
      ))}
    </div>
  )
}

function TracesView({ traces, evidenceError }: { traces: Trace[]; evidenceError: string | null }) {
  const [open, setOpen] = useState<Set<string>>(new Set())
  const grouped = useMemo(() => groupTraces(traces), [traces])
  if (evidenceError) {
    return (
      <EmptyState
        icon={CircleAlert}
        title="Trace request failed"
        detail={evidenceError}
      />
    )
  }

  if (grouped.length === 0) {
    return (
      <EmptyState
        icon={Info}
        title="No trace events"
        detail="Runtime events will appear here as the worker reports progress."
      />
    )
  }

  return (
    <div className="overflow-x-auto text-[12px]">
      {grouped.map((g, i) => {
        const isOpen = open.has(g.id)
        return (
          <div key={g.id + i} className="min-w-[760px] border-b border-border">
            <button
              onClick={() => {
                const n = new Set(open)
                if (n.has(g.id)) n.delete(g.id)
                else n.add(g.id)
                setOpen(n)
              }}
              className="grid w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-accent/40"
              style={{
                gridTemplateColumns:
                  "16px 16px minmax(120px,0.6fr) 60px minmax(220px,1.4fr) 70px 80px",
              }}
            >
              <ChevronRight
                className={cn(
                  "h-3 w-3 transition-transform text-muted-foreground",
                  isOpen ? "rotate-90" : "",
                )}
              />
              <LevelIcon level={g.level} />
              <span className="truncate font-mono text-[11.5px]">{formatReadableLabel(g.span)}</span>
              <span className="font-mono text-[10.5px] text-muted-foreground">
                {g.children.length}x
              </span>
              <span className="truncate text-[11.5px] text-muted-foreground">
                {formatReadableLabel(g.message)}
              </span>
              <span className="font-mono text-[11px] text-muted-foreground">
                {g.latency_ms}ms
              </span>
              <span className="font-mono text-[11px] text-muted-foreground">
                {formatRelative(g.recorded_at)}
              </span>
            </button>
            {isOpen ? (
              <div className="ml-6 border-l border-border bg-card/40">
                {g.children.map((c) => (
                  <div
                    key={c.id}
                    className="grid items-center gap-2 px-3 py-1 hover:bg-accent/40"
                    style={{
                      gridTemplateColumns: "16px minmax(120px,0.6fr) minmax(220px,1.4fr) 70px 80px",
                    }}
                  >
                    <LevelIcon level={c.level} />
                    <span className="truncate font-mono text-[11px] text-muted-foreground">
                      {formatReadableLabel(c.span)}
                    </span>
                    <span className="truncate text-[11px]">{formatReadableLabel(c.message)}</span>
                    <span className="font-mono text-[10.5px] text-muted-foreground">
                      {c.latency_ms}ms
                    </span>
                    <span className="font-mono text-[10.5px] text-muted-foreground">
                      {formatRelative(c.recorded_at)}
                    </span>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
        )
      })}
    </div>
  )
}

function LogsView({ traces, evidenceError }: { traces: Trace[]; evidenceError: string | null }) {
  if (evidenceError) {
    return (
      <EmptyState
        icon={CircleAlert}
        title="Event log request failed"
        detail={evidenceError}
      />
    )
  }

  if (traces.length === 0) {
    return (
      <EmptyState
        icon={Info}
        title="No event log"
        detail="No worker log events have been recorded for this run."
      />
    )
  }

  return (
    <div className="overflow-x-auto bg-card/30 text-[12px]">
      {traces.map((trace) => (
        <div
          key={trace.id}
          className="grid min-w-[640px] items-center gap-2 border-b border-border px-3 py-1 font-mono"
          style={{
            gridTemplateColumns: "90px 64px minmax(120px,0.6fr) minmax(220px,1.4fr) 74px",
          }}
        >
          <span className="text-[10.5px] text-muted-foreground">
            {new Date(trace.recorded_at).toLocaleTimeString()}
          </span>
          <span
            className={cn(
              "w-fit rounded px-1 text-[10px]",
              trace.level === "error"
                ? "bg-destructive/10 text-destructive"
                : trace.level === "warn"
                  ? "bg-warning/10 text-warning"
                  : "bg-info/10 text-info",
            )}
          >
            {trace.level}
          </span>
          <span className="truncate text-[11px] text-muted-foreground">{formatReadableLabel(trace.span)}</span>
          <span className="truncate text-[11.5px]">{formatReadableLabel(trace.message)}</span>
          <span className="text-right text-[10.5px] text-muted-foreground">
            {trace.latency_ms ? `${trace.latency_ms}ms` : "—"}
          </span>
        </div>
      ))}
    </div>
  )
}

function ConfigView({
  run,
  metrics,
  traces,
}: {
  run: Run
  metrics: RunMetric[]
  traces: Trace[]
}) {
  const [showRaw, setShowRaw] = useState(false)
  const metricRows = metricInventory(metrics)
  const hotSpans = traceInventory(traces)
  const latestEvent = [...traces].sort((a, b) => Date.parse(b.recorded_at) - Date.parse(a.recorded_at))[0]
  const errorEvents = traces.filter((trace) => trace.level === "error").length
  const warningEvents = traces.filter((trace) => trace.level === "warn").length
  const command = runReplayCommand(run)
  const payload = {
    run,
    observed: {
      metric_rows: metrics.length,
      trace_events: traces.length,
      error_events: errorEvents,
      warning_events: warningEvents,
    },
  }

  return (
    <div className="grid grid-cols-1 gap-px bg-border xl:grid-cols-[360px_minmax(0,1fr)]">
      <div className="grid gap-px bg-border">
        <section className="bg-background p-3">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Run snapshot</div>
              <div className="mt-1 max-w-full truncate text-[13px] font-medium">
                {formatReadableLabel(run.experiment_name)}
              </div>
            </div>
            <StatusPill status={run.status} />
          </div>
          <div className="mt-3 grid grid-cols-2 gap-px bg-border">
            <ConfigFact label="Variant" value={formatReadableToken(run.variant)} />
            <ConfigFact label="Region" value={run.region} />
            <ConfigFact label="Duration" value={formatDuration(run.duration_ms)} />
            <ConfigFact label="Cost" value={formatUsd(Number(run.cost_usd))} />
            <ConfigFact label="Started" value={formatRelative(run.started_at ?? run.created_at)} />
            <ConfigFact label="Finished" value={run.ended_at ? formatRelative(run.ended_at) : "open"} />
          </div>
        </section>

        <section className="bg-background p-3">
          <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Evidence health</div>
          <div className="mt-2 grid grid-cols-2 gap-px bg-border">
            <MiniKV label="Metrics" value={`${metrics.length}`} />
            <MiniKV label="Events" value={`${traces.length}`} />
            <MiniKV label="Errors" value={`${errorEvents}`} />
            <MiniKV label="Warnings" value={`${warningEvents}`} />
          </div>
          <div className="mt-2 text-[11px] leading-relaxed text-muted-foreground">
            {latestEvent
              ? `Latest event ${formatRelative(latestEvent.recorded_at)} from ${formatReadableLabel(latestEvent.span)}.`
              : "No runtime event stream has been attached to this run yet."}
          </div>
        </section>

        <section className="bg-background p-3">
          <div className="mb-2 flex items-center justify-between">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Replay</div>
            <Button
              variant="ghost"
              size="sm"
              className="h-6 gap-1 text-[11px]"
              onClick={() => copyToClipboard(command)}
            >
              <Copy className="h-3 w-3" /> Copy
            </Button>
          </div>
          <pre className="overflow-x-auto whitespace-pre-wrap rounded border border-border bg-card/60 p-2 font-mono text-[11px] leading-relaxed text-muted-foreground">
            {command}
          </pre>
        </section>
      </div>

      <div className="grid gap-px bg-border">
        <section className="bg-background">
          <ConfigHeader
            title="Metric inventory"
            detail={`${metricRows.length} signal${metricRows.length === 1 ? "" : "s"}`}
          />
          {metricRows.length ? (
            <div className="overflow-x-auto">
              <div
                className="grid min-w-[620px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
                style={{ gridTemplateColumns: "minmax(150px,1fr) 70px 90px 80px 110px" }}
              >
                <span>Signal</span>
                <span>Rows</span>
                <span>Latest</span>
                <span>Step</span>
                <span>Recorded</span>
              </div>
              {metricRows.map((metric) => (
                <div
                  key={metric.name}
                  className="grid min-w-[620px] items-center gap-2 border-b border-border px-3 py-1 hover:bg-accent/40"
                  style={{ gridTemplateColumns: "minmax(150px,1fr) 70px 90px 80px 110px" }}
                >
                  <span className="truncate font-mono text-[11.5px]">{formatReadableToken(metric.name)}</span>
                  <span className="font-mono text-[11px] text-muted-foreground">{metric.rows}</span>
                  <span className="font-mono text-[11.5px]">{metric.value}</span>
                  <span className="font-mono text-[11px] text-muted-foreground">{metric.step}</span>
                  <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(metric.recorded_at)}</span>
                </div>
              ))}
            </div>
          ) : (
            <div className="px-3 py-8 text-center text-[12px] text-muted-foreground">
              Metric observations will appear here once the runtime reports evaluation rows.
            </div>
          )}
        </section>

        <section className="bg-background">
          <ConfigHeader
            title="Slow spans"
            detail={`${hotSpans.length} trace${hotSpans.length === 1 ? "" : "s"}`}
          />
          {hotSpans.length ? (
            <div className="overflow-x-auto">
              <div
                className="grid min-w-[620px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
                style={{ gridTemplateColumns: "16px minmax(150px,0.8fr) minmax(220px,1.2fr) 80px 110px" }}
              >
                <span></span>
                <span>Span</span>
                <span>Message</span>
                <span>Latency</span>
                <span>Recorded</span>
              </div>
              {hotSpans.map((trace) => (
                <div
                  key={trace.id}
                  className="grid min-w-[620px] items-center gap-2 border-b border-border px-3 py-1 hover:bg-accent/40"
                  style={{ gridTemplateColumns: "16px minmax(150px,0.8fr) minmax(220px,1.2fr) 80px 110px" }}
                >
                  <LevelIcon level={trace.level} />
                  <span className="truncate font-mono text-[11px] text-muted-foreground">{formatReadableLabel(trace.span)}</span>
                  <span className="truncate text-[11.5px]">{formatReadableLabel(trace.message)}</span>
                  <span className="font-mono text-[11px] text-muted-foreground">{trace.latency_ms}ms</span>
                  <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(trace.recorded_at)}</span>
                </div>
              ))}
            </div>
          ) : (
            <div className="px-3 py-8 text-center text-[12px] text-muted-foreground">
              Trace latency will appear here after worker events are linked to this run.
            </div>
          )}
        </section>

        <section className="bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <div>
              <div className="text-[12px] font-semibold">Raw snapshot</div>
              <div className="text-[10.5px] text-muted-foreground">Run plus observed evidence counts</div>
            </div>
            <Button
              variant="ghost"
              size="sm"
              className="h-6 text-[11px]"
              onClick={() => setShowRaw((value) => !value)}
            >
              {showRaw ? "Hide raw" : "Show raw"}
            </Button>
          </header>
          {showRaw ? (
            <pre className="max-h-80 max-w-full overflow-auto p-3 font-mono text-[11px] leading-relaxed text-muted-foreground">
              {JSON.stringify(payload, null, 2)}
            </pre>
          ) : null}
        </section>
      </div>
    </div>
  )
}

function ConfigHeader({ title, detail }: { title: string; detail: string }) {
  return (
    <header className="flex h-9 items-center justify-between border-b border-border px-3">
      <h3 className="text-[12px] font-semibold">{title}</h3>
      <span className="font-mono text-[11px] text-muted-foreground">{detail}</span>
    </header>
  )
}

function ConfigFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-card px-2 py-1.5">
      <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="truncate font-mono text-[12px] text-foreground" title={value}>{value}</div>
    </div>
  )
}

function metricInventory(metrics: RunMetric[]) {
  const rows = new Map<string, { name: string; rows: number; value: string; step: number; recorded_at: string }>()
  metrics.forEach((metric) => {
    const recorded = Date.parse(metric.recorded_at) || 0
    const prev = rows.get(metric.name)
    const prevRecorded = prev ? Date.parse(prev.recorded_at) || 0 : -1
    rows.set(metric.name, {
      name: metric.name,
      rows: (prev?.rows ?? 0) + 1,
      value: recorded >= prevRecorded ? metricDisplayValue(metric) : prev?.value ?? metricDisplayValue(metric),
      step: recorded >= prevRecorded ? metric.step : prev?.step ?? metric.step,
      recorded_at: recorded >= prevRecorded ? metric.recorded_at : prev?.recorded_at ?? metric.recorded_at,
    })
  })
  return Array.from(rows.values())
    .sort((a, b) => b.rows - a.rows || Date.parse(b.recorded_at) - Date.parse(a.recorded_at) || a.name.localeCompare(b.name))
    .slice(0, 8)
}

function metricDisplayValue(metric: RunMetric) {
  const value = Number(metric.value)
  if (!Number.isFinite(value)) return "—"
  const unit = metric.unit ? ` ${metric.unit}` : ""
  if (/percent|rate|ratio|pass|score/i.test(`${metric.name} ${metric.unit}`) && Math.abs(value) <= 1) {
    return `${(value * 100).toFixed(1)}%`
  }
  if (/latency|duration|ms/i.test(`${metric.name} ${metric.unit}`)) return formatDuration(value)
  return `${value >= 1000 ? value.toLocaleString() : value.toFixed(value % 1 ? 3 : 0)}${unit}`
}

function traceInventory(traces: Trace[]) {
  return [...traces]
    .sort((a, b) => b.latency_ms - a.latency_ms || Date.parse(b.recorded_at) - Date.parse(a.recorded_at))
    .slice(0, 8)
}

function MiniKV({ label, value }: { label: string; value: string }) {
  return (
    <div className="bg-card px-2 py-2">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="font-mono text-[16px] font-medium">{value}</div>
    </div>
  )
}

function EmptyState({
  icon: Icon,
  title,
  detail,
}: {
  icon: React.ComponentType<{ className?: string }>
  title: string
  detail: string
}) {
  return (
    <div className="flex min-h-[220px] flex-col items-center justify-center gap-2 px-4 py-10 text-center">
      <Icon className="h-5 w-5 text-muted-foreground" />
      <div className="text-[13px] font-medium">{title}</div>
      <div className="max-w-[calc(100vw-7rem)] text-[12px] text-muted-foreground sm:max-w-sm">{detail}</div>
    </div>
  )
}

function MissingDetail({
  title,
  detail,
  action,
  onAction,
}: {
  title: string
  detail: string
  action: string
  onAction: () => void
}) {
  return (
    <div className="flex min-h-[360px] flex-col items-center justify-center gap-2 px-4 text-center">
      <CircleAlert className="h-5 w-5 text-muted-foreground" />
      <div className="text-[14px] font-medium">{title}</div>
      <div className="max-w-sm text-[12px] text-muted-foreground">{detail}</div>
      <Button variant="outline" size="sm" className="mt-2 h-7 text-[12px]" onClick={onAction}>
        {action}
      </Button>
    </div>
  )
}

function groupTraces(t: Trace[]) {
  const groups: { id: string; level: string; span: string; message: string; latency_ms: number; recorded_at: string; children: Trace[] }[] = []
  for (let i = 0; i < t.length; i += 4) {
    const slice = t.slice(i, i + 4)
    const head = slice[0]
    if (!head) break
    groups.push({
      id: head.id,
      level: head.level,
      span: head.span,
      message: head.message,
      latency_ms: slice.reduce((acc, x) => acc + x.latency_ms, 0),
      recorded_at: head.recorded_at,
      children: slice.slice(1),
    })
  }
  return groups
}

function LevelIcon({ level }: { level: string }) {
  if (level === "error") return <CircleAlert className="h-3 w-3 text-destructive" />
  if (level === "warn") return <AlertTriangle className="h-3 w-3 text-warning" />
  return <Info className="h-3 w-3 text-info" />
}

function exportRunEvidence(run: Run, metrics: RunMetric[], traces: Trace[]) {
  downloadCsv(
    `run-evidence-${formatReadableToken(run.id)}.csv`,
    [
      {
        row_type: "run",
        run_id: run.id,
        experiment: run.experiment_name,
        variant: run.variant,
        status: run.status,
        region: run.region,
        duration_ms: run.duration_ms,
        cost_usd: Number(run.cost_usd),
        started_at: run.started_at ?? "",
        ended_at: run.ended_at ?? "",
        name: "",
        value: "",
        unit: "",
        step: "",
        level: "",
        span: "",
        message: "",
        latency_ms: "",
        recorded_at: run.created_at,
      },
      ...metrics.map((metric) => ({
        row_type: "metric",
        run_id: run.id,
        experiment: run.experiment_name,
        variant: run.variant,
        status: run.status,
        region: run.region,
        duration_ms: "",
        cost_usd: "",
        started_at: "",
        ended_at: "",
        name: metric.name,
        value: metric.value,
        unit: metric.unit,
        step: metric.step,
        level: "",
        span: "",
        message: "",
        latency_ms: "",
        recorded_at: metric.recorded_at,
      })),
      ...traces.map((trace) => ({
        row_type: "trace",
        run_id: run.id,
        experiment: run.experiment_name,
        variant: run.variant,
        status: run.status,
        region: run.region,
        duration_ms: "",
        cost_usd: "",
        started_at: "",
        ended_at: "",
        name: "",
        value: "",
        unit: "",
        step: "",
        level: trace.level,
        span: trace.span,
        message: trace.message,
        latency_ms: trace.latency_ms,
        recorded_at: trace.recorded_at,
      })),
    ],
  )
}

function runReplayCommand(run: Run) {
  return [
    "bucephalus run replay",
    `  --experiment ${shellValue(run.experiment_name)}`,
    `  --variant ${shellValue(run.variant)}`,
    `  --region ${shellValue(run.region)}`,
    `  --from-run ${shellValue(run.id)}`,
  ].join(" \\\n")
}

function copyToClipboard(value: string) {
  if (!navigator.clipboard) return
  void navigator.clipboard.writeText(value).catch(() => undefined)
}

function shellValue(value: string) {
  return /\s/.test(value) ? JSON.stringify(value) : value
}

const tooltipStyle = {
  background: "var(--popover)",
  border: "1px solid var(--border)",
  borderRadius: 6,
  fontSize: 11,
}

function runInsight(run: Run | null, metrics: RunMetric[], traces: Trace[]) {
  const latest = latestMetrics(metrics)
  const slowestTrace = [...traces].sort((a, b) => b.latency_ms - a.latency_ms)[0]
  const eventCounts = traces.reduce(
    (acc, trace) => {
      acc[trace.level] = (acc[trace.level] ?? 0) + 1
      return acc
    },
    {} as Record<string, number>,
  )

  return {
    pass: latestMetricValue(latest, [/^pass@?1$/i, /pass/i, /score/i, /accuracy/i, /success/i, /quality/i]),
    latency: latestMetricValue(latest, [/latency/i, /duration/i, /elapsed/i, /p50/i, /p95/i, /_ms$/i]),
    tokens: latestMetricValue(latest, [/token/i, /prompt/i, /completion/i]),
    slowestSpan: slowestTrace ? `${formatReadableLabel(slowestTrace.span)} ${formatDuration(slowestTrace.latency_ms)}` : run?.status === "running" ? "pending" : "no events",
    eventMix: (["info", "warn", "error", "debug"] as const).map((level) => ({
      level,
      count: eventCounts[level] ?? 0,
    })),
  }
}

function buildRunDecisionBrief(
  run: Run | null,
  metrics: RunMetric[],
  traces: Trace[],
  evidenceError: string | null,
): RunDecisionBrief {
  if (!run) {
    return {
      tone: "muted",
      verdict: "Run not loaded",
      detail: "The run record has not been returned by the cloud API yet.",
      action: "Open config",
      tab: "config",
      facts: briefFacts("—", "—", "—", "—"),
    }
  }

  const latest = latestMetrics(metrics)
  const pass = latestMetricValue(latest, [/^pass@?1$/i, /pass/i, /score/i, /accuracy/i, /success/i, /quality/i])
  const latency = latestMetricValue(latest, [/latency/i, /duration/i, /elapsed/i, /p50/i, /p95/i, /_ms$/i])
  const errorEvents = traces.filter((trace) => trace.level === "error").length
  const warningEvents = traces.filter((trace) => trace.level === "warn").length
  const slowestTrace = [...traces].sort((a, b) => b.latency_ms - a.latency_ms)[0]
  const latestEvidenceAt = latestTimestamp([
    ...metrics.map((metric) => metric.recorded_at),
    ...traces.map((trace) => trace.recorded_at),
  ])
  const evidenceFreshness = latestEvidenceAt ? formatRelative(latestEvidenceAt) : "no evidence"
  const passValue = pass == null ? "no score" : `${(pass * 100).toFixed(1)}%`
  const latencyValue = latency == null ? formatDuration(run.duration_ms) : formatDuration(latency)
  const eventValue = `${errorEvents} err / ${warningEvents} warn`
  const slowestValue = slowestTrace
    ? `${formatReadableLabel(slowestTrace.span)} ${formatDuration(slowestTrace.latency_ms)}`
    : "no spans"

  if (evidenceError) {
    return {
      tone: "warning",
      verdict: "Evidence offline",
      detail: "The run record loaded, but metric and trace endpoints failed. Fix the connection before trusting this run's health.",
      action: "Review config",
      tab: "config",
      facts: briefFacts(passValue, "unavailable", "request failed", formatReadableToken(run.id), "warning", {
        fourth: "Run",
      }),
    }
  }

  if (run.status === "failed" || errorEvents > 0) {
    return {
      tone: "danger",
      verdict: run.status === "failed" ? "Investigate failed run" : "Errors in trace stream",
      detail: slowestTrace
        ? `Start with ${formatReadableLabel(slowestTrace.span)}; it is the slowest observed span and trace errors are present.`
        : "The run failed before detailed span telemetry was recorded. Replay with the same variant and region.",
      action: "Open traces",
      tab: "traces",
      facts: briefFacts(eventValue, slowestValue, evidenceFreshness, formatReadableToken(run.id), "danger", {
        first: "Events",
        second: "Slowest",
        fourth: "Run",
      }),
    }
  }

  if (run.status === "running" || run.status === "queued") {
    return {
      tone: run.status === "running" ? "info" : "muted",
      verdict: run.status === "running" ? "Watch live execution" : "Waiting for worker",
      detail:
        run.status === "running"
          ? "The run is active. Watch trace freshness and latency before comparing final quality."
          : "This run is queued. Evidence will appear after a worker starts emitting metrics and traces.",
      action: run.status === "running" ? "Open logs" : "Open config",
      tab: run.status === "running" ? "logs" : "config",
      facts: briefFacts(passValue, latencyValue, evidenceFreshness, formatReadableToken(run.id), run.status === "running" ? "info" : "muted", {
        fourth: "Run",
      }),
    }
  }

  if (metrics.length === 0 && traces.length === 0) {
    return {
      tone: "warning",
      verdict: "Telemetry missing",
      detail: "The run completed, but no metric or trace evidence is attached. Keep the run visible, but avoid drawing conclusions from it.",
      action: "Review config",
      tab: "config",
      facts: briefFacts("0", "0", formatReadableToken(run.id), run.region, "warning", {
        first: "Metrics",
        second: "Traces",
        third: "Run",
        fourth: "Region",
      }),
    }
  }

  if (pass != null && pass < 0.8) {
    return {
      tone: "warning",
      verdict: "Quality below target",
      detail: "The latest score is weak enough to inspect metric shape before promoting this variant into a comparison cohort.",
      action: "Open metrics",
      tab: "metrics",
      facts: briefFacts(passValue, latencyValue, evidenceFreshness, eventValue, "warning"),
    }
  }

  if ((latency ?? run.duration_ms) > 300_000 || warningEvents > 0) {
    return {
      tone: "warning",
      verdict: warningEvents > 0 ? "Warnings need review" : "Latency is high",
      detail:
        warningEvents > 0
          ? "The run succeeded, but warning events can hide degraded behavior. Inspect the trace stream before reuse."
          : "The run succeeded, but runtime latency is high enough to affect cost and queue throughput.",
      action: warningEvents > 0 ? "Open traces" : "Open metrics",
      tab: warningEvents > 0 ? "traces" : "metrics",
      facts: briefFacts(passValue, latencyValue, evidenceFreshness, eventValue, "warning"),
    }
  }

  return {
    tone: "success",
    verdict: "Healthy run evidence",
    detail: "Status, metric rows, and trace events are consistent enough for comparison or replay decisions.",
    action: "Open metrics",
    tab: "metrics",
    facts: briefFacts(passValue, latencyValue, evidenceFreshness, eventValue, "success"),
  }
}

function briefFacts(
  first: string,
  second: string,
  third: string,
  fourth: string,
  tone: RunDecisionTone = "muted",
  labels: Partial<Record<"first" | "second" | "third" | "fourth", string>> = {},
): RunDecisionBrief["facts"] {
  const runtimeTone: RunDecisionTone | undefined = tone === "danger" ? "danger" : undefined
  return [
    { label: labels.first ?? "Quality", value: first, tone },
    { label: labels.second ?? "Runtime", value: second, tone: runtimeTone },
    { label: labels.third ?? "Freshness", value: third },
    { label: labels.fourth ?? "Signal", value: fourth },
  ]
}

function latestTimestamp(values: string[]) {
  return values
    .filter(Boolean)
    .sort((a, b) => Date.parse(b) - Date.parse(a))[0] ?? null
}

function decisionDotClass(tone: RunDecisionTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  if (tone === "info") return "bg-info animate-pulse"
  return "bg-muted-foreground"
}

function decisionTextClass(tone: RunDecisionTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  if (tone === "info") return "text-info"
  return "text-muted-foreground"
}

function latestMetrics(metrics: RunMetric[]) {
  const out = new Map<string, number>()
  const seen = new Map<string, number>()
  metrics.forEach((metric) => {
    const recordedAt = Date.parse(metric.recorded_at)
    const prev = seen.get(metric.name) ?? 0
    if (recordedAt >= prev) {
      seen.set(metric.name, recordedAt)
      out.set(metric.name, Number(metric.value))
    }
  })
  return out
}

function latestMetricValue(latest: Map<string, number>, aliases: RegExp[]) {
  for (const [name, value] of latest.entries()) {
    if (aliases.some((alias) => alias.test(name))) return value
  }
  return null
}

function MetricCard({
  panel,
  hasData,
  evidenceError,
}: {
  panel: MetricPanel
  hasData: boolean
  evidenceError: string | null
}) {
  return (
    <Card title={panel.title} badge={panel.badge}>
      {hasData ? (
        <div>
          {panel.summary ? <MetricSummaryStrip summary={panel.summary} /> : null}
          <MetricPanelChart panel={panel} />
        </div>
      ) : (
        <EmptyMetric detail={evidenceError ? "Metric observations are unavailable for this request." : panel.emptyDetail} />
      )}
    </Card>
  )
}

type MetricFormat = "percent" | "duration" | "number"
type MetricPanelRow = { step: number; value: number; recorded_at: string }
type MetricPanel = {
  id: string
  title: string
  badge: string
  color: string
  chart: "area" | "line"
  format: MetricFormat
  rows: MetricPanelRow[]
  summary: MetricSummary | null
  emptyDetail: string
}
type MetricSummary = { latest: string; points: number; range: string }

const metricCatalog = [
  {
    id: "quality",
    title: "quality signal",
    badge: "quality",
    color: "var(--chart-2)",
    chart: "area" as const,
    format: "percent" as const,
    aliases: [/pass/i, /score/i, /accuracy/i, /success/i, /quality/i],
    emptyDetail: "No quality metric is attached yet. Showing any observed runtime metrics here keeps this run honest.",
  },
  {
    id: "latency",
    title: "latency signal",
    badge: "runtime",
    color: "var(--chart-1)",
    chart: "line" as const,
    format: "duration" as const,
    aliases: [/latency/i, /duration/i, /elapsed/i, /p50/i, /p95/i, /_ms$/i],
    emptyDetail: "No latency metric is attached yet. Trace spans below can still explain runtime pressure.",
  },
  {
    id: "tokens",
    title: "token signal",
    badge: "usage",
    color: "var(--chart-3)",
    chart: "line" as const,
    format: "number" as const,
    aliases: [/token/i, /prompt/i, /completion/i],
    emptyDetail: "No token metric is attached yet. Usage streams will replace this empty state when emitted.",
  },
]

function buildMetricPanels(metrics: RunMetric[]): MetricPanel[] {
  const streams = metricStreams(metrics)
  const used = new Set<string>()
  const panels: MetricPanel[] = []

  for (const catalog of metricCatalog) {
    const stream = streams.find((candidate) =>
      !used.has(candidate.name) && catalog.aliases.some((alias) => alias.test(candidate.name)),
    )
    if (!stream) continue
    used.add(stream.name)
    panels.push(metricPanelFromStream(stream, catalog))
  }

  for (const stream of streams) {
    if (panels.length >= 3) break
    if (used.has(stream.name)) continue
    used.add(stream.name)
    panels.push(metricPanelFromStream(stream, {
      id: `metric-${panels.length + 1}`,
      title: formatReadableToken(stream.name),
      badge: "metric",
      color: metricPanelColor(panels.length),
      chart: panels.length === 0 ? "area" : "line",
      format: metricFormatForStream(stream),
      emptyDetail: "No numeric rows are available for this metric stream.",
    }))
  }

  for (const catalog of metricCatalog) {
    if (panels.length >= 3) break
    if (panels.some((panel) => panel.id === catalog.id)) continue
    panels.push({
      id: catalog.id,
      title: catalog.title,
      badge: catalog.badge,
      color: catalog.color,
      chart: catalog.chart,
      format: catalog.format,
      rows: [],
      summary: null,
      emptyDetail: catalog.emptyDetail,
    })
  }

  return panels.slice(0, 3)
}

function metricStreams(metrics: RunMetric[]) {
  const grouped = new Map<string, RunMetric[]>()
  metrics.forEach((metric) => {
    const value = Number(metric.value)
    if (!Number.isFinite(value)) return
    grouped.set(metric.name, [...(grouped.get(metric.name) ?? []), metric])
  })
  return Array.from(grouped.entries())
    .map(([name, rows]) => ({ name, rows: metricRowsByStep(rows), unit: rows.find((row) => row.unit)?.unit ?? "" }))
    .filter((stream) => stream.rows.length > 0)
    .sort((a, b) => b.rows.length - a.rows.length || a.name.localeCompare(b.name))
}

function metricRowsByStep(metrics: RunMetric[]): MetricPanelRow[] {
  const byStep = new Map<number, MetricPanelRow>()
  metrics.forEach((metric, index) => {
    const step = Number.isFinite(metric.step) ? metric.step : index
    const value = Number(metric.value)
    const recordedAt = Date.parse(metric.recorded_at) || 0
    const previous = byStep.get(step)
    if (!previous || recordedAt >= (Date.parse(previous.recorded_at) || 0)) {
      byStep.set(step, { step, value, recorded_at: metric.recorded_at })
    }
  })
  return Array.from(byStep.values()).sort((a, b) => a.step - b.step || Date.parse(a.recorded_at) - Date.parse(b.recorded_at))
}

function metricPanelFromStream(
  stream: { name: string; rows: MetricPanelRow[]; unit: string },
  spec: {
    id: string
    title: string
    badge: string
    color: string
    chart: "area" | "line"
    format: MetricFormat
    emptyDetail: string
  },
): MetricPanel {
  const format = spec.format === "percent" && !looksPercentLike(stream) ? metricFormatForStream(stream) : spec.format
  return {
    id: `${spec.id}:${stream.name}`,
    title: formatReadableToken(stream.name),
    badge: spec.badge,
    color: spec.color,
    chart: spec.chart,
    format,
    rows: stream.rows,
    summary: metricPanelSummary(stream.rows, format),
    emptyDetail: spec.emptyDetail,
  }
}

function metricFormatForStream(stream: { name: string; rows: MetricPanelRow[]; unit: string }): MetricFormat {
  const haystack = `${stream.name} ${stream.unit}`
  if (/latency|duration|elapsed|ms|seconds?/i.test(haystack)) return "duration"
  if (looksPercentLike(stream)) return "percent"
  return "number"
}

function looksPercentLike(stream: { name: string; rows: MetricPanelRow[]; unit: string }) {
  const haystack = `${stream.name} ${stream.unit}`
  if (/%|percent|rate|ratio|pass|score|accuracy|success|quality/i.test(haystack)) {
    return stream.rows.every((row) => Math.abs(row.value) <= 1)
  }
  return false
}

function metricPanelColor(index: number) {
  return ["var(--chart-2)", "var(--chart-1)", "var(--chart-3)", "var(--chart-4)"][index % 4]
}

function metricPanelSummary(rows: MetricPanelRow[], format: MetricFormat): MetricSummary | null {
  if (!rows.length) return null
  const latest = rows.at(-1)?.value
  const firstStep = rows[0]?.step
  const lastStep = rows.at(-1)?.step
  return {
    latest: formatMetricSummaryValue(Number(latest), format),
    points: rows.length,
    range: firstStep === lastStep ? `step ${firstStep}` : `steps ${firstStep}-${lastStep}`,
  }
}

function formatMetricSummaryValue(value: number, format: MetricFormat) {
  if (format === "percent") return `${(value * 100).toFixed(1)}%`
  if (format === "duration") return formatDuration(value)
  return value >= 1000 ? value.toLocaleString() : value.toFixed(value % 1 ? 2 : 0)
}

function MetricSummaryStrip({ summary }: { summary: MetricSummary }) {
  return (
    <div className="grid grid-cols-3 gap-px border-b border-border bg-border text-[10.5px]">
      <MetricSummaryCell label="latest" value={summary.latest} />
      <MetricSummaryCell label="points" value={`${summary.points}`} />
      <MetricSummaryCell label="range" value={summary.range} />
    </div>
  )
}

function MetricSummaryCell({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-card px-2 py-1">
      <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="truncate font-mono text-[11px] text-foreground">{value}</div>
    </div>
  )
}

function MetricPanelChart({ panel }: { panel: MetricPanel }) {
  const config: ChartConfig = { value: { label: panel.title, color: panel.color } }
  return (
    <ChartContainer config={config} className="h-40 w-full">
      {panel.chart === "area" ? (
        <AreaChart data={panel.rows}>
          <defs>
            <linearGradient id={`g-${panel.id.replace(/[^a-z0-9-]/gi, "-")}`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="var(--color-value)" stopOpacity={0.4} />
              <stop offset="100%" stopColor="var(--color-value)" stopOpacity={0} />
            </linearGradient>
          </defs>
          <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
          <XAxis dataKey="step" hide />
          <YAxis hide domain={panel.format === "percent" ? [0, 1] : undefined} />
          <Tooltip contentStyle={tooltipStyle} formatter={(value) => formatMetricSummaryValue(Number(value), panel.format)} />
          <Area
            type="monotone"
            dataKey="value"
            stroke="var(--color-value)"
            fill={`url(#g-${panel.id.replace(/[^a-z0-9-]/gi, "-")})`}
            strokeWidth={1.5}
            dot={{ r: 2, fill: "var(--color-value)" }}
            isAnimationActive={false}
          />
        </AreaChart>
      ) : (
        <LineChart data={panel.rows}>
          <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
          <XAxis dataKey="step" hide />
          <YAxis hide domain={panel.format === "percent" ? [0, 1] : undefined} />
          <Tooltip contentStyle={tooltipStyle} formatter={(value) => formatMetricSummaryValue(Number(value), panel.format)} />
          <Line
            type="monotone"
            dataKey="value"
            stroke="var(--color-value)"
            strokeWidth={1.5}
            dot={{ r: 2, fill: "var(--color-value)" }}
            isAnimationActive={false}
          />
        </LineChart>
      )}
    </ChartContainer>
  )
}

function EmptyMetric({ detail }: { detail: string }) {
  return (
    <div className="flex h-40 items-center justify-center px-4 text-center text-[12px] text-muted-foreground">
      {detail}
    </div>
  )
}

function Card({
  title,
  badge,
  children,
}: {
  title: string
  badge?: string
  children: React.ReactNode
}) {
  return (
    <section className="bg-background">
      <header className="flex h-8 items-center justify-between border-b border-border px-3">
        <div className="flex items-center gap-2">
          <h3 className="text-[12px] font-semibold">{title}</h3>
          {badge ? (
            <span className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground">
              {badge}
            </span>
          ) : null}
        </div>
        <Cpu className="h-3 w-3 text-muted-foreground" />
      </header>
      <div className="p-2">{children}</div>
    </section>
  )
}

function KV({
  label,
  value,
  mono,
}: {
  label: string
  value: React.ReactNode
  mono?: boolean
}) {
  return (
    <div className="bg-background p-2.5">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className={cn("mt-0.5 text-[12.5px]", mono ? "font-mono text-[12px]" : "")}>
        {value}
      </div>
    </div>
  )
}

import { useEffect, useMemo, useState } from "react"
import {
  Activity,
  AlertTriangle,
  ArrowUpRight,
  CheckCircle2,
  Clock,
  DollarSign,
  GitCommit,
  Package,
  RefreshCw,
  Rocket,
  Settings,
} from "lucide-react"
import {
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
import { ConnectionIssue } from "@/components/connection-issue"
import { KindBadge, StatusPill } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import { cloudApi, type RegistryItem, type Run, type RunMetric, type Trace } from "@/lib/cloud-api"
import {
  formatBytes,
  formatDuration,
  formatNumber,
  formatReadableLabel,
  formatReadableToken,
  formatRelative,
  formatUsd,
} from "@/lib/format"

const runsCfg: ChartConfig = {
  succeeded: { label: "Succeeded", color: "var(--chart-2)" },
  failed: { label: "Failed", color: "var(--chart-4)" },
  running: { label: "Running", color: "var(--chart-1)" },
}

const passCfg: ChartConfig = {
  a: { label: "Primary", color: "var(--chart-3)" },
  b: { label: "Challenger", color: "var(--chart-1)" },
  c: { label: "Control", color: "var(--chart-2)" },
}

const coverageCfg: ChartConfig = {
  rows: { label: "Rows", color: "var(--chart-1)" },
}

type HomeBriefTone = "success" | "warning" | "danger" | "info" | "muted"
type HomeBriefAction = "settings" | "runs" | "registry" | "experiment-new" | "compare" | "refresh"
type HomeBrief = {
  tone: HomeBriefTone
  verdict: string
  detail: string
  action: string
  actionType: HomeBriefAction
  facts: { label: string; value: string; tone?: HomeBriefTone }[]
}

export function HomePage() {
  const { navigate } = useRouter()
  const [runs, setRuns] = useState<Run[]>([])
  const [registry, setRegistry] = useState<RegistryItem[]>([])
  const [metrics, setMetrics] = useState<RunMetric[]>([])
  const [traces, setTraces] = useState<Trace[]>([])
  const [runsLoaded, setRunsLoaded] = useState(false)
  const [registryLoaded, setRegistryLoaded] = useState(false)
  const [metricsLoaded, setMetricsLoaded] = useState(false)
  const [tracesLoaded, setTracesLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)

  async function loadHomeData() {
    setRunsLoaded(false)
    setRegistryLoaded(false)
    setMetricsLoaded(false)
    setTracesLoaded(false)
    setLoadError(null)
    const [runsResult, registryResult, metricsResult, tracesResult] = await Promise.all([
      cloudApi
        .from("runs")
        .select("*")
        .order("created_at", { ascending: false }),
      cloudApi
        .from("registry_items")
        .select("*")
        .order("created_at", { ascending: false }),
      cloudApi
        .from("run_metrics")
        .select("*")
        .order("recorded_at", { ascending: true }),
      cloudApi
        .from("traces")
        .select("*")
        .order("recorded_at", { ascending: false }),
    ])

    setRuns((runsResult.data ?? []) as Run[])
    setRegistry((registryResult.data ?? []) as RegistryItem[])
    setMetrics((metricsResult.data ?? []) as RunMetric[])
    setTraces((tracesResult.data ?? []) as Trace[])
    setLoadError(runsResult.error?.message ?? registryResult.error?.message ?? metricsResult.error?.message ?? tracesResult.error?.message ?? null)
    setRunsLoaded(true)
    setRegistryLoaded(true)
    setMetricsLoaded(true)
    setTracesLoaded(true)
  }

  useEffect(() => {
    void loadHomeData()
  }, [])

  const recentRuns = runs.slice(0, 8)
  const recentRegistry = registry.slice(0, 6)
  const recentTraces = traces.slice(0, 5)
  const activeRuns = runs.filter((r) => r.status === "running").length
  const runSeries = useMemo(() => runsByDay(runs), [runs])
  const primaryMetric = useMemo(() => primaryMetricInsight(runs, metrics), [runs, metrics])
  const metricSeries = useMemo(() => metricSeriesByVariant(runs, metrics, primaryMetric?.name ?? ""), [runs, metrics, primaryMetric?.name])
  const coverage = useMemo(() => metricCoverage(runs, metrics), [runs, metrics])
  const totals = useMemo(() => dashboardTotals(runs, metrics, primaryMetric), [runs, metrics, primaryMetric])
  const signals = useMemo(() => runtimeSignals(runs, traces), [runs, traces])
  const mix = useMemo(() => registryMix(registry), [registry])
  const evidenceRows = useMemo(() => runEvidenceReadiness(runs, metrics, traces), [metrics, runs, traces])
  const evidenceSummary = useMemo(() => evidenceReadinessSummary(evidenceRows), [evidenceRows])
  const controlBrief = useMemo(
    () => homeDecisionBrief(runs, registry, totals, signals, coverage, loadError),
    [coverage, loadError, registry, runs, signals, totals],
  )
  const loaded = runsLoaded && registryLoaded && metricsLoaded && tracesLoaded
  const unavailable = Boolean(loadError)

  return (
    <div className="flex flex-col">
      <Hero
        activeRuns={activeRuns}
        latestRegistry={recentRegistry[0]}
        hourlyTokens={totals.hourlyTokens}
        hourlySpend={totals.hourlySpend}
        readyResources={registry.filter((r) => r.status === "ready").length}
        unavailable={unavailable}
      />

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-4">
        <Stat
          icon={Activity}
          label="Runs (7d)"
          value={unavailable ? "-" : formatNumber(totals.runs7d)}
          delta={unavailable ? "request failed" : `${totals.running} active`}
          trend={unavailable ? "warning" : "up"}
        />
        <Stat
          icon={CheckCircle2}
          label={totals.primaryMetricLabel}
          value={unavailable ? "-" : totals.primaryMetricAvg == null ? "no data" : formatMetricValue(totals.primaryMetricAvg, primaryMetric)}
          delta={unavailable ? "connect API" : `${totals.metricRuns} metric runs`}
          trend={unavailable ? "warning" : "up"}
        />
        <Stat
          icon={Clock}
          label="Trace pressure"
          value={unavailable ? "-" : signals.pressureLabel}
          delta={unavailable ? "unavailable" : `${signals.events} events`}
          trend={unavailable || signals.errors || signals.warnings ? "warning" : "up"}
        />
        <Stat
          icon={DollarSign}
          label="Spend (7d)"
          value={unavailable ? "-" : formatUsd(totals.spend7d)}
          delta={unavailable ? "request failed" : `${formatUsd(totals.hourlySpend)}/hr`}
          trend={unavailable ? "warning" : "down"}
        />
      </div>

      {loaded ? (
        <HomeBriefView
          brief={controlBrief}
          onAction={() => {
            if (controlBrief.actionType === "settings") navigate({ name: "settings" })
            if (controlBrief.actionType === "runs") navigate({ name: "runs" })
            if (controlBrief.actionType === "registry") navigate({ name: "registry" })
            if (controlBrief.actionType === "experiment-new") navigate({ name: "experiment-new" })
            if (controlBrief.actionType === "compare") navigate({ name: "compare" })
            if (controlBrief.actionType === "refresh") void loadHomeData()
          }}
        />
      ) : null}

      {loaded && loadError ? (
        <ConnectionIssue
          title="Dashboard data request failed"
          detail={loadError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadHomeData()}
          compact
        />
      ) : null}

      {!loadError ? (
        <>
          <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-3">
        <Panel
          title="Runs"
          subtitle="Last 7 days, by outcome"
          right={
            <button
              onClick={() => navigate({ name: "runs" })}
              className="text-[11px] text-muted-foreground hover:text-foreground"
            >
              View all
            </button>
          }
          className="lg:col-span-2"
        >
          {loaded && seriesHasRows(runSeries, ["succeeded", "failed", "running"]) ? (
            <ChartContainer config={runsCfg} className="h-44 w-full">
              <BarChart data={runSeries} barCategoryGap={8}>
                <CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis
                  dataKey="d"
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <YAxis
                  tickLine={false}
                  axisLine={false}
                  width={28}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip cursor={{ fill: "var(--accent)", opacity: 0.4 }} contentStyle={tooltipStyle} />
                <Bar stackId="r" dataKey="succeeded" fill="var(--color-succeeded)" />
                <Bar stackId="r" dataKey="failed" fill="var(--color-failed)" />
                <Bar stackId="r" dataKey="running" fill="var(--color-running)" radius={[2, 2, 0, 0]} />
              </BarChart>
            </ChartContainer>
          ) : (
            <EmptyPanel title={loaded ? "No runs this week" : "Loading runs"} detail={loaded ? "Queue an experiment first." : "Fetching recent runs."} />
          )}
        </Panel>

        <Panel
          title="Runtime signals"
          subtitle={loaded ? `${signals.errors} errors · ${signals.warnings} warnings` : "loading traces"}
          right={
            <button
              onClick={() => navigate({ name: "runs" })}
              className="text-[11px] text-muted-foreground hover:text-foreground"
            >
              Inspect
            </button>
          }
        >
          {recentTraces.length > 0 ? (
            <div className="h-44 overflow-hidden text-[12px]">
              {recentTraces.slice(0, 4).map((trace) => {
                const run = runs.find((item) => item.id === trace.run_id)
                return (
                  <button
                    key={trace.id}
                    onClick={() => navigate({ name: "run-detail", id: trace.run_id })}
                    className="grid w-full grid-cols-[16px_minmax(0,1fr)_58px_48px] items-center gap-2 border-b border-border px-3 py-1.5 text-left hover:bg-accent/40"
                  >
                    <TraceDot level={trace.level} />
                    <span className="min-w-0">
                      <span className="block truncate font-mono text-[11.5px]">{formatReadableLabel(trace.span)}</span>
                      <span className="block truncate text-[10.5px] text-muted-foreground">
                        {run ? `${formatReadableLabel(run.experiment_name)}/${formatReadableToken(run.variant)}` : formatReadableLabel(trace.message)}
                      </span>
                    </span>
                    <span className="font-mono text-[10.5px] text-muted-foreground">{formatDuration(trace.latency_ms)}</span>
                    <span className="text-right font-mono text-[10.5px] text-muted-foreground">{formatRelative(trace.recorded_at)}</span>
                  </button>
                )
              })}
              <div className="grid grid-cols-3 gap-px bg-border">
                <SignalFact label="Events" value={`${signals.events}`} />
                <SignalFact label="Slow" value={`${signals.slow}`} />
                <SignalFact label="State" value={signals.pressureLabel} />
              </div>
            </div>
          ) : (
            <EmptyPanel title={loaded ? "No trace events" : "Loading traces"} detail={loaded ? "Runtime spans will appear here once workers report evidence." : "Fetching runtime signals."} />
          )}
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-5">
        <Panel title="Live runs" subtitle="Click to inspect traces" className="lg:col-span-3">
          <div className="overflow-x-auto text-[12px]">
            <RowHeader
              cols={["Experiment", "Variant", "Status", "Duration", "Cost", "Region", ""]}
              widths={["minmax(180px,1fr)", "120px", "100px", "90px", "70px", "100px", "20px"]}
              minWidth="780px"
            />
            {recentRuns.length > 0 ? recentRuns.map((r) => (
              <button
                key={r.id}
                onClick={() => navigate({ name: "run-detail", id: r.id })}
                className="grid min-w-[780px] w-full items-center gap-2 border-t border-border px-3 py-1.5 text-left hover:bg-accent/40"
                style={{ gridTemplateColumns: "minmax(180px,1fr) 120px 100px 90px 70px 100px 20px" }}
              >
                <div className="flex min-w-0 items-center gap-2">
                  <GitCommit className="h-3 w-3 shrink-0 text-muted-foreground" />
                  <span className="truncate font-mono text-[11.5px]">{formatReadableLabel(r.experiment_name)}</span>
                </div>
                <span className="truncate font-mono text-[11px] text-muted-foreground">{formatReadableToken(r.variant)}</span>
                <StatusPill status={r.status} />
                <span className="font-mono text-[11px] text-muted-foreground">{formatDuration(r.duration_ms)}</span>
                <span className="font-mono text-[11px] text-muted-foreground">{formatUsd(Number(r.cost_usd))}</span>
                <span className="truncate font-mono text-[11px] text-muted-foreground">{r.region}</span>
                <ArrowUpRight className="h-3 w-3 text-muted-foreground" />
              </button>
            )) : (
              <EmptyPanel compact title={loaded ? "No runs yet" : "Loading runs"} detail={loaded ? "Queue an experiment first." : "Fetching run history."} />
            )}
          </div>
        </Panel>

        <Panel title={primaryMetric ? formatReadableLabel(primaryMetric.name) : "Metric trend"} subtitle={primaryMetric ? `${primaryMetric.runCount}/${runs.length} runs covered` : "top variants by live metric"} className="lg:col-span-2">
          {metricSeries.keys.length > 0 ? (
            <>
              <ChartContainer config={passCfg} className="h-44 w-full">
                <LineChart data={metricSeries.data}>
                  <CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="2 4" />
                  <XAxis
                    dataKey="d"
                    tickLine={false}
                    axisLine={false}
                    tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                  />
                  <YAxis
                    domain={primaryMetric?.percentLike ? [0, 1] : ["auto", "auto"]}
                    tickLine={false}
                    axisLine={false}
                    width={28}
                    tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                  />
                  <Tooltip contentStyle={tooltipStyle} />
                  {metricSeries.keys.map((key, idx) => (
                    <Line
                      key={key}
                      type="monotone"
                      dataKey={key}
                      name={metricSeries.labels[idx]}
                      stroke={`var(--color-${key})`}
                      strokeWidth={1.7}
                      dot={false}
                      isAnimationActive={false}
                    />
                  ))}
                </LineChart>
              </ChartContainer>
              <div className="grid grid-cols-3 gap-px border-t border-border bg-border">
                {metricSeries.labels.map((label, idx) => (
                  <div key={label} className="min-w-0 bg-background px-2 py-1.5">
                    <div className="flex min-w-0 items-center gap-1.5">
                      <span className="h-1.5 w-1.5 shrink-0 rounded-full" style={{ background: `var(--chart-${idx + 1})` }} />
                      <span className="truncate font-mono text-[10.5px] text-muted-foreground">{label}</span>
                    </div>
                  </div>
                ))}
              </div>
            </>
          ) : (
            <div className="flex h-[205px] items-center justify-center px-3 text-center text-[12px] text-muted-foreground">
              Metric trend lines appear after observations are linked to runs.
            </div>
          )}
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-5">
        <Panel title="Metric coverage" subtitle="real metric rows by signal" className="lg:col-span-3">
          <div className="grid grid-cols-3 gap-px bg-border">
            <Insight
              label={primaryMetric ? `Best ${formatReadableLabel(primaryMetric.name)}` : "Best metric"}
              value={totals.bestPassLabel}
              note={totals.bestPassValue == null ? "no rows" : `${formatMetricValue(totals.bestPassValue, primaryMetric)} latest`}
            />
            <Insight label="Fastest run" value={totals.fastestLabel} note={formatDuration(totals.fastestMs)} />
            <Insight label="Failure share" value={`${(totals.failureShare * 100).toFixed(1)}%`} note={`${totals.failed} failed / ${totals.runs7d} runs`} />
          </div>
          {coverage.length > 0 ? (
            <ChartContainer config={coverageCfg} className="h-40 w-full border-t border-border">
              <BarChart data={coverage} layout="vertical" margin={{ left: 4, right: 16, top: 10, bottom: 4 }}>
                <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis type="number" hide />
                <YAxis
                  type="category"
                  dataKey="name"
                  width={112}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip
                  cursor={{ fill: "var(--accent)", opacity: 0.35 }}
                  contentStyle={tooltipStyle}
                  formatter={(value, _name, item) => [
                    `${value} rows across ${item.payload.runs} runs`,
                    "coverage",
                  ]}
                />
                <Bar dataKey="rows" fill="var(--color-rows)" radius={[0, 2, 2, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          ) : (
            <div className="border-t border-border px-3 py-8 text-center text-[12px] text-muted-foreground">
              Runtime metrics will populate coverage once runs emit observations.
            </div>
          )}
        </Panel>

        <Panel title="Registry mix" subtitle="ready resources by type" className="lg:col-span-2">
          {mix.length > 0 ? (
            <div className="grid grid-cols-2 gap-px bg-border sm:grid-cols-3 md:grid-cols-6">
              {mix.map((item) => (
              <div key={item.kind} className="bg-background px-3 py-2">
                <div className="truncate text-[10px] uppercase tracking-wide text-muted-foreground">{item.kind}</div>
                <div className="mt-1 flex items-baseline gap-1.5">
                  <span className="font-mono text-[18px] font-medium">{item.count}</span>
                  <span className="truncate font-mono text-[10.5px] text-muted-foreground">{formatBytes(item.bytes)}</span>
                </div>
              </div>
              ))}
            </div>
          ) : (
            <EmptyPanel compact title={loaded ? "No registry resources" : "Loading registry"} detail={loaded ? "Accepted resources appear here." : "Fetching registry inventory."} />
          )}
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-5">
        <Panel title="Recently pushed to registry" className="lg:col-span-3">
          <div className="overflow-x-auto text-[12px]">
            <RowHeader
              cols={["Resource", "Kind", "Version", "Size", "Status", "Pushed"]}
              widths={["minmax(220px,1fr)", "90px", "70px", "70px", "100px", "90px"]}
              minWidth="680px"
            />
            {recentRegistry.length > 0 ? recentRegistry.map((it) => (
              <div
                key={it.id}
                className="grid min-w-[680px] items-center gap-2 border-t border-border px-3 py-1.5"
                style={{ gridTemplateColumns: "minmax(220px,1fr) 90px 70px 70px 100px 90px" }}
              >
                <div className="flex min-w-0 items-center gap-2">
                  <Package className="h-3 w-3 shrink-0 text-muted-foreground" />
                  <span className="truncate font-mono text-[11.5px]">{formatReadableLabel(it.name)}</span>
                </div>
                <KindBadge kind={it.kind} />
                <span className="truncate font-mono text-[11px] text-muted-foreground">{formatReadableToken(it.version)}</span>
                <span className="font-mono text-[11px] text-muted-foreground">{formatBytes(it.size_bytes)}</span>
                <StatusPill status={it.status} />
                <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(it.created_at)}</span>
              </div>
            )) : (
              <EmptyPanel compact title={loaded ? "No registry pushes" : "Loading registry"} detail={loaded ? "Push packages first." : "Fetching resources."} />
            )}
          </div>
        </Panel>

        <Panel
          title="Evidence readiness"
          subtitle={loaded ? `${evidenceSummary.ready}/${evidenceSummary.total} decision-ready` : "loading runtime evidence"}
          right={
            <button
              onClick={() => navigate({ name: evidenceSummary.ready >= 2 ? "compare" : "runs" })}
              className="text-[11px] text-muted-foreground hover:text-foreground"
            >
              {evidenceSummary.ready >= 2 ? "Compare" : "Inspect"}
            </button>
          }
          className="lg:col-span-2"
        >
          {evidenceRows.length > 0 ? (
            <div className="flex h-full flex-col">
              <div className="grid grid-cols-3 gap-px bg-border">
                <SignalFact label="Ready" value={`${evidenceSummary.ready}`} />
                <SignalFact label="Thin" value={`${evidenceSummary.thin}`} />
                <SignalFact label="Blocked" value={`${evidenceSummary.blocked}`} />
              </div>
              <div className="min-h-0 flex-1 overflow-hidden">
                {evidenceRows.slice(0, 5).map((row) => (
                  <button
                    key={row.run.id}
                    onClick={() => navigate({ name: "run-detail", id: row.run.id })}
                    className="grid w-full grid-cols-[minmax(0,1fr)_52px] items-center gap-2 border-t border-border px-3 py-1.5 text-left hover:bg-accent/40"
                  >
                    <span className="min-w-0">
                      <span className="flex min-w-0 items-center gap-1.5">
                        <span className={"h-1.5 w-1.5 shrink-0 rounded-full " + evidenceToneDot(row.tone)} />
                        <span className="truncate font-mono text-[11.5px]">
                          {formatReadableLabel(row.run.experiment_name)}/{formatReadableToken(row.run.variant)}
                        </span>
                      </span>
                      <span className="mt-0.5 block truncate text-[10.5px] text-muted-foreground">
                        {row.metricRows} metrics · {row.traceRows} traces · {row.lastEvidence}
                      </span>
                      <span className="mt-1 block h-1 overflow-hidden rounded-full bg-muted">
                        <span className={"block h-full rounded-full " + evidenceToneBar(row.tone)} style={{ width: `${row.score}%` }} />
                      </span>
                    </span>
                    <span className="text-right">
                      <span className={"block font-mono text-[13px] " + evidenceToneText(row.tone)}>{row.score}</span>
                      <span className="block text-[9.5px] uppercase tracking-wide text-muted-foreground">{row.label}</span>
                    </span>
                  </button>
                ))}
              </div>
            </div>
          ) : (
            <EmptyPanel compact title={loaded ? "No run evidence" : "Loading evidence"} detail={loaded ? "Recent run evidence appears after executions emit metrics or traces." : "Fetching runtime evidence."} />
          )}
        </Panel>
      </div>
        </>
      ) : null}
    </div>
  )
}

function Hero({
  activeRuns,
  latestRegistry,
  hourlyTokens,
  hourlySpend,
  readyResources,
  unavailable,
}: {
  activeRuns: number
  latestRegistry?: RegistryItem
  hourlyTokens: number
  hourlySpend: number
  readyResources: number
  unavailable?: boolean
}) {
  return (
    <div className="relative overflow-hidden border-b border-border">
      <div className="grid-bg pointer-events-none absolute inset-0 opacity-30" />
      <div className="relative flex items-center gap-3 px-3 py-4 sm:gap-6 sm:px-5 sm:py-5">
        <div className="grid h-10 w-10 shrink-0 place-items-center rounded-md border border-border bg-card sm:h-12 sm:w-12">
          <Rocket className="h-5 w-5 text-brand" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <h1 className="text-[18px] font-semibold tracking-tight">Cloud control plane</h1>
            <span className="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground">
              {unavailable ? "api attention" : "live API"}
            </span>
          </div>
          <p className="max-w-[calc(100vw-7rem)] text-[12.5px] leading-relaxed text-muted-foreground sm:max-w-none">
            <span className="sm:hidden">
              {unavailable
                ? "Cloud API needs attention."
                : `${activeRuns} active. Last push ${latestRegistry ? formatRelative(latestRegistry.created_at) : "none yet"}.`}
            </span>
            <span className="hidden sm:inline">
              {unavailable
                ? "Cloud API needs attention. Connect credentials to load live control-plane state."
                : `${activeRuns} runs active. Latest registry push: ${
                    latestRegistry ? `${formatReadableLabel(latestRegistry.name)} ${formatRelative(latestRegistry.created_at)}` : "none yet"
                  }.`}
            </span>
          </p>
        </div>
        <div className="hidden items-center gap-2 md:flex">
          <Mini label="Tokens / hr" value={unavailable ? "-" : formatNumber(Math.round(hourlyTokens))} />
          <Mini label="$ / hr" value={unavailable ? "-" : formatUsd(hourlySpend)} />
          <Mini label="Ready assets" value={unavailable ? "-" : formatNumber(readyResources)} />
        </div>
      </div>
    </div>
  )
}

const tooltipStyle = {
  background: "var(--popover)",
  border: "1px solid var(--border)",
  borderRadius: 6,
  fontSize: 11,
}

function Mini({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border border-border bg-card px-2.5 py-1.5">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="font-mono text-[13px] font-medium">{value}</div>
    </div>
  )
}

function HomeBriefView({
  brief,
  onAction,
}: {
  brief: HomeBrief
  onAction: () => void
}) {
  return (
    <section className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(280px,0.9fr)_minmax(0,1.5fr)]">
      <div className="min-w-0 bg-background p-3">
        <div className="flex items-center gap-2">
          <span className={"h-2 w-2 rounded-full " + homeBriefDot(brief.tone)} />
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Control brief</div>
            <div className={"mt-0.5 truncate text-[14px] font-medium " + homeBriefText(brief.tone)}>
              {brief.verdict}
            </div>
          </div>
        </div>
        <p className="mt-2 max-w-[calc(100vw-5.5rem)] text-[11.5px] leading-relaxed text-muted-foreground sm:max-w-none">
          <span className="sm:hidden">{homeBriefMobileDetail(brief.detail)}</span>
          <span className="hidden sm:inline">{brief.detail}</span>
        </p>
        <button
          className="mt-3 inline-flex h-7 w-full items-center justify-between gap-2 rounded-md border border-border px-2.5 text-[12px] hover:bg-accent/40 sm:w-auto"
          onClick={onAction}
        >
          {brief.action}
          <HomeBriefActionIcon actionType={brief.actionType} />
        </button>
      </div>
      <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
        {brief.facts.map((fact) => (
          <div key={fact.label} className="min-w-0 bg-background px-3 py-2.5">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
            <div
              className={"mt-0.5 truncate font-mono text-[12px] " + (fact.tone ? homeBriefText(fact.tone) : "")}
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

function HomeBriefActionIcon({ actionType }: { actionType: HomeBriefAction }) {
  if (actionType === "settings") return <Settings className="h-3 w-3" />
  if (actionType === "refresh") return <RefreshCw className="h-3 w-3" />
  if (actionType === "experiment-new") return <Rocket className="h-3 w-3" />
  if (actionType === "registry") return <Package className="h-3 w-3" />
  return <ArrowUpRight className="h-3 w-3" />
}

function homeBriefMobileDetail(detail: string) {
  if (detail.includes("Inspect runs before comparing variants or queueing more workload")) {
    return "Inspect failed runs before more workload."
  }
  if (detail.includes("Queueable packages are available and no run history exists yet")) {
    return "Ready packages are available. Start the first run to collect evidence."
  }
  if (detail.includes("Metric coverage is thin")) {
    return "Metric coverage is thin. Inspect runs before making a promotion call."
  }
  return detail
}

function Stat({
  icon: Icon,
  label,
  value,
  delta,
  trend,
}: {
  icon: React.ComponentType<{ className?: string }>
  label: string
  value: string
  delta: string
  trend: "up" | "down" | "warning"
}) {
  return (
    <div className="flex items-start justify-between bg-background p-3">
      <div className="min-w-0">
        <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wide text-muted-foreground">
          <Icon className="h-3 w-3" />
          {label}
        </div>
        <div className="mt-1 truncate font-mono text-[20px] font-medium tracking-tight">{value}</div>
      </div>
      <span
        className={
          "rounded px-1 font-mono text-[10px] " +
          (trend === "up" ? "bg-success/10 text-success" : trend === "down" ? "bg-destructive/10 text-destructive" : "bg-warning/10 text-warning")
        }
      >
        {delta}
      </span>
    </div>
  )
}

function Insight({ label, value, note }: { label: string; value: string; note: string }) {
  return (
    <div className="min-w-0 bg-background px-3 py-2">
      <div className="truncate text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="mt-1 truncate font-mono text-[13px] font-medium">{value}</div>
      <div className="truncate text-[10.5px] text-muted-foreground">{note}</div>
    </div>
  )
}

function TraceDot({ level }: { level: string }) {
  return (
    <span
      className={
        "grid h-4 w-4 place-items-center rounded " +
        (level === "error"
          ? "bg-destructive/10 text-destructive"
          : level === "warn"
            ? "bg-warning/10 text-warning"
            : level === "debug"
              ? "bg-muted text-muted-foreground"
              : "bg-info/10 text-info")
      }
      title={formatReadableLabel(level)}
    >
      {level === "error" || level === "warn" ? <AlertTriangle className="h-3 w-3" /> : <Activity className="h-3 w-3" />}
    </span>
  )
}

function SignalFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-background px-2 py-1.5">
      <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="truncate font-mono text-[12px] text-foreground">{value}</div>
    </div>
  )
}

export function Panel({
  title,
  subtitle,
  right,
  children,
  className,
}: {
  title: string
  subtitle?: string
  right?: React.ReactNode
  children: React.ReactNode
  className?: string
}) {
  return (
    <section className={"flex flex-col bg-background " + (className ?? "")}>
      <header className="flex h-9 items-center justify-between border-b border-border px-3">
        <div className="flex min-w-0 items-baseline gap-2">
          <h3 className="shrink-0 text-[12px] font-semibold tracking-tight">{title}</h3>
          {subtitle ? <span className="truncate text-[11px] text-muted-foreground">{subtitle}</span> : null}
        </div>
        {right}
      </header>
      <div className="flex-1">{children}</div>
    </section>
  )
}

export function RowHeader({
  cols,
  widths,
  minWidth,
}: {
  cols: string[]
  widths: string[]
  minWidth?: string
}) {
  return (
    <div
      className="grid items-center gap-2 px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
      style={{ gridTemplateColumns: widths.join(" "), minWidth }}
    >
      {cols.map((c, i) => (
        <span key={i}>{c}</span>
      ))}
    </div>
  )
}

function EmptyPanel({
  title,
  detail,
  compact = false,
}: {
  title: string
  detail: string
  compact?: boolean
}) {
  return (
    <div className={(compact ? "min-h-32" : "h-44") + " flex flex-col items-center justify-center gap-1 px-4 py-8 text-center"}>
      <Activity className="h-4 w-4 text-muted-foreground" />
      <div className="text-[12.5px] font-medium">{title}</div>
      <div className="max-w-[calc(100vw-7rem)] text-[11px] text-muted-foreground sm:max-w-sm">{detail}</div>
    </div>
  )
}

type PrimaryMetricInsight = {
  name: string
  rows: number
  runCount: number
  avg: number | null
  percentLike: boolean
  higherIsBetter: boolean
}

function dashboardTotals(runs: Run[], metrics: RunMetric[], primaryMetric: PrimaryMetricInsight | null) {
  const now = Date.now()
  const sevenDaysAgo = now - 7 * 24 * 60 * 60 * 1000
  const recent = runs.filter((r) => Date.parse(r.created_at) >= sevenDaysAgo)
  const completed = recent.filter((r) => r.status === "succeeded")
  const failed = recent.filter((r) => r.status === "failed")
  const durations = recent.map((r) => r.duration_ms).filter((d) => d > 0)
  const primaryRows = primaryMetric ? metrics.filter((metric) => metric.name === primaryMetric.name) : []
  const primaryByRun = latestMetricByRun(primaryRows)
  const primaryValues = Array.from(primaryByRun.values())
  const tokenRows = metrics.filter((m) => m.name === "tokens_in" || m.name === "tokens_out")
  const sixHoursAgo = now - 6 * 60 * 60 * 1000
  const hourlyTokens = tokenRows
    .filter((m) => Date.parse(m.recorded_at) >= sixHoursAgo)
    .reduce((acc, m) => acc + Number(m.value), 0) / 6
  const spend7d = recent.reduce((acc, r) => acc + Number(r.cost_usd), 0)
  const fastest = [...completed].filter((r) => r.duration_ms > 0).sort((a, b) => a.duration_ms - b.duration_ms)[0]
  const bestPrimary = [...primaryByRun.entries()].sort((a, b) =>
    primaryMetric?.higherIsBetter === false ? a[1].value - b[1].value : b[1].value - a[1].value,
  )[0]
  const bestRun = bestPrimary ? runs.find((r) => r.id === bestPrimary[0]) : undefined

  return {
    runs7d: recent.length,
    running: recent.filter((r) => r.status === "running").length,
    completed: completed.length,
    failed: failed.length,
    avgDurationMs: durations.length ? Math.round(durations.reduce((a, b) => a + b, 0) / durations.length) : 0,
    spend7d,
    hourlySpend: spend7d / (7 * 24),
    hourlyTokens,
    metricRuns: primaryValues.length,
    primaryMetricLabel: primaryMetric ? formatReadableLabel(primaryMetric.name) : "Quality",
    primaryMetricAvg: primaryValues.length ? primaryValues.reduce((acc, item) => acc + item.value, 0) / primaryValues.length : null,
    bestPassLabel: bestRun ? `${formatReadableLabel(bestRun.experiment_name)}/${formatReadableToken(bestRun.variant)}` : "no data",
    bestPassValue: bestPrimary?.[1].value ?? null,
    fastestLabel: fastest ? `${formatReadableLabel(fastest.experiment_name)}/${formatReadableToken(fastest.variant)}` : "no data",
    fastestMs: fastest?.duration_ms ?? 0,
    failureShare: recent.length ? failed.length / recent.length : 0,
  }
}

function runtimeSignals(runs: Run[], traces: Trace[]) {
  const runIds = new Set(runs.map((run) => run.id))
  const scoped = traces.filter((trace) => runIds.size === 0 || runIds.has(trace.run_id))
  const errors = scoped.filter((trace) => trace.level === "error").length
  const warnings = scoped.filter((trace) => trace.level === "warn").length
  const slow = scoped.filter((trace) => trace.latency_ms >= 5000).length
  const pressureLabel = errors ? `${errors} errors` : warnings ? `${warnings} warnings` : slow ? `${slow} slow` : scoped.length ? "nominal" : "no traces"
  return {
    events: scoped.length,
    errors,
    warnings,
    slow,
    pressureLabel,
  }
}

type EvidenceReadinessRow = {
  run: Run
  metricRows: number
  traceRows: number
  lastEvidence: string
  score: number
  label: "ready" | "thin" | "blocked"
  tone: "success" | "warning" | "danger" | "muted"
}

function runEvidenceReadiness(runs: Run[], metrics: RunMetric[], traces: Trace[]): EvidenceReadinessRow[] {
  const metricsByRun = new Map<string, RunMetric[]>()
  const tracesByRun = new Map<string, Trace[]>()
  metrics.forEach((metric) => {
    metricsByRun.set(metric.run_id, [...(metricsByRun.get(metric.run_id) ?? []), metric])
  })
  traces.forEach((trace) => {
    tracesByRun.set(trace.run_id, [...(tracesByRun.get(trace.run_id) ?? []), trace])
  })

  return [...runs]
    .sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))
    .slice(0, 12)
    .map((run) => {
      const runMetrics = metricsByRun.get(run.id) ?? []
      const runTraces = tracesByRun.get(run.id) ?? []
      const latestMetric = newestTimestamp(runMetrics.map((metric) => metric.recorded_at))
      const latestTrace = newestTimestamp(runTraces.map((trace) => trace.recorded_at))
      const latestEvidence = Math.max(latestMetric, latestTrace, Date.parse(run.ended_at ?? run.started_at ?? run.created_at) || 0)
      const fresh = latestEvidence > Date.now() - 24 * 60 * 60 * 1000
      const failed = run.status === "failed"
      const metricScore = Math.min(40, runMetrics.length * 16)
      const traceScore = Math.min(25, runTraces.length * 8)
      const statusScore = failed ? 0 : run.status === "queued" ? 5 : run.status === "running" ? 14 : 20
      const freshnessScore = fresh ? 15 : latestEvidence ? 6 : 0
      const score = Math.max(0, Math.min(100, Math.round(metricScore + traceScore + statusScore + freshnessScore)))
      const label = failed || (runMetrics.length === 0 && run.status !== "running") ? "blocked" : score >= 70 ? "ready" : "thin"
      const tone = label === "ready" ? "success" : label === "thin" ? "warning" : failed ? "danger" : "muted"
      return {
        run,
        metricRows: runMetrics.length,
        traceRows: runTraces.length,
        lastEvidence: latestEvidence ? formatRelative(new Date(latestEvidence).toISOString()) : "no evidence",
        score,
        label,
        tone,
      }
    })
}

function evidenceReadinessSummary(rows: EvidenceReadinessRow[]) {
  return rows.reduce(
    (acc, row) => {
      acc.total += 1
      if (row.label === "ready") acc.ready += 1
      if (row.label === "thin") acc.thin += 1
      if (row.label === "blocked") acc.blocked += 1
      return acc
    },
    { total: 0, ready: 0, thin: 0, blocked: 0 },
  )
}

function newestTimestamp(values: string[]) {
  return values.reduce((latest, value) => Math.max(latest, Date.parse(value) || 0), 0)
}

function evidenceToneDot(tone: EvidenceReadinessRow["tone"]) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  return "bg-muted-foreground"
}

function evidenceToneBar(tone: EvidenceReadinessRow["tone"]) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  return "bg-muted-foreground"
}

function evidenceToneText(tone: EvidenceReadinessRow["tone"]) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  return "text-muted-foreground"
}

function homeDecisionBrief(
  runs: Run[],
  registry: RegistryItem[],
  totals: ReturnType<typeof dashboardTotals>,
  signals: ReturnType<typeof runtimeSignals>,
  coverage: ReturnType<typeof metricCoverage>,
  loadError: string | null,
): HomeBrief {
  const readyPackages = registry.filter((item) => item.kind === "experiment_package" && item.status === "ready").length
  const readyResources = registry.filter((item) => item.status === "ready").length
  const latestRun = [...runs].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]
  const latestResource = [...registry].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]
  const metricCoveragePct = runs.length ? Math.round((totals.metricRuns / runs.length) * 100) : 0

  if (loadError) {
    return {
      tone: "warning",
      verdict: "Control plane needs connection",
      detail: "One or more live endpoints failed. Fix settings before trusting run, metric, registry, or trace state.",
      action: "Check settings",
      actionType: "settings",
      facts: homeBriefFacts("API", "attention", "Runs", `${runs.length}`, "Registry", `${registry.length}`, "Metrics", `${coverage.length} signals`, "warning"),
    }
  }

  if (runs.length === 0 && readyPackages === 0) {
    return {
      tone: "info",
      verdict: "Start with a package",
      detail: "No runs or ready experiment packages are visible. Push or select a package before queueing execution.",
      action: "Open registry",
      actionType: "registry",
      facts: homeBriefFacts("Runs", "0", "Ready packages", "0", "Resources", `${registry.length}`, "Latest", latestResource ? formatRelative(latestResource.created_at) : "none", "info"),
    }
  }

  if (runs.length === 0 && readyPackages > 0) {
    return {
      tone: "success",
      verdict: "Ready to launch",
      detail: "Queueable packages are available and no run history exists yet. Start the first experiment run to collect evidence.",
      action: "New experiment",
      actionType: "experiment-new",
      facts: homeBriefFacts("Ready packages", `${readyPackages}`, "Resources", `${readyResources}`, "Runs", "0", "Latest", latestResource ? formatRelative(latestResource.created_at) : "none", "success"),
    }
  }

  if (signals.errors > 0 || totals.failed > 0) {
    return {
      tone: "danger",
      verdict: "Runtime attention needed",
      detail: "Failures or error traces are present. Inspect runs before comparing variants or queueing more workload.",
      action: "Inspect runs",
      actionType: "runs",
      facts: homeBriefFacts("Errors", `${signals.errors}`, "Failed", `${totals.failed}`, "Trace state", signals.pressureLabel, "Latest run", latestRun ? formatRelative(latestRun.created_at) : "none", "danger"),
    }
  }

  if (totals.running > 0) {
    return {
      tone: "info",
      verdict: "Execution in flight",
      detail: "Runs are active. Watch traces and metric freshness before making a promotion decision.",
      action: "Watch runs",
      actionType: "runs",
      facts: homeBriefFacts("Running", `${totals.running}`, "Trace state", signals.pressureLabel, "Spend", formatUsd(totals.spend7d), "Latest run", latestRun ? formatRelative(latestRun.created_at) : "none", "info"),
    }
  }

  if (runs.length > 0 && (totals.metricRuns === 0 || metricCoveragePct < 50)) {
    return {
      tone: "warning",
      verdict: "Metric coverage is thin",
      detail: "Runs exist, but too few have metric observations. Keep the cohort visible, but avoid final comparison decisions.",
      action: "Inspect runs",
      actionType: "runs",
      facts: homeBriefFacts("Coverage", `${metricCoveragePct}%`, "Metric runs", `${totals.metricRuns}`, "Signals", `${coverage.length}`, "Runs", `${runs.length}`, "warning"),
    }
  }

  if ((totals.primaryMetricAvg ?? 0) >= 0.9 && totals.failureShare === 0 && totals.metricRuns > 1 && (totals.primaryMetricLabel.toLowerCase().includes("pass") || totals.primaryMetricLabel.toLowerCase().includes("accuracy"))) {
    return {
      tone: "success",
      verdict: "Ready to compare",
      detail: "Recent runs have clean status and enough metric evidence for variant comparison.",
      action: "Open compare",
      actionType: "compare",
      facts: homeBriefFacts(totals.primaryMetricLabel, formatMetricValue(totals.primaryMetricAvg, { percentLike: true }), "Metric runs", `${totals.metricRuns}`, "Spend", formatUsd(totals.spend7d), "Latest run", latestRun ? formatRelative(latestRun.created_at) : "none", "success"),
    }
  }

  return {
    tone: "warning",
    verdict: "Collect one more signal",
    detail: "The workspace has live evidence, but quality or coverage is not strong enough for a confident decision yet.",
    action: "Refresh",
    actionType: "refresh",
    facts: homeBriefFacts("Runs", `${runs.length}`, "Coverage", `${metricCoveragePct}%`, "Trace state", signals.pressureLabel, "Ready assets", `${readyResources}`, "warning"),
  }
}

function homeBriefFacts(
  firstLabel: string,
  firstValue: string,
  secondLabel: string,
  secondValue: string,
  thirdLabel: string,
  thirdValue: string,
  fourthLabel: string,
  fourthValue: string,
  tone: HomeBriefTone,
): HomeBrief["facts"] {
  return [
    { label: firstLabel, value: firstValue, tone },
    { label: secondLabel, value: secondValue, tone: tone === "danger" ? "danger" : undefined },
    { label: thirdLabel, value: thirdValue },
    { label: fourthLabel, value: fourthValue },
  ]
}

function homeBriefDot(tone: HomeBriefTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  if (tone === "info") return "bg-info"
  return "bg-muted-foreground"
}

function homeBriefText(tone: HomeBriefTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  if (tone === "info") return "text-info"
  return "text-muted-foreground"
}

function runsByDay(runs: Run[]) {
  const days = Array.from({ length: 7 }, (_, i) => {
    const d = new Date()
    d.setHours(0, 0, 0, 0)
    d.setDate(d.getDate() - (6 - i))
    return {
      date: d,
      d: d.toLocaleDateString(undefined, { weekday: "short" }),
      succeeded: 0,
      failed: 0,
      running: 0,
    }
  })
  runs.forEach((run) => {
    const created = new Date(run.created_at)
    const bucket = days.find((day) => sameDay(day.date, created))
    if (!bucket) return
    if (run.status === "succeeded") bucket.succeeded += 1
    if (run.status === "failed") bucket.failed += 1
    if (run.status === "running" || run.status === "queued") bucket.running += 1
  })
  return days.map(({ date: _date, ...day }) => day)
}

function primaryMetricInsight(runs: Run[], metrics: RunMetric[]): PrimaryMetricInsight | null {
  const runIds = new Set(runs.map((run) => run.id))
  const byName = new Map<string, { name: string; rows: number; runs: Set<string>; values: number[]; latest: number }>()
  metrics.forEach((metric) => {
    if (!runIds.has(metric.run_id)) return
    const value = Number(metric.value)
    if (!Number.isFinite(value)) return
    const prev = byName.get(metric.name) ?? { name: metric.name, rows: 0, runs: new Set<string>(), values: [], latest: 0 }
    prev.rows += 1
    prev.runs.add(metric.run_id)
    prev.values.push(value)
    prev.latest = Math.max(prev.latest, Date.parse(metric.recorded_at) || 0)
    byName.set(metric.name, prev)
  })

  const best = [...byName.values()]
    .map((metric) => {
      const semantic = metricSemanticScore(metric.name)
      const coverage = metric.runs.size * 12
      const density = Math.min(20, metric.rows)
      const freshness = metric.latest > Date.now() - 24 * 60 * 60 * 1000 ? 8 : 0
      return { metric, score: semantic + coverage + density + freshness }
    })
    .sort((a, b) => b.score - a.score || b.metric.runs.size - a.metric.runs.size || b.metric.rows - a.metric.rows)[0]?.metric

  if (!best) return null
  const avg = best.values.length ? best.values.reduce((acc, value) => acc + value, 0) / best.values.length : null
  return {
    name: best.name,
    rows: best.rows,
    runCount: best.runs.size,
    avg,
    percentLike: looksPercentLike(best.name, best.values),
    higherIsBetter: metricHigherIsBetter(best.name),
  }
}

function metricSeriesByVariant(runs: Run[], metrics: RunMetric[], metricName: string) {
  const runById = new Map(runs.map((r) => [r.id, r]))
  const metricRows = metrics.filter((m) => m.name === metricName && runById.has(m.run_id))
  const variantCounts = new Map<string, number>()
  metricRows.forEach((m) => {
    const run = runById.get(m.run_id)
    if (!run) return
    variantCounts.set(run.variant, (variantCounts.get(run.variant) ?? 0) + 1)
  })
  const labels = [...variantCounts.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 3)
    .map(([variant]) => variant)
  const keys = ["a", "b", "c"].slice(0, labels.length)
  const byStep = new Map<number, Record<string, number | string>>()
  labels.forEach((variant, idx) => {
    const key = keys[idx]
    const grouped = new Map<number, number[]>()
    metricRows.forEach((m) => {
      const run = runById.get(m.run_id)
      if (run?.variant !== variant) return
      const arr = grouped.get(m.step) ?? []
      arr.push(Number(m.value))
      grouped.set(m.step, arr)
    })
    grouped.forEach((values, step) => {
      const row = byStep.get(step) ?? { d: String(step) }
      row[key] = values.reduce((a, b) => a + b, 0) / values.length
      byStep.set(step, row)
    })
  })
  return {
    keys,
    labels: labels.length ? labels.map(formatReadableToken) : ["No metric rows"],
    data: [...byStep.entries()].sort((a, b) => a[0] - b[0]).map(([, row]) => row),
  }
}

function registryMix(registry: RegistryItem[]) {
  const byKind = new Map<string, { kind: string; count: number; bytes: number }>()
  registry.forEach((item) => {
    const prev = byKind.get(item.kind) ?? { kind: item.kind, count: 0, bytes: 0 }
    prev.count += 1
    prev.bytes += item.size_bytes
    byKind.set(item.kind, prev)
  })
  return [...byKind.values()].sort((a, b) => b.count - a.count).slice(0, 6)
}

function metricCoverage(runs: Run[], metrics: RunMetric[]) {
  const runIds = new Set(runs.map((run) => run.id))
  const byMetric = new Map<string, { name: string; rows: number; runs: Set<string>; latest: number }>()
  metrics.forEach((metric) => {
    if (!runIds.has(metric.run_id)) return
    const prev = byMetric.get(metric.name) ?? {
      name: metric.name,
      rows: 0,
      runs: new Set<string>(),
      latest: 0,
    }
    prev.rows += 1
    prev.runs.add(metric.run_id)
    prev.latest = Math.max(prev.latest, Date.parse(metric.recorded_at) || 0)
    byMetric.set(metric.name, prev)
  })
  return Array.from(byMetric.values())
    .sort((a, b) => b.runs.size - a.runs.size || b.rows - a.rows || b.latest - a.latest)
    .slice(0, 6)
    .map((metric) => ({
      name: metric.name,
      rows: metric.rows,
      runs: metric.runs.size,
      latest: metric.latest,
    }))
}

function seriesHasRows<T extends Record<string, unknown>>(rows: T[], keys: string[]) {
  return rows.some((row) => keys.some((key) => Number(row[key]) > 0))
}

function latestMetricByRun(metrics: RunMetric[]) {
  const byRun = new Map<string, { value: number; recordedAt: number }>()
  metrics.forEach((metric) => {
    const recordedAt = Date.parse(metric.recorded_at)
    const prev = byRun.get(metric.run_id)
    if (!prev || recordedAt >= prev.recordedAt) {
      byRun.set(metric.run_id, { value: Number(metric.value), recordedAt })
    }
  })
  return byRun
}

function metricSemanticScore(name: string) {
  const n = name.toLowerCase()
  if (n === "pass@1" || n === "pass" || n.includes("accuracy") || n.includes("success")) return 80
  if (n.includes("score") || n.includes("quality") || n.includes("correct")) return 70
  if (n.includes("latency") || n.includes("duration")) return 42
  if (n.includes("cost") || n.includes("token")) return 30
  return 20
}

function metricHigherIsBetter(name: string) {
  const n = name.toLowerCase()
  return !(n.includes("latency") || n.includes("duration") || n.includes("cost") || n.includes("error") || n.includes("fail") || n.includes("token"))
}

function looksPercentLike(name: string, values: number[]) {
  const n = name.toLowerCase()
  if (n.includes("latency") || n.includes("duration") || n.includes("token") || n.includes("cost")) return false
  if (n.includes("percent") || n.includes("rate") || n.includes("ratio") || n.includes("accuracy") || n.includes("pass") || n.includes("score")) {
    return values.length === 0 || values.every((value) => value >= 0 && value <= 1)
  }
  return values.length > 0 && values.every((value) => value >= 0 && value <= 1)
}

function formatMetricValue(value: number | null | undefined, metric: Pick<PrimaryMetricInsight, "percentLike"> | null) {
  if (value == null || !Number.isFinite(value)) return "no data"
  if (metric?.percentLike) return `${(value * 100).toFixed(1)}%`
  if (Math.abs(value) >= 1000) return formatNumber(Math.round(value))
  if (Number.isInteger(value)) return `${value}`
  return value.toFixed(Math.abs(value) >= 10 ? 1 : 3)
}

function sameDay(a: Date, b: Date) {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate()
}

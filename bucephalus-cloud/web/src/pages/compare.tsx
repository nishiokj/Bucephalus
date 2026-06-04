import { useEffect, useMemo, useState } from "react"
import {
  Activity,
  ArrowDown,
  ArrowUp,
  Boxes,
  CircleAlert,
  Download,
  GitCommit,
  Plus,
  Save,
  Search,
  Settings2,
  X,
} from "lucide-react"
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Line,
  LineChart,
  Scatter,
  ScatterChart,
  Tooltip,
  XAxis,
  YAxis,
  ZAxis,
} from "recharts"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { FilterStrip, SegmentedControl, type FilterOption } from "@/components/filter-strip"
import { ConnectionIssue } from "@/components/connection-issue"
import { StatusPill } from "@/components/status-pill"
import { cloudApi, type Run, type RunMetric } from "@/lib/cloud-api"
import { formatDuration, formatReadableLabel, formatReadableToken, formatRelative, formatUsd } from "@/lib/format"
import { downloadCsv } from "@/lib/export"
import { useRouter } from "@/lib/router"
import { PageHeader } from "@/pages/registry"
import { cn } from "@/lib/utils"

const PALETTE = [
  "var(--chart-1)",
  "var(--chart-2)",
  "var(--chart-3)",
  "var(--chart-4)",
  "var(--chart-5)",
]

const efficiencyCfg: ChartConfig = {
  frontier: { label: "Frontier", color: "var(--chart-1)" },
}

const SAVED_COMPARE_VIEW_KEY = "buc.compare.savedView"

export function ComparePage() {
  const { navigate } = useRouter()
  const [allRuns, setAllRuns] = useState<Run[]>([])
  const [allMetrics, setAllMetrics] = useState<RunMetric[]>([])
  const [runsLoaded, setRunsLoaded] = useState(false)
  const [metricsLoaded, setMetricsLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [pickerOpen, setPickerOpen] = useState(false)
  const [selectedIds, setSelectedIds] = useState<string[]>([])
  const [primaryMetric, setPrimaryMetric] = useState("")
  const [viewMode, setViewMode] = useState<"raw" | "delta">("raw")
  const [savedAt, setSavedAt] = useState<string | null>(null)

  async function loadCompareData() {
    setRunsLoaded(false)
    setMetricsLoaded(false)
    setLoadError(null)
    const runsResult = await cloudApi
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
    if (runsResult.error) {
      setAllRuns([])
      setAllMetrics([])
      setSelectedIds([])
      setLoadError(runsResult.error.message)
      setRunsLoaded(true)
      setMetricsLoaded(true)
      return
    }

    const rows = (runsResult.data ?? []) as Run[]
    const savedView = readSavedCompareView()
    setAllRuns(rows)
    if (savedView) {
      const savedIds = savedView.selectedIds.filter((id) => rows.some((r) => r.id === id))
      if (savedIds.length) setSelectedIds(savedIds)
      else setSelectedIds(defaultSelectedIds(rows))
      if (savedView.primaryMetric) setPrimaryMetric(savedView.primaryMetric)
      if (savedView.viewMode === "raw" || savedView.viewMode === "delta") {
        setViewMode(savedView.viewMode)
      }
      setSavedAt(savedView.savedAt)
    } else {
      setSelectedIds(defaultSelectedIds(rows))
    }
    setRunsLoaded(true)

    const metricsResult = await cloudApi
      .from("run_metrics")
      .select("*")
      .order("step", { ascending: true })
    if (metricsResult.error) {
      setAllMetrics([])
      setLoadError(metricsResult.error.message)
      setMetricsLoaded(true)
      return
    }
    setAllMetrics((metricsResult.data ?? []) as RunMetric[])
    setMetricsLoaded(true)
  }

  useEffect(() => {
    void loadCompareData()
  }, [])

  const loaded = runsLoaded && metricsLoaded
  const unavailable = Boolean(loadError)

  const selected = useMemo(
    () => selectedIds.map((id) => allRuns.find((r) => r.id === id)).filter(Boolean) as Run[],
    [selectedIds, allRuns],
  )

  const seriesByRun = useMemo(() => {
    const out: Record<string, Record<string, number>[]> = {}
    selectedIds.forEach((id) => {
      out[id] = []
    })
    allMetrics.forEach((m) => {
      if (!selectedIds.includes(m.run_id) || m.name !== primaryMetric) return
      out[m.run_id].push({ step: m.step, value: Number(m.value) })
    })
    Object.keys(out).forEach((k) => out[k].sort((a, b) => a.step - b.step))
    return out
  }, [allMetrics, selectedIds, primaryMetric])

  const chartData = useMemo(() => {
    const merged = new Map<number, Record<string, number>>()
    selectedIds.forEach((id, idx) => {
      seriesByRun[id]?.forEach((p) => {
        const e = merged.get(p.step) ?? { step: p.step }
        e[`s${idx}`] = p.value
        merged.set(p.step, e)
      })
    })
    const rows = Array.from(merged.values()).sort((a, b) => a.step - b.step)
    if (viewMode === "raw") return rows
    return rows.map((row) => {
      const baseline = Number(row.s0)
      const next: Record<string, number> = { step: row.step }
      selectedIds.forEach((_, idx) => {
        const key = `s${idx}`
        const value = Number(row[key])
        if (!Number.isFinite(value)) return
        next[key] = baseline && Number.isFinite(baseline) ? ((value - baseline) / baseline) * 100 : 0
      })
      return next
    })
  }, [seriesByRun, selectedIds, allRuns, viewMode])

  const finals = useMemo(() => {
    return selected.map((r) => {
      const arr = seriesByRun[r.id] ?? []
      const last = arr[arr.length - 1]?.value
      return { run: r, final: typeof last === "number" ? last : null, points: arr.length }
    })
  }, [selected, seriesByRun])

  const baseline = typeof finals[0]?.final === "number" ? finals[0].final : null
  const baselineLabel = labelFor(finals[0]?.run) || "baseline"
  const metricInsights = useMemo(
    () => buildMetricInsights(allMetrics, selectedIds),
    [allMetrics, selectedIds],
  )
  const metricOptions = metricInsights.options.map((option) => option.value)

  useEffect(() => {
    if (!metricOptions.length && primaryMetric) {
      setPrimaryMetric("")
    } else if (metricOptions.length && !metricOptions.includes(primaryMetric)) {
      setPrimaryMetric(metricInsights.bestMetric ?? metricOptions[0] ?? primaryMetric)
    } else if (metricOptions.length && !primaryMetric) {
      setPrimaryMetric(metricInsights.bestMetric ?? metricOptions[0])
    }
  }, [metricInsights.bestMetric, metricOptions, primaryMetric])

  const frontier = useMemo(
    () =>
      finals.filter((f) => typeof f.final === "number").map((f) => ({
        name: labelFor(f.run),
        metric: f.final as number,
        duration: Math.max(0, f.run.duration_ms / 60000),
        cost: Number(f.run.cost_usd),
      })),
    [finals],
  )
  const finalChartData = useMemo(
    () =>
      finals
        .filter((f) => typeof f.final === "number")
        .map((f, idx) => ({ name: labelFor(f.run), v: f.final as number, idx })),
    [finals],
  )
  const selectedMetricRows = useMemo(
    () => finals.reduce((acc, row) => acc + row.points, 0),
    [finals],
  )
  const metricRowsByRun = useMemo(() => metricCountByRun(allMetrics, primaryMetric), [allMetrics, primaryMetric])
  const cohortQuality = useMemo(
    () => buildCohortQuality(selected, allMetrics, primaryMetric, metricOptions),
    [selected, allMetrics, primaryMetric, metricOptions],
  )
  const decision = useMemo(
    () => compareDecisionBrief(finals, primaryMetric, cohortQuality),
    [cohortQuality, finals, primaryMetric],
  )
  const needsMetrics = loaded && allRuns.length > 0 && selected.length > 0 && allMetrics.length === 0
  const hasMetricSeries = loaded && selected.length > 0 && selectedMetricRows > 0 && chartData.length > 0
  const suggestedRuns = useMemo(
    () =>
      allRuns
        .filter((run) => !selectedIds.includes(run.id))
        .filter((run) => run.status === "succeeded" || run.status === "running")
        .slice(0, 4),
    [allRuns, selectedIds],
  )

  const chartCfg: ChartConfig = useMemo(() => {
    const c: ChartConfig = {}
    selectedIds.forEach((id, idx) => {
      const r = allRuns.find((x) => x.id === id)
      c[`s${idx}`] = { label: labelFor(r), color: PALETTE[idx % PALETTE.length] }
    })
    return c
  }, [selectedIds, allRuns])

  function saveView() {
    const nextSavedAt = new Date().toLocaleTimeString()
    localStorage.setItem(
      SAVED_COMPARE_VIEW_KEY,
      JSON.stringify({
        selectedIds,
        primaryMetric,
        viewMode,
        savedAt: nextSavedAt,
      }),
    )
    setSavedAt(nextSavedAt)
  }

  function exportCohortCsv() {
    downloadCsv(
      `compare-${primaryMetric}-${new Date().toISOString().slice(0, 10)}.csv`,
      finals.map((f, idx) => {
        const hasFinal = typeof f.final === "number"
        const canCompare = idx > 0 && hasFinal && baseline !== null && baseline !== 0
        const pct = canCompare ? (((f.final as number) - baseline) / baseline) * 100 : null
        return {
          run_id: f.run.id,
          experiment: f.run.experiment_name,
          variant: f.run.variant,
          status: f.run.status,
          metric: primaryMetric,
          value: hasFinal ? f.final : "",
          delta_pct: pct === null ? "" : Number(pct.toFixed(4)),
          duration_ms: f.run.duration_ms,
          cost_usd: Number(f.run.cost_usd),
          region: f.run.region,
          started_at: f.run.started_at ?? f.run.created_at,
        }
      }),
    )
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Compare"
        subtitle="Pick a metric and cohort."
        rightSlot={
          <div className="flex items-center gap-1.5">
            <Settings2 className="h-3 w-3 text-muted-foreground" />
            <SegmentedControl
              value={viewMode}
              onValueChange={(next) => setViewMode(next as "raw" | "delta")}
              options={[
                { value: "raw", label: "overlay" },
                { value: "delta", label: "baseline" },
              ]}
            />
          </div>
        }
        primaryAction={
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={saveView}
          >
            <Save className="h-3 w-3" />
            {savedAt ? "Saved" : "Save view"}
          </Button>
        }
      />

      <div className="border-b border-border bg-background px-3 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <div className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">
            <GitCommit className="h-3 w-3" /> Cohort
          </div>
          {selected.map((r, idx) => (
            <span
              key={r.id}
              className="group flex min-w-0 items-center gap-1.5 rounded-md border border-border bg-card px-1.5 py-1 text-[11.5px]"
              style={{ boxShadow: `inset 3px 0 0 ${PALETTE[idx % PALETTE.length]}` }}
            >
              <span className="max-w-[220px] truncate font-mono text-[11px]">{labelFor(r)}</span>
              <span
                className={cn(
                  "rounded px-1 font-mono text-[10px]",
                  (metricRowsByRun.get(r.id) ?? 0) > 0
                    ? "bg-success/10 text-success"
                    : "bg-muted/50 text-muted-foreground",
                )}
              >
                {(metricRowsByRun.get(r.id) ?? 0) || "no"} rows
              </span>
              <StatusPill status={r.status} withDot={false} />
              <button
                onClick={() => setSelectedIds((s) => s.filter((id) => id !== r.id))}
                className="rounded text-muted-foreground hover:text-foreground"
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-[12px]"
            onClick={() => setPickerOpen(true)}
          >
            <Plus className="h-3 w-3" /> Add run
          </Button>
          {metricOptions.length ? (
            <FilterStrip
              label="Metric"
              value={primaryMetric}
              options={metricInsights.options}
              onValueChange={setPrimaryMetric}
              max={6}
              className="w-full max-w-full sm:ml-auto sm:w-auto"
            />
          ) : (
            <MetricReadinessControl loaded={loaded} />
          )}
        </div>
      </div>

      <div className="grid grid-cols-2 gap-px border-b border-border bg-border lg:grid-cols-4">
        <CompareStat
          label="Runs available"
          value={unavailable ? "-" : loaded ? `${allRuns.length}` : "loading"}
          detail={unavailable ? "request failed" : "from runs API"}
        />
        <CompareStat
          label="Selected cohort"
          value={unavailable ? "-" : `${selected.length}`}
          detail={unavailable ? "unavailable" : selected.length ? `${cohortQuality.coveragePct}% metric coverage` : "choose runs"}
        />
        <CompareStat
          label="Metric rows"
          value={unavailable ? "-" : loaded ? `${selectedMetricRows}` : "loading"}
          detail={unavailable ? "request failed" : primaryMetric ? metricOptionDetail(metricInsights.byName.get(primaryMetric)) : "choose metric"}
        />
        <CompareStat
          label="Baseline"
          value={unavailable ? "-" : baselineLabel}
          detail={unavailable ? "connect API" : baseline === null ? "no metric yet" : `${primaryMetric}=${baseline.toFixed(3)}`}
          mono
        />
      </div>

      {!loaded ? (
        <CompareSkeleton />
      ) : loadError ? (
        <ConnectionIssue
          title="Compare data request failed"
          detail={loadError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadCompareData()}
        />
      ) : allRuns.length === 0 ? (
        <CompareEmptyState
          title="No runs available"
          detail="Runs appear after queuing."
          primary="New experiment"
          onPrimary={() => navigate({ name: "experiment-new" })}
        />
      ) : selected.length === 0 ? (
        <CompareEmptyState
          title="No cohort selected"
          detail="Add two to four runs to compare metrics, latency, and cost."
          primary="Add run"
          onPrimary={() => setPickerOpen(true)}
          secondary="View runs"
          onSecondary={() => navigate({ name: "runs" })}
          suggestions={suggestedRuns}
          onAddSuggestion={(id) => setSelectedIds((s) => (s.includes(id) ? s : [...s, id]))}
        />
      ) : null}

      {loaded && allRuns.length > 0 && selected.length > 0 ? (
        <section className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[300px_minmax(0,1fr)]">
          <div className="grid grid-cols-3 gap-px bg-border lg:grid-cols-1">
            <CompareStat
              label="Primary coverage"
              value={`${cohortQuality.coveragePct}%`}
              detail={`${cohortQuality.runsWithPrimary}/${selected.length} runs emit ${primaryMetric}`}
            />
            <CompareStat
              label="Point spread"
              value={cohortQuality.pointSpread}
              detail="min-max rows per selected run"
            />
            <CompareStat
              label="Metric breadth"
              value={`${cohortQuality.metricBreadth}`}
              detail="distinct signals in cohort"
            />
          </div>
          <div className="bg-background">
            <header className="flex h-8 items-center justify-between border-b border-border px-3">
              <div className="flex items-center gap-2">
                <h3 className="text-[12px] font-semibold">Metric availability</h3>
                <span className="text-[11px] text-muted-foreground">rows by run, before charting</span>
              </div>
              <span className="font-mono text-[11px] text-muted-foreground">
                {cohortQuality.metricBreadth} signals
              </span>
            </header>
            {cohortQuality.rows.length && cohortQuality.metricColumns.length ? (
              <div className="overflow-x-auto text-[12px]">
                <div
                  className="grid min-w-[720px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
                  style={{
                    gridTemplateColumns: `minmax(180px,1.2fr) repeat(${Math.max(1, cohortQuality.metricColumns.length)}, minmax(74px,0.7fr)) 76px`,
                  }}
                >
                  <span>Run</span>
                  {cohortQuality.metricColumns.map((metric) => (
                    <span key={metric} className="truncate">{metric}</span>
                  ))}
                  <span>Total</span>
                </div>
                {cohortQuality.rows.map((row) => (
                  <div
                    key={row.run.id}
                    className="grid min-w-[720px] items-center gap-2 border-b border-border px-3 py-1.5"
                    style={{
                      gridTemplateColumns: `minmax(180px,1.2fr) repeat(${Math.max(1, cohortQuality.metricColumns.length)}, minmax(74px,0.7fr)) 76px`,
                    }}
                  >
                    <span className="truncate font-mono text-[11.5px]">{labelFor(row.run)}</span>
                    {cohortQuality.metricColumns.map((metric) => {
                      const count = row.counts.get(metric) ?? 0
                      const active = metric === primaryMetric
                      return (
                        <span
                          key={metric}
                          className={cn(
                            "inline-flex h-5 w-fit min-w-8 items-center justify-center rounded px-1 font-mono text-[10.5px]",
                            count
                              ? active
                                ? "bg-brand/15 text-brand"
                                : "bg-success/10 text-success"
                              : "bg-muted/40 text-muted-foreground",
                          )}
                        >
                          {count || "—"}
                        </span>
                      )
                    })}
                    <span className="font-mono text-[11px] text-muted-foreground">{row.total}</span>
                  </div>
                ))}
              </div>
            ) : (
              <PanelEmptyState
                compact
                title="No metric evidence"
                detail="Queue measured runs with runtime observations to populate the availability matrix."
              />
            )}
          </div>
        </section>
      ) : null}

      {loaded && allRuns.length > 0 && selected.length > 0 && primaryMetric ? (
        <CompareDecisionPanel decision={decision} />
      ) : null}

      {needsMetrics ? (
        <MetricReadinessPanel
          selected={selected}
          onRuns={() => navigate({ name: "runs" })}
          onNewExperiment={() => navigate({ name: "experiment-new" })}
        />
      ) : null}

      {loaded && allRuns.length > 0 && selected.length > 0 && primaryMetric ? (
        <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-3">
          <div className="bg-background lg:col-span-2">
            <header className="flex h-9 items-center justify-between border-b border-border px-3">
              <div className="flex items-center gap-2">
                <h3 className="text-[12px] font-semibold">{primaryMetric}</h3>
                <span className="text-[11px] text-muted-foreground">{viewMode === "raw" ? "overlay" : "delta vs baseline"}</span>
              </div>
              <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
                <span className="font-mono">N={selected.length}</span>
              </div>
            </header>
            <div className="p-3">
              {hasMetricSeries ? (
                <ChartContainer config={chartCfg} className="h-72 w-full">
                  <LineChart data={chartData}>
                    <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
                    <XAxis
                      dataKey="step"
                      tickLine={false}
                      axisLine={false}
                      tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                    />
                    <YAxis
                      tickLine={false}
                      axisLine={false}
                      width={36}
                      tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                    />
                    <Tooltip
                      contentStyle={{
                        background: "var(--popover)",
                        border: "1px solid var(--border)",
                        borderRadius: 6,
                        fontSize: 11,
                      }}
                    />
                    {selectedIds.map((_, idx) => (
                      <Line
                        key={idx}
                        type="monotone"
                        dataKey={`s${idx}`}
                        stroke={PALETTE[idx % PALETTE.length]}
                        strokeWidth={1.5}
                        dot={false}
                        isAnimationActive={false}
                      />
                    ))}
                  </LineChart>
                </ChartContainer>
              ) : (
                <PanelEmptyState
                  title={`No ${primaryMetric} series`}
                  detail="The selected runs exist, but no metric rows for this metric are available yet."
                />
              )}
            </div>
          </div>

          <div className="bg-background">
            <header className="flex h-9 items-center justify-between border-b border-border px-3">
              <h3 className="text-[12px] font-semibold">Final {primaryMetric}</h3>
              <span className="text-[11px] text-muted-foreground">
                vs {baselineLabel}
              </span>
            </header>
            <div className="p-3">
              {finalChartData.length ? (
                <ChartContainer config={chartCfg} className="h-72 w-full">
                  <BarChart data={finalChartData}>
                    <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
                    <XAxis
                      dataKey="name"
                      tickLine={false}
                      axisLine={false}
                      tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                    />
                    <YAxis
                      tickLine={false}
                      axisLine={false}
                      width={36}
                      tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                    />
                    <Tooltip
                      contentStyle={{
                        background: "var(--popover)",
                        border: "1px solid var(--border)",
                        borderRadius: 6,
                        fontSize: 11,
                      }}
                    />
                    <Bar dataKey="v" radius={[2, 2, 0, 0]}>
                      {finalChartData.map((row) => (
                        <Cell key={row.idx} fill={PALETTE[row.idx % PALETTE.length]} />
                      ))}
                    </Bar>
                  </BarChart>
                </ChartContainer>
              ) : (
                <PanelEmptyState
                  title="No final values"
                  detail="Final bars appear after at least one selected run has a recorded value."
                />
              )}
            </div>
          </div>
        </div>
      ) : null}

      {loaded && allRuns.length > 0 && selected.length > 0 && primaryMetric ? (
        <section className="border-b border-border bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <div className="flex items-center gap-2">
              <h3 className="text-[12px] font-semibold">Efficiency frontier</h3>
              <span className="text-[11px] text-muted-foreground">
                {primaryMetric} vs duration, bubble = cost
              </span>
            </div>
            <span className="font-mono text-[11px] text-muted-foreground">
              upper left is best
            </span>
          </header>
          <div className="p-3">
            {frontier.length ? (
              <ChartContainer config={efficiencyCfg} className="h-48 w-full">
                <ScatterChart data={frontier}>
                  <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" />
                  <XAxis
                    type="number"
                    dataKey="duration"
                    name="minutes"
                    tickLine={false}
                    axisLine={false}
                    tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                  />
                  <YAxis
                    type="number"
                    dataKey="metric"
                    name={primaryMetric}
                    tickLine={false}
                    axisLine={false}
                    tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                  />
                  <ZAxis type="number" dataKey="cost" range={[50, 260]} />
                  <Tooltip
                    cursor={{ stroke: "var(--border)", strokeDasharray: "2 4" }}
                    contentStyle={{
                      background: "var(--popover)",
                      border: "1px solid var(--border)",
                      borderRadius: 6,
                      fontSize: 11,
                    }}
                  />
                  <Scatter dataKey="metric" fill="var(--color-frontier)" isAnimationActive={false} />
                </ScatterChart>
              </ChartContainer>
            ) : (
              <PanelEmptyState
                compact
                title="No efficiency points"
                detail="Frontier analysis needs a metric value, duration, and cost on at least one selected run."
              />
            )}
          </div>
        </section>
      ) : null}

      {loaded && allRuns.length > 0 && selected.length > 0 && primaryMetric ? (
        <section className="bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <div className="flex items-center gap-2">
              <h3 className="text-[12px] font-semibold">Cohort table</h3>
              <span className="text-[11px] text-muted-foreground">
                Δ vs {baselineLabel}
              </span>
              {savedAt ? (
                <span className="font-mono text-[10.5px] text-muted-foreground">
                  saved {savedAt}
                </span>
              ) : null}
            </div>
            <Button
              size="xs"
              variant="ghost"
              className="h-6 gap-1 text-[11px] text-muted-foreground"
              onClick={exportCohortCsv}
              disabled={!primaryMetric || finals.length === 0}
            >
              <Download className="h-3 w-3" /> Export CSV
            </Button>
          </header>
          <div className="overflow-x-auto text-[12px]">
            <div
              className="grid min-w-[880px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
              style={{
                gridTemplateColumns:
                  "minmax(220px,1.4fr) 110px 90px 110px 90px 80px 100px 90px",
              }}
            >
              <span>Run</span>
              <span>Status</span>
              <span>{primaryMetric}</span>
              <span>Δ</span>
              <span>Duration</span>
              <span>Cost</span>
              <span>Region</span>
              <span>Started</span>
            </div>
            {finals.map((f, idx) => {
              const hasFinal = typeof f.final === "number"
              const baselineValue = baseline ?? 0
              const hasDelta = idx > 0 && hasFinal && baseline !== null && baselineValue !== 0
              const delta = hasDelta ? (f.final as number) - baselineValue : 0
              const pct = hasDelta ? (delta / baselineValue) * 100 : 0
              const positive = delta >= 0
              return (
                <div
                  key={f.run.id}
                  className="grid min-w-[880px] items-center gap-2 border-b border-border px-3 py-1.5"
                  style={{
                    gridTemplateColumns:
                      "minmax(220px,1.4fr) 110px 90px 110px 90px 80px 100px 90px",
                  }}
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <span
                      className="h-2 w-2 shrink-0 rounded-full"
                      style={{ background: PALETTE[idx % PALETTE.length] }}
                    />
                    <span className="truncate font-mono text-[11.5px]">
                      {formatReadableLabel(f.run.experiment_name)}/<span className="text-muted-foreground">{formatReadableToken(f.run.variant)}</span>
                    </span>
                  </div>
                  <StatusPill status={f.run.status} />
                  <span className="font-mono text-[11.5px]">
                    {hasFinal ? (f.final as number).toFixed(3) : "—"}
                  </span>
                  <span
                    className={cn(
                      "inline-flex w-fit items-center gap-1 rounded px-1 font-mono text-[10.5px]",
                      !hasDelta
                        ? "text-muted-foreground"
                        : positive
                          ? "bg-success/10 text-success"
                          : "bg-destructive/10 text-destructive",
                    )}
                  >
                    {!hasDelta ? (
                      "—"
                    ) : (
                      <>
                        {positive ? <ArrowUp className="h-3 w-3" /> : <ArrowDown className="h-3 w-3" />}
                        {pct >= 0 ? "+" : ""}
                        {pct.toFixed(1)}%
                      </>
                    )}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {formatDuration(f.run.duration_ms)}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {formatUsd(Number(f.run.cost_usd))}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {f.run.region}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {formatRelative(f.run.started_at ?? f.run.created_at)}
                  </span>
                </div>
              )
            })}
            {finals.length === 0 ? (
              <PanelEmptyState
                compact
                title="No cohort rows"
                detail="Select runs from the cohort picker to populate this comparison table."
              />
            ) : null}
          </div>
        </section>
      ) : null}

      {pickerOpen ? (
        <RunPicker
          runs={allRuns}
          selected={selectedIds}
          primaryMetric={primaryMetric}
          metricRowsByRun={metricRowsByRun}
          onClose={() => setPickerOpen(false)}
          onAdd={(id) => {
            setSelectedIds((s) => (s.includes(id) ? s : [...s, id]))
          }}
        />
      ) : null}
    </div>
  )
}

function labelFor(r?: Run) {
  if (!r) return ""
  return `${formatReadableLabel(r.experiment_name.split("/").pop())}/${formatReadableToken(r.variant)}`
}

type MetricInsight = {
  name: string
  option: FilterOption
  selectedRows: number
  selectedRuns: number
  selectedCount: number
  totalRows: number
  totalRuns: number
  direction: "higher" | "lower"
  score: number
}

function buildMetricInsights(metrics: RunMetric[], selectedIds: string[]) {
  const selected = new Set(selectedIds)
  const byName = new Map<string, {
    name: string
    selectedRows: number
    selectedRuns: Set<string>
    totalRows: number
    totalRuns: Set<string>
  }>()

  metrics.forEach((metric) => {
    const value = Number(metric.value)
    if (!Number.isFinite(value)) return
    const next = byName.get(metric.name) ?? {
      name: metric.name,
      selectedRows: 0,
      selectedRuns: new Set<string>(),
      totalRows: 0,
      totalRuns: new Set<string>(),
    }
    next.totalRows += 1
    next.totalRuns.add(metric.run_id)
    if (selected.has(metric.run_id)) {
      next.selectedRows += 1
      next.selectedRuns.add(metric.run_id)
    }
    byName.set(metric.name, next)
  })

  const insights = Array.from(byName.values()).map((metric): MetricInsight => {
    const selectedRuns = metric.selectedRuns.size
    const totalRuns = metric.totalRuns.size
    const selectedRows = metric.selectedRows
    const totalRows = metric.totalRows
    const semantic = metricSemanticScore(metric.name)
    const coverage = selectedIds.length ? selectedRuns / selectedIds.length : totalRuns ? 0.5 : 0
    const score = selectedRows * 4 + selectedRuns * 32 + totalRuns * 3 + coverage * 24 + semantic
    const count = selectedIds.length ? selectedRows || totalRows : totalRows
    return {
      name: metric.name,
      selectedRows,
      selectedRuns,
      selectedCount: selectedIds.length,
      totalRows,
      totalRuns,
      direction: metricDirection(metric.name),
      score,
      option: {
        value: metric.name,
        label: metric.name,
        count,
        detail: metricOptionDetail({
          selectedRows,
          selectedRuns,
          totalRows,
          totalRuns,
          selectedCount: selectedIds.length,
          direction: metricDirection(metric.name),
        }),
      },
    }
  })

  insights.sort((a, b) =>
    b.score - a.score ||
    b.selectedRuns - a.selectedRuns ||
    b.selectedRows - a.selectedRows ||
    b.totalRows - a.totalRows ||
    a.name.localeCompare(b.name),
  )

  return {
    options: insights.map((insight) => insight.option),
    byName: new Map(insights.map((insight) => [insight.name, insight])),
    bestMetric: insights.find((insight) => insight.selectedRows > 0)?.name ?? insights[0]?.name ?? "",
  }
}

function metricSemanticScore(metric: string) {
  if (/pass|score|accuracy|success|quality|win|reward/i.test(metric)) return 28
  if (/latency|duration|elapsed|p50|p95|runtime|ms$/i.test(metric)) return 20
  if (/cost|token|prompt|completion|usage/i.test(metric)) return 14
  if (/error|fail|loss|timeout|retry/i.test(metric)) return 10
  return 0
}

function metricOptionDetail(
  insight:
    | MetricInsight
    | {
        selectedRows: number
        selectedRuns: number
        totalRows: number
        totalRuns: number
        selectedCount?: number
        direction: "higher" | "lower"
      }
    | undefined,
) {
  if (!insight) return "waiting for metric evidence"
  const selectedCount = "selectedCount" in insight ? insight.selectedCount ?? 0 : 0
  const direction = insight.direction === "lower" ? "lower is better" : "higher is better"
  if (selectedCount > 0) {
    return `${insight.selectedRuns}/${selectedCount} selected runs · ${insight.selectedRows || insight.totalRows} rows · ${direction}`
  }
  return `${insight.totalRuns} run${insight.totalRuns === 1 ? "" : "s"} · ${insight.totalRows} rows · ${direction}`
}

type FinalMetricRow = { run: Run; final: number | null; points: number }
type CohortQuality = ReturnType<typeof buildCohortQuality>
type CompareDecision = {
  metricLabel: string
  direction: "higher" | "lower"
  best: DecisionCell
  efficient: DecisionCell
  baseline: DecisionCell
  risk: DecisionCell
}
type DecisionCell = {
  label: string
  value: string
  detail: string
  tone: "success" | "warning" | "muted"
}

function CompareDecisionPanel({ decision }: { decision: CompareDecision }) {
  return (
    <section className="border-b border-border bg-background">
      <header className="flex h-9 items-center justify-between border-b border-border px-3">
        <div className="flex items-center gap-2">
          <h3 className="text-[12px] font-semibold">Decision brief</h3>
          <span className="text-[11px] text-muted-foreground">
            {decision.metricLabel} · {decision.direction === "higher" ? "higher is better" : "lower is better"}
          </span>
        </div>
      </header>
      <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-4">
        <DecisionTile title="Best result" cell={decision.best} />
        <DecisionTile title="Efficient pick" cell={decision.efficient} />
        <DecisionTile title="Baseline move" cell={decision.baseline} />
        <DecisionTile title="Evidence risk" cell={decision.risk} />
      </div>
    </section>
  )
}

function DecisionTile({ title, cell }: { title: string; cell: DecisionCell }) {
  return (
    <div className="min-w-0 bg-background p-3">
      <div className="flex items-center justify-between gap-2">
        <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{title}</div>
        <span
          className={cn(
            "h-1.5 w-1.5 rounded-full",
            cell.tone === "success" ? "bg-success" : cell.tone === "warning" ? "bg-warning" : "bg-muted-foreground",
          )}
        />
      </div>
      <div className="mt-1 truncate font-mono text-[13px]" title={cell.label}>{cell.label}</div>
      <div
        className={cn(
          "mt-0.5 truncate font-mono text-[16px] font-medium",
          cell.tone === "success" ? "text-success" : cell.tone === "warning" ? "text-warning" : "text-foreground",
        )}
        title={cell.value}
      >
        {cell.value}
      </div>
      <div className="truncate text-[10.5px] text-muted-foreground" title={cell.detail}>{cell.detail}</div>
    </div>
  )
}

function compareDecisionBrief(finals: FinalMetricRow[], primaryMetric: string, quality: CohortQuality): CompareDecision {
  const direction = metricDirection(primaryMetric)
  const valid = finals.filter((row): row is FinalMetricRow & { final: number } => typeof row.final === "number")
  const best = [...valid].sort((a, b) => metricCompare(a.final, b.final, direction))[0]
  const baseline = valid[0]
  const baselineMove = baseline && best && best.run.id !== baseline.run.id
    ? metricDelta(best.final, baseline.final, direction)
    : null
  const efficient = [...valid]
    .sort((a, b) => efficiencyScore(b, direction) - efficiencyScore(a, direction))
    [0]
  const riskTone = quality.coveragePct >= 90 && quality.pointSpread !== "0" ? "success" : quality.coveragePct >= 50 ? "warning" : "muted"

  return {
    metricLabel: primaryMetric || "metric",
    direction,
    best: best
      ? {
          label: labelFor(best.run),
          value: formatCompareMetricValue(best.final, primaryMetric),
          detail: baselineMove ? `${baselineMove} vs baseline` : `${best.points} point${best.points === 1 ? "" : "s"}`,
          tone: "success",
        }
      : {
          label: "No winner yet",
          value: "waiting",
          detail: "Selected runs have no final metric value.",
          tone: "muted",
        },
    efficient: efficient
      ? {
          label: labelFor(efficient.run),
          value: formatUsd(Number(efficient.run.cost_usd)),
          detail: `${formatDuration(efficient.run.duration_ms)} · ${formatCompareMetricValue(efficient.final, primaryMetric)}`,
          tone: "success",
        }
      : {
          label: "No frontier",
          value: "waiting",
          detail: "Needs metric, duration, and cost.",
          tone: "muted",
        },
    baseline: baseline && best
      ? {
          label: labelFor(baseline.run),
          value: best.run.id === baseline.run.id ? "baseline leads" : baselineMove ?? "flat",
          detail: best.run.id === baseline.run.id ? "First selected run is still best." : `${labelFor(best.run)} is leading.`,
          tone: best.run.id === baseline.run.id ? "success" : "warning",
        }
      : {
          label: "No baseline",
          value: "waiting",
          detail: "First selected run needs a metric value.",
          tone: "muted",
        },
    risk: {
      label: `${quality.runsWithPrimary}/${finals.length} runs`,
      value: `${quality.coveragePct}% covered`,
      detail: `${quality.metricBreadth} signals · point spread ${quality.pointSpread}`,
      tone: riskTone,
    },
  }
}

function metricDirection(metric: string): "higher" | "lower" {
  return /latency|duration|cost|error|fail|loss|handoff|timeout|tokens/i.test(metric) ? "lower" : "higher"
}

function metricCompare(a: number, b: number, direction: "higher" | "lower") {
  return direction === "higher" ? b - a : a - b
}

function metricDelta(best: number, baseline: number, direction: "higher" | "lower") {
  if (!Number.isFinite(best) || !Number.isFinite(baseline)) return null
  const delta = direction === "higher" ? best - baseline : baseline - best
  if (baseline === 0) return delta === 0 ? "flat" : `${delta > 0 ? "+" : ""}${delta.toFixed(3)}`
  return `${delta >= 0 ? "+" : ""}${((delta / Math.abs(baseline)) * 100).toFixed(1)}%`
}

function efficiencyScore(row: FinalMetricRow & { final: number }, direction: "higher" | "lower") {
  const cost = Math.max(0.01, Number(row.run.cost_usd) || 0.01)
  const minutes = Math.max(0.1, row.run.duration_ms / 60000 || 0.1)
  const metric = direction === "higher" ? Math.max(0, row.final) : 1 / Math.max(0.0001, row.final)
  return metric / (cost * minutes)
}

function formatCompareMetricValue(value: number, metric: string) {
  if (/latency|duration|ms/i.test(metric)) return formatDuration(value)
  if (/rate|ratio|pass|score|accuracy|success/i.test(metric) && Math.abs(value) <= 1) return `${(value * 100).toFixed(1)}%`
  return value >= 1000 ? value.toLocaleString() : value.toFixed(value % 1 ? 3 : 0)
}

function defaultSelectedIds(rows: Run[]) {
  return rows
    .filter((r) => r.status === "succeeded" || r.status === "running")
    .slice(0, 4)
    .map((r) => r.id)
}

function countStatuses(runs: Run[]) {
  return runs.reduce(
    (acc, run) => {
      acc[run.status] += 1
      return acc
    },
    { queued: 0, running: 0, succeeded: 0, failed: 0 } satisfies Record<Run["status"], number>,
  )
}

function metricCountByRun(metrics: RunMetric[], metricName: string) {
  const counts = new Map<string, number>()
  metrics.forEach((metric) => {
    if (metric.name !== metricName) return
    counts.set(metric.run_id, (counts.get(metric.run_id) ?? 0) + 1)
  })
  return counts
}

function buildCohortQuality(
  selected: Run[],
  metrics: RunMetric[],
  primaryMetric: string,
  metricOptions: string[],
) {
  const selectedIds = new Set(selected.map((run) => run.id))
  const rowsByRun = new Map<string, RunMetric[]>()
  metrics.forEach((metric) => {
    if (!selectedIds.has(metric.run_id)) return
    const rows = rowsByRun.get(metric.run_id) ?? []
    rows.push(metric)
    rowsByRun.set(metric.run_id, rows)
  })
  const metricTotals = new Map<string, number>()
  rowsByRun.forEach((rows) => {
    rows.forEach((metric) => metricTotals.set(metric.name, (metricTotals.get(metric.name) ?? 0) + 1))
  })
  const metricColumns = [
    ...(primaryMetric ? [primaryMetric] : []),
    ...metricOptions
      .filter((metric) => metric !== primaryMetric && (metricTotals.get(metric) ?? 0) > 0)
      .sort((a, b) => (metricTotals.get(b) ?? 0) - (metricTotals.get(a) ?? 0) || a.localeCompare(b))
      .slice(0, 5),
  ]
  const primaryCounts = selected.map((run) => rowsByRun.get(run.id)?.filter((metric) => metric.name === primaryMetric).length ?? 0)
  const rows = selected.map((run) => {
    const counts = new Map<string, number>()
    const runRows = rowsByRun.get(run.id) ?? []
    runRows.forEach((metric) => counts.set(metric.name, (counts.get(metric.name) ?? 0) + 1))
    return {
      run,
      counts,
      total: runRows.length,
    }
  })
  const runsWithPrimary = primaryCounts.filter((count) => count > 0).length
  const nonZeroPrimary = primaryCounts.filter((count) => count > 0)
  const minPoints = nonZeroPrimary.length ? Math.min(...nonZeroPrimary) : 0
  const maxPoints = nonZeroPrimary.length ? Math.max(...nonZeroPrimary) : 0
  return {
    rows,
    metricColumns,
    runsWithPrimary,
    coveragePct: selected.length ? Math.round((runsWithPrimary / selected.length) * 100) : 0,
    pointSpread: maxPoints ? `${minPoints}-${maxPoints}` : "0",
    metricBreadth: metricTotals.size,
  }
}

function readSavedCompareView() {
  try {
    const raw = localStorage.getItem(SAVED_COMPARE_VIEW_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as {
      selectedIds?: unknown
      primaryMetric?: unknown
      viewMode?: unknown
      savedAt?: unknown
    }
    return {
      selectedIds: Array.isArray(parsed.selectedIds)
        ? parsed.selectedIds.filter((id): id is string => typeof id === "string")
        : [],
      primaryMetric: typeof parsed.primaryMetric === "string" ? parsed.primaryMetric : "",
      viewMode: parsed.viewMode,
      savedAt: typeof parsed.savedAt === "string" ? parsed.savedAt : null,
    }
  } catch {
    return null
  }
}

function MetricReadinessControl({ loaded }: { loaded: boolean }) {
  return (
    <div className="flex w-full min-w-0 max-w-full items-center gap-2 rounded-md border border-border bg-card px-2 py-1.5 text-[11.5px] sm:ml-auto sm:w-auto">
      <span className="flex min-w-0 items-center gap-1.5 text-muted-foreground">
        <Activity className="h-3 w-3" />
        Metric
      </span>
      <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-[10.5px] text-foreground">
        {loaded ? "waiting" : "loading"}
      </span>
    </div>
  )
}

function MetricReadinessPanel({
  selected,
  onRuns,
  onNewExperiment,
}: {
  selected: Run[]
  onRuns: () => void
  onNewExperiment: () => void
}) {
  const statusCounts = countStatuses(selected)
  return (
    <section className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(0,1fr)_320px]">
      <div className="bg-background p-3">
        <div className="flex items-start gap-2">
          <CircleAlert className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
          <div className="min-w-0">
            <h3 className="text-[13px] font-semibold">Metric rows have not arrived yet</h3>
            <p className="mt-1 max-w-3xl text-[12px] leading-relaxed text-muted-foreground">
              Compare can see the selected run records, but the runtime results endpoint has not returned metric observations for this workspace. The page is holding the cohort instead of showing guessed metrics.
            </p>
          </div>
        </div>
        <div className="mt-3 grid grid-cols-2 gap-px bg-border md:grid-cols-4">
          <CompareStat label="Selected" value={`${selected.length}`} detail="runs retained" />
          <CompareStat label="Succeeded" value={`${statusCounts.succeeded}`} detail="ready candidates" />
          <CompareStat label="Running" value={`${statusCounts.running}`} detail="may emit later" />
          <CompareStat label="Failed" value={`${statusCounts.failed}`} detail="inspect traces" />
        </div>
        <div className="mt-3 flex flex-wrap items-center gap-1.5">
          <Button size="sm" className="h-7 gap-1 bg-brand text-[12px] text-brand-foreground hover:bg-brand/90" onClick={onRuns}>
            View runs
          </Button>
          <Button size="sm" variant="outline" className="h-7 gap-1 text-[12px]" onClick={onNewExperiment}>
            Queue measured run
          </Button>
        </div>
      </div>
      <div className="bg-background p-3">
        <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Selected run evidence</div>
        <div className="mt-2 grid gap-px bg-border">
          {selected.slice(0, 5).map((run) => (
            <div key={run.id} className="min-w-0 bg-card p-2">
              <div className="flex items-center justify-between gap-2">
                <span className="truncate font-mono text-[11.5px]">{labelFor(run)}</span>
                <StatusPill status={run.status} withDot={false} />
              </div>
              <div className="mt-1 flex items-center justify-between gap-2 text-[10.5px] text-muted-foreground">
                <span className="truncate font-mono">{run.region}</span>
                <span className="shrink-0 font-mono">{formatRelative(run.created_at)}</span>
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  )
}

function CompareStat({
  label,
  value,
  detail,
  mono = false,
}: {
  label: string
  value: string
  detail: string
  mono?: boolean
}) {
  return (
    <div className="min-w-0 bg-background p-2.5">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className={cn("mt-0.5 truncate font-mono text-[16px] font-medium", mono ? "text-[13px]" : "")}>
        {value}
      </div>
      <div className="truncate text-[10.5px] text-muted-foreground">{detail}</div>
    </div>
  )
}

function CompareSkeleton() {
  return (
    <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-3">
      <div className="bg-background p-3 lg:col-span-2">
        <div className="h-3 w-28 rounded bg-muted" />
        <div className="mt-5 h-60 rounded bg-muted/60" />
      </div>
      <div className="bg-background p-3">
        <div className="h-3 w-24 rounded bg-muted" />
        <div className="mt-5 h-60 rounded bg-muted/60" />
      </div>
    </div>
  )
}

function CompareEmptyState({
  title,
  detail,
  primary,
  onPrimary,
  secondary,
  onSecondary,
  suggestions = [],
  onAddSuggestion,
}: {
  title: string
  detail: string
  primary: string
  onPrimary: () => void
  secondary?: string
  onSecondary?: () => void
  suggestions?: Run[]
  onAddSuggestion?: (id: string) => void
}) {
  return (
    <div className="border-b border-border bg-background px-3 py-12">
      <div className="mx-auto flex max-w-xl flex-col items-center gap-2 text-center">
        <CircleAlert className="h-5 w-5 text-muted-foreground" />
        <div className="text-[14px] font-medium">{title}</div>
        <div className="max-w-[calc(100vw-7rem)] text-[12px] text-muted-foreground sm:max-w-md">{detail}</div>
        <div className="mt-2 flex items-center gap-1.5">
          <Button size="sm" className="h-7 gap-1 bg-brand text-[12px] text-brand-foreground hover:bg-brand/90" onClick={onPrimary}>
            <Plus className="h-3 w-3" />
            {primary}
          </Button>
          {secondary && onSecondary ? (
            <Button size="sm" variant="outline" className="h-7 text-[12px]" onClick={onSecondary}>
              {secondary}
            </Button>
          ) : null}
        </div>
        {suggestions.length > 0 && onAddSuggestion ? (
          <div className="mt-4 flex max-w-full flex-wrap justify-center gap-1.5">
            {suggestions.map((run) => (
              <button
                key={run.id}
                onClick={() => onAddSuggestion(run.id)}
                className="flex max-w-[220px] items-center gap-1.5 rounded-md border border-border bg-card px-2 py-1 text-left text-[11.5px] hover:bg-accent/50"
              >
                <GitCommit className="h-3 w-3 shrink-0 text-muted-foreground" />
                <span className="truncate font-mono">{labelFor(run)}</span>
                <StatusPill status={run.status} withDot={false} />
              </button>
            ))}
          </div>
        ) : null}
      </div>
    </div>
  )
}

function PanelEmptyState({
  title,
  detail,
  compact = false,
}: {
  title: string
  detail: string
  compact?: boolean
}) {
  return (
    <div className={cn("flex flex-col items-center justify-center gap-2 rounded border border-dashed border-border bg-muted/20 px-4 text-center", compact ? "min-h-32 py-7" : "h-72 py-10")}>
      <CircleAlert className="h-4 w-4 text-muted-foreground" />
      <div className="text-[13px] font-medium">{title}</div>
      <div className="max-w-[calc(100vw-7rem)] text-[12px] text-muted-foreground sm:max-w-sm">{detail}</div>
    </div>
  )
}

function RunPicker({
  runs,
  selected,
  primaryMetric,
  metricRowsByRun,
  onClose,
  onAdd,
}: {
  runs: Run[]
  selected: string[]
  primaryMetric: string
  metricRowsByRun: Map<string, number>
  onClose: () => void
  onAdd: (id: string) => void
}) {
  const [q, setQ] = useState("")
  const [coverageFilter, setCoverageFilter] = useState<"all" | "with-metric" | "missing">("all")
  const metricLabel = primaryMetric || "metric"
  const candidates = useMemo(
    () => buildRunCandidates(runs, selected, metricRowsByRun),
    [metricRowsByRun, runs, selected],
  )
  const filteredRuns = useMemo(() => {
    const needle = q.trim().toLowerCase()
    return candidates.filter((candidate) => {
      const { run, metricRows } = candidate
      if (coverageFilter === "with-metric" && metricRows === 0) return false
      if (coverageFilter === "missing" && metricRows > 0) return false
      if (!needle) return true
      return [
        run.experiment_name,
        run.variant,
        run.status,
        run.region,
        labelFor(run),
        candidate.signal,
        formatReadableToken(run.id),
      ]
        .filter(Boolean)
        .some((part) => part.toLowerCase().includes(needle))
    })
  }, [candidates, coverageFilter, q])
  const recommended = useMemo(
    () => candidates.filter((candidate) => !candidate.selected).slice(0, 3),
    [candidates],
  )
  const readyRecommendations = useMemo(
    () => candidates.filter((candidate) => !candidate.selected && candidate.metricRows > 0).slice(0, 3),
    [candidates],
  )
  const pickerBrief = useMemo(
    () => buildPickerBrief(candidates, selected, primaryMetric),
    [candidates, primaryMetric, selected],
  )

  return (
    <div className="fixed inset-0 z-30 flex items-start justify-center bg-background/70 pt-20 backdrop-blur">
      <div className="w-[880px] max-w-[94vw] overflow-hidden rounded-md border border-border bg-popover shadow-2xl">
        <header className="flex flex-col gap-2 border-b border-border px-3 py-2">
          <div className="flex items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-2">
              <Boxes className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <div className="min-w-0">
                <div className="text-[12px] font-semibold">Add runs to cohort</div>
                <div className="truncate text-[10.5px] text-muted-foreground">
                  Showing {filteredRuns.length}/{runs.length} runs · {selected.length} selected
                </div>
              </div>
            </div>
            <button onClick={onClose} className="rounded text-muted-foreground hover:text-foreground" aria-label="Close run picker">
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
          <RunPickerBriefView
            brief={pickerBrief}
            disabled={readyRecommendations.length === 0}
            onAddReady={() => readyRecommendations.forEach((candidate) => onAdd(candidate.run.id))}
          />
          <div className="flex min-w-0 flex-col gap-2 sm:flex-row sm:items-center">
            <div className="relative min-w-0 flex-1">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={q}
                onChange={(event) => setQ(event.target.value)}
                placeholder="Search experiment, variant, status, region, or evidence"
                className="h-8 pl-8 text-[12px]"
                autoFocus
              />
            </div>
            <SegmentedControl
              value={coverageFilter}
              onValueChange={(value) => setCoverageFilter(value as typeof coverageFilter)}
              options={[
                { value: "all", label: "All", count: runs.length },
                {
                  value: "with-metric",
                  label: metricLabel,
                  count: runs.filter((run) => (metricRowsByRun.get(run.id) ?? 0) > 0).length,
                },
                {
                  value: "missing",
                  label: "Missing",
                  count: runs.filter((run) => (metricRowsByRun.get(run.id) ?? 0) === 0).length,
                },
              ]}
              className="shrink-0"
            />
          </div>
        </header>
        {recommended.length ? (
          <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-3">
            {recommended.map((candidate) => (
              <button
                key={candidate.run.id}
                onClick={() => onAdd(candidate.run.id)}
                className="min-w-0 bg-popover p-2 text-left hover:bg-accent/40"
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="truncate font-mono text-[11.5px] text-foreground">
                    {labelFor(candidate.run)}
                  </span>
                  <span className={cn("shrink-0 rounded px-1.5 py-0.5 font-mono text-[10px]", candidate.tone)}>
                    {candidate.score}
                  </span>
                </div>
                <div className="mt-1 flex items-center justify-between gap-2 text-[10.5px] text-muted-foreground">
                  <span className="truncate">{candidate.signal}</span>
                  <span className="shrink-0 font-mono">{formatRelative(candidate.run.created_at)}</span>
                </div>
              </button>
            ))}
          </div>
        ) : null}
        <div className="overflow-x-auto">
          <div
            className="grid min-w-[840px] border-b border-border bg-muted/20 px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
            style={{
              gridTemplateColumns:
                "minmax(240px,1.35fr) minmax(130px,.8fr) 96px 118px 82px 74px 64px",
            }}
          >
            <span>Run</span>
            <span>Variant</span>
            <span>Status</span>
            <span>Evidence</span>
            <span>Duration</span>
            <span>Cost</span>
            <span>Age</span>
          </div>
          <div className="max-h-[420px] min-w-[840px] overflow-auto scrollbar-thin">
            {filteredRuns.length > 0 ? filteredRuns.map((candidate) => {
              const r = candidate.run
              const taken = candidate.selected
              return (
                <button
                  key={r.id}
                  disabled={taken}
                  onClick={() => {
                    onAdd(r.id)
                  }}
                  className={cn(
                    "grid w-full items-center gap-2 border-b border-border px-3 py-1.5 text-left text-[12px]",
                    taken ? "opacity-50" : "hover:bg-accent/40",
                  )}
                  style={{
                    gridTemplateColumns:
                      "minmax(240px,1.35fr) minmax(130px,.8fr) 96px 118px 82px 74px 64px",
                  }}
                  aria-label={`${taken ? "Already selected" : "Add"} ${labelFor(r)} to cohort`}
                >
                  <span className="min-w-0">
                    <span className="block truncate font-mono text-[11.5px] text-foreground">
                      {formatReadableLabel(r.experiment_name)}
                    </span>
                    <span className="block truncate text-[10.5px] text-muted-foreground">
                      {candidate.signal}
                    </span>
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate font-mono text-[11px] text-foreground">
                      {formatReadableToken(r.variant)}
                    </span>
                    <span className="block truncate font-mono text-[10.5px] text-muted-foreground">
                      {r.region || "region —"}
                    </span>
                  </span>
                  <StatusPill status={r.status} />
                  <span
                    className={cn(
                      "inline-flex w-fit items-center gap-1 rounded px-1 font-mono text-[10.5px]",
                      candidate.metricRows
                        ? "bg-success/10 text-success"
                        : candidate.statusReady
                          ? "bg-info/10 text-info"
                          : "bg-muted/40 text-muted-foreground",
                    )}
                    title={`${metricLabel} evidence`}
                  >
                    {candidate.metricRows ? `${candidate.metricRows} rows` : candidate.statusReady ? "pending" : "missing"}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {formatDuration(r.duration_ms)}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {formatUsd(Number(r.cost_usd))}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {formatRelative(r.created_at)}
                  </span>
                </button>
              )
            }) : (
              <PanelEmptyState
                compact
                title={runs.length ? "No matching runs" : "No runs to add"}
                detail={
                  runs.length
                    ? "Adjust the search or metric coverage filter."
                    : "Queue an experiment run, then return here to build a comparison cohort."
                }
              />
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

type RunCandidate = {
  run: Run
  metricRows: number
  selected: boolean
  statusReady: boolean
  score: number
  signal: string
  tone: string
}

type RunPickerBrief = {
  tone: "success" | "warning" | "danger" | "info" | "muted"
  verdict: string
  detail: string
  facts: { label: string; value: string; tone?: RunPickerBrief["tone"] }[]
}

function RunPickerBriefView({
  brief,
  disabled,
  onAddReady,
}: {
  brief: RunPickerBrief
  disabled: boolean
  onAddReady: () => void
}) {
  return (
    <div className="grid grid-cols-1 gap-px overflow-hidden rounded-md border border-border bg-border lg:grid-cols-[minmax(220px,1fr)_minmax(0,1.8fr)_auto]">
      <div className="min-w-0 bg-popover px-2.5 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className={cn("h-2 w-2 shrink-0 rounded-full", pickerBriefDot(brief.tone))} />
          <div className="min-w-0">
            <div className={cn("truncate text-[12px] font-medium", pickerBriefText(brief.tone))}>{brief.verdict}</div>
            <div className="truncate text-[10.5px] text-muted-foreground" title={brief.detail}>{brief.detail}</div>
          </div>
        </div>
      </div>
      <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
        {brief.facts.map((fact) => (
          <div key={fact.label} className="min-w-0 bg-popover px-2.5 py-2">
            <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
            <div className={cn("truncate font-mono text-[11.5px]", fact.tone ? pickerBriefText(fact.tone) : "")}>{fact.value}</div>
          </div>
        ))}
      </div>
      <div className="bg-popover p-2">
        <Button
          variant="outline"
          size="sm"
          className="h-7 w-full justify-between gap-2 text-[12px] lg:w-auto"
          disabled={disabled}
          onClick={onAddReady}
        >
          Add best ready
          <Plus className="h-3 w-3" />
        </Button>
      </div>
    </div>
  )
}

function buildPickerBrief(
  candidates: RunCandidate[],
  selected: string[],
  primaryMetric: string,
): RunPickerBrief {
  const available = candidates.filter((candidate) => !candidate.selected)
  const ready = available.filter((candidate) => candidate.metricRows > 0)
  const pending = available.filter((candidate) => candidate.statusReady && candidate.metricRows === 0)
  const failed = available.filter((candidate) => candidate.run.status === "failed")
  const metricLabel = primaryMetric || "metric"

  if (ready.length >= 3) {
    return {
      tone: "success",
      verdict: "Ready cohort candidates",
      detail: `Enough unselected runs already emit ${metricLabel}. Add the best ready set or search manually.`,
      facts: pickerBriefFacts("Ready", `${ready.length}`, "Selected", `${selected.length}`, "Pending", `${pending.length}`, "Failed", `${failed.length}`, "success"),
    }
  }

  if (ready.length > 0) {
    return {
      tone: "warning",
      verdict: "Thin ready set",
      detail: `${ready.length} run${ready.length === 1 ? "" : "s"} emit ${metricLabel}; consider adding pending runs only if they are still active.`,
      facts: pickerBriefFacts("Ready", `${ready.length}`, "Selected", `${selected.length}`, "Pending", `${pending.length}`, "Failed", `${failed.length}`, "warning"),
    }
  }

  if (pending.length > 0) {
    return {
      tone: "info",
      verdict: "Waiting on metrics",
      detail: `No unselected run has ${metricLabel} rows yet, but active or queued runs may report soon.`,
      facts: pickerBriefFacts("Ready", "0", "Selected", `${selected.length}`, "Pending", `${pending.length}`, "Failed", `${failed.length}`, "info"),
    }
  }

  return {
    tone: failed.length ? "danger" : "muted",
    verdict: failed.length ? "Inspect failures first" : "No addable evidence",
    detail: failed.length
      ? "Available runs are mostly failed or missing metrics. Inspect traces before adding them to a comparison."
      : "Queue or complete more runs before expanding this cohort.",
    facts: pickerBriefFacts("Ready", "0", "Selected", `${selected.length}`, "Pending", "0", "Failed", `${failed.length}`, failed.length ? "danger" : "muted"),
  }
}

function pickerBriefFacts(
  firstLabel: string,
  firstValue: string,
  secondLabel: string,
  secondValue: string,
  thirdLabel: string,
  thirdValue: string,
  fourthLabel: string,
  fourthValue: string,
  tone: RunPickerBrief["tone"],
): RunPickerBrief["facts"] {
  return [
    { label: firstLabel, value: firstValue, tone },
    { label: secondLabel, value: secondValue },
    { label: thirdLabel, value: thirdValue, tone: tone === "info" ? "info" : undefined },
    { label: fourthLabel, value: fourthValue, tone: tone === "danger" ? "danger" : undefined },
  ]
}

function pickerBriefDot(tone: RunPickerBrief["tone"]) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  if (tone === "info") return "bg-info animate-pulse"
  return "bg-muted-foreground"
}

function pickerBriefText(tone: RunPickerBrief["tone"]) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  if (tone === "info") return "text-info"
  return "text-foreground"
}

function buildRunCandidates(
  runs: Run[],
  selected: string[],
  metricRowsByRun: Map<string, number>,
): RunCandidate[] {
  const selectedSet = new Set(selected)
  return runs
    .map((run) => {
      const metricRows = metricRowsByRun.get(run.id) ?? 0
      const hoursOld = Math.max(0, (Date.now() - Date.parse(run.created_at)) / 3_600_000)
      const freshScore = hoursOld < 24 ? 8 : hoursOld < 168 ? 3 : 0
      const statusScore = metricRows
        ? run.status === "succeeded"
          ? 32
          : run.status === "running"
            ? 24
            : run.status === "queued"
              ? 8
              : -10
        : run.status === "running"
          ? 18
          : run.status === "queued"
            ? 14
            : run.status === "succeeded"
              ? 8
              : 0
      const metricScore = metricRows ? 46 + Math.min(metricRows, 30) : 0
      const score = Math.max(0, Math.round(metricScore + statusScore + freshScore))
      const statusReady = run.status === "running" || run.status === "queued"
      const signal = metricRows
        ? `${metricRows} metric rows ready`
        : statusReady
          ? `${formatReadableLabel(run.status)}; waiting for metrics`
          : run.status === "failed"
            ? "failed; inspect trace before adding"
            : "no metric evidence yet"
      const tone = metricRows
        ? "bg-success/10 text-success"
        : statusReady
          ? "bg-info/10 text-info"
          : "bg-muted/50 text-muted-foreground"
      return {
        run,
        metricRows,
        selected: selectedSet.has(run.id),
        statusReady,
        score,
        signal,
        tone,
      }
    })
    .sort((a, b) => {
      if (a.selected !== b.selected) return a.selected ? 1 : -1
      return b.score - a.score || Date.parse(b.run.created_at) - Date.parse(a.run.created_at)
    })
}

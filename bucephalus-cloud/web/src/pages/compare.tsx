import { useEffect, useMemo, useState } from "react"
import {
  ArrowDown,
  ArrowUp,
  Boxes,
  ChevronDown,
  Filter,
  GitCommit,
  Plus,
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
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { Button } from "@/components/ui/button"
import { StatusPill } from "@/components/status-pill"
import { supabase, type Run, type RunMetric } from "@/lib/supabase"
import { formatDuration, formatRelative, formatUsd } from "@/lib/format"
import { PageHeader } from "@/pages/registry"
import { cn } from "@/lib/utils"

const PALETTE = [
  "var(--chart-1)",
  "var(--chart-2)",
  "var(--chart-3)",
  "var(--chart-4)",
  "var(--chart-5)",
]

export function ComparePage() {
  const [allRuns, setAllRuns] = useState<Run[]>([])
  const [allMetrics, setAllMetrics] = useState<RunMetric[]>([])
  const [pickerOpen, setPickerOpen] = useState(false)
  const [selectedIds, setSelectedIds] = useState<string[]>([])
  const [primaryMetric, setPrimaryMetric] = useState("pass@1")

  useEffect(() => {
    void supabase
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
      .then(({ data }) => {
        const rows = (data ?? []) as Run[]
        setAllRuns(rows)
        setSelectedIds(
          rows
            .filter((r) => r.status === "succeeded" || r.status === "running")
            .slice(0, 4)
            .map((r) => r.id),
        )
      })
    void supabase
      .from("run_metrics")
      .select("*")
      .order("step", { ascending: true })
      .then(({ data }) => setAllMetrics((data ?? []) as RunMetric[]))
  }, [])

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
      const label = labelFor(allRuns.find((r) => r.id === id))
      seriesByRun[id]?.forEach((p) => {
        const e = merged.get(p.step) ?? { step: p.step }
        e[`s${idx}`] = p.value
        e[`l${idx}`] = label as unknown as number
        merged.set(p.step, e)
      })
    })
    return Array.from(merged.values()).sort((a, b) => a.step - b.step)
  }, [seriesByRun, selectedIds, allRuns])

  const finals = useMemo(() => {
    return selected.map((r) => {
      const arr = seriesByRun[r.id] ?? []
      const last = arr[arr.length - 1]?.value ?? 0
      return { run: r, final: last }
    })
  }, [selected, seriesByRun])

  const baseline = finals[0]?.final ?? 0

  const chartCfg: ChartConfig = useMemo(() => {
    const c: ChartConfig = {}
    selectedIds.forEach((id, idx) => {
      const r = allRuns.find((x) => x.id === id)
      c[`s${idx}`] = { label: labelFor(r), color: PALETTE[idx % PALETTE.length] }
    })
    return c
  }, [selectedIds, allRuns])

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Compare"
        subtitle="Run-over-run, variant-over-variant. Pick the metric, slice the cohort."
        rightSlot={
          <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
            <Settings2 className="h-3 w-3" /> View options <ChevronDown className="h-3 w-3" />
          </Button>
        }
        primaryAction={
          <Button size="sm" className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90">
            Save view
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
              className="group flex items-center gap-1.5 rounded-md border border-border bg-card px-1.5 py-1 text-[11.5px]"
              style={{ boxShadow: `inset 3px 0 0 ${PALETTE[idx % PALETTE.length]}` }}
            >
              <span className="font-mono text-[11px]">{labelFor(r)}</span>
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
          <div className="ml-auto flex items-center gap-1.5">
            <span className="text-[11px] text-muted-foreground">Metric</span>
            <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
              {primaryMetric} <ChevronDown className="h-3 w-3" />
            </Button>
            <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
              <Filter className="h-3 w-3" /> Tags <ChevronDown className="h-3 w-3" />
            </Button>
          </div>
        </div>
        <div className="mt-1.5 flex items-center gap-2">
          {(["pass@1", "latency_p50", "tokens_in"] as const).map((m) => (
            <button
              key={m}
              onClick={() => setPrimaryMetric(m)}
              className={cn(
                "rounded-md px-2 py-1 text-[11.5px]",
                primaryMetric === m
                  ? "bg-secondary text-foreground"
                  : "text-muted-foreground hover:bg-secondary",
              )}
            >
              {m}
            </button>
          ))}
        </div>
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-3">
        <div className="bg-background lg:col-span-2">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <div className="flex items-center gap-2">
              <h3 className="text-[12px] font-semibold">{primaryMetric}</h3>
              <span className="text-[11px] text-muted-foreground">overlay</span>
            </div>
            <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
              <span className="font-mono">N={selected.length}</span>
            </div>
          </header>
          <div className="p-3">
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
          </div>
        </div>

        <div className="bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <h3 className="text-[12px] font-semibold">Final {primaryMetric}</h3>
            <span className="text-[11px] text-muted-foreground">
              vs {labelFor(finals[0]?.run)}
            </span>
          </header>
          <div className="p-3">
            <ChartContainer config={chartCfg} className="h-72 w-full">
              <BarChart data={finals.map((f, idx) => ({ name: labelFor(f.run), v: f.final, idx }))}>
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
                  {finals.map((_, idx) => (
                    <Cell key={idx} fill={PALETTE[idx % PALETTE.length]} />
                  ))}
                </Bar>
              </BarChart>
            </ChartContainer>
          </div>
        </div>
      </div>

      <section className="bg-background">
        <header className="flex h-9 items-center justify-between border-b border-border px-3">
          <div className="flex items-center gap-2">
            <h3 className="text-[12px] font-semibold">Cohort table</h3>
            <span className="text-[11px] text-muted-foreground">
              Δ vs {labelFor(finals[0]?.run)}
            </span>
          </div>
          <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
            export csv
          </div>
        </header>
        <div className="text-[12px]">
          <div
            className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
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
            const delta = f.final - baseline
            const pct = baseline ? (delta / baseline) * 100 : 0
            const positive = delta >= 0
            return (
              <div
                key={f.run.id}
                className="grid items-center gap-2 border-b border-border px-3 py-1.5"
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
                    {f.run.experiment_name}/<span className="text-muted-foreground">{f.run.variant}</span>
                  </span>
                </div>
                <StatusPill status={f.run.status} />
                <span className="font-mono text-[11.5px]">{f.final.toFixed(3)}</span>
                <span
                  className={cn(
                    "inline-flex w-fit items-center gap-1 rounded px-1 font-mono text-[10.5px]",
                    idx === 0
                      ? "text-muted-foreground"
                      : positive
                        ? "bg-success/10 text-success"
                        : "bg-destructive/10 text-destructive",
                  )}
                >
                  {idx === 0 ? (
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
        </div>
      </section>

      {pickerOpen ? (
        <RunPicker
          runs={allRuns}
          selected={selectedIds}
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
  return `${r.experiment_name.split("/").pop()}/${r.variant}`
}

function RunPicker({
  runs,
  selected,
  onClose,
  onAdd,
}: {
  runs: Run[]
  selected: string[]
  onClose: () => void
  onAdd: (id: string) => void
}) {
  return (
    <div className="fixed inset-0 z-30 flex items-start justify-center bg-background/70 pt-20 backdrop-blur">
      <div className="w-[640px] max-w-[92vw] overflow-hidden rounded-md border border-border bg-popover shadow-2xl">
        <header className="flex items-center justify-between border-b border-border px-3 py-2">
          <div className="flex items-center gap-2">
            <Boxes className="h-3.5 w-3.5 text-muted-foreground" />
            <span className="text-[12px] font-semibold">Add runs to cohort</span>
          </div>
          <button onClick={onClose} className="rounded text-muted-foreground hover:text-foreground">
            <X className="h-3.5 w-3.5" />
          </button>
        </header>
        <div className="max-h-[420px] overflow-auto scrollbar-thin">
          {runs.map((r) => {
            const taken = selected.includes(r.id)
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
                    "minmax(180px,1fr) 110px 100px 90px 80px 100px",
                }}
              >
                <span className="truncate font-mono text-[11.5px]">
                  {r.experiment_name}
                </span>
                <span className="truncate font-mono text-[11px] text-muted-foreground">
                  {r.variant}
                </span>
                <StatusPill status={r.status} />
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
          })}
        </div>
      </div>
    </div>
  )
}

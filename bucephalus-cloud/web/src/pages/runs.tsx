import { useEffect, useMemo, useState } from "react"
import {
  Activity,
  AlertTriangle,
  ArrowUpRight,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Cpu,
  Filter,
  GitCommit,
  Info,
  Pause,
  Play,
  Search,
  Square,
} from "lucide-react"
import {
  Area,
  AreaChart,
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
import { StatusPill } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import {
  supabase,
  type Run,
  type RunMetric,
  type Trace,
} from "@/lib/supabase"
import { formatDuration, formatRelative, formatUsd } from "@/lib/format"
import { PageHeader } from "@/pages/registry"
import { cn } from "@/lib/utils"

export function RunsPage() {
  const { navigate } = useRouter()
  const [runs, setRuns] = useState<Run[]>([])
  const [q, setQ] = useState("")
  const [statusFilter, setStatusFilter] = useState<string | null>(null)

  useEffect(() => {
    void supabase
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
      .then(({ data }) => setRuns((data ?? []) as Run[]))
  }, [])

  const filtered = useMemo(
    () =>
      runs.filter((r) => {
        if (statusFilter && r.status !== statusFilter) return false
        if (q && !`${r.experiment_name} ${r.variant} ${r.region}`.toLowerCase().includes(q.toLowerCase())) return false
        return true
      }),
    [runs, q, statusFilter],
  )

  const counts = useMemo(() => {
    const c: Record<string, number> = {}
    runs.forEach((r) => (c[r.status] = (c[r.status] ?? 0) + 1))
    return c
  }, [runs])

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Runs"
        subtitle="Live and historical executions. Click any run to inspect traces and metrics."
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
        <Stat label="Running" value={`${counts.running ?? 0}`} dot="bg-info animate-pulse" />
        <Stat label="Queued" value={`${counts.queued ?? 0}`} dot="bg-muted-foreground" />
        <Stat label="Succeeded (24h)" value={`${counts.succeeded ?? 0}`} dot="bg-success" />
        <Stat label="Failed (24h)" value={`${counts.failed ?? 0}`} dot="bg-destructive" />
      </div>

      <div className="sticky top-11 z-10 flex items-center justify-between gap-3 border-b border-border bg-background/95 px-3 py-1.5 backdrop-blur">
        <div className="flex items-center gap-1.5">
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
        <div className="flex items-center gap-1.5">
          <div className="relative">
            <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="Filter runs..."
              className="h-7 w-72 pl-7 text-[12px]"
            />
          </div>
          <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
            <Filter className="h-3 w-3" /> region <ChevronDown className="h-3 w-3" />
          </Button>
        </div>
      </div>

      <div className="text-[12px]">
        <div
          className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
          style={{
            gridTemplateColumns:
              "minmax(220px,1.4fr) minmax(140px,0.8fr) 100px 90px 80px 100px 90px 24px",
          }}
        >
          <span>Experiment</span>
          <span>Variant</span>
          <span>Status</span>
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
            className="grid w-full items-center gap-2 border-b border-border px-3 py-1.5 text-left hover:bg-accent/40"
            style={{
              gridTemplateColumns:
                "minmax(220px,1.4fr) minmax(140px,0.8fr) 100px 90px 80px 100px 90px 24px",
            }}
          >
            <div className="flex min-w-0 items-center gap-2">
              <Activity className="h-3 w-3 shrink-0 text-muted-foreground" />
              <span className="truncate font-mono text-[11.5px]">{r.experiment_name}</span>
            </div>
            <span className="truncate font-mono text-[11px] text-muted-foreground">{r.variant}</span>
            <StatusPill status={r.status} />
            <span className="font-mono text-[11px] text-muted-foreground">{formatDuration(r.duration_ms)}</span>
            <span className="font-mono text-[11px] text-muted-foreground">{formatUsd(Number(r.cost_usd))}</span>
            <span className="font-mono text-[11px] text-muted-foreground">{r.region}</span>
            <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(r.started_at ?? r.created_at)}</span>
            <ArrowUpRight className="h-3 w-3 text-muted-foreground" />
          </button>
        ))}
      </div>
    </div>
  )
}

function Stat({ label, value, dot }: { label: string; value: string; dot: string }) {
  return (
    <div className="flex items-center justify-between bg-background p-2.5">
      <div className="flex items-center gap-1.5 text-[10.5px] uppercase tracking-wide text-muted-foreground">
        <span className={cn("inline-block h-1.5 w-1.5 rounded-full", dot)} />
        {label}
      </div>
      <div className="font-mono text-[16px] font-medium">{value}</div>
    </div>
  )
}

const traceCfg: ChartConfig = {
  pass: { label: "pass@1", color: "var(--chart-2)" },
  latency: { label: "latency", color: "var(--chart-1)" },
  tokens: { label: "tokens", color: "var(--chart-3)" },
}

export function RunDetailPage() {
  const { route, navigate } = useRouter()
  const id = route.name === "run-detail" ? route.id : ""
  const [run, setRun] = useState<Run | null>(null)
  const [metrics, setMetrics] = useState<RunMetric[]>([])
  const [traces, setTraces] = useState<Trace[]>([])
  const [tab, setTab] = useState<"traces" | "metrics" | "config" | "logs">("traces")

  useEffect(() => {
    if (!id) return
    void supabase.from("runs").select("*").eq("id", id).maybeSingle().then(({ data }) => setRun((data as Run) ?? null))
    void supabase
      .from("run_metrics")
      .select("*")
      .eq("run_id", id)
      .order("step", { ascending: true })
      .then(({ data }) => setMetrics((data ?? []) as RunMetric[]))
    void supabase
      .from("traces")
      .select("*")
      .eq("run_id", id)
      .order("recorded_at", { ascending: true })
      .then(({ data }) => setTraces((data ?? []) as Trace[]))
  }, [id])

  const series = useMemo(() => {
    const byStep = new Map<number, { step: number; pass?: number; latency?: number; tokens?: number }>()
    metrics.forEach((m) => {
      const e = byStep.get(m.step) ?? { step: m.step }
      if (m.name === "pass@1") e.pass = Number(m.value)
      if (m.name === "latency_p50") e.latency = Number(m.value)
      if (m.name === "tokens_in") e.tokens = Number(m.value)
      byStep.set(m.step, e)
    })
    return Array.from(byStep.values()).sort((a, b) => a.step - b.step)
  }, [metrics])

  if (!run) {
    return <div className="p-6 text-[12px] text-muted-foreground">Loading…</div>
  }

  const isRunning = run.status === "running"

  return (
    <div className="flex flex-col">
      <PageHeader
        title={run.experiment_name + " · " + run.variant}
        subtitle={
          isRunning
            ? "Live run. Traces stream in below."
            : `Completed ${formatRelative(run.ended_at)} · ${formatDuration(run.duration_ms)}`
        }
        rightSlot={
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-[12px]"
            onClick={() => navigate({ name: "runs" })}
          >
            All runs
          </Button>
        }
        primaryAction={
          isRunning ? (
            <div className="flex items-center gap-1.5">
              <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
                <Pause className="h-3 w-3" /> Pause
              </Button>
              <Button
                variant="outline"
                size="sm"
                className="h-7 gap-1 text-[12px] text-destructive hover:text-destructive"
              >
                <Square className="h-3 w-3" /> Stop
              </Button>
            </div>
          ) : (
            <Button size="sm" className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90">
              <Play className="h-3 w-3" /> Re-run
            </Button>
          )
        }
      />

      <div className="grid grid-cols-2 gap-px border-b border-border bg-border md:grid-cols-6">
        <KV label="Status" value={<StatusPill status={run.status} />} />
        <KV label="Variant" value={run.variant} mono />
        <KV label="Region" value={run.region} mono />
        <KV label="Duration" value={formatDuration(run.duration_ms)} mono />
        <KV label="Cost" value={formatUsd(Number(run.cost_usd))} mono />
        <KV label="Started" value={formatRelative(run.started_at)} mono />
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-3">
        <Card title="pass@1" badge="metric">
          <ChartContainer config={traceCfg} className="h-40 w-full">
            <AreaChart data={series}>
              <defs>
                <linearGradient id="g-pass" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor="var(--color-pass)" stopOpacity={0.4} />
                  <stop offset="100%" stopColor="var(--color-pass)" stopOpacity={0} />
                </linearGradient>
              </defs>
              <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
              <XAxis dataKey="step" hide />
              <YAxis hide domain={[0, 1]} />
              <Tooltip
                contentStyle={{
                  background: "var(--popover)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  fontSize: 11,
                }}
              />
              <Area
                type="monotone"
                dataKey="pass"
                stroke="var(--color-pass)"
                fill="url(#g-pass)"
                strokeWidth={1.5}
              />
            </AreaChart>
          </ChartContainer>
        </Card>
        <Card title="latency p50 (ms)" badge="metric">
          <ChartContainer config={traceCfg} className="h-40 w-full">
            <LineChart data={series}>
              <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
              <XAxis dataKey="step" hide />
              <YAxis hide />
              <Tooltip
                contentStyle={{
                  background: "var(--popover)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  fontSize: 11,
                }}
              />
              <Line
                type="monotone"
                dataKey="latency"
                stroke="var(--color-latency)"
                strokeWidth={1.5}
                dot={false}
              />
            </LineChart>
          </ChartContainer>
        </Card>
        <Card title="tokens in (per step)" badge="metric">
          <ChartContainer config={traceCfg} className="h-40 w-full">
            <LineChart data={series}>
              <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
              <XAxis dataKey="step" hide />
              <YAxis hide />
              <Tooltip
                contentStyle={{
                  background: "var(--popover)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  fontSize: 11,
                }}
              />
              <Line
                type="monotone"
                dataKey="tokens"
                stroke="var(--color-tokens)"
                strokeWidth={1.5}
                dot={false}
              />
            </LineChart>
          </ChartContainer>
        </Card>
      </div>

      <div className="border-b border-border">
        <nav className="flex h-9 items-center gap-1 px-2">
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

      {tab === "traces" ? <TracesView traces={traces} /> : null}
      {tab === "metrics" ? <MetricsView metrics={metrics} /> : null}
      {tab === "config" ? (
        <pre className="overflow-auto p-3 font-mono text-[11px] leading-relaxed text-muted-foreground">
          {JSON.stringify(run, null, 2)}
        </pre>
      ) : null}
      {tab === "logs" ? (
        <pre className="overflow-auto bg-card p-3 font-mono text-[11px] leading-relaxed text-muted-foreground">
{`[06:12:01] starting variant ${run.variant} on ${run.region}
[06:12:02] pulling agent image (cached) ...
[06:12:03] pulling benchmark dataset ...
[06:12:08] sandbox spawned (microvm-93af)
[06:12:14] task 1/500 -> ok
[06:12:35] task 2/500 -> ok
[06:13:02] task 3/500 -> retry (rate-limit, backoff 4s)
[06:13:08] task 3/500 -> ok
...`}
        </pre>
      ) : null}
    </div>
  )
}

function MetricsView({ metrics }: { metrics: RunMetric[] }) {
  return (
    <div className="text-[12px]">
      <div
        className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
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
          className="grid items-center gap-2 border-b border-border px-3 py-1 hover:bg-accent/40"
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

function TracesView({ traces }: { traces: Trace[] }) {
  const [open, setOpen] = useState<Set<string>>(new Set())
  const grouped = useMemo(() => groupTraces(traces), [traces])
  return (
    <div className="text-[12px]">
      {grouped.map((g, i) => {
        const isOpen = open.has(g.id)
        return (
          <div key={g.id + i} className="border-b border-border">
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
              <span className="truncate font-mono text-[11.5px]">{g.span}</span>
              <span className="font-mono text-[10.5px] text-muted-foreground">
                {g.children.length}x
              </span>
              <span className="truncate text-[11.5px] text-muted-foreground">
                {g.message}
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
                      {c.span}
                    </span>
                    <span className="truncate text-[11px]">{c.message}</span>
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

import { useEffect, useState } from "react"
import {
  Activity,
  ArrowUpRight,
  CheckCircle2,
  Clock,
  CpuIcon,
  DollarSign,
  GitCommit,
  Package,
  Plus,
  Rocket,
} from "lucide-react"
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { StatusPill, KindBadge } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import { supabase, type Run, type RegistryItem } from "@/lib/supabase"
import { formatRelative, formatUsd, formatDuration } from "@/lib/format"

const RUNS_TIMESERIES = [
  { d: "Mon", succeeded: 38, failed: 4, running: 0 },
  { d: "Tue", succeeded: 41, failed: 6, running: 0 },
  { d: "Wed", succeeded: 52, failed: 2, running: 0 },
  { d: "Thu", succeeded: 48, failed: 8, running: 0 },
  { d: "Fri", succeeded: 63, failed: 3, running: 0 },
  { d: "Sat", succeeded: 22, failed: 1, running: 0 },
  { d: "Sun", succeeded: 47, failed: 5, running: 3 },
]

const COMPUTE_USAGE = Array.from({ length: 48 }, (_, i) => ({
  t: i,
  gpu: 30 + Math.sin(i / 6) * 20 + Math.random() * 12,
  cpu: 50 + Math.cos(i / 5) * 15 + Math.random() * 10,
}))

const PASS_AT_1 = [
  { d: "Mon", sonnet: 0.42, gpt5: 0.39 },
  { d: "Tue", sonnet: 0.46, gpt5: 0.41 },
  { d: "Wed", sonnet: 0.48, gpt5: 0.45 },
  { d: "Thu", sonnet: 0.51, gpt5: 0.46 },
  { d: "Fri", sonnet: 0.55, gpt5: 0.49 },
  { d: "Sat", sonnet: 0.57, gpt5: 0.5 },
  { d: "Sun", sonnet: 0.6, gpt5: 0.52 },
]

const runsCfg: ChartConfig = {
  succeeded: { label: "Succeeded", color: "var(--chart-2)" },
  failed: { label: "Failed", color: "var(--chart-4)" },
  running: { label: "Running", color: "var(--chart-1)" },
}

const compCfg: ChartConfig = {
  gpu: { label: "GPU", color: "var(--chart-1)" },
  cpu: { label: "CPU", color: "var(--chart-2)" },
}

const passCfg: ChartConfig = {
  sonnet: { label: "sonnet", color: "var(--chart-3)" },
  gpt5: { label: "gpt5", color: "var(--chart-1)" },
}

export function HomePage() {
  const { navigate } = useRouter()
  const [runs, setRuns] = useState<Run[]>([])
  const [registry, setRegistry] = useState<RegistryItem[]>([])

  useEffect(() => {
    void supabase
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
      .limit(8)
      .then(({ data }) => setRuns((data ?? []) as Run[]))
    void supabase
      .from("registry_items")
      .select("*")
      .order("created_at", { ascending: false })
      .limit(6)
      .then(({ data }) => setRegistry((data ?? []) as RegistryItem[]))
  }, [])

  return (
    <div className="flex flex-col">
      <Hero />

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-4">
        <Stat
          icon={Activity}
          label="Runs (7d)"
          value="324"
          delta="+12.4%"
          trend="up"
        />
        <Stat
          icon={CheckCircle2}
          label="Pass rate"
          value="94.2%"
          delta="+1.8pp"
          trend="up"
        />
        <Stat
          icon={Clock}
          label="Avg duration"
          value="42m 11s"
          delta="-3m"
          trend="up"
        />
        <Stat
          icon={DollarSign}
          label="Spend (mtd)"
          value="$1,284.30"
          delta="+$182"
          trend="down"
        />
      </div>

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
          <ChartContainer config={runsCfg} className="h-44 w-full">
            <BarChart data={RUNS_TIMESERIES} barCategoryGap={8}>
              <CartesianGrid
                vertical={false}
                stroke="var(--border)"
                strokeDasharray="2 4"
              />
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
              <Tooltip
                cursor={{ fill: "var(--accent)", opacity: 0.4 }}
                contentStyle={{
                  background: "var(--popover)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  fontSize: 11,
                }}
              />
              <Bar
                stackId="r"
                dataKey="succeeded"
                fill="var(--color-succeeded)"
                radius={[0, 0, 0, 0]}
              />
              <Bar
                stackId="r"
                dataKey="failed"
                fill="var(--color-failed)"
                radius={[0, 0, 0, 0]}
              />
              <Bar
                stackId="r"
                dataKey="running"
                fill="var(--color-running)"
                radius={[2, 2, 0, 0]}
              />
            </BarChart>
          </ChartContainer>
        </Panel>

        <Panel title="Compute" subtitle="48h GPU/CPU utilization">
          <ChartContainer config={compCfg} className="h-44 w-full">
            <AreaChart data={COMPUTE_USAGE}>
              <defs>
                <linearGradient id="g-gpu" x1="0" y1="0" x2="0" y2="1">
                  <stop
                    offset="0%"
                    stopColor="var(--color-gpu)"
                    stopOpacity={0.5}
                  />
                  <stop
                    offset="100%"
                    stopColor="var(--color-gpu)"
                    stopOpacity={0}
                  />
                </linearGradient>
                <linearGradient id="g-cpu" x1="0" y1="0" x2="0" y2="1">
                  <stop
                    offset="0%"
                    stopColor="var(--color-cpu)"
                    stopOpacity={0.4}
                  />
                  <stop
                    offset="100%"
                    stopColor="var(--color-cpu)"
                    stopOpacity={0}
                  />
                </linearGradient>
              </defs>
              <CartesianGrid
                vertical={false}
                stroke="var(--border)"
                strokeDasharray="2 4"
              />
              <XAxis dataKey="t" tickLine={false} axisLine={false} hide />
              <YAxis hide />
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
                dataKey="gpu"
                stroke="var(--color-gpu)"
                fill="url(#g-gpu)"
                strokeWidth={1.5}
              />
              <Area
                type="monotone"
                dataKey="cpu"
                stroke="var(--color-cpu)"
                fill="url(#g-cpu)"
                strokeWidth={1.5}
              />
            </AreaChart>
          </ChartContainer>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-5">
        <Panel
          title="Live runs"
          subtitle="Click to inspect traces"
          className="lg:col-span-3"
        >
          <div className="text-[12px]">
            <RowHeader
              cols={["Experiment", "Variant", "Status", "Duration", "Cost", "Region", ""]}
              widths={[
                "minmax(180px,1fr)",
                "120px",
                "100px",
                "90px",
                "70px",
                "100px",
                "20px",
              ]}
            />
            {runs.map((r) => (
              <button
                key={r.id}
                onClick={() => navigate({ name: "run-detail", id: r.id })}
                className="grid w-full items-center gap-2 border-t border-border px-3 py-1.5 text-left hover:bg-accent/40"
                style={{
                  gridTemplateColumns:
                    "minmax(180px,1fr) 120px 100px 90px 70px 100px 20px",
                }}
              >
                <div className="flex min-w-0 items-center gap-2">
                  <GitCommit className="h-3 w-3 shrink-0 text-muted-foreground" />
                  <span className="truncate font-mono text-[11.5px]">
                    {r.experiment_name}
                  </span>
                </div>
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
                <span className="truncate font-mono text-[11px] text-muted-foreground">
                  {r.region}
                </span>
                <ArrowUpRight className="h-3 w-3 text-muted-foreground" />
              </button>
            ))}
          </div>
        </Panel>

        <Panel
          title="pass@1"
          subtitle="sonnet vs gpt5 (7d)"
          className="lg:col-span-2"
        >
          <ChartContainer config={passCfg} className="h-44 w-full">
            <AreaChart data={PASS_AT_1}>
              <CartesianGrid
                vertical={false}
                stroke="var(--border)"
                strokeDasharray="2 4"
              />
              <XAxis
                dataKey="d"
                tickLine={false}
                axisLine={false}
                tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
              />
              <YAxis
                domain={[0.3, 0.7]}
                tickLine={false}
                axisLine={false}
                width={28}
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
              <Area
                type="monotone"
                dataKey="sonnet"
                stroke="var(--color-sonnet)"
                fill="var(--color-sonnet)"
                fillOpacity={0.18}
                strokeWidth={1.5}
              />
              <Area
                type="monotone"
                dataKey="gpt5"
                stroke="var(--color-gpt5)"
                fill="var(--color-gpt5)"
                fillOpacity={0.18}
                strokeWidth={1.5}
              />
            </AreaChart>
          </ChartContainer>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-5">
        <Panel title="Recently pushed to registry" className="lg:col-span-3">
          <div className="text-[12px]">
            <RowHeader
              cols={["Resource", "Kind", "Version", "Size", "Status", "Pushed"]}
              widths={[
                "minmax(220px,1fr)",
                "90px",
                "70px",
                "70px",
                "100px",
                "90px",
              ]}
            />
            {registry.map((it) => (
              <div
                key={it.id}
                className="grid items-center gap-2 border-t border-border px-3 py-1.5"
                style={{
                  gridTemplateColumns:
                    "minmax(220px,1fr) 90px 70px 70px 100px 90px",
                }}
              >
                <div className="flex min-w-0 items-center gap-2">
                  <Package className="h-3 w-3 shrink-0 text-muted-foreground" />
                  <span className="truncate font-mono text-[11.5px]">
                    {it.name}
                  </span>
                </div>
                <KindBadge kind={it.kind} />
                <span className="font-mono text-[11px] text-muted-foreground">
                  {it.version}
                </span>
                <span className="font-mono text-[11px] text-muted-foreground">
                  {(it.size_bytes / 1024 / 1024).toFixed(0)} MB
                </span>
                <StatusPill status={it.status} />
                <span className="font-mono text-[11px] text-muted-foreground">
                  {formatRelative(it.created_at)}
                </span>
              </div>
            ))}
          </div>
        </Panel>

        <Panel title="Quick actions" className="lg:col-span-2">
          <div className="grid grid-cols-2 gap-px bg-border">
            {QUICK_ACTIONS.map((q) => (
              <button
                key={q.label}
                onClick={() => navigate(q.route)}
                className="group flex flex-col gap-1 bg-card p-3 text-left hover:bg-accent/40"
              >
                <q.icon className="h-3.5 w-3.5 text-muted-foreground group-hover:text-foreground" />
                <div className="text-[12px] font-medium">{q.label}</div>
                <div className="text-[10.5px] leading-tight text-muted-foreground">
                  {q.hint}
                </div>
              </button>
            ))}
          </div>
        </Panel>
      </div>
    </div>
  )
}

function Hero() {
  return (
    <div className="relative overflow-hidden border-b border-border">
      <div className="grid-bg pointer-events-none absolute inset-0 opacity-30" />
      <div className="relative flex items-center gap-6 px-5 py-5">
        <div className="grid h-12 w-12 place-items-center rounded-md border border-border bg-card">
          <Rocket className="h-5 w-5 text-brand" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h1 className="text-[18px] font-semibold tracking-tight">
              Welcome back, kira
            </h1>
            <span className="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground">
              prod
            </span>
          </div>
          <p className="text-[12.5px] text-muted-foreground">
            3 runs active. Last deploy of agent/coding-claude-sonnet 4.7.2 was 18m ago.
          </p>
        </div>
        <div className="hidden items-center gap-2 md:flex">
          <Mini label="Tokens / hr" value="284k" />
          <Mini label="$ / hr" value="$2.18" />
          <Mini label="Active GPUs" value="14" />
        </div>
      </div>
    </div>
  )
}

const QUICK_ACTIONS = [
  {
    label: "New experiment",
    icon: Plus,
    hint: "Author and queue a new run",
    route: { name: "experiment-new" } as const,
  },
  {
    label: "Push to registry",
    icon: Package,
    hint: "Upload an agent, benchmark or MCP",
    route: { name: "registry" } as const,
  },
  {
    label: "Compare runs",
    icon: GitCommit,
    hint: "Run-over-run, variant-over-variant",
    route: { name: "compare" } as const,
  },
  {
    label: "Compute settings",
    icon: CpuIcon,
    hint: "Regions, GPU pools, quotas",
    route: { name: "settings" } as const,
  },
]

function Mini({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border border-border bg-card px-2.5 py-1.5">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className="font-mono text-[13px] font-medium">{value}</div>
    </div>
  )
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
  trend: "up" | "down"
}) {
  return (
    <div className="flex items-start justify-between bg-background p-3">
      <div>
        <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wide text-muted-foreground">
          <Icon className="h-3 w-3" />
          {label}
        </div>
        <div className="mt-1 font-mono text-[20px] font-medium tracking-tight">
          {value}
        </div>
      </div>
      <span
        className={
          "rounded px-1 font-mono text-[10px] " +
          (trend === "up"
            ? "bg-success/10 text-success"
            : "bg-destructive/10 text-destructive")
        }
      >
        {delta}
      </span>
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
        <div className="flex items-baseline gap-2">
          <h3 className="text-[12px] font-semibold tracking-tight">{title}</h3>
          {subtitle ? (
            <span className="text-[11px] text-muted-foreground">
              {subtitle}
            </span>
          ) : null}
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
}: {
  cols: string[]
  widths: string[]
}) {
  return (
    <div
      className="grid items-center gap-2 px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
      style={{ gridTemplateColumns: widths.join(" ") }}
    >
      {cols.map((c, i) => (
        <span key={i}>{c}</span>
      ))}
    </div>
  )
}

// (chart helpers above)

import { useEffect, useMemo, useState } from "react"
import {
  Activity,
  CheckCircle2,
  CircleAlert,
  CreditCard,
  Download,
  Globe,
  Key,
  RefreshCw,
  Settings,
  Shield,
} from "lucide-react"
import {
  Bar,
  BarChart,
  CartesianGrid,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ConnectionIssue } from "@/components/connection-issue"
import { SegmentedControl } from "@/components/filter-strip"
import { StatusPill } from "@/components/status-pill"
import { PageHeader } from "@/pages/registry"
import { useRouter } from "@/lib/router"
import { cn } from "@/lib/utils"
import {
  probeCloudConnection,
  supabase,
  type CloudProbeResult,
  type Experiment,
  type RegistryItem,
  type Run,
} from "@/lib/supabase"
import { formatDuration, formatReadableLabel, formatRelative, formatUsd } from "@/lib/format"
import { downloadCsv } from "@/lib/export"
import {
  useWorkspacePreferences,
  writeWorkspacePreferences,
  type WorkspacePreferences,
} from "@/lib/workspace"

const billingTimelineCfg: ChartConfig = {
  spend: { label: "Spend", color: "var(--chart-1)" },
  runs: { label: "Runs", color: "var(--chart-2)" },
  unpriced: { label: "Unpriced", color: "var(--chart-5)" },
}

const principalSourceCfg: ChartConfig = {
  objects: { label: "Evidence", color: "var(--chart-3)" },
}

type PrincipalRow = {
  name: string
  detail: string
  role: string
  source: string
  objects: number
  lastSeen: string | null
  initials: string
  sources: Record<string, number>
}

export function SettingsPage() {
  const workspace = useWorkspacePreferences()
  const [form, setForm] = useState<WorkspacePreferences>(workspace)
  const [runs, setRuns] = useState<Run[]>([])
  const [runsError, setRunsError] = useState<string | null>(null)
  const [savedAt, setSavedAt] = useState<string | null>(null)
  const [diagnostics, setDiagnostics] = useState<CloudProbeResult[]>([])
  const [checking, setChecking] = useState(false)
  const [checkedAt, setCheckedAt] = useState<string | null>(null)

  useEffect(() => {
    setForm(workspace)
  }, [workspace])

  useEffect(() => {
    let alive = true
    setChecking(true)
    void probeCloudConnection({ apiBase: workspace.apiBase, userToken: workspace.userToken })
      .then((results) => {
        if (!alive) return
        setDiagnostics(results)
        setCheckedAt(new Date().toLocaleTimeString())
      })
      .finally(() => {
        if (alive) setChecking(false)
      })
    return () => {
      alive = false
    }
  }, [workspace.apiBase, workspace.userToken])

  async function loadRunEvidence() {
    setRunsError(null)
    const { data, error } = await supabase
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
    setRuns(error ? [] : ((data ?? []) as Run[]))
    setRunsError(error?.message ?? null)
  }

  useEffect(() => {
    void loadRunEvidence()
  }, [])

  async function runDiagnostics(next: WorkspacePreferences = form) {
    setChecking(true)
    const results = await probeCloudConnection({ apiBase: next.apiBase, userToken: next.userToken })
    setDiagnostics(results)
    setCheckedAt(new Date().toLocaleTimeString())
    setChecking(false)
  }

  function saveConnection() {
    writeWorkspacePreferences(form)
    setSavedAt(new Date().toLocaleTimeString())
    void runDiagnostics(form)
  }

  const regions = useMemo(() => regionSummary(runs, form.defaultRegion), [form.defaultRegion, runs])
  const apiHost = form.apiBase ? safeHost(form.apiBase) : "bundled default"
  const connection = useMemo(() => connectionSummary(form, runs, savedAt), [form, runs, savedAt])
  const diagnosticSummary = useMemo(() => summarizeDiagnostics(diagnostics, checking), [checking, diagnostics])
  const runEvidenceUnavailable = Boolean(runsError)

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Settings"
        subtitle="Workspace, compute, and integration preferences."
        primaryAction={
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={saveConnection}
          >
            Save connection
          </Button>
        }
        rightSlot={
          <Button
            size="sm"
            variant="outline"
            className="h-7 gap-1 text-[12px]"
            onClick={() => void runDiagnostics()}
            disabled={checking}
          >
            <RefreshCw className={cn("h-3 w-3", checking ? "animate-spin" : "")} />
            Check API
          </Button>
        }
      />
      <div className="grid grid-cols-2 gap-px border-b border-border bg-border md:grid-cols-5">
        <ConnectionStat label="API host" value={connection.host} detail={connection.transport} />
        <ConnectionStat label="Auth" value={connection.auth} detail={connection.credential} />
        <ConnectionStat
          label="Region"
          value={form.defaultRegion}
          detail={runEvidenceUnavailable ? "request failed" : `${connection.observedRegions} observed`}
        />
        <ConnectionStat label="API health" value={diagnosticSummary.value} detail={diagnosticSummary.detail} />
        <ConnectionStat label="Saved" value={connection.saved} detail={connection.storage} className="col-span-2 md:col-span-1" />
      </div>
      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[200px_minmax(0,1fr)]">
        <SideNav
          items={[
            { label: "Workspace", icon: Settings, hint: form.slug },
            { label: "Connection", icon: Key, hint: form.userToken ? "token set" : "anonymous" },
            { label: "Compute", icon: Globe, hint: runEvidenceUnavailable ? "request failed" : `${regions.length} observed` },
            { label: "Security", icon: Shield },
          ]}
          activeIndex={-1}
        />
        <div className="flex flex-col gap-px bg-border">
          <Section title="Workspace">
            <Field label="Name">
              <Input
                value={form.name}
                onChange={(e) => setForm((next) => ({ ...next, name: e.target.value }))}
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Slug">
              <Input
                value={form.slug}
                onChange={(e) => setForm((next) => ({ ...next, slug: e.target.value }))}
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Default region">
              <SegmentedControl
                value={form.defaultRegion}
                onValueChange={(defaultRegion) => setForm((next) => ({ ...next, defaultRegion }))}
                options={regionOptions(regions, form.defaultRegion)}
              />
            </Field>
            <Field label="Auto-tear-down" hint="Stop idle sandboxes after">
              <div className="flex items-center gap-2">
                <Input
                  type="number"
                  value={form.idleMinutes}
                  onChange={(e) => setForm((next) => ({ ...next, idleMinutes: Number(e.target.value) }))}
                  className="h-8 w-20 font-mono text-[12px]"
                />
                <span className="text-[12px] text-muted-foreground">min</span>
              </div>
            </Field>
          </Section>

          <Section title="Cloud API connection">
            <Field label="API base" hint="Overrides the bundled default">
              <Input
                value={form.apiBase}
                onChange={(e) => setForm((next) => ({ ...next, apiBase: e.target.value }))}
                placeholder="https://api.bucephalus.example"
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="User token" hint="Stored locally in this browser">
              <Input
                value={form.userToken}
                onChange={(e) => setForm((next) => ({ ...next, userToken: e.target.value }))}
                placeholder="Bearer token"
                type="password"
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-3">
              <ConnectionFact label="API host" value={apiHost} />
              <ConnectionFact label="Auth" value={form.userToken ? "token set" : "anonymous"} />
              <ConnectionFact label="Saved" value={savedAt ?? "not this session"} />
            </div>
            <ConnectionDiagnostics
              checkedAt={checkedAt}
              checking={checking}
              diagnostics={diagnostics}
              onCheck={() => void runDiagnostics()}
            />
          </Section>

          <Section title="Local access">
            <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-4">
              <ConnectionFact label="Credential source" value={form.userToken ? "browser token" : "none"} />
              <ConnectionFact label="Token storage" value="local only" />
              <ConnectionFact label="API override" value={form.apiBase ? "enabled" : "disabled"} />
              <ConnectionFact label="Workspace slug" value={form.slug} />
            </div>
            <p className="px-1 text-[11px] text-muted-foreground">
              Persistent API key management is not exposed by this console yet. This view only controls the local browser token used for cloud API requests.
            </p>
          </Section>

          <Section title="Observed compute regions">
            {runsError ? (
              <div className="border border-border bg-card p-3">
                <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                  <div className="flex min-w-0 items-start gap-2">
                    <CircleAlert className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
                    <div className="min-w-0">
                      <div className="text-[12.5px] font-medium">Run evidence request failed</div>
                      <div className="mt-1 max-w-[min(32rem,calc(100vw-7rem))] break-words text-[11px] leading-relaxed text-muted-foreground">
                        {runsError}
                      </div>
                    </div>
                  </div>
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 w-full gap-1 text-[12px] sm:w-auto"
                    onClick={() => void loadRunEvidence()}
                  >
                    <RefreshCw className="h-3 w-3" />
                    Retry
                  </Button>
                </div>
              </div>
            ) : (
              <div className="grid grid-cols-1 gap-px bg-border">
                {regions.map((region) => (
                  <div key={region.code} className="bg-card p-3">
                    <div className="flex items-center gap-2">
                      <span
                        className={cn(
                          "h-1.5 w-1.5 rounded-full",
                          region.isDefault ? "bg-brand" : region.runs > 0 ? "bg-success" : "bg-muted-foreground",
                        )}
                      />
                      <span className="font-mono text-[12px]">{region.code}</span>
                      {region.isDefault ? (
                        <span className="rounded bg-brand/10 px-1 font-mono text-[10px] text-brand">
                          default
                        </span>
                      ) : null}
                    </div>
                    <div className="mt-1 text-[11px] text-muted-foreground">
                      {region.runs} runs observed · {formatUsd(region.spend)} spend
                    </div>
                  </div>
                ))}
              </div>
            )}
          </Section>

          <Section title="Connection posture">
            <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-3">
              <ConnectionFact label="Transport" value={form.apiBase.startsWith("https://") ? "https" : form.apiBase ? "non-https" : "default"} />
              <ConnectionFact label="Authentication" value={form.userToken ? "bearer token" : "anonymous"} />
              <ConnectionFact label="Idle policy" value={`${form.idleMinutes || 20} min`} />
            </div>
          </Section>

          <Section title="Security posture">
            <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-4">
              <ConnectionFact label="Token scope" value={form.userToken ? "browser only" : "none"} />
              <ConnectionFact label="Persistence" value="localStorage" />
              <ConnectionFact label="Transport" value={connection.transport} />
              <ConnectionFact label="Server keys" value="not managed here" />
            </div>
          </Section>
        </div>
      </div>
    </div>
  )
}

function ConnectionDiagnostics({
  checkedAt,
  checking,
  diagnostics,
  onCheck,
}: {
  checkedAt: string | null
  checking: boolean
  diagnostics: CloudProbeResult[]
  onCheck: () => void
}) {
  return (
    <div className="border border-border bg-background">
      <div className="flex flex-col gap-2 border-b border-border p-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 items-start gap-2">
          <Activity className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <div className="min-w-0">
            <div className="text-[12.5px] font-medium">Live endpoint diagnostics</div>
            <div className="truncate text-[11px] text-muted-foreground">
              {checkedAt ? `Last checked ${checkedAt}` : "Runs against the current form values"}
            </div>
          </div>
        </div>
        <Button
          size="sm"
          variant="outline"
          className="h-7 w-full gap-1 text-[12px] sm:w-auto"
          onClick={onCheck}
          disabled={checking}
        >
          <RefreshCw className={cn("h-3 w-3", checking ? "animate-spin" : "")} />
          Refresh
        </Button>
      </div>
      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-3">
        {diagnostics.length === 0 ? (
          <div className="bg-card p-3 lg:col-span-3">
            <div className="text-[12.5px] font-medium">
              {checking ? "Checking API endpoints" : "No diagnostics yet"}
            </div>
            <div className="mt-1 text-[11px] text-muted-foreground">
              Registry, package, and run checks appear here with endpoint-level latency.
            </div>
          </div>
        ) : null}
        {diagnostics.map((result) => {
          const Icon = result.ok ? CheckCircle2 : CircleAlert
          return (
            <div key={result.id} className="min-w-0 bg-card p-3">
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="flex items-center gap-1.5">
                    <Icon className={cn("h-3.5 w-3.5 shrink-0", result.ok ? "text-success" : "text-destructive")} />
                    <span className="truncate text-[12.5px] font-medium">{result.label}</span>
                  </div>
                  <div className="mt-1 truncate font-mono text-[10.5px] text-muted-foreground">
                    {result.path}
                  </div>
                </div>
                <span
                  className={cn(
                    "shrink-0 rounded border px-1.5 py-0.5 font-mono text-[10px]",
                    result.ok
                      ? "border-success/30 bg-success/10 text-success"
                      : "border-destructive/30 bg-destructive/10 text-destructive",
                  )}
                >
                  {result.ok ? "ok" : "error"}
                </span>
              </div>
              <div className="mt-3 grid grid-cols-2 gap-px bg-border">
                <ConnectionFact label="Rows" value={result.rows == null ? "n/a" : String(result.rows)} />
                <ConnectionFact label="Latency" value={formatLatency(result.latencyMs)} />
              </div>
              <div className="mt-2 min-h-8 break-words text-[11px] leading-snug text-muted-foreground">
                {result.message}
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

export function BillingPage() {
  const { navigate } = useRouter()
  const [runs, setRuns] = useState<Run[]>([])
  const [loaded, setLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)

  async function loadBillingRuns() {
    setLoaded(false)
    setLoadError(null)
    const { data, error } = await supabase
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
    if (error) {
      setRuns([])
      setLoadError(error.message)
      setLoaded(true)
      return
    }
    setRuns((data ?? []) as Run[])
    setLoaded(true)
  }

  useEffect(() => {
    void loadBillingRuns()
  }, [])

  const billing = useMemo(() => billingSummary(runs), [runs])
  const unavailable = Boolean(loadError)

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Billing"
        subtitle="Run-cost telemetry derived from cloud executions."
        primaryAction={
          <Button
            size="sm"
            variant="outline"
            className="h-7 gap-1 text-[12px]"
            onClick={() => exportBillingCsv(runs)}
            disabled={unavailable || runs.length === 0}
          >
            <Download className="h-3 w-3" /> Download CSV
          </Button>
        }
      />
      <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-4">
        <Stat label="Pricing coverage" value={!loaded ? "loading" : unavailable ? "—" : `${billing.coveragePct}%`} hint={unavailable ? "request failed" : `${billing.pricedRuns}/${billing.recentRuns} recent runs`} />
        <Stat label="Spend (7d)" value={!loaded ? "loading" : unavailable ? "—" : formatUsd(billing.spend7d)} hint={unavailable ? "connect API" : `${formatUsd(billing.hourlyRunRate)}/hr run rate`} />
        <Stat label="Avg/run" value={!loaded ? "loading" : unavailable ? "—" : formatUsd(billing.avgRunCost)} hint={unavailable ? "unavailable" : formatDuration(billing.avgDurationMs)} />
        <Stat label="Unpriced" value={!loaded ? "loading" : unavailable ? "—" : `${billing.unpricedRuns}`} hint={unavailable ? "request failed" : "Cost still pending"} />
      </div>

      {loaded && loadError ? (
        <ConnectionIssue
          title="Billing telemetry request failed"
          detail={loadError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadBillingRuns()}
        />
      ) : null}

      {!loadError ? (
        <>
      <Section title="Pricing by region">
        {billing.byRegion.length === 0 ? (
          <EmptyBillingState title="No regional spend" detail="Costs appear after priced runs." />
        ) : null}
        <div className="overflow-x-auto">
          {billing.byRegion.map((u) => (
            <div key={u.name} className="grid min-w-[620px] grid-cols-[minmax(180px,1fr)_80px_72px_82px_minmax(120px,1fr)] items-center gap-2 border-b border-border py-1.5">
              <span className="font-mono text-[12px]">{u.name}</span>
              <span className="font-mono text-[12px]">{formatUsd(u.value)}</span>
              <span className="font-mono text-[11px] text-muted-foreground">{u.runs} runs</span>
              <span className="font-mono text-[11px] text-muted-foreground">{u.unpriced} unpriced</span>
              <div className="h-2 overflow-hidden rounded-full bg-muted">
                <div className="h-full rounded-full bg-brand" style={{ width: `${u.pct}%` }} />
              </div>
            </div>
          ))}
        </div>
      </Section>

      <Section title="Spend timeline">
        {billing.timeline.some((row) => row.spend > 0 || row.runs > 0) ? (
          <ChartContainer config={billingTimelineCfg} className="h-56 w-full">
            <BarChart data={billing.timeline} barCategoryGap={8}>
              <CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="2 4" />
              <XAxis
                dataKey="label"
                tickLine={false}
                axisLine={false}
                tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
              />
              <YAxis
                yAxisId="spend"
                tickLine={false}
                axisLine={false}
                width={36}
                tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
              />
              <YAxis yAxisId="runs" orientation="right" hide />
              <Tooltip contentStyle={chartTooltipStyle} />
              <Bar yAxisId="spend" dataKey="spend" fill="var(--color-spend)" radius={[2, 2, 0, 0]} isAnimationActive={false} />
              <Bar yAxisId="runs" dataKey="runs" fill="var(--color-runs)" radius={[2, 2, 0, 0]} isAnimationActive={false} />
              <Bar yAxisId="runs" dataKey="unpriced" fill="var(--color-unpriced)" radius={[2, 2, 0, 0]} isAnimationActive={false} />
            </BarChart>
          </ChartContainer>
        ) : (
          <EmptyBillingState title="No run-cost timeline" detail="Daily bars appear after runs." />
        )}
      </Section>

      <Section title="Run cost ledger">
        <div className="overflow-x-auto">
          <div
            className="grid min-w-[700px] items-center gap-2 border-b border-border py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
            style={{ gridTemplateColumns: "minmax(180px,1fr) 100px 90px 90px 90px 90px" }}
          >
            <span>Run</span>
            <span>Status</span>
            <span>Amount</span>
            <span>Duration</span>
            <span>Region</span>
            <span>Created</span>
          </div>
          {runs.slice(0, 12).map((run) => (
            <div
              key={run.id}
              className="grid min-w-[700px] items-center gap-2 border-b border-border py-1.5"
              style={{ gridTemplateColumns: "minmax(180px,1fr) 100px 90px 90px 90px 90px" }}
            >
              <span className="truncate font-mono text-[12px]">{formatReadableLabel(run.experiment_name)}</span>
              <StatusPill status={run.status} />
              <span className="font-mono text-[12px]">{formatUsd(Number(run.cost_usd))}</span>
              <span className="font-mono text-[11px] text-muted-foreground">{formatDuration(run.duration_ms)}</span>
              <span className="font-mono text-[11px] text-muted-foreground">{run.region}</span>
              <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(run.created_at)}</span>
            </div>
          ))}
        </div>
        {runs.length === 0 ? (
          <EmptyBillingState title="No cost ledger" detail="Run-level costs will appear here after executions are recorded." />
        ) : null}
      </Section>

      <Section title="Billing source">
        <div className="flex items-center gap-3 border border-border bg-card p-3">
          <CreditCard className="h-4 w-4 text-muted-foreground" />
          <div className="flex-1">
            <div className="text-[12.5px] font-medium">Managed outside this console</div>
            <div className="text-[11px] text-muted-foreground">
              The app is showing run-cost telemetry, not payment processor data.
            </div>
          </div>
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-[12px]"
            onClick={() => navigate({ name: "settings" })}
          >
            <Settings className="h-3 w-3" /> Connection settings
          </Button>
        </div>
      </Section>
        </>
      ) : null}
    </div>
  )
}

function exportBillingCsv(runs: Run[]) {
  downloadCsv(
    `run-cost-ledger-${new Date().toISOString().slice(0, 10)}.csv`,
    runs.map((run) => ({
      run_id: run.id,
      experiment_id: run.experiment_id ?? "",
      experiment: run.experiment_name,
      variant: run.variant,
      status: run.status,
      amount_usd: Number(run.cost_usd),
      duration_ms: run.duration_ms,
      region: run.region,
      started_at: run.started_at ?? "",
      ended_at: run.ended_at ?? "",
      created_at: run.created_at,
    })),
  )
}

export function TeamPage() {
  const { navigate } = useRouter()
  const workspace = useWorkspacePreferences()
  const [runs, setRuns] = useState<Run[]>([])
  const [registry, setRegistry] = useState<RegistryItem[]>([])
  const [experiments, setExperiments] = useState<Experiment[]>([])
  const [loaded, setLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)

  async function loadTeamEvidence() {
    setLoaded(false)
    setLoadError(null)
    const [runsResult, registryResult, experimentsResult] = await Promise.all([
      supabase
        .from("runs")
        .select("*")
        .order("created_at", { ascending: false }),
      supabase
        .from("registry_items")
        .select("*")
        .order("created_at", { ascending: false }),
      supabase
        .from("experiments")
        .select("*")
        .order("created_at", { ascending: false }),
    ])
    setRuns((runsResult.data ?? []) as Run[])
    setRegistry((registryResult.data ?? []) as RegistryItem[])
    setExperiments((experimentsResult.data ?? []) as Experiment[])
    setLoadError(runsResult.error?.message ?? registryResult.error?.message ?? experimentsResult.error?.message ?? null)
    setLoaded(true)
  }

  useEffect(() => {
    void loadTeamEvidence()
  }, [])

  const principals = useMemo(
    () => principalSummary(workspace, runs, registry, experiments),
    [experiments, registry, runs, workspace],
  )
  const sourceRows = useMemo(() => principalSourceRows(principals), [principals])
  const runActors = principals.filter((principal) => principal.sources.runs).length
  const observedObjects = runs.length + registry.length + experiments.length
  const unavailable = Boolean(loadError)

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Team"
        subtitle={`Principals inferred from ${workspace.slug} activity.`}
      />
      <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-4">
        <Stat label="Principals" value={!loaded ? "loading" : unavailable ? "—" : `${principals.length}`} hint={unavailable ? "request failed" : "API evidence"} />
        <Stat label="Registry owners" value={!loaded ? "loading" : unavailable ? "—" : `${new Set(registry.map((item) => item.owner)).size}`} hint={unavailable ? "connect API" : `${registry.length} resources`} />
        <Stat label="Run actors" value={!loaded ? "loading" : unavailable ? "—" : `${runActors}`} hint={unavailable ? "unavailable" : `${runs.length} runs`} />
        <Stat label="Auth mode" value={workspace.userToken ? "token" : "anonymous"} hint={workspace.apiBase ? safeHost(workspace.apiBase) : "default API"} />
      </div>

      {loaded && loadError ? (
        <ConnectionIssue
          title="Team evidence request failed"
          detail={loadError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadTeamEvidence()}
        />
      ) : null}

      {!loadError ? (
        <>
      <Section title="Access evidence">
        <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[minmax(0,1fr)_260px]">
          <div className="bg-card p-3">
            <ChartContainer config={principalSourceCfg} className="h-44 w-full">
              <BarChart data={sourceRows} layout="vertical" margin={{ left: 4, right: 12, top: 8, bottom: 4 }}>
                <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis type="number" hide allowDecimals={false} />
                <YAxis
                  type="category"
                  dataKey="source"
                  width={86}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip contentStyle={chartTooltipStyle} />
                <Bar dataKey="objects" fill="var(--color-objects)" radius={[0, 2, 2, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          </div>
          <div className="bg-card p-3">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Evidence coverage</div>
            <div className="mt-2 grid grid-cols-2 gap-px bg-border">
              <ConnectionFact label="Runs" value={`${runs.length}`} />
              <ConnectionFact label="Registry" value={`${registry.length}`} />
              <ConnectionFact label="Experiments" value={`${experiments.length}`} />
              <ConnectionFact label="Objects" value={`${observedObjects}`} />
            </div>
          </div>
        </div>
        {observedObjects === 0 ? (
          <EmptyBillingState
            icon={Shield}
            title="Workspace only"
            detail="More principals appear after shared activity."
          />
        ) : null}
      </Section>

      <div className="overflow-x-auto text-[12px]">
        <div
          className="grid min-w-[720px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
          style={{ gridTemplateColumns: "minmax(220px,1fr) 120px 120px 100px 120px" }}
        >
          <span>Principal</span>
          <span>Role</span>
          <span>Evidence</span>
          <span>Objects</span>
          <span>Last seen</span>
        </div>
        {principals.map((principal) => (
          <div
            key={principal.name + principal.source}
            className="grid min-w-[720px] items-center gap-2 border-b border-border px-3 py-1.5"
            style={{ gridTemplateColumns: "minmax(220px,1fr) 120px 120px 100px 120px" }}
          >
            <div className="flex items-center gap-2">
              <span className="grid h-5 w-5 place-items-center rounded-full bg-info/15 text-[10px] font-medium text-info">
                {principal.initials}
              </span>
              <div className="min-w-0 leading-tight">
                <div className="truncate text-[12.5px]">{principal.name}</div>
                <div className="truncate font-mono text-[10.5px] text-muted-foreground">
                  {principal.detail}
                </div>
              </div>
            </div>
            <span className="rounded border border-border px-1.5 py-0.5 font-mono text-[10.5px] text-muted-foreground w-fit">
              {principal.role}
            </span>
            <span className="font-mono text-[11px] text-muted-foreground">{principal.source}</span>
            <span className="font-mono text-[11px] text-muted-foreground">{principal.objects}</span>
            <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(principal.lastSeen)}</span>
          </div>
        ))}
      </div>
        </>
      ) : null}
    </div>
  )
}

function Stat({
  label,
  value,
  hint,
}: {
  label: string
  value: string
  hint?: string
}) {
  return (
    <div className="bg-background p-3">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className="mt-0.5 font-mono text-[18px] font-medium">{value}</div>
      {hint ? <div className="text-[10.5px] text-muted-foreground">{hint}</div> : null}
    </div>
  )
}

function billingSummary(runs: Run[]) {
  const now = Date.now()
  const sevenDaysAgo = now - 7 * 24 * 60 * 60 * 1000
  const recent = runs.filter((run) => Date.parse(run.created_at) >= sevenDaysAgo)
  const priced = recent.filter((run) => Number(run.cost_usd) > 0)
  const durations = recent.map((run) => run.duration_ms).filter((duration) => duration > 0)
  const spend7d = recent.reduce((acc, run) => acc + Number(run.cost_usd), 0)
  const byRegion = new Map<string, { spend: number; runs: number; unpriced: number }>()
  recent.forEach((run) => {
    const cost = Number(run.cost_usd)
    const prev = byRegion.get(run.region) ?? { spend: 0, runs: 0, unpriced: 0 }
    prev.spend += cost
    prev.runs += 1
    if (cost <= 0) prev.unpriced += 1
    byRegion.set(run.region, prev)
  })

  return {
    spend7d,
    hourlyRunRate: spend7d / (7 * 24),
    recentRuns: recent.length,
    pricedRuns: priced.length,
    unpricedRuns: recent.length - priced.length,
    coveragePct: recent.length ? Math.round((priced.length / recent.length) * 100) : 0,
    avgRunCost: priced.length ? spend7d / priced.length : 0,
    avgDurationMs: durations.length ? durations.reduce((acc, value) => acc + value, 0) / durations.length : 0,
    byRegion: Array.from(byRegion.entries())
      .map(([name, value]) => ({
        name,
        value: value.spend,
        runs: value.runs,
        unpriced: value.unpriced,
        pct: spend7d ? Math.max(3, Math.round((value.spend / spend7d) * 100)) : 0,
      }))
      .sort((a, b) => b.value - a.value || b.runs - a.runs),
    timeline: billingTimeline(runs),
  }
}

function billingTimeline(runs: Run[]) {
  const buckets = Array.from({ length: 14 }, (_, index) => {
    const date = new Date()
    date.setHours(0, 0, 0, 0)
    date.setDate(date.getDate() - (13 - index))
    return {
      date,
      label: date.toLocaleDateString(undefined, { month: "numeric", day: "numeric" }),
      spend: 0,
      runs: 0,
      unpriced: 0,
    }
  })
  runs.forEach((run) => {
    const created = new Date(run.created_at)
    const bucket = buckets.find((item) => sameDay(item.date, created))
    if (!bucket) return
    const cost = Number(run.cost_usd)
    bucket.spend += cost
    bucket.runs += 1
    if (cost <= 0) bucket.unpriced += 1
  })
  return buckets.map(({ date: _date, ...bucket }) => ({
    ...bucket,
    spend: Number(bucket.spend.toFixed(4)),
  }))
}

function regionSummary(runs: Run[], defaultRegion: string) {
  const regions = new Map<string, { code: string; runs: number; spend: number; isDefault: boolean }>()
  if (defaultRegion) {
    regions.set(defaultRegion, { code: defaultRegion, runs: 0, spend: 0, isDefault: true })
  }
  runs.forEach((run) => {
    const prev = regions.get(run.region) ?? {
      code: run.region,
      runs: 0,
      spend: 0,
      isDefault: run.region === defaultRegion,
    }
    prev.runs += 1
    prev.spend += Number(run.cost_usd)
    prev.isDefault = prev.isDefault || run.region === defaultRegion
    regions.set(run.region, prev)
  })
  return Array.from(regions.values()).sort((a, b) => Number(b.isDefault) - Number(a.isDefault) || b.runs - a.runs)
}

function safeHost(value: string) {
  try {
    return new URL(value).host
  } catch {
    return value
  }
}

function connectionSummary(form: WorkspacePreferences, runs: Run[], savedAt: string | null) {
  const transport = form.apiBase
    ? form.apiBase.startsWith("https://")
      ? "https transport"
      : "non-https override"
    : "default transport"
  return {
    host: form.apiBase ? safeHost(form.apiBase) : "bundled default",
    transport,
    auth: form.userToken ? "token" : "anonymous",
    credential: form.userToken ? "browser token" : "no local token",
    observedRegions: new Set(runs.map((run) => run.region)).size,
    saved: savedAt ?? "not saved",
    storage: "local browser",
  }
}

function summarizeDiagnostics(diagnostics: CloudProbeResult[], checking: boolean) {
  if (checking && diagnostics.length === 0) {
    return { value: "checking", detail: "probing endpoints" }
  }
  if (diagnostics.length === 0) {
    return { value: "not checked", detail: "run diagnostics" }
  }
  const failed = diagnostics.filter((result) => !result.ok).length
  const reachable = diagnostics.length - failed
  const avgLatency = Math.round(
    diagnostics.reduce((acc, result) => acc + result.latencyMs, 0) / diagnostics.length,
  )
  if (failed > 0) {
    return { value: `${reachable}/${diagnostics.length} ok`, detail: `${failed} failing · ${formatLatency(avgLatency)} avg` }
  }
  return { value: "reachable", detail: `${diagnostics.length} endpoints · ${formatLatency(avgLatency)} avg` }
}

function formatLatency(value: number) {
  if (!Number.isFinite(value)) return "n/a"
  return value >= 1000 ? `${(value / 1000).toFixed(1)}s` : `${Math.max(1, Math.round(value))}ms`
}

function regionOptions(regions: ReturnType<typeof regionSummary>, current: string) {
  const defaults = ["us-east-1", "us-west-2", "eu-west-1", "ap-southeast-1"]
  const all = Array.from(new Set([current, ...regions.map((region) => region.code), ...defaults].filter(Boolean)))
  return all.map((region) => ({
    value: region,
    label: shortRegion(region),
    count: regions.find((item) => item.code === region)?.runs,
  }))
}

function shortRegion(region: string) {
  const labels: Record<string, string> = {
    "us-east-1": "use1",
    "us-west-2": "usw2",
    "eu-west-1": "euw1",
    "ap-southeast-1": "apse1",
  }
  return labels[region] ?? region
}

function sameDay(left: Date, right: Date) {
  return left.getFullYear() === right.getFullYear() && left.getMonth() === right.getMonth() && left.getDate() === right.getDate()
}

function principalSummary(
  workspace: WorkspacePreferences,
  runs: Run[],
  registry: RegistryItem[],
  experiments: Experiment[],
) {
  const principals = new Map<string, PrincipalRow>()
  const observedLastSeen = newestMany([
    ...runs.map((run) => run.created_at),
    ...registry.map((item) => item.created_at),
    ...experiments.map((experiment) => experiment.created_at),
  ])

  const workspacePrincipal = {
    name: workspace.name,
    detail: workspace.slug,
    role: "Workspace",
    source: "local settings",
    objects: runs.length + registry.length + experiments.length,
    lastSeen: observedLastSeen,
    initials: initialsFor(workspace.name),
    sources: { "local settings": 1 },
  }
  principals.set(`workspace:${workspace.slug}`, workspacePrincipal)

  registry.forEach((item) => {
    const key = `owner:${item.owner}`
    const prev = principals.get(key) ?? {
      name: item.owner,
      detail: "registry owner",
      role: item.owner === "cloud" || item.owner === "registry" ? "Service" : "Owner",
      source: "registry",
      objects: 0,
      lastSeen: item.created_at,
      initials: initialsFor(item.owner),
      sources: {},
    }
    prev.objects += 1
    prev.lastSeen = newest(prev.lastSeen, item.created_at)
    addPrincipalSource(prev, "registry", 1)
    prev.source = sourceLabel(prev.sources)
    prev.detail = `${prev.source} evidence`
    principals.set(key, prev)
  })

  experiments.forEach((experiment) => {
    const key = `owner:${experiment.owner}`
    const prev = principals.get(key) ?? {
      name: experiment.owner,
      detail: "experiment owner",
      role: "Owner",
      source: "experiments",
      objects: 0,
      lastSeen: experiment.created_at,
      initials: initialsFor(experiment.owner),
      sources: {},
    }
    prev.objects += 1
    prev.lastSeen = newest(prev.lastSeen, experiment.created_at)
    addPrincipalSource(prev, "experiments", 1)
    prev.source = sourceLabel(prev.sources)
    prev.detail = `${prev.source} evidence`
    principals.set(key, prev)
  })

  if (runs.length > 0) {
    const lastRunSeen = newestMany(runs.map((run) => run.created_at))
    principals.set("service:cloud-runner", {
      name: "cloud-runner",
      detail: "run executor",
      role: "Service",
      source: "runs",
      objects: runs.length,
      lastSeen: lastRunSeen,
      initials: "CR",
      sources: { runs: runs.length },
    })
  }

  return Array.from(principals.values()).sort((a, b) => {
    const left = a.lastSeen ? Date.parse(a.lastSeen) : 0
    const right = b.lastSeen ? Date.parse(b.lastSeen) : 0
    return right - left
  })
}

function principalSourceRows(principals: ReturnType<typeof principalSummary>) {
  const rows = new Map<string, { source: string; objects: number }>()
  principals.forEach((principal) => {
    Object.entries(principal.sources).forEach(([source, count]) => {
      const prev = rows.get(source) ?? { source, objects: 0 }
      prev.objects += Math.max(1, count)
      rows.set(source, prev)
    })
  })
  return Array.from(rows.values()).sort((a, b) => b.objects - a.objects || a.source.localeCompare(b.source))
}

function addPrincipalSource(principal: PrincipalRow, source: string, count: number) {
  principal.sources[source] = (principal.sources[source] ?? 0) + count
}

function sourceLabel(sources: Record<string, number>) {
  return Object.entries(sources)
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([source]) => source)
    .join(" + ")
}

function newestMany(values: (string | null)[]) {
  return values.reduce<string | null>((latest, value) => newest(latest, value), null)
}

function newest(a: string | null, b: string | null) {
  if (!a) return b
  if (!b) return a
  return Date.parse(b) > Date.parse(a) ? b : a
}

function initialsFor(value: string) {
  return (
    value
      .split(/\s|\/|-/)
      .map((part) => part[0]?.toUpperCase())
      .join("")
      .slice(0, 2) || "?"
  )
}

function EmptyBillingState({
  icon: Icon = CreditCard,
  title,
  detail,
}: {
  icon?: React.ComponentType<{ className?: string }>
  title: string
  detail: string
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-1 border-b border-border py-8 text-center">
      <Icon className="h-4 w-4 text-muted-foreground" />
      <div className="text-[12.5px] font-medium">{title}</div>
      <div className="max-w-[calc(100vw-7rem)] text-[11px] text-muted-foreground sm:max-w-md">{detail}</div>
    </div>
  )
}

function ConnectionStat({
  label,
  value,
  detail,
  className,
}: {
  label: string
  value: string
  detail: string
  className?: string
}) {
  return (
    <div className={cn("bg-background p-2.5", className)}>
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className="mt-0.5 truncate font-mono text-[15px] font-medium">{value}</div>
      <div className="truncate text-[10.5px] text-muted-foreground">{detail}</div>
    </div>
  )
}

function ConnectionFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="bg-card px-3 py-2">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className="mt-0.5 truncate font-mono text-[12px]">{value}</div>
    </div>
  )
}

const chartTooltipStyle = {
  background: "var(--popover)",
  border: "1px solid var(--border)",
  borderRadius: 6,
  fontSize: 11,
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="border-b border-border bg-background">
      <header className="flex h-9 items-center justify-between border-b border-border px-3">
        <h3 className="text-[12px] font-semibold">{title}</h3>
      </header>
      <div className="flex flex-col gap-2 px-3 py-3">{children}</div>
    </section>
  )
}

function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <div className="grid grid-cols-1 gap-1.5 md:grid-cols-[180px_minmax(0,360px)] md:items-center">
      <Label className="text-[12px] font-medium">
        {label}
        {hint ? (
          <span className="ml-1 text-[10.5px] font-normal text-muted-foreground">
            {hint}
          </span>
        ) : null}
      </Label>
      <div>{children}</div>
    </div>
  )
}

function SideNav({
  items,
  activeIndex,
}: {
  items: { label: string; icon: React.ComponentType<{ className?: string }>; hint?: string }[]
  activeIndex: number
}) {
  return (
    <nav className="flex flex-col border-r border-border bg-background">
      {items.map((it, i) => {
        const Icon = it.icon
        const active = i === activeIndex
        return (
          <div
            key={it.label}
            className={cn(
              "flex items-center justify-between gap-2 px-3 py-2 text-left text-[12.5px]",
              active
                ? "bg-secondary text-foreground"
                : "border-b border-border text-muted-foreground",
            )}
          >
            <span className="flex items-center gap-2">
              <Icon className="h-3.5 w-3.5" />
              {it.label}
            </span>
            {it.hint ? (
              <span className="font-mono text-[10.5px] text-muted-foreground">
                {it.hint}
              </span>
            ) : null}
          </div>
        )
      })}
    </nav>
  )
}

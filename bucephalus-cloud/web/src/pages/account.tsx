import { useEffect, useMemo, useState } from "react"
import {
  Activity,
  CheckCircle2,
  CircleAlert,
  CreditCard,
  Download,
  Globe,
  Key,
  LogOut,
  RefreshCw,
  Settings,
  Shield,
  UserCircle,
} from "lucide-react"
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
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
  cloudApi,
  type CloudProbeResult,
  type Experiment,
  type RegistryItem,
  type Run,
} from "@/lib/cloud-api"
import { GoogleSignInButton, useAuth } from "@/lib/auth"
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

const billingStatusCfg: ChartConfig = {
  spend: { label: "Spend", color: "var(--chart-1)" },
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

type TeamBriefTone = "success" | "warning" | "info" | "muted"
type TeamBriefAction = "settings" | "refresh"
type TeamBrief = {
  tone: TeamBriefTone
  verdict: string
  detail: string
  action: string
  actionType: TeamBriefAction
  facts: { label: string; value: string; tone?: TeamBriefTone }[]
}
type ConnectionBriefTone = "success" | "warning" | "info" | "muted"
type ConnectionBrief = {
  tone: ConnectionBriefTone
  verdict: string
  detail: string
  action: string
  facts: { label: string; value: string; tone?: ConnectionBriefTone }[]
}

export function SettingsPage() {
  const workspace = useWorkspacePreferences()
  const auth = useAuth()
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
    void probeCloudConnection({ apiBase: workspace.apiBase })
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
  }, [workspace.apiBase, auth.idToken])

  async function loadRunEvidence() {
    setRunsError(null)
    const { data, error } = await cloudApi
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
    const results = await probeCloudConnection({ apiBase: next.apiBase })
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
  const connection = useMemo(() => connectionSummary(form, runs, savedAt, auth.status), [auth.status, form, runs, savedAt])
  const diagnosticSummary = useMemo(() => summarizeDiagnostics(diagnostics, checking), [checking, diagnostics])
  const formDirty = useMemo(() => workspacePreferencesChanged(form, workspace), [form, workspace])
  const connectionBrief = useMemo(
    () => buildConnectionBrief({
      diagnostics,
      form,
      formDirty,
      regions,
      runsError,
      savedAt,
      checking,
      authStatus: auth.status,
    }),
    [auth.status, checking, diagnostics, form, formDirty, regions, runsError, savedAt],
  )
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
      <ConnectionBriefView
        brief={connectionBrief}
        checking={checking}
        onAction={() => {
          if (formDirty) saveConnection()
          else void runDiagnostics()
        }}
      />
      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[200px_minmax(0,1fr)]">
        <SideNav
          items={[
            { label: "Workspace", icon: Settings, hint: form.slug },
            { label: "Connection", icon: Key, hint: auth.status === "signed_in" ? "Google OAuth" : "signed out" },
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
            <OAuthSessionPanel />
            <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-3">
              <ConnectionFact label="API host" value={apiHost} />
              <ConnectionFact label="Auth" value={auth.status === "signed_in" ? "Google OAuth" : "signed out"} />
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
              <ConnectionFact label="Credential source" value={auth.user?.email || "Google"} />
              <ConnectionFact label="Token storage" value="session" />
              <ConnectionFact label="API override" value={form.apiBase ? "enabled" : "disabled"} />
              <ConnectionFact label="Workspace slug" value={form.slug} />
            </div>
            <p className="px-1 text-[11px] text-muted-foreground">
              API requests use the active Google ID token for this browser session.
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
              <ConnectionFact label="Authentication" value={auth.status === "signed_in" ? "bearer JWT" : "signed out"} />
              <ConnectionFact label="Idle policy" value={`${form.idleMinutes || 20} min`} />
            </div>
          </Section>

          <Section title="Security posture">
            <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-4">
              <ConnectionFact label="Token scope" value={auth.user?.email ? "Google user" : "none"} />
              <ConnectionFact label="Persistence" value="sessionStorage" />
              <ConnectionFact label="Transport" value={connection.transport} />
              <ConnectionFact label="Server keys" value="not managed here" />
            </div>
          </Section>
        </div>
      </div>
    </div>
  )
}

function ConnectionBriefView({
  brief,
  checking,
  onAction,
}: {
  brief: ConnectionBrief
  checking: boolean
  onAction: () => void
}) {
  return (
    <section className="border-b border-border bg-background">
      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[minmax(260px,1.1fr)_minmax(0,2fr)_auto]">
        <div className="min-w-0 bg-background p-3">
          <div className="flex min-w-0 items-start gap-2">
            <span className={cn("mt-1 h-2 w-2 shrink-0 rounded-full", connectionBriefDot(brief.tone))} />
            <div className="min-w-0">
              <div className={cn("truncate text-[13px] font-semibold", connectionBriefText(brief.tone))}>
                {brief.verdict}
              </div>
              <div className="mt-1 line-clamp-2 text-[11px] leading-relaxed text-muted-foreground" title={brief.detail}>
                {brief.detail}
              </div>
            </div>
          </div>
        </div>
        <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
          {brief.facts.map((fact) => (
            <div key={fact.label} className="min-w-0 bg-background p-3">
              <div className="truncate text-[10px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
              <div className={cn("mt-0.5 truncate font-mono text-[13px]", fact.tone ? connectionBriefText(fact.tone) : "")}>
                {fact.value}
              </div>
            </div>
          ))}
        </div>
        <div className="flex bg-background p-3">
          <Button
            size="sm"
            variant={brief.tone === "success" ? "outline" : "default"}
            className={cn(
              "h-7 w-full gap-1 text-[12px] lg:w-auto",
              brief.tone === "success" ? "" : "bg-brand text-brand-foreground hover:bg-brand/90",
            )}
            onClick={onAction}
            disabled={checking}
          >
            <RefreshCw className={cn("h-3 w-3", checking ? "animate-spin" : "")} />
            {brief.action}
          </Button>
        </div>
      </div>
    </section>
  )
}

function OAuthSessionPanel() {
  const auth = useAuth()
  const expiresAt = auth.user ? new Date(auth.user.expiresAt * 1000).toLocaleTimeString() : "n/a"
  return (
    <div className="border border-border bg-background">
      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[minmax(0,1fr)_220px]">
        <div className="min-w-0 bg-card p-3">
          <div className="flex min-w-0 items-start gap-2">
            {auth.user?.picture ? (
              <img src={auth.user.picture} alt="" className="h-8 w-8 shrink-0 rounded-full" referrerPolicy="no-referrer" />
            ) : (
              <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-info/15 text-info">
                <UserCircle className="h-4 w-4" />
              </span>
            )}
            <div className="min-w-0">
              <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Google OAuth session</div>
              <div className="mt-0.5 truncate text-[13px] font-medium">
                {auth.user?.name || "Not signed in"}
              </div>
              <div className="truncate text-[11px] text-muted-foreground">
                {auth.user?.email || "Sign in with a Google account authorized for this deployment."}
              </div>
            </div>
          </div>
          <div className="mt-3 grid grid-cols-1 gap-px bg-border sm:grid-cols-3">
            <ConnectionFact label="Status" value={auth.status === "signed_in" ? "signed in" : auth.status} />
            <ConnectionFact label="Client ID" value={auth.configured ? "configured" : "missing"} />
            <ConnectionFact label="Expires" value={expiresAt} />
          </div>
        </div>
        <div className="flex min-w-0 flex-col justify-center gap-2 bg-card p-3">
          {auth.status === "signed_in" ? (
            <Button size="sm" variant="outline" className="h-8 gap-1 text-[12px]" onClick={auth.signOut}>
              <LogOut className="h-3.5 w-3.5" />
              Sign out
            </Button>
          ) : (
            <GoogleSignInButton />
          )}
          {auth.error ? (
            <div className="break-words text-[11px] leading-snug text-warning">
              {auth.error}
            </div>
          ) : null}
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
    const { data, error } = await cloudApi
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
          <BillingBrief billing={billing} />
          <BillingOperations billing={billing} onOpenRuns={() => navigate({ name: "runs" })} />

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

type BillingSummary = ReturnType<typeof billingSummary>

function BillingBrief({ billing }: { billing: BillingSummary }) {
  const topRegion = billing.byRegion[0]
  const projectedMonthly = billing.hourlyRunRate * 24 * 30
  const concentrationPct = billing.spend7d && topRegion ? Math.round((topRegion.value / billing.spend7d) * 100) : 0
  const coverageTone = billing.coveragePct >= 95 ? "success" : billing.coveragePct >= 70 ? "warning" : "muted"
  return (
    <section className="border-b border-border bg-background">
      <header className="flex h-9 items-center justify-between border-b border-border px-3">
        <div className="flex items-center gap-2">
          <h3 className="text-[12px] font-semibold">Billing brief</h3>
          <span className="text-[11px] text-muted-foreground">run-cost telemetry, not payment processor data</span>
        </div>
      </header>
      <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-4">
        <BillingBriefTile
          label="Projected month"
          value={formatUsd(projectedMonthly)}
          detail={`${formatUsd(billing.hourlyRunRate)}/hr from 7d run rate`}
          tone={projectedMonthly > 500 ? "warning" : "success"}
        />
        <BillingBriefTile
          label="Coverage risk"
          value={`${billing.coveragePct}% priced`}
          detail={`${billing.unpricedRuns} unpriced of ${billing.recentRuns} recent runs`}
          tone={coverageTone}
        />
        <BillingBriefTile
          label="Hot region"
          value={topRegion?.name ?? "no spend"}
          detail={topRegion ? `${concentrationPct}% of spend · ${topRegion.runs} runs` : "waiting for priced runs"}
          tone={concentrationPct > 80 ? "warning" : topRegion ? "success" : "muted"}
        />
        <BillingBriefTile
          label="Run shape"
          value={billing.avgRunCost ? formatUsd(billing.avgRunCost) : "no priced runs"}
          detail={`${formatDuration(billing.avgDurationMs)} avg duration`}
          tone={billing.avgRunCost ? "success" : "muted"}
        />
      </div>
    </section>
  )
}

function BillingBriefTile({
  label,
  value,
  detail,
  tone,
}: {
  label: string
  value: string
  detail: string
  tone: "success" | "warning" | "muted"
}) {
  return (
    <div className="min-w-0 bg-background p-3">
      <div className="flex items-center justify-between gap-2">
        <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
        <span
          className={cn(
            "h-1.5 w-1.5 rounded-full",
            tone === "success" ? "bg-success" : tone === "warning" ? "bg-warning" : "bg-muted-foreground",
          )}
        />
      </div>
      <div className={cn("mt-0.5 truncate font-mono text-[17px] font-medium", tone === "warning" ? "text-warning" : "")}>
        {value}
      </div>
      <div className="truncate text-[10.5px] text-muted-foreground" title={detail}>{detail}</div>
    </div>
  )
}

function BillingOperations({
  billing,
  onOpenRuns,
}: {
  billing: BillingSummary
  onOpenRuns: () => void
}) {
  return (
    <section className="border-b border-border bg-background">
      <header className="flex h-9 items-center justify-between border-b border-border px-3">
        <div className="flex min-w-0 items-center gap-2">
          <h3 className="text-[12px] font-semibold">Spend operations</h3>
          <span className="truncate text-[11px] text-muted-foreground">
            failed spend, unpriced runs, and expensive outliers
          </span>
        </div>
        <Button variant="ghost" size="sm" className="h-6 gap-1 text-[11px]" onClick={onOpenRuns}>
          Open runs
        </Button>
      </header>
      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.25fr)]">
        <div className="min-w-0 bg-background p-3">
          <div className="mb-2 grid grid-cols-3 gap-px overflow-hidden rounded border border-border bg-border">
            <BillingOpsMini label="Failed spend" value={formatUsd(billing.failedSpend)} tone={billing.failedSpend > 0 ? "warning" : "success"} />
            <BillingOpsMini label="Unpriced" value={`${billing.unpricedRuns}`} tone={billing.unpricedRuns ? "warning" : "success"} />
            <BillingOpsMini label="Max run" value={billing.maxRunCost ? formatUsd(billing.maxRunCost) : "none"} tone={billing.maxRunCost > Math.max(5, billing.avgRunCost * 3) ? "warning" : billing.maxRunCost ? "success" : "muted"} />
          </div>
          {billing.byStatus.some((row) => row.runs > 0) ? (
            <ChartContainer config={billingStatusCfg} className="h-44 w-full">
              <BarChart data={billing.byStatus} layout="vertical" margin={{ left: 4, right: 16, top: 8, bottom: 4 }}>
                <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis type="number" hide />
                <YAxis
                  type="category"
                  dataKey="status"
                  width={76}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip
                  contentStyle={chartTooltipStyle}
                  formatter={(value, _name, item) => [
                    `${formatUsd(Number(value))} · ${item.payload.runs} runs · ${item.payload.unpriced} unpriced`,
                    "spend",
                  ]}
                />
                <Bar dataKey="spend" radius={[0, 2, 2, 0]} isAnimationActive={false}>
                  {billing.byStatus.map((row) => (
                    <Cell key={row.status} fill={billingStatusColor(row.status)} />
                  ))}
                </Bar>
              </BarChart>
            </ChartContainer>
          ) : (
            <EmptyBillingState title="No status spend" detail="Status spend appears after runs are recorded." />
          )}
        </div>

        <div className="min-w-0 overflow-x-auto bg-background">
          <div
            className="grid min-w-[620px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
            style={{ gridTemplateColumns: "minmax(180px,1fr) 92px 84px 90px minmax(150px,1fr)" }}
          >
            <span>Run</span>
            <span>Status</span>
            <span>Cost</span>
            <span>Duration</span>
            <span>Why it matters</span>
          </div>
          {billing.outliers.length ? (
            billing.outliers.map((row) => (
              <button
                key={row.run.id}
                className="grid min-w-[620px] w-full items-center gap-2 border-b border-border px-3 py-1.5 text-left hover:bg-accent/40"
                style={{ gridTemplateColumns: "minmax(180px,1fr) 92px 84px 90px minmax(150px,1fr)" }}
                onClick={onOpenRuns}
              >
                <span className="min-w-0">
                  <span className="block truncate font-mono text-[12px]">{formatReadableLabel(row.run.experiment_name)}</span>
                  <span className="block truncate text-[10.5px] text-muted-foreground">{formatReadableLabel(row.run.variant)}</span>
                </span>
                <StatusPill status={row.run.status} />
                <span className={cn("font-mono text-[12px]", row.tone === "warning" ? "text-warning" : row.tone === "danger" ? "text-destructive" : "")}>
                  {formatUsd(Number(row.run.cost_usd))}
                </span>
                <span className="font-mono text-[11px] text-muted-foreground">{formatDuration(row.run.duration_ms)}</span>
                <span className="truncate text-[11px] text-muted-foreground" title={row.reason}>{row.reason}</span>
              </button>
            ))
          ) : (
            <EmptyBillingState title="No billing outliers" detail="Expensive, failed, slow, and unpriced runs will be called out here." />
          )}
        </div>
      </div>
    </section>
  )
}

function BillingOpsMini({
  label,
  value,
  tone,
}: {
  label: string
  value: string
  tone: "success" | "warning" | "muted"
}) {
  return (
    <div className="min-w-0 bg-card px-2 py-1.5">
      <div className="flex items-center justify-between gap-2">
        <span className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</span>
        <span className={cn("h-1.5 w-1.5 rounded-full", tone === "success" ? "bg-success" : tone === "warning" ? "bg-warning" : "bg-muted-foreground")} />
      </div>
      <div className={cn("mt-0.5 truncate font-mono text-[12px]", tone === "warning" ? "text-warning" : "")}>{value}</div>
    </div>
  )
}

export function TeamPage() {
  const { navigate } = useRouter()
  const workspace = useWorkspacePreferences()
  const auth = useAuth()
  const [runs, setRuns] = useState<Run[]>([])
  const [registry, setRegistry] = useState<RegistryItem[]>([])
  const [experiments, setExperiments] = useState<Experiment[]>([])
  const [loaded, setLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)

  async function loadTeamEvidence() {
    setLoaded(false)
    setLoadError(null)
    const [runsResult, registryResult, experimentsResult] = await Promise.all([
      cloudApi
        .from("runs")
        .select("*")
        .order("created_at", { ascending: false }),
      cloudApi
        .from("registry_items")
        .select("*")
        .order("created_at", { ascending: false }),
      cloudApi
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
  const teamBrief = useMemo(
    () => teamAccessBrief(workspace, auth.status === "signed_in", principals, runs, registry, experiments, loadError),
    [auth.status, experiments, loadError, principals, registry, runs, workspace],
  )

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
        <Stat label="Auth mode" value={auth.status === "signed_in" ? "Google" : "signed out"} hint={workspace.apiBase ? safeHost(workspace.apiBase) : "default API"} />
      </div>

      {loaded ? (
        <TeamBriefView
          brief={teamBrief}
          onAction={() => {
            if (teamBrief.actionType === "settings") navigate({ name: "settings" })
            else void loadTeamEvidence()
          }}
        />
      ) : null}

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
                {sourceRows.length > 0 ? (
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
                ) : (
                  <div className="flex h-44 items-center justify-center px-3 text-center text-[12px] text-muted-foreground">
                    Evidence sources appear after runs, registry pushes, or experiment packages are visible to this workspace.
                  </div>
                )}
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
                <div className="truncate text-[12.5px]">{formatReadableLabel(principal.name)}</div>
                <div className="truncate font-mono text-[10.5px] text-muted-foreground">
                  {formatReadableLabel(principal.detail)}
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

function TeamBriefView({
  brief,
  onAction,
}: {
  brief: TeamBrief
  onAction: () => void
}) {
  return (
    <section className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(280px,0.9fr)_minmax(0,1.5fr)]">
      <div className="min-w-0 bg-background p-3">
        <div className="flex items-center gap-2">
          <span className={cn("h-2 w-2 rounded-full", teamBriefDot(brief.tone))} />
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Access brief</div>
            <div className={cn("mt-0.5 truncate text-[14px] font-medium", teamBriefText(brief.tone))}>
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
          onClick={onAction}
        >
          {brief.action}
          {brief.actionType === "settings" ? <Settings className="h-3 w-3" /> : <RefreshCw className="h-3 w-3" />}
        </Button>
      </div>
      <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
        {brief.facts.map((fact) => (
          <div key={fact.label} className="min-w-0 bg-background px-3 py-2.5">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
            <div
              className={cn("mt-0.5 truncate font-mono text-[12px]", fact.tone ? teamBriefText(fact.tone) : "")}
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
  const failedSpend = recent
    .filter((run) => run.status === "failed")
    .reduce((acc, run) => acc + Number(run.cost_usd), 0)
  const maxRunCost = priced.reduce((max, run) => Math.max(max, Number(run.cost_usd)), 0)
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
    failedSpend,
    maxRunCost,
    byRegion: Array.from(byRegion.entries())
      .map(([name, value]) => ({
        name,
        value: value.spend,
        runs: value.runs,
        unpriced: value.unpriced,
        pct: spend7d ? Math.max(3, Math.round((value.spend / spend7d) * 100)) : 0,
      }))
      .sort((a, b) => b.value - a.value || b.runs - a.runs),
    byStatus: billingStatusRows(recent, spend7d),
    outliers: billingOutliers(recent, {
      avgCost: priced.length ? spend7d / priced.length : 0,
      avgDurationMs: durations.length ? durations.reduce((acc, value) => acc + value, 0) / durations.length : 0,
    }),
    timeline: billingTimeline(runs),
  }
}

function billingStatusRows(runs: Run[], spend: number) {
  const statuses: Run["status"][] = ["succeeded", "failed", "running", "queued"]
  return statuses.map((status) => {
    const rows = runs.filter((run) => run.status === status)
    const statusSpend = rows.reduce((acc, run) => acc + Number(run.cost_usd), 0)
    return {
      status,
      spend: Number(statusSpend.toFixed(4)),
      runs: rows.length,
      unpriced: rows.filter((run) => Number(run.cost_usd) <= 0).length,
      pct: spend ? Math.round((statusSpend / spend) * 100) : 0,
    }
  })
}

function billingOutliers(
  runs: Run[],
  baseline: { avgCost: number; avgDurationMs: number },
) {
  return runs
    .map((run) => {
      const cost = Number(run.cost_usd)
      const reasons = [
        run.status === "failed" && cost > 0 ? "failed run still consumed spend" : "",
        cost <= 0 ? "missing price coverage" : "",
        baseline.avgCost > 0 && cost > baseline.avgCost * 2 ? `${Math.round(cost / baseline.avgCost)}x average cost` : "",
        baseline.avgDurationMs > 0 && run.duration_ms > baseline.avgDurationMs * 1.75 ? "runtime materially above average" : "",
      ].filter(Boolean)
      const score =
        (run.status === "failed" ? 100 : 0) +
        (cost <= 0 ? 60 : 0) +
        (baseline.avgCost > 0 ? (cost / baseline.avgCost) * 20 : cost * 20) +
        (baseline.avgDurationMs > 0 ? (run.duration_ms / baseline.avgDurationMs) * 8 : 0)
      return {
        run,
        score,
        reason: reasons[0] ?? (cost > 0 ? "highest priced recent run" : "recent run needs pricing"),
        tone: run.status === "failed" ? "danger" as const : cost <= 0 || reasons.length > 0 ? "warning" as const : "muted" as const,
      }
    })
    .filter((row) => row.reason)
    .sort((a, b) => b.score - a.score)
    .slice(0, 8)
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

function connectionSummary(form: WorkspacePreferences, runs: Run[], savedAt: string | null, authStatus: string) {
  const transport = form.apiBase
    ? form.apiBase.startsWith("https://")
      ? "https transport"
      : "non-https override"
    : "default transport"
  return {
    host: form.apiBase ? safeHost(form.apiBase) : "bundled default",
    transport,
    auth: authStatus === "signed_in" ? "Google" : "signed out",
    credential: authStatus === "signed_in" ? "ID token" : "no session",
    observedRegions: new Set(runs.map((run) => run.region)).size,
    saved: savedAt ?? "not saved",
    storage: "local browser",
  }
}

function buildConnectionBrief({
  diagnostics,
  form,
  formDirty,
  regions,
  runsError,
  savedAt,
  checking,
  authStatus,
}: {
  diagnostics: CloudProbeResult[]
  form: WorkspacePreferences
  formDirty: boolean
  regions: ReturnType<typeof regionSummary>
  runsError: string | null
  savedAt: string | null
  checking: boolean
  authStatus: string
}): ConnectionBrief {
  const total = diagnostics.length
  const failures = diagnostics.filter((result) => !result.ok)
  const ok = total - failures.length
  const avgLatency = total
    ? Math.round(diagnostics.reduce((acc, result) => acc + result.latencyMs, 0) / total)
    : null
  const usesUnsafeOverride = Boolean(form.apiBase && !form.apiBase.startsWith("https://"))
  const authLabel = authStatus === "signed_in" ? "Google" : "signed out"
  const endpointValue = total ? `${ok}/${total} ok` : checking ? "checking" : "not checked"
  const latencyValue = avgLatency == null ? "n/a" : formatLatency(avgLatency)
  const regionValue = runsError
    ? "unverified"
    : regions.length
      ? `${regions.filter((region) => region.runs > 0).length}/${regions.length} active`
      : "none"
  const saveValue = formDirty ? "unsaved" : savedAt ? "saved" : "stored"

  if (checking && total === 0) {
    return {
      tone: "info",
      verdict: "Checking workspace connection",
      detail: "Endpoint probes are running against the current workspace preferences.",
      action: "Checking",
      facts: connectionBriefFacts("Endpoints", endpointValue, "Latency", "pending", "Auth", authLabel, "Regions", regionValue, "info"),
    }
  }

  if (formDirty) {
    return {
      tone: "warning",
      verdict: "Unsaved connection changes",
      detail: "The form differs from the active browser configuration. Save before trusting the diagnostics or opening other workspace pages.",
      action: "Save & check",
      facts: connectionBriefFacts("Saved", saveValue, "Endpoints", endpointValue, "Auth", authLabel, "Regions", regionValue, "warning"),
    }
  }

  if (failures.length > 0) {
    return {
      tone: "warning",
      verdict: "Connection needs attention",
      detail: `${failures.map((failure) => failure.label).join(", ")} ${failures.length === 1 ? "is" : "are"} failing. Fix the API base or Google session before relying on this workspace.`,
      action: "Recheck API",
      facts: connectionBriefFacts("Endpoints", endpointValue, "Latency", latencyValue, "Auth", authLabel, "Regions", regionValue, "warning"),
    }
  }

  if (usesUnsafeOverride) {
    return {
      tone: "warning",
      verdict: "Non-HTTPS API override",
      detail: "The API override is reachable but not HTTPS. Use this only for local development or trusted private networks.",
      action: "Recheck API",
      facts: connectionBriefFacts("Transport", "non-https", "Endpoints", endpointValue, "Auth", authLabel, "Regions", regionValue, "warning"),
    }
  }

  if (runsError) {
    return {
      tone: "info",
      verdict: "API reachable, run evidence missing",
      detail: "Core endpoint checks passed, but run history could not be loaded for regional evidence.",
      action: "Recheck API",
      facts: connectionBriefFacts("Endpoints", endpointValue, "Latency", latencyValue, "Auth", authLabel, "Regions", "request failed", "info"),
    }
  }

  if (total === 0) {
    return {
      tone: "muted",
      verdict: "Connection not checked yet",
      detail: "Run diagnostics to confirm registry, package, and run endpoints before treating this workspace as ready.",
      action: "Check API",
      facts: connectionBriefFacts("Endpoints", endpointValue, "Latency", latencyValue, "Auth", authLabel, "Regions", regionValue, "muted"),
    }
  }

  return {
    tone: "success",
    verdict: "Workspace connection ready",
    detail: "Registry, package, and run endpoints are reachable. Current preferences can support the operational pages.",
    action: "Recheck API",
    facts: connectionBriefFacts("Endpoints", endpointValue, "Latency", latencyValue, "Auth", authLabel, "Regions", regionValue, "success"),
  }
}

function connectionBriefFacts(
  firstLabel: string,
  firstValue: string,
  secondLabel: string,
  secondValue: string,
  thirdLabel: string,
  thirdValue: string,
  fourthLabel: string,
  fourthValue: string,
  tone: ConnectionBriefTone,
): ConnectionBrief["facts"] {
  return [
    { label: firstLabel, value: firstValue, tone },
    { label: secondLabel, value: secondValue },
    { label: thirdLabel, value: thirdValue },
    { label: fourthLabel, value: fourthValue, tone: fourthValue === "request failed" ? "warning" : undefined },
  ]
}

function connectionBriefDot(tone: ConnectionBriefTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "info") return "bg-info animate-pulse"
  return "bg-muted-foreground"
}

function connectionBriefText(tone: ConnectionBriefTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "info") return "text-info"
  return "text-foreground"
}

function workspacePreferencesChanged(left: WorkspacePreferences, right: WorkspacePreferences) {
  return (
    left.name !== right.name ||
    left.slug !== right.slug ||
    left.defaultRegion !== right.defaultRegion ||
    left.idleMinutes !== right.idleMinutes ||
    left.apiBase !== right.apiBase
  )
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

function teamAccessBrief(
  workspace: WorkspacePreferences,
  signedIn: boolean,
  principals: PrincipalRow[],
  runs: Run[],
  registry: RegistryItem[],
  experiments: Experiment[],
  loadError: string | null,
): TeamBrief {
  const observedObjects = runs.length + registry.length + experiments.length
  const owners = new Set([
    ...registry.map((item) => item.owner),
    ...experiments.map((experiment) => experiment.owner),
  ].filter(Boolean))
  const latestSeen = newestMany([
    ...runs.map((run) => run.created_at),
    ...registry.map((item) => item.created_at),
    ...experiments.map((experiment) => experiment.created_at),
  ])
  const authLabel = signedIn ? "Google" : "signed out"
  const latestLabel = latestSeen ? formatRelative(latestSeen) : "none"

  if (loadError) {
    return {
      tone: "warning",
      verdict: "Access evidence offline",
      detail: "The team surface cannot prove current principals until runs, registry, and experiment evidence load from the API.",
      action: "Check settings",
      actionType: "settings",
      facts: teamBriefFacts("Auth", authLabel, "Evidence", "request failed", "Objects", `${observedObjects}`, "API", workspace.apiBase ? safeHost(workspace.apiBase) : "default", "warning"),
    }
  }

  if (observedObjects === 0) {
    return {
      tone: "info",
      verdict: "Workspace-only visibility",
      detail: "Only the local workspace principal is visible. Shared owners appear after packages, experiments, or runs are created.",
      action: "Refresh evidence",
      actionType: "refresh",
      facts: teamBriefFacts("Principals", `${principals.length}`, "Owners", "0", "Objects", "0", "Auth", authLabel, "info"),
    }
  }

  if (!signedIn) {
    return {
      tone: "warning",
      verdict: "Signed-out evidence view",
      detail: "Activity is visible, but this browser has no active Google session. Sign in before using it as an access audit.",
      action: "Sign in",
      actionType: "settings",
      facts: teamBriefFacts("Auth", "signed out", "Principals", `${principals.length}`, "Owners", `${owners.size}`, "Latest", latestLabel, "warning"),
    }
  }

  if (owners.size <= 1 && observedObjects > 0) {
    return {
      tone: "warning",
      verdict: "Single-owner activity",
      detail: "The workspace has evidence, but ownership is concentrated. Confirm whether this is expected before relying on team coverage.",
      action: "Refresh evidence",
      actionType: "refresh",
      facts: teamBriefFacts("Owners", `${owners.size}`, "Principals", `${principals.length}`, "Objects", `${observedObjects}`, "Latest", latestLabel, "warning"),
    }
  }

  return {
    tone: "success",
    verdict: "Shared activity visible",
    detail: "Runs, registry resources, and experiment packages expose enough owner evidence to understand current workspace participation.",
    action: "Refresh evidence",
    actionType: "refresh",
    facts: teamBriefFacts("Principals", `${principals.length}`, "Owners", `${owners.size}`, "Objects", `${observedObjects}`, "Latest", latestLabel, "success"),
  }
}

function teamBriefFacts(
  firstLabel: string,
  firstValue: string,
  secondLabel: string,
  secondValue: string,
  thirdLabel: string,
  thirdValue: string,
  fourthLabel: string,
  fourthValue: string,
  tone: TeamBriefTone,
): TeamBrief["facts"] {
  return [
    { label: firstLabel, value: firstValue, tone },
    { label: secondLabel, value: secondValue, tone: tone === "warning" ? "warning" : undefined },
    { label: thirdLabel, value: thirdValue },
    { label: fourthLabel, value: fourthValue },
  ]
}

function teamBriefDot(tone: TeamBriefTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "info") return "bg-info"
  return "bg-muted-foreground"
}

function teamBriefText(tone: TeamBriefTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "info") return "text-info"
  return "text-muted-foreground"
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

function billingStatusColor(status: Run["status"]) {
  if (status === "succeeded") return "var(--success)"
  if (status === "failed") return "var(--destructive)"
  if (status === "running") return "var(--info)"
  return "var(--muted-foreground)"
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

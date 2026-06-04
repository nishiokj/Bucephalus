import { useEffect, useMemo, useState } from "react"
import {
  Activity,
  AlertTriangle,
  ChevronRight,
  Command as CommandIcon,
  GitCompare,
  Package,
  Plus,
  Search,
  Settings,
  Slash,
  TerminalSquare,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
} from "@/components/ui/command"
import { KindBadge, StatusPill } from "@/components/status-pill"
import { useRouter, type Route } from "@/lib/router"
import { ModeToggle } from "@/components/mode-toggle"
import { formatDuration, formatNumber, formatReadableLabel, formatReadableToken, formatRelative, formatShortId, formatUsd } from "@/lib/format"
import { cloudApi, type Experiment, type RegistryItem, type Run, type Trace } from "@/lib/cloud-api"
import { useWorkspacePreferences } from "@/lib/workspace"

type Crumb = { label: string; mono?: boolean }

function useCrumbs({
  activeRun,
  activeExperiment,
}: {
  activeRun: Run | null
  activeExperiment: Experiment | null
}) {
  const { route } = useRouter()
  const workspace = useWorkspacePreferences()
  const crumbs: Crumb[] = [{ label: workspace.slug }]
  switch (route.name) {
    case "home":
      crumbs.push({ label: "overview" })
      break
    case "registry":
      crumbs.push({ label: "registry" })
      if (route.kind) crumbs.push({ label: registryCrumb(route.kind) })
      break
    case "experiments":
      crumbs.push({ label: "experiments" })
      break
    case "experiment-new":
      crumbs.push({ label: "experiments" }, { label: "new", mono: true })
      break
    case "experiment-detail":
      crumbs.push(
        { label: "experiments" },
        activeExperiment
          ? { label: formatReadableLabel(activeExperiment.name) }
          : { label: "experiment loading" },
      )
      break
    case "runs":
      crumbs.push({ label: "runs" })
      break
    case "run-detail":
      crumbs.push(
        { label: "runs" },
        activeRun
          ? { label: runCommandLabel(activeRun) }
          : { label: "run loading" },
      )
      break
    case "compare":
      crumbs.push({ label: "compare" })
      break
    case "billing":
      crumbs.push({ label: "billing" })
      break
    case "settings":
      crumbs.push({ label: "settings" })
      break
    case "team":
      crumbs.push({ label: "team" })
      break
  }
  return crumbs
}

function registryCrumb(kind: NonNullable<Extract<Route, { name: "registry" }>["kind"]>) {
  return kind === "experiment_package" ? "packages" : kind
}

export function TopBar() {
  const { navigate, route } = useRouter()
  const workspace = useWorkspacePreferences()
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [runs, setRuns] = useState<Run[]>([])
  const [registry, setRegistry] = useState<RegistryItem[]>([])
  const [experiments, setExperiments] = useState<Experiment[]>([])
  const [traces, setTraces] = useState<Trace[]>([])
  const [routeRun, setRouteRun] = useState<Run | null>(null)
  const [routeExperiment, setRouteExperiment] = useState<Experiment | null>(null)
  const [paletteError, setPaletteError] = useState<string | null>(null)
  const [paletteLoading, setPaletteLoading] = useState(false)

  const showNewBtn = route.name === "home"

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault()
        setPaletteOpen((open) => !open)
      }
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [])

  useEffect(() => {
    if (new URLSearchParams(window.location.search).get("command") === "1") {
      setPaletteOpen(true)
    }
  }, [])

  useEffect(() => {
    if (!paletteOpen) return
    setPaletteError(null)
    setPaletteLoading(true)
    let cancelled = false
    void (async () => {
      try {
        const [runRows, traceRows, registryRows, experimentRows] = await Promise.all([
          cloudApi
            .from("runs")
            .select("*")
            .order("created_at", { ascending: false })
            .limit(12),
          cloudApi
            .from("traces")
            .select("*")
            .order("recorded_at", { ascending: false })
            .limit(12),
          cloudApi
            .from("registry_items")
            .select("*")
            .order("created_at", { ascending: false })
            .limit(12),
          cloudApi
            .from("experiments")
            .select("*")
            .order("created_at", { ascending: false })
            .limit(12),
        ])
        if (cancelled) return
        setRuns((runRows.data ?? []) as Run[])
        setTraces((traceRows.data ?? []) as Trace[])
        setRegistry((registryRows.data ?? []) as RegistryItem[])
        setExperiments((experimentRows.data ?? []) as Experiment[])
        setPaletteError(runRows.error?.message ?? traceRows.error?.message ?? registryRows.error?.message ?? experimentRows.error?.message ?? null)
      } catch (error) {
        if (!cancelled) setPaletteError(error instanceof Error ? error.message : String(error))
      } finally {
        if (!cancelled) setPaletteLoading(false)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [paletteOpen])

  const paletteRun = route.name === "run-detail" ? runs.find((run) => run.id === route.id) ?? null : null
  const cachedRun = route.name === "run-detail" && routeRun?.id === route.id ? routeRun : null
  const activeRun = paletteRun ?? cachedRun
  const paletteExperiment = route.name === "experiment-detail" ? experiments.find((experiment) => experiment.id === route.id) ?? null : null
  const cachedExperiment = route.name === "experiment-detail" && routeExperiment?.id === route.id ? routeExperiment : null
  const activeExperiment = paletteExperiment ?? cachedExperiment
  const crumbs = useCrumbs({ activeRun, activeExperiment })
  const paletteErrorSummary = paletteError ? summarizePaletteError(paletteError) : null

  useEffect(() => {
    if (route.name !== "run-detail") {
      setRouteRun(null)
      return
    }
    if (activeRun) return
    let cancelled = false
    void cloudApi
      .from<Run>("runs")
      .select("*")
      .eq("id", route.id)
      .maybeSingle()
      .then((result) => {
        if (!cancelled && result.data) setRouteRun(result.data)
      })
    return () => {
      cancelled = true
    }
  }, [activeRun, route])

  useEffect(() => {
    if (route.name !== "experiment-detail") {
      setRouteExperiment(null)
      return
    }
    if (activeExperiment) return
    let cancelled = false
    void cloudApi
      .from<Experiment>("experiments")
      .select("*")
      .eq("id", route.id)
      .maybeSingle()
      .then((result) => {
        if (!cancelled && result.data) setRouteExperiment(result.data)
      })
    return () => {
      cancelled = true
    }
  }, [activeExperiment, route])

  const navItems = useMemo(
    () => [
      { label: "Overview", route: { name: "home" } as const, icon: Search, hint: "Dashboard" },
      { label: "Registry", route: { name: "registry" } as const, icon: Package, hint: "Resources" },
      { label: "Experiments", route: { name: "experiments" } as const, icon: TerminalSquare, hint: "Author evals" },
      { label: "Runs", route: { name: "runs" } as const, icon: Activity, hint: "Executions" },
      { label: "Compare", route: { name: "compare" } as const, icon: GitCompare, hint: "Analyze variants" },
      { label: "Settings", route: { name: "settings" } as const, icon: Settings, hint: "Workspace" },
    ],
    [],
  )
  const runById = useMemo(() => new Map(runs.map((run) => [run.id, run])), [runs])
  const suggestions = useMemo(
    () => paletteSuggestions({ runs, traces, registry }),
    [registry, runs, traces],
  )
  const snapshot = useMemo(
    () => paletteSnapshot({ runs, traces, registry, experiments, loading: paletteLoading, error: paletteError }),
    [experiments, paletteError, paletteLoading, registry, runs, traces],
  )

  function go(next: Route) {
    navigate(next)
    setPaletteOpen(false)
  }

  function goSuggestion(suggestion: PaletteSuggestion) {
    if (suggestion.draftPackage) {
      writePackageExperimentDraft(suggestion.draftPackage, workspace.defaultRegion)
    }
    go(suggestion.route)
  }

  return (
    <>
      <header className="sticky top-0 z-20 flex h-11 w-full max-w-[calc(100vw-3rem)] shrink-0 items-center gap-2 overflow-hidden border-b border-border bg-background/80 px-3 backdrop-blur md:max-w-none">
        <nav className="flex min-w-0 flex-1 items-center gap-1 overflow-hidden text-[12.5px]">
        {crumbs.map((c, i) => (
          <span key={i} className="flex min-w-0 items-center gap-1">
            {i > 0 ? (
              <Slash className="h-3 w-3 -rotate-12 text-muted-foreground/60" />
            ) : null}
            <span
              className={
                i === crumbs.length - 1
                  ? "truncate font-medium text-foreground"
                  : "truncate text-muted-foreground hover:text-foreground"
              }
              style={c.mono ? { fontFamily: "var(--font-mono)" } : undefined}
            >
              {c.label}
            </span>
          </span>
        ))}
        </nav>

        <div className="flex shrink-0 items-center gap-1.5 md:hidden">
          <Button
            variant="outline"
            size="sm"
            className="h-7 w-7 border-border bg-card p-0 text-muted-foreground shadow-sm"
            onClick={() => setPaletteOpen(true)}
            aria-label="Open command search"
            title="Search"
          >
            <Search className="h-3.5 w-3.5" />
          </Button>
          <ModeToggle />
          {showNewBtn ? (
            <Button
              size="sm"
              className="h-7 w-7 bg-brand p-0 text-brand-foreground shadow-sm hover:bg-brand/90"
              onClick={() => navigate({ name: "experiment-new" })}
              aria-label="New experiment"
              title="New experiment"
            >
              <Plus className="h-3.5 w-3.5" />
            </Button>
          ) : null}
        </div>

        <div className="ml-auto hidden shrink-0 items-center gap-1.5 md:flex">
        <button
          onClick={() => setPaletteOpen(true)}
          className="hidden items-center gap-2 rounded-md border border-border bg-card px-2 py-1 text-[12px] text-muted-foreground hover:bg-accent md:flex"
        >
          <Search className="h-3.5 w-3.5" />
          <span>Search resources, runs, traces</span>
          <span className="ml-8 inline-flex items-center gap-0.5 rounded border border-border bg-muted px-1 font-mono text-[10px]">
            <CommandIcon className="h-2.5 w-2.5" />K
          </span>
        </button>
        <ModeToggle />
        {showNewBtn ? (
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={() => navigate({ name: "experiment-new" })}
          >
            <Plus className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">New experiment</span>
            <ChevronRight className="hidden h-3 w-3 opacity-60 sm:block" />
          </Button>
        ) : null}
        </div>
      </header>
      <CommandDialog
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        title="Search Bucephalus"
        description="Jump to pages, runs, experiments, and registry resources."
        className="top-[22%] translate-y-0 sm:max-w-3xl"
      >
        <CommandInput placeholder="Search runs, packages, traces..." />
        <CommandList className="max-h-[560px]">
          <CommandEmpty>No matching resource found.</CommandEmpty>
          <CommandGroup heading="Workspace snapshot">
            <PaletteSnapshotView facts={snapshot} />
          </CommandGroup>
          <CommandSeparator />
          {suggestions.length > 0 ? (
            <>
              <CommandGroup heading="Suggested next">
                {suggestions.map((suggestion) => (
                  <CommandItem
                    key={suggestion.id}
                    value={`${suggestion.label} ${suggestion.detail} ${suggestion.meta}`}
                    onSelect={() => goSuggestion(suggestion)}
                    className="grid grid-cols-[18px_minmax(0,1fr)] gap-2 sm:grid-cols-[18px_minmax(180px,1fr)_150px]"
                  >
                    <suggestion.icon className={suggestion.iconClass} />
                    <span className="min-w-0">
                      <span className="block truncate text-[12px] font-medium">{suggestion.label}</span>
                      <span className="block truncate text-[10.5px] text-muted-foreground">{suggestion.detail}</span>
                    </span>
                    <CommandShortcut className="hidden sm:block">{suggestion.meta}</CommandShortcut>
                  </CommandItem>
                ))}
              </CommandGroup>
              <CommandSeparator />
            </>
          ) : null}
          <CommandGroup heading="Go to">
            {navItems.map((item) => (
              <CommandItem
                key={item.label}
                value={`${item.label} ${item.hint}`}
                onSelect={() => go(item.route)}
                className="gap-3"
              >
                <item.icon className="h-4 w-4 text-muted-foreground" />
                <span className="font-medium">{item.label}</span>
                <CommandShortcut>{item.hint}</CommandShortcut>
              </CommandItem>
            ))}
          </CommandGroup>
          <CommandSeparator />
          {runs.length > 0 ? (
            <CommandGroup heading={`Runs (${runs.length})`}>
              {runs.map((run) => (
                <CommandItem
                  key={run.id}
                  value={`${runCommandLabel(run)} ${run.experiment_name} ${run.variant} ${run.status} ${run.region}`}
                  onSelect={() => go({ name: "run-detail", id: run.id })}
                  className="grid grid-cols-[16px_minmax(0,1fr)_88px] gap-2 sm:grid-cols-[16px_minmax(180px,1fr)_96px_84px_84px]"
                >
                  <Activity className="h-3.5 w-3.5 text-muted-foreground" />
                  <span className="min-w-0">
                    <span className="block truncate font-mono text-[12px]">{formatReadableLabel(run.experiment_name)}</span>
                    <span className="block truncate text-[10.5px] text-muted-foreground">{formatReadableToken(run.variant)}</span>
                  </span>
                  <StatusPill status={run.status} />
                  <span className="hidden font-mono text-[11px] text-muted-foreground sm:block">{formatDuration(run.duration_ms)}</span>
                  <span className="hidden font-mono text-[11px] text-muted-foreground sm:block">{formatUsd(Number(run.cost_usd))}</span>
                </CommandItem>
              ))}
            </CommandGroup>
          ) : null}
          {traces.length > 0 ? (
            <CommandGroup heading={`Trace events (${traces.length})`}>
              {traces.map((trace) => {
                const run = runById.get(trace.run_id)
                const runLabel = run ? runCommandLabel(run) : `run ${formatShortId(trace.run_id)}`
                return (
                  <CommandItem
                    key={trace.id}
                    value={`${trace.level} ${trace.span} ${trace.message} ${runLabel}`}
                    onSelect={() => go({ name: "run-detail", id: trace.run_id })}
                    className="grid grid-cols-[16px_minmax(0,1fr)_72px] gap-2 sm:grid-cols-[16px_minmax(190px,1fr)_minmax(130px,.7fr)_72px_76px_84px]"
                  >
                    <Activity className="h-3.5 w-3.5 text-muted-foreground" />
                    <span className="hidden min-w-0 sm:block">
                      <span className="block truncate font-mono text-[12px]">
                        {formatReadableLabel(trace.span)}
                      </span>
                      <span className="block truncate text-[10.5px] text-muted-foreground">
                        {formatReadableLabel(trace.message)}
                      </span>
                    </span>
                    <span className="min-w-0">
                      <span className="block truncate font-mono text-[11px] text-foreground">
                        {runLabel}
                      </span>
                      <span className="block truncate font-mono text-[10.5px] text-muted-foreground">
                        {run?.region ?? "runtime"}
                      </span>
                    </span>
                    <TraceLevelBadge level={trace.level} />
                    <span className="hidden font-mono text-[11px] text-muted-foreground sm:block">{formatDuration(trace.latency_ms)}</span>
                    <span className="hidden font-mono text-[11px] text-muted-foreground sm:block">{formatRelative(trace.recorded_at)}</span>
                  </CommandItem>
                )
              })}
            </CommandGroup>
          ) : null}
          {registry.length > 0 ? (
            <CommandGroup heading={`Registry (${registry.length})`}>
              {registry.map((item) => (
                <CommandItem
                  key={item.id}
                  value={`${item.name} ${item.kind} ${item.version} ${item.tags.join(" ")}`}
                  onSelect={() => go({ name: "registry", kind: kindRoute(item.kind) })}
                  className="grid grid-cols-[16px_minmax(180px,1fr)_112px_72px_84px] gap-2"
                >
                  <Package className="h-3.5 w-3.5 text-muted-foreground" />
                  <span className="truncate font-mono text-[12px]">{formatReadableLabel(item.name)}</span>
                  <KindBadge kind={item.kind} />
                  <span className="truncate font-mono text-[11px] text-muted-foreground">{formatReadableToken(item.version)}</span>
                  <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(item.created_at)}</span>
                </CommandItem>
              ))}
            </CommandGroup>
          ) : null}
          {experiments.length > 0 ? (
            <CommandGroup heading={`Experiments (${experiments.length})`}>
              {experiments.map((experiment) => (
                <CommandItem
                  key={experiment.id}
                  value={`${experiment.name} ${experiment.description} ${experiment.tags.join(" ")}`}
                  onSelect={() => go({ name: "experiment-detail", id: experiment.id })}
                  className="grid grid-cols-[16px_minmax(180px,1fr)_96px_84px] gap-2"
                >
                  <TerminalSquare className="h-3.5 w-3.5 text-muted-foreground" />
                  <span className="truncate font-mono text-[12px]">{formatReadableLabel(experiment.name)}</span>
                  <span className="truncate font-mono text-[11px] text-muted-foreground">{experiment.owner}</span>
                  <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(experiment.created_at)}</span>
                </CommandItem>
              ))}
            </CommandGroup>
          ) : null}
          {paletteError && runs.length + traces.length + registry.length + experiments.length > 0 ? (
            <CommandGroup heading="Data source warning">
              <CommandItem
                value={`Configure API connection settings request failed ${paletteErrorSummary ?? ""}`}
                onSelect={() => go({ name: "settings" })}
                className="gap-3"
              >
                <Settings className="h-4 w-4 text-warning" />
                <span className="font-medium">Some palette results are unavailable</span>
                <CommandShortcut className="max-w-[220px] truncate normal-case tracking-normal">{paletteErrorSummary}</CommandShortcut>
              </CommandItem>
            </CommandGroup>
          ) : null}
          {runs.length + traces.length + registry.length + experiments.length === 0 ? (
            <CommandGroup heading="Data sources">
              <CommandItem
                value={`Configure API connection settings request failed ${paletteErrorSummary ?? ""}`}
                onSelect={() => go({ name: "settings" })}
                className="gap-3"
              >
                <Settings className="h-4 w-4 text-muted-foreground" />
                <span className="font-medium">
                  {paletteError ? "API request failed" : "Configure API connection"}
                </span>
                <CommandShortcut className="max-w-[220px] truncate normal-case tracking-normal">{paletteErrorSummary ?? "No rows loaded"}</CommandShortcut>
              </CommandItem>
            </CommandGroup>
          ) : null}
        </CommandList>
      </CommandDialog>
    </>
  )
}

type PaletteSuggestion = {
  id: string
  label: string
  detail: string
  meta: string
  route: Route
  icon: typeof Activity
  iconClass: string
  draftPackage?: RegistryItem
}

type PaletteSnapshotFact = {
  label: string
  value: string
  detail: string
  tone: "success" | "warning" | "danger" | "info" | "muted"
}

function PaletteSnapshotView({ facts }: { facts: PaletteSnapshotFact[] }) {
  return (
    <div className="grid grid-cols-2 gap-px overflow-hidden rounded-md border border-border bg-border md:grid-cols-4">
      {facts.map((fact) => (
        <div key={fact.label} className="min-w-0 bg-popover px-2.5 py-2">
          <div className="flex min-w-0 items-center gap-1.5">
            <span className={["h-1.5 w-1.5 shrink-0 rounded-full", paletteSnapshotDot(fact.tone)].join(" ")} />
            <span className="truncate text-[10px] uppercase tracking-wide text-muted-foreground">{fact.label}</span>
          </div>
          <div className={["mt-0.5 truncate font-mono text-[12px] font-medium", paletteSnapshotText(fact.tone)].join(" ")}>
            {fact.value}
          </div>
          <div className="truncate text-[10.5px] text-muted-foreground" title={fact.detail}>
            {fact.detail}
          </div>
        </div>
      ))}
    </div>
  )
}

function paletteSnapshot({
  runs,
  traces,
  registry,
  experiments,
  loading,
  error,
}: {
  runs: Run[]
  traces: Trace[]
  registry: RegistryItem[]
  experiments: Experiment[]
  loading: boolean
  error: string | null
}): PaletteSnapshotFact[] {
  if (loading && runs.length + traces.length + registry.length + experiments.length === 0) {
    return [
      { label: "Runs", value: "loading", detail: "fetching recent executions", tone: "muted" },
      { label: "Signals", value: "loading", detail: "fetching runtime traces", tone: "muted" },
      { label: "Registry", value: "loading", detail: "fetching resources", tone: "muted" },
      { label: "Experiments", value: "loading", detail: "fetching packages", tone: "muted" },
    ]
  }

  const failed = runs.filter((run) => run.status === "failed").length
  const running = runs.filter((run) => run.status === "running").length
  const errors = traces.filter((trace) => trace.level === "error").length
  const warnings = traces.filter((trace) => trace.level === "warn").length
  const readyPackages = registry.filter((item) => item.kind === "experiment_package" && item.status === "ready").length
  const building = registry.filter((item) => item.status === "building").length

  return [
    {
      label: "Runs",
      value: runs.length ? `${formatNumber(runs.length)} loaded` : "no rows",
      detail: failed ? `${failed} failed · ${running} active` : running ? `${running} active` : "recent execution window",
      tone: failed ? "danger" : running ? "info" : runs.length ? "success" : error ? "warning" : "muted",
    },
    {
      label: "Signals",
      value: errors ? `${errors} errors` : warnings ? `${warnings} warnings` : traces.length ? `${formatNumber(traces.length)} events` : "no traces",
      detail: traces.length ? "runtime evidence" : error ? "trace unavailable" : "no worker events",
      tone: errors ? "danger" : warnings ? "warning" : traces.length ? "success" : error ? "warning" : "muted",
    },
    {
      label: "Registry",
      value: readyPackages ? `${readyPackages} packages` : registry.length ? `${formatNumber(registry.length)} resources` : "no resources",
      detail: building ? `${building} building` : "resource inventory",
      tone: readyPackages ? "success" : registry.length ? "info" : error ? "warning" : "muted",
    },
    {
      label: "Experiments",
      value: experiments.length ? `${formatNumber(experiments.length)} loaded` : "no packages",
      detail: error ? "partial API" : "authoring targets",
      tone: error ? "warning" : experiments.length ? "success" : "muted",
    },
  ]
}

function paletteSnapshotDot(tone: PaletteSnapshotFact["tone"]) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  if (tone === "info") return "bg-info animate-pulse"
  return "bg-muted-foreground"
}

function paletteSnapshotText(tone: PaletteSnapshotFact["tone"]) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  if (tone === "info") return "text-info"
  return "text-foreground"
}

function summarizePaletteError(error: string) {
  const clean = error.replace(/\s+/g, " ").trim()
  if (!clean) return "Request failed"
  return clean.length > 46 ? `${clean.slice(0, 43)}...` : clean
}

function paletteSuggestions({
  runs,
  traces,
  registry,
}: {
  runs: Run[]
  traces: Trace[]
  registry: RegistryItem[]
}): PaletteSuggestion[] {
  const suggestions: PaletteSuggestion[] = []
  const add = (suggestion: PaletteSuggestion) => {
    if (!suggestions.some((item) => item.id === suggestion.id)) suggestions.push(suggestion)
  }
  const sortedRuns = [...runs].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))
  const latestError = traces.find((trace) => trace.level === "error")
  const failedRun = sortedRuns.find((run) => run.status === "failed")
  const runningRun = sortedRuns.find((run) => run.status === "running")
  const acceptedPackage = registry.find((item) => item.kind === "experiment_package" && item.status === "ready")

  if (latestError) {
    const run = runs.find((item) => item.id === latestError.run_id)
    add({
      id: `trace:${latestError.id}`,
      label: "Inspect latest error",
      detail: run ? `${runCommandLabel(run)} · ${formatReadableLabel(latestError.span)}` : formatReadableLabel(latestError.span),
      meta: `${formatDuration(latestError.latency_ms)} · ${formatRelative(latestError.recorded_at)}`,
      route: { name: "run-detail", id: latestError.run_id },
      icon: AlertTriangle,
      iconClass: "h-4 w-4 text-destructive",
    })
  }

  if (failedRun) {
    add({
      id: `failed:${failedRun.id}`,
      label: "Triage failed run",
      detail: runCommandLabel(failedRun),
      meta: `${formatDuration(failedRun.duration_ms)} · ${formatUsd(Number(failedRun.cost_usd))}`,
      route: { name: "run-detail", id: failedRun.id },
      icon: AlertTriangle,
      iconClass: "h-4 w-4 text-warning",
    })
  }

  if (runningRun) {
    add({
      id: `running:${runningRun.id}`,
      label: "Watch live run",
      detail: runCommandLabel(runningRun),
      meta: `${formatDuration(runningRun.duration_ms)} · ${runningRun.region}`,
      route: { name: "run-detail", id: runningRun.id },
      icon: Activity,
      iconClass: "h-4 w-4 text-info",
    })
  }

  if (runs.length >= 2) {
    add({
      id: "compare:recent",
      label: "Compare recent variants",
      detail: `${Math.min(6, runs.length)} runs loaded from the current API window`,
      meta: "analysis",
      route: { name: "compare" },
      icon: GitCompare,
      iconClass: "h-4 w-4 text-info",
    })
  }

  if (suggestions.length < 3 && acceptedPackage) {
    add({
      id: `queue:${acceptedPackage.id}`,
      label: "Queue accepted package",
      detail: formatReadableLabel(acceptedPackage.name),
      meta: "new run",
      route: { name: "experiment-new" },
      icon: TerminalSquare,
      iconClass: "h-4 w-4 text-brand",
      draftPackage: acceptedPackage,
    })
  }

  return suggestions.slice(0, 4)
}

function TraceLevelBadge({ level }: { level: string }) {
  return (
    <span
      className={[
        "w-fit rounded px-1.5 py-0.5 font-mono text-[10px]",
        level === "error"
          ? "bg-destructive/10 text-destructive"
          : level === "warn"
            ? "bg-warning/10 text-warning"
            : level === "debug"
              ? "bg-muted text-muted-foreground"
              : "bg-info/10 text-info",
      ].join(" ")}
    >
      {formatReadableToken(level || "info")}
    </span>
  )
}

function runCommandLabel(run: Run) {
  return `${formatReadableLabel(run.experiment_name)}/${formatReadableToken(run.variant)}`
}

function writePackageExperimentDraft(item: RegistryItem, region: string) {
  localStorage.setItem(
    "buc.experiment.draft",
    JSON.stringify({
      name: `run-${draftSlug(item.name)}`,
      description: item.description || `Experiment from ${formatReadableLabel(item.name)}`,
      benchmark: item.id,
      agents: [],
      mcps: [],
      seeds: "7,11,42",
      region,
      maxParallel: 8,
      budget: 50,
      variantSweep: [{ key: "temperature", values: "0.0, 0.4, 0.8" }],
    }),
  )
}

function draftSlug(value: string) {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48) || "registry-package"
}

function kindRoute(kind: RegistryItem["kind"]): "agent" | "benchmark" | "mcp" | "experiment_package" | undefined {
  return kind === "agent" || kind === "benchmark" || kind === "mcp" || kind === "experiment_package" ? kind : undefined
}

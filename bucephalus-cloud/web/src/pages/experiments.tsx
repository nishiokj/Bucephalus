import { useEffect, useMemo, useState } from "react"
import {
  ArrowUpRight,
  Beaker,
  CheckCircle2,
  Copy,
  GitBranch,
  Play,
  Plus,
  Search,
  Tag,
  XCircle,
} from "lucide-react"
import {
  Bar,
  BarChart,
  Cell,
  CartesianGrid,
  Line,
  LineChart,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Label } from "@/components/ui/label"
import { ConnectionIssue } from "@/components/connection-issue"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { FilterStrip, SegmentedControl } from "@/components/filter-strip"
import { KindBadge, StatusPill } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import { cloudApi, type Experiment, type RegistryItem, type Run, type RunMetric, type SecretRequirement } from "@/lib/cloud-api"
import { formatBytes, formatDuration, formatReadableLabel, formatReadableToken, formatRelative, formatShortId, formatUsd } from "@/lib/format"
import { PageHeader } from "@/pages/registry"
import { cn } from "@/lib/utils"
import { useWorkspacePreferences } from "@/lib/workspace"

const EXPERIMENT_DRAFT_KEY = "buc.experiment.draft"

const experimentRunCfg: ChartConfig = {
  duration: { label: "Duration", color: "var(--chart-1)" },
  cost: { label: "Cost", color: "var(--chart-3)" },
}

const experimentStatusCfg: ChartConfig = {
  count: { label: "Runs", color: "var(--chart-2)" },
}

const experimentMetricCfg: ChartConfig = {
  rows: { label: "Rows", color: "var(--chart-1)" },
}

const experimentPackageCfg: ChartConfig = {
  packages: { label: "Packages", color: "var(--chart-4)" },
}

const SEED_PRESETS = [
  { label: "smoke", values: [7] },
  { label: "stable", values: [7, 11, 42] },
  { label: "wide", values: [3, 7, 11, 19, 42] },
]

type ExperimentDecisionTone = "success" | "warning" | "danger" | "info" | "muted"
type ExperimentDecisionAction = "queue" | "compare" | "config"
type ExperimentDecisionBrief = {
  tone: ExperimentDecisionTone
  verdict: string
  detail: string
  action: string
  actionType: ExperimentDecisionAction
  facts: { label: string; value: string; tone?: ExperimentDecisionTone }[]
}

export function ExperimentsPage() {
  const { navigate } = useRouter()
  const [items, setItems] = useState<Experiment[]>([])
  const [loaded, setLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [q, setQ] = useState("")
  const [ownerFilter, setOwnerFilter] = useState("all")
  const [tagFilter, setTagFilter] = useState("all")
  const [sortMode, setSortMode] = useState("recent")
  const [queueingId, setQueueingId] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  async function loadExperiments() {
    setLoaded(false)
    setLoadError(null)
    const { data, error } = await cloudApi
      .from("experiments")
      .select("*")
      .order("created_at", { ascending: false })
    if (error) {
      setItems([])
      setLoadError(error.message)
      setLoaded(true)
      return
    }
    setItems((data ?? []) as Experiment[])
    setLoaded(true)
  }

  useEffect(() => {
    void loadExperiments()
  }, [])

  const owners = useMemo(
    () => filterOptions(items.map((i) => i.owner), "all owners"),
    [items],
  )
  const tags = useMemo(
    () => filterOptions(items.flatMap((i) => i.tags), "all tags"),
    [items],
  )
  const filtered = useMemo(() => {
    const rows = items.filter((i) => {
      if (ownerFilter !== "all" && i.owner !== ownerFilter) return false
      if (tagFilter !== "all" && !i.tags.includes(tagFilter)) return false
      if (q && !`${i.name} ${i.description} ${i.tags.join(" ")}`.toLowerCase().includes(q.toLowerCase())) return false
      return true
    })
    return [...rows].sort((a, b) => {
      if (sortMode === "name") return a.name.localeCompare(b.name)
      if (sortMode === "owner") return a.owner.localeCompare(b.owner)
      return Date.parse(b.created_at) - Date.parse(a.created_at)
    })
  }, [items, ownerFilter, q, sortMode, tagFilter])
  const summary = useMemo(() => experimentInventorySummary(items), [items])
  const packageMix = useMemo(() => experimentPackageMix(items), [items])
  const hasFilters = Boolean(q || ownerFilter !== "all" || tagFilter !== "all")
  const unavailable = Boolean(loadError)

  async function queueFromList(experiment: Experiment) {
    setQueueingId(experiment.id)
    setNotice(null)
    const { data, error } = await cloudApi
      .from("experiments")
      .insert({
        name: experiment.name,
        description: experiment.description,
        tags: experiment.tags,
        owner: experiment.owner,
        config: experiment.config,
      })
      .select("*")
      .maybeSingle()
    setQueueingId(null)
    if (data?.id) navigate({ name: "run-detail", id: data.id })
    else setNotice(error?.message ?? "Unable to queue this package from the current API response.")
  }

  function clearFilters() {
    setQ("")
    setOwnerFilter("all")
    setTagFilter("all")
    setSortMode("recent")
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Experiments"
        subtitle="Author and queue eval packages."
        primaryAction={
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={() => navigate({ name: "experiment-new" })}
          >
            <Plus className="h-3.5 w-3.5" /> New experiment
          </Button>
        }
      />

      {notice ? (
        <div className="flex items-center justify-between border-b border-border bg-warning/10 px-3 py-2 text-[12px]">
          <span>{notice}</span>
          <button
            className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:text-foreground"
            onClick={() => setNotice(null)}
            aria-label="Dismiss experiment notice"
          >
            <XCircle className="h-3 w-3" />
          </button>
        </div>
      ) : null}

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border sm:grid-cols-2 lg:grid-cols-5">
        <ExperimentInventoryStat
          label="Packages"
          value={!loaded ? "loading" : unavailable ? "-" : `${summary.total}`}
          detail={unavailable ? "request failed" : `${summary.queueable} queueable`}
        />
        <ExperimentInventoryStat
          label="Owners"
          value={!loaded ? "loading" : unavailable ? "-" : `${summary.owners}`}
          detail={unavailable ? "unavailable" : "package authors"}
        />
        <ExperimentInventoryStat
          label="Tags"
          value={!loaded ? "loading" : unavailable ? "-" : `${summary.tags}`}
          detail={unavailable ? "connect API" : "status + target"}
        />
        <ExperimentInventoryStat
          label="Latest"
          value={!loaded ? "loading" : unavailable ? "-" : summary.latestLabel}
          detail={unavailable ? "request failed" : summary.latestOwner}
        />
        <ExperimentInventoryStat
          label="Filtered"
          value={!loaded ? "loading" : unavailable ? "-" : `${filtered.length}`}
          detail={unavailable ? "unavailable" : hasFilters ? "active filters" : "full inventory"}
        />
      </div>

      {loaded && !loadError ? (
        <div className="sticky top-11 z-10 flex flex-col gap-1.5 border-b border-border bg-background/95 px-3 py-1.5 backdrop-blur">
          <div className="flex flex-col gap-2 lg:flex-row lg:items-center lg:justify-between">
            <div className="relative min-w-[180px] flex-1 lg:max-w-sm">
              <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={q}
                onChange={(e) => setQ(e.target.value)}
                placeholder="Filter experiments..."
                className="h-7 w-full pl-7 text-[12px]"
              />
            </div>
            <SegmentedControl
              label="Sort"
              value={sortMode}
              onValueChange={setSortMode}
              options={[
                { value: "recent", label: "recent" },
                { value: "name", label: "name" },
                { value: "owner", label: "owner" },
              ]}
            />
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <FilterStrip label="Owner" value={ownerFilter} options={owners} onValueChange={setOwnerFilter} max={5} className="w-full max-w-full sm:w-auto" />
            <FilterStrip label="Tag" value={tagFilter} options={tags} onValueChange={setTagFilter} max={6} className="w-full max-w-full sm:w-auto" />
          </div>
        </div>
      ) : null}

      {!loaded ? <ExperimentsSkeleton /> : null}

      {loaded && items.length > 0 ? (
        <section className="border-b border-border bg-background">
          <header className="flex h-8 items-center justify-between border-b border-border px-3">
            <div className="flex items-center gap-2">
              <h3 className="text-[12px] font-semibold">Package mix</h3>
              <span className="text-[11px] text-muted-foreground">by status tag</span>
            </div>
            <span className="font-mono text-[11px] text-muted-foreground">{summary.total} packages</span>
          </header>
          <div className="p-3">
            <ChartContainer config={experimentPackageCfg} className="h-36 w-full">
              <BarChart data={packageMix} layout="vertical" margin={{ left: 4, right: 12, top: 8, bottom: 4 }}>
                <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis type="number" hide allowDecimals={false} />
                <YAxis
                  type="category"
                  dataKey="status"
                  width={88}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip contentStyle={experimentTooltipStyle} />
                <Bar dataKey="packages" fill="var(--color-packages)" radius={[0, 2, 2, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          </div>
        </section>
      ) : null}

      {loaded && loadError ? (
        <ConnectionIssue
          title="Experiment package request failed"
          detail={loadError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadExperiments()}
        />
      ) : null}

      {loaded && !loadError && items.length === 0 ? (
        <ExperimentsEmptyState
          title="No experiment packages"
          detail="Accepted packages appear here."
          action="Open registry"
          onAction={() => navigate({ name: "registry" })}
        />
      ) : null}
      {loaded && !loadError && items.length > 0 && filtered.length === 0 ? (
        <ExperimentsEmptyState
          title="No experiments match"
          detail="Clear filters to return to the full package inventory."
          action="Clear filters"
          onAction={clearFilters}
        />
      ) : null}

      {loaded && !loadError && filtered.length > 0 ? (
      <div className="overflow-x-auto text-[12px]">
        <div
          className="grid min-w-[880px] items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
          style={{
            gridTemplateColumns:
              "minmax(220px,1.6fr) minmax(220px,1fr) minmax(160px,0.8fr) 90px 90px 80px",
          }}
        >
          <span>Experiment</span>
          <span>Description</span>
          <span>Tags</span>
          <span>Owner</span>
          <span>Updated</span>
          <span>Actions</span>
        </div>

        {filtered.map((e) => (
          <div
            key={e.id}
            className="group grid min-w-[880px] items-center gap-2 border-b border-border px-3 py-1.5 hover:bg-accent/40"
            style={{
              gridTemplateColumns:
                "minmax(220px,1.6fr) minmax(220px,1fr) minmax(160px,0.8fr) 90px 90px 80px",
            }}
          >
            <button
              onClick={() => navigate({ name: "experiment-detail", id: e.id })}
              className="flex min-w-0 items-center gap-2 text-left"
            >
              <Beaker className="h-3 w-3 shrink-0 text-brand" />
              <span className="truncate font-mono text-[12.5px] font-medium">
                {formatReadableLabel(e.name)}
              </span>
            </button>
            <span className="truncate text-[11.5px] text-muted-foreground">
              {e.description}
            </span>
            <div className="flex flex-wrap items-center gap-1">
              {e.tags.slice(0, 3).map((t) => (
                <span
                  key={t}
                  className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
                >
                  {formatReadableToken(t)}
                </span>
              ))}
            </div>
            <span className="font-mono text-[11px] text-muted-foreground">
              {e.owner}
            </span>
            <span className="font-mono text-[11px] text-muted-foreground">
              {formatRelative(e.created_at)}
            </span>
            <div className="flex items-center gap-1 opacity-100 lg:opacity-0 lg:transition-opacity lg:group-hover:opacity-100">
              <button
                title={queueingId === e.id ? "Queueing" : "Queue run"}
                onClick={() => void queueFromList(e)}
                disabled={queueingId === e.id}
                className="grid h-5 w-5 place-items-center rounded text-success hover:bg-success/10"
              >
                <Play className="h-3 w-3" />
              </button>
              <button
                title="Open"
                onClick={() => navigate({ name: "experiment-detail", id: e.id })}
                className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-secondary"
              >
                <ArrowUpRight className="h-3 w-3" />
              </button>
              <button
                title="Duplicate"
                onClick={() => {
                  writeExperimentDraft(e)
                  navigate({ name: "experiment-new" })
                }}
                className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-secondary"
              >
                <Copy className="h-3 w-3" />
              </button>
            </div>
          </div>
        ))}
      </div>
      ) : null}
    </div>
  )
}

export function NewExperimentPage() {
  const { navigate } = useRouter()
  const workspace = useWorkspacePreferences()
  const [name, setName] = useState("")
  const [description, setDescription] = useState("")
  const [benchmark, setBenchmark] = useState<string | null>(null)
  const [agents, setAgents] = useState<string[]>([])
  const [mcps, setMcps] = useState<string[]>([])
  const [seeds, setSeeds] = useState("7,11,42")
  const [region, setRegion] = useState("us-east-1")
  const [maxParallel, setMaxParallel] = useState(8)
  const [budget, setBudget] = useState(50)
  const [secretRefs, setSecretRefs] = useState<Record<string, string>>({})
  const [registry, setRegistry] = useState<RegistryItem[]>([])
  const [registryLoaded, setRegistryLoaded] = useState(false)
  const [registryError, setRegistryError] = useState<string | null>(null)
  const [queueNotice, setQueueNotice] = useState<string | null>(null)
  const [variantSweep, setVariantSweep] = useState<{ key: string; values: string }[]>([
    { key: "temperature", values: "0.0, 0.4, 0.8" },
  ])

  async function loadRegistryOptions() {
    setRegistryLoaded(false)
    setRegistryError(null)
    const { data, error } = await cloudApi
      .from("registry_items")
      .select("*")
      .order("name")
    if (error) {
      setRegistry([])
      setRegistryError(error.message)
      setRegistryLoaded(true)
      return
    }
    setRegistry((data ?? []) as RegistryItem[])
    setRegistryLoaded(true)
  }

  useEffect(() => {
    void loadRegistryOptions()
  }, [])

  useEffect(() => {
    const draft = readExperimentDraft()
    if (!draft) return
    setName(draft.name)
    setDescription(draft.description)
    setBenchmark(draft.benchmark)
    setAgents(draft.agents)
    setMcps(draft.mcps)
    setSeeds(draft.seeds)
    setRegion(draft.region)
    setMaxParallel(draft.maxParallel)
    setBudget(draft.budget)
    setVariantSweep(draft.variantSweep)
    localStorage.removeItem(EXPERIMENT_DRAFT_KEY)
  }, [])

  const queuePackages = useMemo(
    () => registry.filter((r) => r.kind === "benchmark" || r.kind === "experiment_package"),
    [registry],
  )
  const agentsList = registry.filter((r) => r.kind === "agent")
  const mcpsList = registry.filter((r) => r.kind === "mcp")
  const selectedPackage = queuePackages.find((item) => item.id === benchmark || item.name === benchmark)
  const secretRequirements = selectedPackage?.secret_requirements ?? []
  const seedsList = useMemo(() => parseSeeds(seeds), [seeds])
  const sweep = useMemo(() => normalizedSweep(variantSweep), [variantSweep])
  const variantCount = Math.max(1, agents.length || 1) * Math.max(1, sweep.count) * Math.max(1, seedsList.length || 1)
  const planPreview = useMemo(
    () => executionPlanPreview({
      agents,
      seeds: seedsList,
      sweep,
      maxParallel,
      variantCount,
      budget,
    }),
    [agents, budget, maxParallel, seedsList, sweep, variantCount],
  )
  const readiness = useMemo(
    () => experimentReadiness({
      name,
      packageItem: selectedPackage,
      selectedAgents: agents,
      selectedMcps: mcps,
      registry,
      seeds: seedsList,
      sweep,
      budget,
      registryLoaded,
      registryError,
      queuePackages,
      secretRequirements,
      secretRefs,
    }),
    [agents, budget, mcps, name, queuePackages, registry, registryError, registryLoaded, secretRefs, secretRequirements, seedsList, selectedPackage, sweep],
  )
  const canQueue = readiness.every((item) => item.ok)
  const blockingReadiness = readiness.find((item) => !item.ok)
  const planBlocked = !canQueue
  const planBlockReason = blockingReadiness?.detail ?? "Complete readiness checks before queueing."
  const command = useMemo(
    () => experimentCommand({
      name,
      packageItem: selectedPackage,
      agents,
      mcps,
      region,
      seeds: seedsList,
      maxParallel,
      budget,
      sweep,
      secretRequirements,
    }),
    [agents, budget, maxParallel, mcps, name, region, secretRequirements, seedsList, selectedPackage, sweep],
  )

  useEffect(() => {
    if (benchmark || queuePackages.length !== 1) return
    setBenchmark(queuePackages[0].id)
  }, [benchmark, queuePackages])

  useEffect(() => {
    setSecretRefs((current) => {
      const next: Record<string, string> = {}
      for (const requirement of secretRequirements) {
        next[requirement.id] = current[requirement.id] ?? ""
      }
      return next
    })
  }, [secretRequirements])

  function toggle(list: string[], v: string) {
    return list.includes(v) ? list.filter((x) => x !== v) : [...list, v]
  }

  function setSeedList(next: number[]) {
    const seen = new Set<number>()
    const clean = next.filter((seed) => {
      if (!Number.isFinite(seed) || seen.has(seed)) return false
      seen.add(seed)
      return true
    })
    setSeeds(clean.join(","))
  }

  async function submit() {
    if (!canQueue || !selectedPackage) {
      setQueueNotice(readiness.find((item) => !item.ok)?.detail ?? "Complete the required fields before queueing.")
      return
    }
    const { data, error } = await cloudApi
      .from("experiments")
      .insert({
        name,
        description,
        tags: [],
        owner: workspace.slug,
        config: {
          benchmark: selectedPackage.id,
          package_digest: selectedPackage.id,
          secret_refs: trimSecretRefs(secretRefs),
          agents,
          mcps,
          seeds: seedsList,
          region,
          maxParallel,
          budgetUsd: budget,
          sweep: Object.fromEntries(sweep.dimensions.map((dimension) => [dimension.key, dimension.values])),
        },
      })
      .select("*")
      .maybeSingle()
    if (data?.id) navigate({ name: "run-detail", id: data.id })
    else setQueueNotice(error?.message ?? "Unable to queue this experiment from the current API response.")
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title="New experiment"
        subtitle="Compose a reproducible eval."
        rightSlot={
          <Button
            variant="outline"
            size="sm"
            className="h-7 text-[12px]"
            onClick={() => navigate({ name: "experiments" })}
          >
            <XCircle className="h-3 w-3" /> Cancel
          </Button>
        }
        primaryAction={
          <Button
            size="sm"
            className={cn(
              "h-7 gap-1",
              canQueue
                ? "bg-brand text-brand-foreground hover:bg-brand/90"
                : "border border-border bg-card text-muted-foreground",
            )}
            onClick={submit}
            disabled={!canQueue}
            title={canQueue ? "Queue this experiment" : readiness.find((item) => !item.ok)?.detail}
          >
            <Play className="h-3 w-3" /> Save and queue
          </Button>
        }
      />

      {registryError ? (
        <ConnectionIssue
          title="Registry options request failed"
          detail={registryError}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadRegistryOptions()}
          compact
        />
      ) : null}

      <div className="grid grid-cols-1 items-start gap-px bg-border lg:grid-cols-[minmax(0,1fr)_360px]">
        <div className="flex flex-col gap-px bg-border">
          <Section title="Identity">
            <Field label="Name" hint="Lowercase, kebab, descriptive">
              <Input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="sonnet-vs-gpt5-swe-bench"
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Description">
              <Textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What are you measuring and why?"
                className="min-h-[64px] resize-none text-[12px]"
              />
            </Field>
          </Section>

          <Section title="Execution package" hint="Accepted package to queue">
            <RadioGrid
              options={queuePackages.map((item) => registryGridOption(item, "package"))}
              value={selectedPackage?.id ?? benchmark ?? ""}
              onChange={setBenchmark}
              searchPlaceholder="Search packages by target, owner, tag"
              empty={
                registryError
                  ? {
                    title: "Package request failed",
                    detail: registryError,
                    action: "Settings",
                    onAction: () => navigate({ name: "settings" }),
                  }
                  : registryLoaded
                  ? {
                    title: "No queueable packages",
                    detail: "Accepted experiment packages and benchmarks will appear here.",
                    action: "Open registry",
                    onAction: () => navigate({ name: "registry" }),
                  }
                  : {
                    title: "Loading packages",
                    detail: "Fetching accepted packages from the registry.",
                  }
              }
            />
          </Section>

          <Section title="Secrets" hint="Provider refs, never plaintext">
            <SecretRefEditor
              requirements={secretRequirements}
              values={secretRefs}
              onChange={(id, value) => setSecretRefs((current) => ({ ...current, [id]: value }))}
            />
          </Section>

          <Section title="Agents" hint="Pick one or many for head-to-head">
            <CheckGrid
              options={agentsList.map((item) => registryGridOption(item, "name"))}
              values={agents}
              onChange={(v) => setAgents(toggle(agents, v))}
              searchPlaceholder="Search agents by name, owner, tag"
              empty={{
                title: registryError ? "Registry request failed" : registryLoaded ? "No agents registered" : "Loading agents",
                detail: registryError ?? (registryLoaded
                  ? "Optional; package defaults can run."
                  : "Fetching reusable agents from the registry."),
                action: registryError ? "Settings" : undefined,
                onAction: registryError ? () => navigate({ name: "settings" }) : undefined,
              }}
            />
          </Section>

          <Section title="MCP servers" hint="Tools available to agents">
            <CheckGrid
              options={mcpsList.map((item) => registryGridOption(item, "name"))}
              values={mcps}
              onChange={(v) => setMcps(toggle(mcps, v))}
              searchPlaceholder="Search MCP servers by name, owner, tag"
              empty={{
                title: registryError ? "Registry request failed" : registryLoaded ? "No MCP servers registered" : "Loading MCP servers",
                detail: registryError ?? (registryLoaded
                  ? "Optional tool servers."
                  : "Fetching tool servers from the registry."),
                action: registryError ? "Settings" : undefined,
                onAction: registryError ? () => navigate({ name: "settings" }) : undefined,
              }}
            />
          </Section>

          <Section title="Variant sweep" hint="Cartesian over keys">
            <div className="flex flex-col gap-1.5">
              {variantSweep.map((s, i) => (
                <div key={i} className="grid grid-cols-[minmax(90px,120px)_minmax(0,1fr)_24px] items-center gap-2 md:grid-cols-[140px_1fr_24px]">
                  <Input
                    value={s.key}
                    onChange={(e) =>
                      setVariantSweep((vs) =>
                        vs.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)),
                      )
                    }
                    placeholder="parameter"
                    className="h-7 font-mono text-[12px]"
                  />
                  <Input
                    value={s.values}
                    onChange={(e) =>
                      setVariantSweep((vs) =>
                        vs.map((x, j) => (j === i ? { ...x, values: e.target.value } : x)),
                      )
                    }
                    placeholder="0.0, 0.4, 0.8"
                    className="h-7 font-mono text-[12px]"
                  />
                  <button
                    onClick={() =>
                      setVariantSweep((vs) => vs.filter((_, j) => j !== i))
                    }
                    className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-secondary"
                  >
                    <XCircle className="h-3 w-3" />
                  </button>
                </div>
              ))}
              <Button
                variant="outline"
                size="sm"
                className="h-7 self-start text-[12px]"
                onClick={() =>
                  setVariantSweep((vs) => [...vs, { key: "", values: "" }])
                }
              >
                <Plus className="h-3 w-3" /> Add dimension
              </Button>
            </div>
          </Section>
        </div>

        <aside className="flex h-full flex-col gap-px bg-background self-stretch">
          <Section title="Compute">
            <Field label="Region">
              <SegmentedControl
                value={region}
                onValueChange={setRegion}
                options={[
                  { value: "us-east-1", label: "use1" },
                  { value: "us-west-2", label: "usw2" },
                  { value: "eu-west-1", label: "euw1" },
                  { value: "ap-southeast-1", label: "apse1" },
                ]}
              />
            </Field>
            <Field label="Max parallel">
              <Input
                type="number"
                value={maxParallel}
                onChange={(e) => setMaxParallel(Number(e.target.value))}
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Budget cap (USD)">
              <Input
                type="number"
                value={budget}
                onChange={(e) => setBudget(Number(e.target.value))}
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Seeds">
              <SeedPlanner
                seeds={seedsList}
                onSeedsChange={setSeedList}
              />
            </Field>
          </Section>

          <Section title="Summary">
            <PackageIntelligence
              packageItem={selectedPackage}
              registryLoaded={registryLoaded}
              agents={agents}
              mcps={mcps}
              seeds={seedsList}
              sweep={sweep}
            />
            <ReadinessList items={readiness} />
            <LaunchShape
              variantCount={variantCount}
              maxParallel={maxParallel}
              waves={planPreview.waves}
              budgetPerRun={planPreview.budgetPerRun}
              blocked={planBlocked}
              blockedDetail={planBlockReason}
            />
            <ExecutionPlan rows={planPreview.rows} waves={planPreview.waves} budgetPerRun={planPreview.budgetPerRun} blocked={planBlocked} blockedDetail={planBlockReason} />
            <Summary
              label="Variants"
              value={planBlocked ? "blocked" : `${variantCount} runs`}
            />
            <Summary
              label="Budget"
              value={`$${budget.toFixed(2)} cap`}
            />
            <Summary label="Region" value={region} />
            <Summary label="Owner" value={workspace.slug} />
            {queueNotice ? (
              <div className="rounded border border-warning/30 bg-warning/10 px-2 py-1.5 text-[11px] text-warning">
                {queueNotice}
              </div>
            ) : null}
          </Section>

          <Section title="Reproducibility">
            <div className={cn("rounded border p-2 font-mono text-[10.5px] text-muted-foreground", planBlocked ? "border-dashed border-border bg-card" : "border-border bg-muted/30")}>
              <div className="flex items-center justify-between">
                <span>CLI command</span>
                <button
                  onClick={() => {
                    if (planBlocked) {
                      setQueueNotice(planBlockReason)
                      return
                    }
                    copyToClipboard(command)
                    setQueueNotice("Command copied to clipboard.")
                  }}
                  className={cn(
                    "grid h-5 w-5 place-items-center rounded",
                    planBlocked
                      ? "cursor-not-allowed text-muted-foreground/50"
                      : "text-muted-foreground hover:bg-secondary hover:text-foreground",
                  )}
                  title={planBlocked ? planBlockReason : "Copy command"}
                  aria-disabled={planBlocked}
                >
                  <Copy className="h-3 w-3" />
                </button>
              </div>
              {planBlocked ? (
                <div className="mt-1 whitespace-pre-wrap leading-tight">
                  Command available when the readiness checks pass.
                </div>
              ) : (
                <pre className="mt-1 whitespace-pre-wrap leading-tight">{command}</pre>
              )}
            </div>
          </Section>
        </aside>
      </div>
    </div>
  )
}

function Section({
  title,
  hint,
  children,
}: {
  title: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <section className="bg-background">
      <header className="flex h-8 items-center justify-between border-b border-border px-3">
        <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
          {title}
        </div>
        {hint ? (
          <span className="hidden max-w-[45%] truncate text-[11px] text-muted-foreground sm:block">{hint}</span>
        ) : null}
      </header>
      <div className="flex flex-col gap-2 p-3">{children}</div>
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
    <div className="grid grid-cols-1 gap-1.5 md:grid-cols-[minmax(96px,140px)_minmax(0,1fr)] md:items-start md:gap-3">
      <Label className="pt-1.5 text-[12px] font-medium">
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

function SecretRefEditor({
  requirements,
  values,
  onChange,
}: {
  requirements: SecretRequirement[]
  values: Record<string, string>
  onChange: (id: string, value: string) => void
}) {
  if (requirements.length === 0) {
    return (
      <div className="rounded-md border border-dashed border-border bg-card px-2 py-2 text-[11.5px] text-muted-foreground">
        This package does not declare runtime secrets.
      </div>
    )
  }

  return (
    <div className="overflow-hidden rounded-md border border-border bg-border">
      {requirements.map((requirement) => {
        const value = values[requirement.id] ?? ""
        const hasValue = value.trim().length > 0
        const valid = !hasValue || secretRefLooksLikeProviderRef(value)
        return (
          <div key={requirement.id} className="grid gap-1 bg-card p-2 md:grid-cols-[minmax(128px,180px)_minmax(0,1fr)] md:items-start md:gap-2">
            <div className="min-w-0">
              <div className="truncate font-mono text-[12px] text-foreground" title={requirement.id}>
                {requirement.id}
              </div>
              <div className="mt-0.5 truncate text-[10.5px] text-muted-foreground" title={requirement.target || "runtime secret"}>
                {requirement.target || "runtime secret"}
              </div>
              {requirement.required_for_variants.length ? (
                <div className="mt-1 truncate text-[10px] text-muted-foreground" title={requirement.required_for_variants.join(", ")}>
                  {requirement.required_for_variants.join(", ")}
                </div>
              ) : null}
            </div>
            <div className="min-w-0">
              <Input
                value={value}
                onChange={(event) => onChange(requirement.id, event.target.value)}
                placeholder="gcp-secret-manager://projects/.../secrets/.../versions/latest"
                className={cn(
                  "h-8 font-mono text-[12px]",
                  valid ? "" : "border-destructive/60 focus-visible:ring-destructive/20",
                )}
              />
              <div className={cn("mt-1 text-[10.5px]", valid ? "text-muted-foreground" : "text-destructive")}>
                {hasValue
                  ? valid
                    ? "Linked by provider reference."
                    : "Use a supported provider reference."
                  : "Required before queueing."}
              </div>
            </div>
          </div>
        )
      })}
    </div>
  )
}

function Summary({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between text-[12px]">
      <span className="text-muted-foreground">{label}</span>
      <span className="font-mono">{value}</span>
    </div>
  )
}

function ExperimentInventoryStat({
  label,
  value,
  detail,
}: {
  label: string
  value: string
  detail: string
}) {
  return (
    <div className="min-w-0 bg-background p-2.5">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="mt-0.5 truncate font-mono text-[16px] font-medium">{value}</div>
      <div className="truncate text-[10.5px] text-muted-foreground">{detail}</div>
    </div>
  )
}

function ExperimentsSkeleton() {
  return (
    <div className="overflow-x-auto border-b border-border bg-background p-3">
      {[0, 1, 2, 3, 4].map((row) => (
        <div key={row} className="mb-2 grid min-w-[720px] grid-cols-[1.5fr_1fr_120px_80px_80px] gap-3">
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

function ExperimentsEmptyState({
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
    <div className="flex min-h-[280px] flex-col items-center justify-center gap-2 border-b border-border bg-background px-4 py-12 text-center">
      <Beaker className="h-5 w-5 text-muted-foreground" />
      <div className="text-[14px] font-medium">{title}</div>
      <div className="max-w-[calc(100vw-7rem)] text-[12px] text-muted-foreground sm:max-w-sm">{detail}</div>
      <Button variant="outline" size="sm" className="mt-2 h-7 text-[12px]" onClick={onAction}>
        {action}
      </Button>
    </div>
  )
}

type EmptyGridState = {
  title: string
  detail: string
  action?: string
  onAction?: () => void
}

function GridEmptyState({ empty }: { empty?: EmptyGridState }) {
  return (
    <div className="flex min-w-0 flex-col items-center justify-center gap-1 overflow-hidden rounded-md border border-border bg-card px-3 py-8 text-center">
      <Beaker className="h-4 w-4 text-muted-foreground" />
      <div className="text-[12.5px] font-medium">{empty?.title ?? "No options available"}</div>
      <div className="max-w-[220px] text-balance px-2 text-[11px] text-muted-foreground sm:max-w-md">
        {empty?.detail ?? "Registry-backed options will appear here when available."}
      </div>
      {empty?.action && empty.onAction ? (
        <Button variant="outline" size="sm" className="mt-1 h-7 text-[12px]" onClick={empty.onAction}>
          {empty.action}
        </Button>
      ) : null}
    </div>
  )
}

function ReadinessList({ items }: { items: { label: string; detail: string; ok: boolean }[] }) {
  return (
    <div className="grid gap-1 border-b border-border pb-2">
      {items.map((item) => (
        <div key={item.label} className="flex items-start gap-2 text-[11.5px]">
          {item.ok ? (
            <CheckCircle2 className="mt-0.5 h-3 w-3 text-success" />
          ) : (
            <XCircle className="mt-0.5 h-3 w-3 text-muted-foreground" />
          )}
          <div className="min-w-0">
            <div className={cn("font-medium", item.ok ? "text-foreground" : "text-muted-foreground")}>{item.label}</div>
            <div className="truncate text-[10.5px] text-muted-foreground">{item.detail}</div>
          </div>
        </div>
      ))}
    </div>
  )
}

function PackageIntelligence({
  packageItem,
  registryLoaded,
  agents,
  mcps,
  seeds,
  sweep,
}: {
  packageItem?: RegistryItem
  registryLoaded: boolean
  agents: string[]
  mcps: string[]
  seeds: number[]
  sweep: ReturnType<typeof normalizedSweep>
}) {
  const explicitAgents = agents.length > 0
  const explicitTools = mcps.length > 0
  const hasSweep = sweep.dimensions.length > 0
  const packageReady = packageItem?.status === "ready"
  return (
    <div className="overflow-hidden rounded-md border border-border bg-card">
      <div className="border-b border-border p-2">
        <div className="flex min-w-0 items-center justify-between gap-2">
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Selected package</div>
            <div className="mt-0.5 truncate font-mono text-[12px] text-foreground">
              {packageItem ? formatReadableLabel(packageItem.name) : registryLoaded ? "choose a package" : "loading registry"}
            </div>
          </div>
          {packageItem ? <StatusPill status={packageItem.status} withDot={false} /> : null}
        </div>
        {packageItem?.description ? (
          <div className="mt-1 line-clamp-2 text-[10.5px] leading-snug text-muted-foreground">
            {formatReadableLabel(packageItem.description)}
          </div>
        ) : null}
      </div>
      <div className="grid grid-cols-2 gap-px bg-border">
        <PackageFact
          label="Kind"
          value={packageItem ? <KindBadge kind={packageItem.kind} /> : <span className="text-muted-foreground">none</span>}
        />
        <PackageFact
          label="Version"
          value={packageItem ? formatReadableToken(packageItem.version) : "—"}
          mono
        />
        <PackageFact
          label="Owner"
          value={packageItem?.owner || "—"}
          mono
        />
        <PackageFact
          label="Pushed"
          value={packageItem ? formatRelative(packageItem.created_at) : "—"}
          mono
        />
      </div>
      <div className="grid gap-px border-t border-border bg-border">
        <LaunchAssumption
          ok={packageReady}
          label="Package"
          detail={
            packageReady
              ? "Ready registry artifact selected."
              : packageItem
                ? `${formatReadableLabel(packageItem.name)} is ${packageItem.status}; choose a ready package.`
                : "Select an accepted package before queueing."
          }
        />
        <LaunchAssumption
          ok={explicitAgents}
          label="Agents"
          detail={explicitAgents ? `${agents.length} explicit agent${agents.length === 1 ? "" : "s"}` : "Using package default agent contract."}
        />
        <LaunchAssumption
          ok={explicitTools}
          label="Tools"
          detail={explicitTools ? `${mcps.length} MCP server${mcps.length === 1 ? "" : "s"} attached` : "No extra tool servers attached."}
        />
        <LaunchAssumption
          ok={seeds.length > 0 && hasSweep}
          label="Reproducibility"
          detail={`${seeds.length || 0} seed${seeds.length === 1 ? "" : "s"} · ${hasSweep ? `${sweep.count} sweep cells` : "no sweep"}`}
        />
      </div>
    </div>
  )
}

function PackageFact({
  label,
  value,
  mono = false,
}: {
  label: string
  value: React.ReactNode
  mono?: boolean
}) {
  return (
    <div className="min-w-0 bg-card px-2 py-1.5">
      <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className={cn("mt-0.5 truncate text-[11.5px]", mono ? "font-mono" : "")}>{value}</div>
    </div>
  )
}

function LaunchAssumption({ ok, label, detail }: { ok: boolean; label: string; detail: string }) {
  return (
    <div className="flex min-w-0 items-start gap-2 bg-card px-2 py-1.5">
      {ok ? (
        <CheckCircle2 className="mt-0.5 h-3 w-3 shrink-0 text-success" />
      ) : (
        <XCircle className="mt-0.5 h-3 w-3 shrink-0 text-muted-foreground" />
      )}
      <div className="min-w-0">
        <div className="text-[11px] font-medium text-foreground">{label}</div>
        <div className="truncate text-[10.5px] text-muted-foreground" title={detail}>{detail}</div>
      </div>
    </div>
  )
}

function SeedPlanner({
  seeds,
  onSeedsChange,
}: {
  seeds: number[]
  onSeedsChange: (value: number[]) => void
}) {
  const [draft, setDraft] = useState("")
  const presetKey = seeds.join(",")

  function addDraft() {
    const next = Number(draft.trim())
    if (!Number.isFinite(next)) return
    onSeedsChange([...seeds, next])
    setDraft("")
  }

  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap gap-1">
        {SEED_PRESETS.map((preset) => {
          const active = preset.values.join(",") === presetKey
          return (
            <button
              key={preset.label}
              type="button"
              onClick={() => onSeedsChange(preset.values)}
              className={cn(
                "h-7 rounded-md border px-2 font-mono text-[11px] transition-colors",
                active
                  ? "border-brand bg-brand text-brand-foreground"
                  : "border-border bg-card text-muted-foreground hover:bg-accent/50 hover:text-foreground",
              )}
            >
              {preset.label}
            </button>
          )
        })}
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-1 rounded-md border border-border bg-card p-1">
        {seeds.length ? (
          seeds.map((seed) => (
            <button
              key={seed}
              type="button"
              onClick={() => onSeedsChange(seeds.filter((value) => value !== seed))}
              className="inline-flex h-6 items-center gap-1 rounded bg-muted px-2 font-mono text-[11px] text-foreground hover:bg-secondary"
              title="Remove seed"
            >
              {seed}
              <XCircle className="h-3 w-3 text-muted-foreground" />
            </button>
          ))
        ) : (
          <span className="px-1.5 text-[11px] text-muted-foreground">none</span>
        )}
        <div className="flex w-full items-center gap-1 sm:ml-auto sm:w-auto sm:min-w-[120px]">
          <Input
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault()
                addDraft()
              }
            }}
            placeholder="seed"
            className="h-6 border-0 bg-transparent px-1 font-mono text-[11px] shadow-none focus-visible:ring-0"
          />
          <button
            type="button"
            onClick={addDraft}
            className="grid h-6 w-6 shrink-0 place-items-center rounded text-muted-foreground hover:bg-secondary hover:text-foreground"
            title="Add seed"
          >
            <Plus className="h-3 w-3" />
          </button>
        </div>
      </div>
    </div>
  )
}

function LaunchShape({
  variantCount,
  maxParallel,
  waves,
  budgetPerRun,
  blocked = false,
  blockedDetail = "Complete readiness checks.",
}: {
  variantCount: number
  maxParallel: number
  waves: number
  budgetPerRun: number
  blocked?: boolean
  blockedDetail?: string
}) {
  const concurrency = Math.min(Math.max(1, maxParallel || 1), Math.max(1, variantCount))
  const utilization = variantCount ? Math.min(100, Math.round((concurrency / Math.max(1, maxParallel || 1)) * 100)) : 0
  const budgetTone = budgetPerRun >= 5 ? "text-success" : budgetPerRun >= 1 ? "text-warning" : "text-destructive"
  return (
    <div className="grid grid-cols-3 gap-px overflow-hidden rounded-md border border-border bg-border">
      <LaunchCell label="waves" value={blocked ? "-" : `${waves}`} detail={blocked ? "not ready" : `${concurrency} parallel`} tone="text-foreground" />
      <LaunchCell label="budget/run" value={blocked ? "-" : `$${budgetPerRun.toFixed(2)}`} detail={blocked ? "blocked" : budgetPerRun >= 1 ? "usable" : "tight"} tone={blocked ? "text-muted-foreground" : budgetTone} />
      <LaunchCell label="fill" value={blocked ? "-" : `${utilization}%`} detail={blocked ? blockedDetail : `${variantCount} runs`} tone={blocked ? "text-muted-foreground" : "text-info"} />
    </div>
  )
}

function LaunchCell({
  label,
  value,
  detail,
  tone,
}: {
  label: string
  value: string
  detail: string
  tone: string
}) {
  return (
    <div className="min-w-0 bg-card px-2 py-1.5">
      <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className={cn("truncate font-mono text-[13px] font-medium", tone)}>{value}</div>
      <div className="truncate text-[10px] text-muted-foreground">{detail}</div>
    </div>
  )
}

function ExecutionPlan({
  rows,
  waves,
  budgetPerRun,
  blocked = false,
  blockedDetail = "Complete readiness checks before this becomes an executable plan.",
}: {
  rows: { label: string; value: number; detail: string }[]
  waves: number
  budgetPerRun: number
  blocked?: boolean
  blockedDetail?: string
}) {
  const max = Math.max(1, ...rows.map((row) => row.value))
  return (
    <div className="border-b border-border pb-2">
      <div className="mb-1.5 flex items-center justify-between text-[11.5px]">
        <span className="font-medium">Execution plan</span>
        <span className="font-mono text-muted-foreground">{blocked ? "blocked" : `${waves} wave${waves === 1 ? "" : "s"}`}</span>
      </div>
      {blocked ? (
        <div className="rounded border border-dashed border-border bg-card px-2 py-2 text-[11px] text-muted-foreground">
          {blockedDetail}
        </div>
      ) : (
        <div className="grid gap-1">
          {rows.map((row) => (
            <div key={row.label} className="grid grid-cols-[72px_minmax(0,1fr)_44px] items-center gap-2 text-[10.5px]">
              <span className="truncate text-muted-foreground">{row.label}</span>
              <span className="h-1.5 overflow-hidden rounded bg-muted">
                <span
                  className="block h-full rounded bg-brand"
                  style={{ width: `${Math.max(6, (row.value / max) * 100)}%` }}
                />
              </span>
              <span className="text-right font-mono">{row.detail}</span>
            </div>
          ))}
        </div>
      )}
      <div className="mt-1.5 flex items-center justify-between text-[10.5px] text-muted-foreground">
        <span>Budget per run</span>
        <span className="font-mono">{blocked ? "-" : `$${budgetPerRun.toFixed(2)}`}</span>
      </div>
    </div>
  )
}

type RegistryGridTone = "success" | "warning" | "danger" | "muted"
type GridOption = {
  value: string
  label: string
  hint?: string
  badge?: string
  tone?: RegistryGridTone
  meta?: string
  facts?: { label: string; value: string; tone?: RegistryGridTone }[]
}

function CheckGrid({
  options,
  values,
  onChange,
  empty,
  searchPlaceholder = "Search resources",
}: {
  options: GridOption[]
  values: string[]
  onChange: (v: string) => void
  empty?: EmptyGridState
  searchPlaceholder?: string
}) {
  const [query, setQuery] = useState("")
  if (options.length === 0) return <GridEmptyState empty={empty} />
  const filtered = filterGridOptions(options, query)
  const visible = sortGridOptions(filtered, (option) => values.includes(option.value))
  const hasQuery = query.trim().length > 0
  const summary = gridSelectorSummary(options, values.length)

  return (
    <div className="overflow-hidden rounded-md border border-border bg-border">
      <GridSelectorToolbar
        query={query}
        onQueryChange={setQuery}
        placeholder={searchPlaceholder}
        countLabel={summary.countLabel}
        action={values.length ? "Clear selected" : undefined}
        onAction={values.length ? () => values.forEach(onChange) : undefined}
      />
      <GridSelectorBrief summary={summary} />
      {visible.length > 0 ? (
        <div className="grid max-h-[360px] grid-cols-1 gap-px overflow-y-auto scrollbar-thin md:grid-cols-2">
          {visible.map((o) => {
            const checked = values.includes(o.value)
            return (
              <button
                key={o.value}
                onClick={() => onChange(o.value)}
                className={cn(
                  "flex min-w-0 items-start gap-2 bg-card p-2 text-left transition-colors",
                  checked ? "ring-1 ring-inset ring-brand" : "hover:bg-accent/50",
                )}
              >
                <span
                  className={cn(
                    "mt-0.5 grid h-3.5 w-3.5 shrink-0 place-items-center rounded border",
                    checked ? "border-brand bg-brand" : registryGridBorder(o.tone),
                  )}
                >
                  {checked ? (
                    <span className="h-1.5 w-1.5 rounded-sm bg-brand-foreground" />
                  ) : null}
                </span>
                <RegistryOptionBody option={o} />
              </button>
            )
          })}
        </div>
      ) : (
        <GridEmptyState
          empty={{
            title: hasQuery ? "No matches" : "No options available",
            detail: hasQuery ? `No registry options match "${query}".` : "Registry-backed options will appear here when available.",
          }}
        />
      )}
    </div>
  )
}

function RadioGrid({
  options,
  value,
  onChange,
  empty,
  searchPlaceholder = "Search packages",
}: {
  options: GridOption[]
  value: string
  onChange: (v: string) => void
  empty?: EmptyGridState
  searchPlaceholder?: string
}) {
  const [query, setQuery] = useState("")
  if (options.length === 0) return <GridEmptyState empty={empty} />
  const filtered = filterGridOptions(options, query)
  const visible = sortGridOptions(filtered, (option) => option.value === value)
  const selected = options.find((option) => option.value === value)
  const hasQuery = query.trim().length > 0
  const summary = gridSelectorSummary(options, selected ? 1 : 0, selected)

  return (
    <div className="overflow-hidden rounded-md border border-border bg-border">
      <GridSelectorToolbar
        query={query}
        onQueryChange={setQuery}
        placeholder={searchPlaceholder}
        countLabel={summary.countLabel}
      />
      <GridSelectorBrief summary={summary} />
      {visible.length > 0 ? (
        <div className="grid max-h-[420px] grid-cols-1 gap-px overflow-y-auto scrollbar-thin md:grid-cols-2">
          {visible.map((o) => {
            const checked = value === o.value
            return (
              <button
                key={o.value}
                onClick={() => onChange(o.value)}
                className={cn(
                  "flex min-w-0 items-start gap-2 bg-card p-2 text-left transition-colors",
                  checked ? "ring-1 ring-inset ring-brand" : "hover:bg-accent/50",
                )}
              >
                <span
                  className={cn(
                    "mt-0.5 grid h-3.5 w-3.5 shrink-0 place-items-center rounded-full border",
                    checked ? "border-brand" : registryGridBorder(o.tone),
                  )}
                >
                  {checked ? <span className="h-1.5 w-1.5 rounded-full bg-brand" /> : null}
                </span>
                <RegistryOptionBody option={o} />
              </button>
            )
          })}
        </div>
      ) : (
        <GridEmptyState
          empty={{
            title: hasQuery ? "No matching package" : "No options available",
            detail: hasQuery ? `No queueable package matches "${query}".` : "Accepted packages will appear here when available.",
          }}
        />
      )}
    </div>
  )
}

function RegistryOptionBody({ option }: { option: GridOption }) {
  return (
    <span className="min-w-0 flex-1">
      <span className="flex min-w-0 items-center gap-1.5">
        <span className={cn("h-1.5 w-1.5 shrink-0 rounded-full", registryGridDot(option.tone))} />
        <span className="truncate font-mono text-[12px]">{option.label}</span>
        {option.badge ? (
          <span className="shrink-0 rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground">
            {option.badge}
          </span>
        ) : null}
      </span>
      {option.hint || option.meta ? (
        <span className="mt-0.5 block max-w-[calc(100vw-8rem)] truncate text-[10.5px] leading-tight text-muted-foreground sm:max-w-none">
          {formatReadableLabel(option.hint || option.meta)}
        </span>
      ) : null}
      {option.facts?.length ? (
        <span className="mt-1 flex min-w-0 flex-wrap gap-1">
          {option.facts.slice(0, 4).map((fact) => (
            <span
              key={`${fact.label}-${fact.value}`}
              className={cn(
                "inline-flex min-w-0 items-center gap-1 rounded border px-1.5 py-0.5 text-[10px]",
                registryGridFactClass(fact.tone),
              )}
            >
              <span className="shrink-0 text-muted-foreground">{fact.label}</span>
              <span className="max-w-[7rem] truncate font-mono">{fact.value}</span>
            </span>
          ))}
        </span>
      ) : null}
    </span>
  )
}

function GridSelectorToolbar({
  query,
  onQueryChange,
  placeholder,
  countLabel,
  action,
  onAction,
}: {
  query: string
  onQueryChange: (value: string) => void
  placeholder: string
  countLabel: string
  action?: string
  onAction?: () => void
}) {
  return (
    <div className="grid gap-1 border-b border-border bg-background p-1.5 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
      <div className="relative min-w-0">
        <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          onChange={(event) => onQueryChange(event.target.value)}
          placeholder={placeholder}
          className="h-7 w-full border-border bg-card pl-7 text-[12px]"
        />
      </div>
      <div className="flex min-w-0 items-center justify-between gap-2 px-1 sm:justify-end">
        <span className="min-w-0 truncate text-[10.5px] text-muted-foreground">{countLabel}</span>
        {action && onAction ? (
          <button
            type="button"
            onClick={onAction}
            className="shrink-0 rounded px-1.5 py-0.5 text-[10.5px] text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            {action}
          </button>
        ) : null}
      </div>
    </div>
  )
}

type GridSelectorSummary = {
  countLabel: string
  verdict: string
  detail: string
  tone: RegistryGridTone
  facts: { label: string; value: string; tone?: RegistryGridTone }[]
}

function GridSelectorBrief({ summary }: { summary: GridSelectorSummary }) {
  return (
    <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)]">
      <div className="min-w-0 bg-background px-2.5 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className={cn("h-2 w-2 shrink-0 rounded-full", registryGridDot(summary.tone))} />
          <span className="min-w-0">
            <span className={cn("block truncate text-[12px] font-medium", registryGridText(summary.tone))}>
              {summary.verdict}
            </span>
            <span className="block truncate text-[10.5px] text-muted-foreground" title={summary.detail}>
              {summary.detail}
            </span>
          </span>
        </div>
      </div>
      <div className="grid grid-cols-4 gap-px bg-border">
        {summary.facts.map((fact) => (
          <div key={fact.label} className="min-w-0 bg-background px-2 py-1.5">
            <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
            <div className={cn("truncate font-mono text-[11.5px]", fact.tone ? registryGridText(fact.tone) : "text-foreground")}>{fact.value}</div>
          </div>
        ))}
      </div>
    </div>
  )
}

function gridSelectorSummary(options: GridOption[], selectedCount: number, selected?: GridOption): GridSelectorSummary {
  const ready = options.filter((option) => option.tone === "success").length
  const building = options.filter((option) => option.tone === "warning").length
  const failed = options.filter((option) => option.tone === "danger").length
  const total = options.length
  const countLabel = selected
    ? `${formatReadableLabel(selected.label)} selected`
    : selectedCount
      ? `${selectedCount}/${total} selected`
      : `${ready}/${total} ready`

  if (selected) {
    const selectedReady = selected.tone === "success"
    return {
      countLabel,
      verdict: selectedReady ? "Ready artifact selected" : "Selected artifact needs attention",
      detail: selectedReady
        ? `${formatReadableLabel(selected.label)} can be queued now.`
        : `${formatReadableLabel(selected.label)} is not ready; pick a ready package before queueing.`,
      tone: selected.tone ?? "muted",
      facts: gridSelectorFacts(ready, building, failed, total),
    }
  }

  if (selectedCount > 0) {
    return {
      countLabel,
      verdict: `${selectedCount} resource${selectedCount === 1 ? "" : "s"} selected`,
      detail: ready ? "Ready resources are ranked first; unavailable resources stay visible for context." : "Selected resources will be checked against package readiness.",
      tone: failed ? "warning" : "success",
      facts: gridSelectorFacts(ready, building, failed, total),
    }
  }

  if (ready > 0) {
    return {
      countLabel,
      verdict: `${ready} ready resource${ready === 1 ? "" : "s"}`,
      detail: "Ready resources are ranked first. Search by owner or tag to narrow the list.",
      tone: "success",
      facts: gridSelectorFacts(ready, building, failed, total),
    }
  }

  if (building > 0) {
    return {
      countLabel,
      verdict: "Resources still building",
      detail: "These resources are visible but should not be selected for a run that needs to queue now.",
      tone: "warning",
      facts: gridSelectorFacts(ready, building, failed, total),
    }
  }

  return {
    countLabel,
    verdict: failed ? "Only failed resources loaded" : "No ready resources loaded",
    detail: failed ? "Review registry failures before composing this run." : "Registry-backed resources will appear here after they are accepted.",
    tone: failed ? "danger" : "muted",
    facts: gridSelectorFacts(ready, building, failed, total),
  }
}

function gridSelectorFacts(ready: number, building: number, failed: number, total: number): GridSelectorSummary["facts"] {
  return [
    { label: "Ready", value: `${ready}`, tone: ready ? "success" : undefined },
    { label: "Building", value: `${building}`, tone: building ? "warning" : undefined },
    { label: "Failed", value: `${failed}`, tone: failed ? "danger" : undefined },
    { label: "Total", value: `${total}` },
  ]
}

function sortGridOptions(options: GridOption[], selected: (option: GridOption) => boolean) {
  return [...options].sort((a, b) =>
    Number(selected(b)) - Number(selected(a)) ||
    registryToneRank(b.tone) - registryToneRank(a.tone) ||
    gridOptionAge(b) - gridOptionAge(a) ||
    a.label.localeCompare(b.label),
  )
}

function registryToneRank(tone?: RegistryGridTone) {
  if (tone === "success") return 3
  if (tone === "warning") return 2
  if (tone === "danger") return 1
  return 0
}

function gridOptionAge(option: GridOption) {
  const age = option.facts?.find((fact) => fact.label === "age")?.value ?? ""
  const match = age.match(/^(\d+)([smhd])/)
  if (!match) return 0
  const value = Number(match[1])
  const unit = match[2]
  const minutes = unit === "s" ? value / 60 : unit === "m" ? value : unit === "h" ? value * 60 : value * 1440
  return -minutes
}

function filterGridOptions(
  options: GridOption[],
  query: string,
) {
  const needle = query.trim().toLowerCase()
  if (!needle) return options
  return options.filter((option) =>
    `${option.label} ${option.value} ${option.hint ?? ""} ${option.badge ?? ""} ${option.meta ?? ""} ${option.facts?.map((fact) => `${fact.label} ${fact.value}`).join(" ") ?? ""}`.toLowerCase().includes(needle),
  )
}

export function ExperimentDetailPage() {
  const { route, navigate } = useRouter()
  const id = route.name === "experiment-detail" ? route.id : ""
  const [exp, setExp] = useState<Experiment | null>(null)
  const [expLoaded, setExpLoaded] = useState(false)
  const [detailError, setDetailError] = useState<string | null>(null)
  const [runs, setRuns] = useState<Run[]>([])
  const [metrics, setMetrics] = useState<RunMetric[]>([])
  const [queueing, setQueueing] = useState(false)
  const [detailNotice, setDetailNotice] = useState<string | null>(null)
  const [showRawConfig, setShowRawConfig] = useState(false)

  async function loadExperimentDetail() {
    if (!id) return
    setExp(null)
    setExpLoaded(false)
    setDetailError(null)
    setDetailNotice(null)
    setShowRawConfig(false)
    setRuns([])
    setMetrics([])
    const { data: expData, error: expError } = await cloudApi
      .from("experiments")
      .select("*")
      .eq("id", id)
      .maybeSingle()
    if (expError) {
      setExp(null)
      setDetailError(expError.message)
      setExpLoaded(true)
      return
    }
    const experiment = (expData as Experiment) ?? null
    setExp(experiment)
    setExpLoaded(true)
    if (!experiment) return
    const { data: runsData, error: runsError } = await cloudApi
      .from("runs")
      .select("*")
      .order("created_at", { ascending: false })
      .limit(100)
    const linkedRuns = ((runsData ?? []) as Run[])
      .filter((run) => experimentMatchesRun(experiment, run))
      .slice(0, 40)
    setRuns(linkedRuns)
    if (runsError) {
      setDetailNotice(`Linked run evidence unavailable: ${runsError.message}`)
      return
    }
    const { data: metricData, error: metricError } = await cloudApi
      .from("run_metrics")
      .select("*")
      .order("recorded_at", { ascending: true })
    const linkedRunIds = new Set(linkedRuns.map((run) => run.id))
    setMetrics(((metricData ?? []) as RunMetric[]).filter((metric) => linkedRunIds.has(metric.run_id)))
    if (metricError) setDetailNotice(`Metric evidence unavailable: ${metricError.message}`)
  }

  useEffect(() => {
    void loadExperimentDetail()
  }, [id])

  if (!exp && !expLoaded) {
    return (
      <div className="p-6 text-[12px] text-muted-foreground">Loading experiment…</div>
    )
  }

  if (detailError) {
    return (
      <ConnectionIssue
        title="Experiment request failed"
        detail={detailError}
        onSettings={() => navigate({ name: "settings" })}
        onRetry={() => void loadExperimentDetail()}
      />
    )
  }

  if (!exp) {
    return (
      <MissingExperiment
        title="Experiment not found"
        detail="This experiment id is not present in the current package registry response."
        onAction={() => navigate({ name: "experiments" })}
      />
    )
  }

  const summary = experimentSummary(runs)
  const history = experimentRunHistory(runs)
  const statusMix = experimentStatusMix(runs)
  const evidence = experimentMetricEvidence(runs, metrics)
  const configSummary = experimentConfigSummary(exp)
  const decisionBrief = experimentDecisionBrief(summary, runs, evidence, configSummary, detailNotice)

  async function queueRun() {
    if (!exp) return
    setQueueing(true)
    const { data, error } = await cloudApi
      .from("experiments")
      .insert({
        name: exp.name,
        description: exp.description,
        tags: exp.tags,
        owner: exp.owner,
        config: exp.config,
      })
      .select("*")
      .maybeSingle()
    setQueueing(false)
    if (data?.id) navigate({ name: "run-detail", id: data.id })
    else setDetailNotice(error?.message ?? "Unable to queue this experiment from its current config.")
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title={formatReadableLabel(exp.name)}
        subtitle={exp.description}
        rightSlot={
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-[12px]"
            onClick={() => navigate({ name: "experiments" })}
          >
            <ArrowUpRight className="h-3 w-3 -rotate-180" /> All experiments
          </Button>
        }
        primaryAction={
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={() => void queueRun()}
            disabled={queueing}
          >
            <Play className="h-3 w-3" /> {queueing ? "Queueing" : "Queue run"}
          </Button>
        }
      />

      {detailNotice ? (
        <div className="flex items-center justify-between border-b border-border bg-warning/10 px-3 py-2 text-[12px]">
          <span className="text-foreground">{detailNotice}</span>
          <button
            className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:text-foreground"
            onClick={() => setDetailNotice(null)}
            aria-label="Dismiss experiment notice"
          >
            <XCircle className="h-3 w-3" />
          </button>
        </div>
      ) : null}

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-4">
        <KV label="Owner" value={exp.owner} mono />
        <KV
          label="Tags"
          value={
            <div className="flex flex-wrap items-center gap-1">
              {exp.tags.map((t) => (
                <span
                  key={t}
                  className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
                >
                  {formatReadableToken(t)}
                </span>
              ))}
            </div>
          }
        />
        <KV label="Created" value={formatRelative(exp.created_at)} mono />
        <KV label="Linked runs" value={`${runs.length}`} mono />
      </div>

      <div className="grid grid-cols-2 gap-px border-b border-border bg-border md:grid-cols-5">
        <ExperimentFact label="Runs" value={`${runs.length}`} detail={`${summary.completed} complete`} />
        <ExperimentFact label="Success" value={`${(summary.successRate * 100).toFixed(1)}%`} detail={`${summary.failed} failed`} />
        <ExperimentFact label="Avg duration" value={formatDuration(summary.avgDurationMs)} detail={`${summary.timedRuns} timed`} />
        <ExperimentFact label="Metric rows" value={`${metrics.length}`} detail={`${evidence.coveragePct}% coverage`} />
        <ExperimentFact label="Latest" value={summary.latestLabel} detail={summary.latestStatus} className="col-span-2 md:col-span-1" />
      </div>

      <ExperimentDecisionBriefView
        brief={decisionBrief}
        disabled={decisionBrief.actionType === "queue" && queueing}
        onAction={() => {
          if (decisionBrief.actionType === "queue") void queueRun()
          if (decisionBrief.actionType === "compare") navigate({ name: "compare" })
          if (decisionBrief.actionType === "config") setShowRawConfig(true)
        }}
      />

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(0,1fr)_minmax(0,420px)]">
        <div className="bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <div className="flex items-center gap-2">
              <h3 className="text-[12px] font-semibold">Run history</h3>
              <span className="text-[11px] text-muted-foreground">
                {runs.length} runs
              </span>
            </div>
            <Button
              variant="outline"
              size="sm"
              className="h-6 gap-1 text-[11px]"
              onClick={() => navigate({ name: "compare" })}
              disabled={runs.length === 0}
            >
              <GitBranch className="h-3 w-3" /> Compare
            </Button>
          </header>

          {runs.length > 0 ? (
            <div className="grid grid-cols-1 gap-px border-b border-border bg-border xl:grid-cols-[minmax(0,1fr)_220px]">
              <div className="bg-background p-3">
                <ChartContainer config={experimentRunCfg} className="h-44 w-full">
                  <LineChart data={history}>
                    <CartesianGrid vertical={false} stroke="var(--border)" strokeDasharray="2 4" />
                    <XAxis
                      dataKey="label"
                      tickLine={false}
                      axisLine={false}
                      tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                    />
                    <YAxis yAxisId="duration" hide />
                    <YAxis yAxisId="cost" orientation="right" hide />
                    <Tooltip contentStyle={experimentTooltipStyle} />
                    <Line
                      yAxisId="duration"
                      type="monotone"
                      dataKey="duration"
                      name="duration (s)"
                      stroke="var(--color-duration)"
                      strokeWidth={1.6}
                      dot={false}
                      isAnimationActive={false}
                    />
                    <Line
                      yAxisId="cost"
                      type="monotone"
                      dataKey="cost"
                      name="cost"
                      stroke="var(--color-cost)"
                      strokeWidth={1.6}
                      dot={false}
                      isAnimationActive={false}
                    />
                  </LineChart>
                </ChartContainer>
              </div>
              <div className="bg-background p-3">
                <ChartContainer config={experimentStatusCfg} className="h-44 w-full">
                  <BarChart data={statusMix} layout="vertical" margin={{ left: 4, right: 12, top: 8, bottom: 4 }}>
                    <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                    <XAxis type="number" hide />
                    <YAxis
                      type="category"
                      dataKey="status"
                      width={72}
                      tickLine={false}
                      axisLine={false}
                      tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                    />
                    <Tooltip contentStyle={experimentTooltipStyle} />
                    <Bar dataKey="count" radius={[0, 2, 2, 0]} isAnimationActive={false}>
                      {statusMix.map((row) => (
                        <Cell key={row.status} fill={statusChartColor(row.status)} />
                      ))}
                    </Bar>
                  </BarChart>
                </ChartContainer>
              </div>
            </div>
          ) : (
            <div className="border-b border-border px-3 py-10 text-center text-[12px] text-muted-foreground">
              Run duration, cost, and status trends will appear after this experiment queues executions.
            </div>
          )}

          <div
            className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
            style={{
              gridTemplateColumns:
                "minmax(120px,1fr) 100px 90px 70px 90px 24px",
            }}
          >
            <span>Variant</span>
            <span>Status</span>
            <span>Duration</span>
            <span>Cost</span>
            <span>Started</span>
            <span></span>
          </div>
          {runs.map((r) => (
            <button
              key={r.id}
              onClick={() => navigate({ name: "run-detail", id: r.id })}
              className="grid w-full items-center gap-2 border-b border-border px-3 py-1.5 text-left hover:bg-accent/40"
              style={{
                gridTemplateColumns:
                  "minmax(120px,1fr) 100px 90px 70px 90px 24px",
              }}
            >
              <span className="truncate font-mono text-[11.5px]">{formatReadableToken(r.variant)}</span>
              <span className="text-[11.5px]">
                <span
                  className={cn(
                    "inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10.5px] font-medium",
                    statusToneBg(r.status),
                    statusToneText(r.status),
                  )}
                >
                  <span
                    className={cn(
                      "h-1.5 w-1.5 rounded-full",
                      statusToneDot(r.status),
                    )}
                  />
                  {r.status}
                </span>
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
              <ArrowUpRight className="h-3 w-3 text-muted-foreground" />
            </button>
          ))}
        </div>

        <div className="bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <h3 className="text-[12px] font-semibold">Metric evidence</h3>
            <span className="font-mono text-[11px] text-muted-foreground">
              {evidence.runsWithMetrics}/{runs.length || 0} runs
            </span>
          </header>
          <div className="grid grid-cols-3 gap-px border-b border-border bg-border">
            <MiniEvidence label="Coverage" value={`${evidence.coveragePct}%`} />
            <MiniEvidence label="Signals" value={`${evidence.signals}`} />
            <MiniEvidence label="Latest" value={evidence.latestLabel} />
          </div>
          {evidence.rows.length > 0 ? (
            <ChartContainer config={experimentMetricCfg} className="h-36 w-full border-b border-border">
              <BarChart data={evidence.rows} layout="vertical" margin={{ left: 4, right: 12, top: 8, bottom: 4 }}>
                <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis type="number" hide />
                <YAxis
                  type="category"
                  dataKey="name"
                  width={92}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip
                  contentStyle={experimentTooltipStyle}
                  formatter={(value, _name, item) => [`${value} rows across ${item.payload.runs} runs`, "coverage"]}
                />
                <Bar dataKey="rows" fill="var(--color-rows)" radius={[0, 2, 2, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          ) : (
            <div className="border-b border-border px-3 py-8 text-center text-[12px] text-muted-foreground">
              Metric evidence will populate here when linked runs emit observations.
            </div>
          )}

          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <div className="flex min-w-0 items-center gap-2">
              <h3 className="text-[12px] font-semibold">Config</h3>
              <span className="truncate text-[11px] text-muted-foreground">{configSummary.packageRef}</span>
            </div>
            <div className="flex items-center gap-1">
              <Button
                variant="ghost"
                size="sm"
                className="h-6 gap-1 text-[11px]"
                onClick={() => setShowRawConfig((value) => !value)}
              >
                {showRawConfig ? "Hide raw" : "Show raw"}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="h-6 gap-1 text-[11px]"
                onClick={() => {
                  copyToClipboard(JSON.stringify(exp.config, null, 2))
                  setDetailNotice("Experiment config copied to clipboard.")
                }}
              >
                <Copy className="h-3 w-3" /> Copy
              </Button>
            </div>
          </header>
          <div className="grid grid-cols-2 gap-px border-b border-border bg-border">
            {configSummary.facts.map((fact) => (
              <ConfigFact key={fact.label} label={fact.label} value={fact.value} />
            ))}
          </div>
          <div className="border-b border-border p-3">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Manifest</div>
            <div className="mt-1 text-[12px] leading-relaxed text-foreground">{configSummary.description}</div>
            <div className="mt-2 flex flex-wrap gap-1">
              {configSummary.tags.map((tag) => (
                <span key={tag} className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground">
                  {formatReadableToken(tag)}
                </span>
              ))}
            </div>
          </div>
          {showRawConfig ? (
            <pre className="max-h-80 overflow-auto p-3 font-mono text-[11px] leading-relaxed text-muted-foreground">
              {JSON.stringify(exp.config, null, 2)}
            </pre>
          ) : null}
        </div>
      </div>

      <div className="border-b border-border bg-background">
        <header className="flex h-9 items-center justify-between border-b border-border px-3">
          <h3 className="text-[12px] font-semibold">Notes</h3>
          <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
            <Tag className="h-3 w-3" /> {exp.owner}
          </span>
        </header>
        <div className="p-3 text-[12px] text-muted-foreground leading-relaxed">
          {runs.length > 0
            ? `${runs.length} run${runs.length === 1 ? "" : "s"} recorded for this experiment. Use Compare to inspect metric movement across variants.`
            : "No runs are linked yet. Queue a run to start collecting metrics, traces, and reproducibility evidence."}
        </div>
      </div>
    </div>
  )
}

type ExperimentDraft = {
  name: string
  description: string
  benchmark: string | null
  agents: string[]
  mcps: string[]
  seeds: string
  region: string
  maxParallel: number
  budget: number
  variantSweep: { key: string; values: string }[]
}

function writeExperimentDraft(experiment: Experiment) {
  const config = experiment.config
  const sweep = isRecord(config.sweep) ? config.sweep : {}
  localStorage.setItem(
    EXPERIMENT_DRAFT_KEY,
    JSON.stringify({
      name: `${experiment.name}-copy`,
      description: experiment.description,
      benchmark: stringValue(config.benchmark),
      agents: stringArray(config.agents),
      mcps: stringArray(config.mcps),
      seeds: numberArray(config.seeds).join(","),
      region: stringValue(config.region) || "us-east-1",
      maxParallel: numberValue(config.maxParallel, 8),
      budget: numberValue(config.budgetUsd, 50),
      variantSweep: Object.entries(sweep).map(([key, value]) => ({
        key,
        values: stringArray(value).join(", "),
      })),
    } satisfies ExperimentDraft),
  )
}

function readExperimentDraft(): ExperimentDraft | null {
  try {
    const raw = localStorage.getItem(EXPERIMENT_DRAFT_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Partial<ExperimentDraft>
    return {
      name: parsed.name || "",
      description: parsed.description || "",
      benchmark: parsed.benchmark || null,
      agents: Array.isArray(parsed.agents) ? parsed.agents : [],
      mcps: Array.isArray(parsed.mcps) ? parsed.mcps : [],
      seeds: parsed.seeds || "7,11,42",
      region: parsed.region || "us-east-1",
      maxParallel: parsed.maxParallel || 8,
      budget: parsed.budget || 50,
      variantSweep: parsed.variantSweep?.length
        ? parsed.variantSweep
        : [{ key: "temperature", values: "0.0, 0.4, 0.8" }],
    }
  } catch {
    return null
  }
}

function copyToClipboard(value: string) {
  if (!navigator.clipboard) return
  void navigator.clipboard.writeText(value).catch(() => undefined)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value))
}

function stringValue(value: unknown) {
  return typeof value === "string" ? value : ""
}

function stringArray(value: unknown) {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : []
}

function trimSecretRefs(values: Record<string, string>) {
  return Object.fromEntries(
    Object.entries(values)
      .map(([key, value]) => [key, value.trim()])
      .filter(([, value]) => value.length > 0),
  )
}

function secretRefLooksLikeProviderRef(value: string) {
  const trimmed = value.trim()
  return /^(gcp-secret-manager|aws-secrets-manager):\/\/\S+$/i.test(trimmed)
}

function numberArray(value: unknown) {
  return Array.isArray(value)
    ? value
        .map((item) => Number(item))
        .filter((item) => Number.isFinite(item))
    : []
}

function numberValue(value: unknown, fallback: number) {
  const number = Number(value)
  return Number.isFinite(number) && number > 0 ? number : fallback
}

function parseSeeds(value: string) {
  return value
    .split(",")
    .map((item) => Number(item.trim()))
    .filter((item) => Number.isFinite(item))
}

function normalizedSweep(rows: { key: string; values: string }[]) {
  const dimensions = rows
    .map((row) => ({
      key: row.key.trim(),
      values: row.values
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean),
    }))
    .filter((row) => row.key && row.values.length > 0)
  return {
    dimensions,
    count: dimensions.reduce((acc, dimension) => acc * Math.max(1, dimension.values.length), 1),
  }
}

function experimentReadiness({
  name,
  packageItem,
  selectedAgents,
  selectedMcps,
  registry,
  seeds,
  sweep,
  budget,
  registryLoaded,
  registryError,
  queuePackages,
  secretRequirements,
  secretRefs,
}: {
  name: string
  packageItem?: RegistryItem
  selectedAgents: string[]
  selectedMcps: string[]
  registry: RegistryItem[]
  seeds: number[]
  sweep: ReturnType<typeof normalizedSweep>
  budget: number
  registryLoaded: boolean
  registryError: string | null
  queuePackages: RegistryItem[]
  secretRequirements: SecretRequirement[]
  secretRefs: Record<string, string>
}) {
  const selectedToolNames = new Set([...selectedAgents, ...selectedMcps])
  const blockedTools = registry.filter((item) =>
    selectedToolNames.has(item.name) && item.status !== "ready",
  )
  const invalidSecretIds = secretRequirements
    .filter((requirement) => !secretRefLooksLikeProviderRef(secretRefs[requirement.id] ?? ""))
    .map((requirement) => requirement.id)
  return [
    {
      label: "Name",
      ok: Boolean(name.trim()),
      detail: name.trim() ? name.trim() : "Give the run a readable label.",
    },
    {
      label: "Package",
      ok: Boolean(packageItem && packageItem.status === "ready"),
      detail: packageItem
        ? packageItem.status === "ready"
          ? formatReadableLabel(packageItem.name)
          : `${formatReadableLabel(packageItem.name)} is ${packageItem.status}`
        : registryError
          ? "Connect the registry API before selecting a package."
        : registryLoaded && queuePackages.length === 0
          ? "No accepted queueable packages are loaded."
        : "Choose the accepted package that defines the eval.",
    },
    {
      label: "Secrets",
      ok: invalidSecretIds.length === 0,
      detail: secretRequirements.length === 0
        ? "No runtime secrets declared."
        : invalidSecretIds.length === 0
          ? `${secretRequirements.length} provider ref${secretRequirements.length === 1 ? "" : "s"} linked`
          : `Link ${invalidSecretIds.slice(0, 3).join(", ")}${invalidSecretIds.length > 3 ? ` and ${invalidSecretIds.length - 3} more` : ""}`,
    },
    {
      label: "Tools",
      ok: blockedTools.length === 0,
      detail: blockedTools.length
        ? `${blockedTools.length} selected tool${blockedTools.length === 1 ? "" : "s"} not ready`
        : selectedToolNames.size
          ? `${selectedToolNames.size} selected agent/MCP resource${selectedToolNames.size === 1 ? "" : "s"}`
          : "Package defaults can run without extra tools.",
    },
    {
      label: "Seeds",
      ok: seeds.length > 0,
      detail: seeds.length ? `${seeds.length} deterministic seed${seeds.length === 1 ? "" : "s"}` : "Add at least one numeric seed.",
    },
    {
      label: "Budget",
      ok: Number.isFinite(budget) && budget > 0,
      detail: budget > 0 ? `$${budget.toFixed(2)} cap` : "Set a positive budget cap.",
    },
    {
      label: "Sweep",
      ok: sweep.dimensions.length > 0,
      detail: sweep.dimensions.length ? `${sweep.dimensions.length} dimension${sweep.dimensions.length === 1 ? "" : "s"}` : "Add at least one variant dimension.",
    },
  ]
}

function experimentCommand({
  name,
  packageItem,
  agents,
  mcps,
  region,
  seeds,
  maxParallel,
  budget,
  sweep,
  secretRequirements,
}: {
  name: string
  packageItem?: RegistryItem
  agents: string[]
  mcps: string[]
  region: string
  seeds: number[]
  maxParallel: number
  budget: number
  sweep: ReturnType<typeof normalizedSweep>
  secretRequirements: SecretRequirement[]
}) {
  const lines = [
    "bucephalus run create",
    `  --name ${shellValue(name || "<name>")}`,
    `  --package ${shellValue(packageItem?.id || "<accepted-package>")}`,
    `  --region ${shellValue(region)}`,
    `  --max-parallel ${Math.max(1, maxParallel || 1)}`,
    `  --budget-usd ${Math.max(0, budget || 0).toFixed(2)}`,
  ]
  if (seeds.length) lines.push(`  --seeds ${shellValue(seeds.join(","))}`)
  agents.forEach((agent) => lines.push(`  --agent ${shellValue(agent)}`))
  mcps.forEach((mcp) => lines.push(`  --mcp ${shellValue(mcp)}`))
  if (secretRequirements.length) lines.push("  --secret-ref-file secrets.yaml")
  sweep.dimensions.forEach((dimension) => {
    lines.push(`  --sweep ${shellValue(`${dimension.key}=${dimension.values.join(",")}`)}`)
  })
  return lines.join(" \\\n")
}

function executionPlanPreview({
  agents,
  seeds,
  sweep,
  maxParallel,
  variantCount,
  budget,
}: {
  agents: string[]
  seeds: number[]
  sweep: ReturnType<typeof normalizedSweep>
  maxParallel: number
  variantCount: number
  budget: number
}) {
  const agentCount = Math.max(1, agents.length || 1)
  const sweepCount = Math.max(1, sweep.count)
  const seedCount = Math.max(1, seeds.length || 1)
  return {
    rows: [
      { label: "Agents", value: agentCount, detail: `${agentCount}x` },
      { label: "Sweep", value: sweepCount, detail: `${sweepCount}x` },
      { label: "Seeds", value: seedCount, detail: `${seedCount}x` },
      { label: "Runs", value: variantCount, detail: `${variantCount}` },
    ],
    waves: Math.max(1, Math.ceil(variantCount / Math.max(1, maxParallel || 1))),
    budgetPerRun: variantCount ? Math.max(0, budget || 0) / variantCount : 0,
  }
}

function packageBadge(item: RegistryItem) {
  const kind = item.kind === "experiment_package" ? "package" : item.kind
  return `${kind} ${formatReadableToken(item.version)}`
}

function registryGridOption(item: RegistryItem, valueMode: "package" | "name"): GridOption {
  const value = valueMode === "package" ? item.id : item.name
  const kind = item.kind === "experiment_package" ? "package" : item.kind
  const tone = registryGridTone(item.status)
  const statusFact = { label: "status", value: item.status, tone }
  return {
    value,
    label: formatReadableLabel(item.name),
    hint: item.description,
    badge: valueMode === "package" ? packageBadge(item) : formatReadableToken(item.version),
    tone,
    meta: `${kind} ${item.owner} ${item.status} ${item.tags.join(" ")}`,
    facts: [
      statusFact,
      { label: "owner", value: formatReadableToken(item.owner) },
      { label: "age", value: formatRelative(item.created_at) },
      { label: "size", value: formatBytes(item.size_bytes) },
    ],
  }
}

function registryGridTone(status: RegistryItem["status"]): RegistryGridTone {
  if (status === "ready") return "success"
  if (status === "building") return "warning"
  if (status === "failed") return "danger"
  return "muted"
}

function registryGridBorder(tone?: RegistryGridTone) {
  if (tone === "success") return "border-success/40"
  if (tone === "warning") return "border-warning/50"
  if (tone === "danger") return "border-destructive/50"
  return "border-border"
}

function registryGridDot(tone?: RegistryGridTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  return "bg-muted-foreground"
}

function registryGridText(tone?: RegistryGridTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  return "text-foreground"
}

function registryGridFactClass(tone?: RegistryGridTone) {
  if (tone === "success") return "border-success/20 bg-success/10 text-success"
  if (tone === "warning") return "border-warning/25 bg-warning/10 text-warning"
  if (tone === "danger") return "border-destructive/25 bg-destructive/10 text-destructive"
  return "border-border bg-muted/30 text-foreground"
}

function shellValue(value: string) {
  return /\s/.test(value) ? JSON.stringify(value) : value
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

function experimentInventorySummary(items: Experiment[]) {
  const latest = [...items].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]
  return {
    total: items.length,
    queueable: items.filter((item) => Boolean(stringValue(item.config.benchmark) || stringValue(item.config.package_digest))).length,
    owners: new Set(items.map((item) => item.owner).filter(Boolean)).size,
    tags: new Set(items.flatMap((item) => item.tags).filter(Boolean)).size,
    latestLabel: latest ? formatRelative(latest.created_at) : "none",
    latestOwner: latest?.owner ?? "no packages",
  }
}

function experimentPackageMix(items: Experiment[]) {
  const counts = new Map<string, number>()
  items.forEach((item) => {
    const status =
      item.tags.find((tag) => ["accepted", "ready", "building", "failed", "pending"].includes(tag)) ??
      item.tags[0] ??
      "untagged"
    counts.set(status, (counts.get(status) ?? 0) + 1)
  })
  if (counts.size === 0) return [{ status: "none", packages: 0 }]
  return Array.from(counts.entries())
    .map(([status, packages]) => ({ status, packages }))
    .sort((a, b) => b.packages - a.packages || a.status.localeCompare(b.status))
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
    <div className="bg-background p-3">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div
        className={cn(
          "mt-0.5 text-[12.5px]",
          mono ? "font-mono text-[12px]" : "",
        )}
      >
        {value}
      </div>
    </div>
  )
}

function ExperimentFact({
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
      <div className="mt-0.5 truncate font-mono text-[16px] font-medium">{value}</div>
      <div className="truncate text-[10.5px] text-muted-foreground">{detail}</div>
    </div>
  )
}

function ExperimentDecisionBriefView({
  brief,
  disabled,
  onAction,
}: {
  brief: ExperimentDecisionBrief
  disabled?: boolean
  onAction: () => void
}) {
  return (
    <section className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(280px,0.9fr)_minmax(0,1.5fr)]">
      <div className="min-w-0 bg-background p-3">
        <div className="flex items-center gap-2">
          <span className={cn("h-2 w-2 rounded-full", experimentDecisionDot(brief.tone))} />
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Experiment brief</div>
            <div className={cn("mt-0.5 truncate text-[14px] font-medium", experimentDecisionText(brief.tone))}>
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
          disabled={disabled}
        >
          {brief.action}
          {brief.actionType === "queue" ? <Play className="h-3 w-3" /> : <ArrowUpRight className="h-3 w-3" />}
        </Button>
      </div>
      <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
        {brief.facts.map((fact) => (
          <div key={fact.label} className="min-w-0 bg-background px-3 py-2.5">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
            <div
              className={cn("mt-0.5 truncate font-mono text-[12px]", fact.tone ? experimentDecisionText(fact.tone) : "")}
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

function MiniEvidence({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-background px-2 py-1.5">
      <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="truncate font-mono text-[12px] text-foreground">{value}</div>
    </div>
  )
}

function ConfigFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-background px-2 py-1.5">
      <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="truncate font-mono text-[12px] text-foreground" title={value}>{value}</div>
    </div>
  )
}

function MissingExperiment({
  title,
  detail,
  onAction,
}: {
  title: string
  detail: string
  onAction: () => void
}) {
  return (
    <div className="flex min-h-[360px] flex-col items-center justify-center gap-2 px-4 text-center">
      <XCircle className="h-5 w-5 text-muted-foreground" />
      <div className="text-[14px] font-medium">{title}</div>
      <div className="max-w-sm text-[12px] text-muted-foreground">{detail}</div>
      <Button variant="outline" size="sm" className="mt-2 h-7 text-[12px]" onClick={onAction}>
        All experiments
      </Button>
    </div>
  )
}

const experimentTooltipStyle = {
  background: "var(--popover)",
  border: "1px solid var(--border)",
  borderRadius: 6,
  fontSize: 11,
}

function experimentSummary(runs: Run[]) {
  const completed = runs.filter((run) => run.status === "succeeded")
  const failed = runs.filter((run) => run.status === "failed")
  const timed = runs.filter((run) => run.duration_ms > 0)
  const spend = runs.reduce((acc, run) => acc + Number(run.cost_usd), 0)
  const latest = [...runs].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]

  return {
    completed: completed.length,
    failed: failed.length,
    timedRuns: timed.length,
    successRate: runs.length ? completed.length / runs.length : 0,
    spend,
    avgCost: runs.length ? spend / runs.length : 0,
    avgDurationMs: timed.length
      ? timed.reduce((acc, run) => acc + run.duration_ms, 0) / timed.length
      : 0,
    latestLabel: latest ? formatReadableToken(latest.variant) : "none",
    latestStatus: latest ? `${latest.status} ${formatRelative(latest.created_at)}` : "no runs",
  }
}

function experimentDecisionBrief(
  summary: ReturnType<typeof experimentSummary>,
  runs: Run[],
  evidence: ReturnType<typeof experimentMetricEvidence>,
  configSummary: ReturnType<typeof experimentConfigSummary>,
  detailNotice: string | null,
): ExperimentDecisionBrief {
  const running = runs.filter((run) => run.status === "running").length
  const queued = runs.filter((run) => run.status === "queued").length
  const completedRuns = runs.filter((run) => run.status === "succeeded" || run.status === "failed").length
  const latest = [...runs].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]
  const failureRate = completedRuns ? summary.failed / completedRuns : 0
  const hasPackage = configSummary.packageRef !== "experiment package"
  const spend = summary.spend ? formatUsd(summary.spend) : "no spend"

  if (detailNotice) {
    return {
      tone: "warning",
      verdict: "Evidence partially unavailable",
      detail: "The experiment loaded, but linked run or metric evidence did not fully return. Review the config before queueing another execution.",
      action: "Review config",
      actionType: "config",
      facts: experimentBriefFacts("Notice", "present", "Coverage", `${evidence.coveragePct}%`, "Spend", spend, "Package", configSummary.packageRef, "warning"),
    }
  }

  if (runs.length === 0) {
    return {
      tone: hasPackage ? "info" : "warning",
      verdict: hasPackage ? "Ready for first run" : "Package reference missing",
      detail: hasPackage
        ? "This package has enough configuration to start collecting runtime evidence."
        : "This experiment does not expose a concrete package reference in the API payload, so queueing may fail.",
      action: hasPackage ? "Queue first run" : "Review config",
      actionType: hasPackage ? "queue" : "config",
      facts: experimentBriefFacts("Runs", "0", "Metrics", "0 rows", "Budget", configFactValue(configSummary, "Budget"), "Package", configSummary.packageRef, hasPackage ? "info" : "warning"),
    }
  }

  if (running || queued) {
    return {
      tone: "info",
      verdict: running ? "Execution in flight" : "Waiting for workers",
      detail:
        running > 0
          ? "A run is active. Let it finish before making comparison decisions unless trace evidence shows a failure."
          : "Runs are queued. The experiment is waiting for runtime evidence from workers.",
      action: "Open compare",
      actionType: "compare",
      facts: experimentBriefFacts("Active", `${running} run / ${queued} queued`, "Coverage", `${evidence.coveragePct}%`, "Spend", spend, "Latest", latest ? formatRelative(latest.created_at) : "none", "info"),
    }
  }

  if (failureRate >= 0.25) {
    return {
      tone: "danger",
      verdict: "Stabilize failures first",
      detail: "Failure rate is high enough that comparing quality metrics may hide runtime instability.",
      action: "Review config",
      actionType: "config",
      facts: experimentBriefFacts("Failure", `${Math.round(failureRate * 100)}%`, "Failed", `${summary.failed}`, "Coverage", `${evidence.coveragePct}%`, "Spend", spend, "danger"),
    }
  }

  if (evidence.coveragePct < 70 || metricsSignalTooThin(evidence)) {
    return {
      tone: "warning",
      verdict: "Metric coverage is thin",
      detail: "The linked runs exist, but not enough of them emitted metric observations to support a confident comparison.",
      action: "Queue run",
      actionType: "queue",
      facts: experimentBriefFacts("Coverage", `${evidence.coveragePct}%`, "Signals", `${evidence.signals}`, "Runs", `${runs.length}`, "Latest", evidence.latestLabel, "warning"),
    }
  }

  if (summary.successRate >= 0.9 && evidence.coveragePct >= 90) {
    return {
      tone: "success",
      verdict: "Ready to compare",
      detail: "Strong success and metric coverage. Ready for cohort comparison or promotion decisions.",
      action: "Open compare",
      actionType: "compare",
      facts: experimentBriefFacts("Success", `${Math.round(summary.successRate * 100)}%`, "Coverage", `${evidence.coveragePct}%`, "Spend", spend, "Latest", summary.latestLabel, "success"),
    }
  }

  return {
    tone: "warning",
    verdict: "Needs one more pass",
    detail: "The experiment has usable evidence, but success or coverage is not strong enough to treat it as final.",
    action: "Queue run",
    actionType: "queue",
    facts: experimentBriefFacts("Success", `${Math.round(summary.successRate * 100)}%`, "Coverage", `${evidence.coveragePct}%`, "Spend", spend, "Latest", summary.latestLabel, "warning"),
  }
}

function experimentBriefFacts(
  firstLabel: string,
  firstValue: string,
  secondLabel: string,
  secondValue: string,
  thirdLabel: string,
  thirdValue: string,
  fourthLabel: string,
  fourthValue: string,
  tone: ExperimentDecisionTone,
): ExperimentDecisionBrief["facts"] {
  return [
    { label: firstLabel, value: firstValue, tone },
    { label: secondLabel, value: secondValue, tone: tone === "danger" ? "danger" : undefined },
    { label: thirdLabel, value: thirdValue },
    { label: fourthLabel, value: fourthValue },
  ]
}

function metricsSignalTooThin(evidence: ReturnType<typeof experimentMetricEvidence>) {
  return evidence.signals === 0 || evidence.rows.every((row) => row.rows < 2)
}

function configFactValue(configSummary: ReturnType<typeof experimentConfigSummary>, label: string) {
  return configSummary.facts.find((fact) => fact.label === label)?.value ?? "not set"
}

function experimentRunHistory(runs: Run[]) {
  return [...runs]
    .sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at))
    .slice(-24)
    .map((run, index) => ({
      label: `#${index + 1}`,
      duration: Math.round(run.duration_ms / 1000),
      cost: Number(run.cost_usd),
      status: run.status,
      variant: run.variant,
    }))
}

function experimentStatusMix(runs: Run[]) {
  const counts = new Map<string, number>()
  runs.forEach((run) => counts.set(run.status, (counts.get(run.status) ?? 0) + 1))
  return (["queued", "running", "succeeded", "failed"] as const).map((status) => ({
    status,
    count: counts.get(status) ?? 0,
  }))
}

function experimentMatchesRun(experiment: Experiment, run: Run) {
  const config = isRecord(experiment.config) ? experiment.config : {}
  const packageDigest = stringValue(config.package_digest)
  const runExperimentName = formatReadableLabel(run.experiment_name)
  const experimentName = formatReadableLabel(experiment.name)
  return Boolean(
    run.experiment_id === experiment.id ||
      run.experiment_id === packageDigest ||
      run.experiment_name === experiment.name ||
      runExperimentName === experimentName,
  )
}

function experimentMetricEvidence(runs: Run[], metrics: RunMetric[]) {
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
  const rows = Array.from(byMetric.values())
    .sort((a, b) => b.runs.size - a.runs.size || b.rows - a.rows || b.latest - a.latest)
    .slice(0, 5)
    .map((metric) => ({
      name: formatReadableToken(metric.name),
      rows: metric.rows,
      runs: metric.runs.size,
      latest: metric.latest,
    }))
  const runsWithMetrics = new Set(metrics.map((metric) => metric.run_id).filter((runId) => runIds.has(runId))).size
  const latest = [...metrics]
    .filter((metric) => runIds.has(metric.run_id))
    .sort((a, b) => Date.parse(b.recorded_at) - Date.parse(a.recorded_at))[0]
  return {
    rows,
    runsWithMetrics,
    signals: byMetric.size,
    coveragePct: runs.length ? Math.round((runsWithMetrics / runs.length) * 100) : 0,
    latestLabel: latest ? formatReadableToken(latest.name) : "no rows",
  }
}

function experimentConfigSummary(experiment: Experiment) {
  const config = isRecord(experiment.config) ? experiment.config : {}
  const manifest = isRecord(config.manifest) ? config.manifest : {}
  const resolved = isRecord(config.resolved_experiment) ? config.resolved_experiment : {}
  const packageDigest = stringValue(config.package_digest)
  const target = stringValue(config.target) || stringValue(resolved.target) || "experiment package"
  const region = stringValue(config.region) || stringValue(resolved.region) || "cloud default"
  const budget = numberValue(config.budgetUsd, 0) || numberValue(resolved.budgetUsd, 0)
  const seeds = numberArray(config.seeds)
  const agents = stringArray(config.agents)
  const mcps = stringArray(config.mcps)
  const sweep = isRecord(config.sweep) ? config.sweep : {}
  const dimensions = Object.keys(sweep).length
  const manifestTags = stringArray(manifest.tags)
  const tags = uniqueStrings([...experiment.tags, ...manifestTags, target]).slice(0, 8)
  const packageRef = packageDigest ? `package ${formatShortId(packageDigest)}` : formatReadableLabel(target)

  return {
    packageRef,
    description:
      stringValue(manifest.description) ||
      stringValue(resolved.description) ||
      experiment.description ||
      "No package description is present in the current API payload.",
    tags: tags.length ? tags : ["untagged"],
    facts: [
      { label: "Package", value: packageRef },
      { label: "Target", value: formatReadableLabel(target) },
      { label: "Region", value: region },
      { label: "Budget", value: budget ? `$${budget.toFixed(2)}` : "not set" },
      { label: "Seeds", value: seeds.length ? `${seeds.length}` : "not set" },
      { label: "Sweep", value: dimensions ? `${dimensions} dim` : "none" },
      { label: "Agents", value: agents.length ? `${agents.length}` : "package default" },
      { label: "MCPs", value: mcps.length ? `${mcps.length}` : "none" },
    ],
  }
}

function statusToneBg(s: string) {
  return s === "succeeded"
    ? "bg-success/10"
    : s === "running"
      ? "bg-info/10"
      : s === "failed"
        ? "bg-destructive/10"
        : "bg-muted/40"
}
function statusToneText(s: string) {
  return s === "succeeded"
    ? "text-success"
    : s === "running"
      ? "text-info"
      : s === "failed"
        ? "text-destructive"
        : "text-muted-foreground"
}
function statusToneDot(s: string) {
  return s === "succeeded"
    ? "bg-success"
    : s === "running"
      ? "bg-info animate-pulse"
      : s === "failed"
        ? "bg-destructive"
        : "bg-muted-foreground"
}

function experimentDecisionDot(tone: ExperimentDecisionTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  if (tone === "info") return "bg-info animate-pulse"
  return "bg-muted-foreground"
}

function experimentDecisionText(tone: ExperimentDecisionTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  if (tone === "info") return "text-info"
  return "text-muted-foreground"
}

function statusChartColor(status: string) {
  if (status === "succeeded") return "var(--success)"
  if (status === "running") return "var(--info)"
  if (status === "failed") return "var(--destructive)"
  return "var(--muted-foreground)"
}

function uniqueStrings(values: string[]) {
  return Array.from(new Set(values.map((value) => value.trim()).filter(Boolean)))
}

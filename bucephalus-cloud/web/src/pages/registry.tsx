import { useEffect, useMemo, useState } from "react"
import {
  ArrowUpRight,
  Boxes,
  Copy,
  Download,
  Filter,
  Info,
  Package,
  Search,
  Server,
  CircleDot,
  BarChart3,
  CheckCircle2,
  Clock,
  X,
} from "lucide-react"
import {
  Bar,
  BarChart,
  CartesianGrid,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { FilterStrip } from "@/components/filter-strip"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { ConnectionIssue } from "@/components/connection-issue"
import { KindBadge } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import { cloudApi, type RegistryItem } from "@/lib/cloud-api"
import { formatBytes, formatReadableLabel, formatReadableToken, formatRelative, formatShortId } from "@/lib/format"
import { downloadCsv } from "@/lib/export"
import { cn } from "@/lib/utils"
import { useWorkspacePreferences } from "@/lib/workspace"

const KIND_TABS: {
  label: string
  kind: "agent" | "benchmark" | "mcp" | "experiment_package" | undefined
  icon: React.ComponentType<{ className?: string }>
}[] = [
  { label: "All", kind: undefined, icon: Boxes },
  { label: "Packages", kind: "experiment_package", icon: Package },
  { label: "Agents", kind: "agent", icon: CircleDot },
  { label: "Benchmarks", kind: "benchmark", icon: BarChart3 },
  { label: "MCP servers", kind: "mcp", icon: Server },
]

const statusByKindCfg: ChartConfig = {
  ready: { label: "Ready", color: "var(--success)" },
  building: { label: "Building", color: "var(--warning)" },
  failed: { label: "Failed", color: "var(--destructive)" },
}

const freshnessCfg: ChartConfig = {
  resources: { label: "Resources", color: "var(--chart-2)" },
}

type RegistryBriefTone = "success" | "warning" | "danger" | "info" | "muted"
type RegistryBriefAction = "status" | "packages" | "settings"
type RegistryBrief = {
  tone: RegistryBriefTone
  verdict: string
  detail: string
  action: string
  actionType: RegistryBriefAction
  status?: RegistryItem["status"]
  facts: { label: string; value: string; tone?: RegistryBriefTone }[]
}

export function RegistryPage() {
  const { route, navigate } = useRouter()
  const workspace = useWorkspacePreferences()
  const activeKind = route.name === "registry" ? route.kind : undefined
  const [items, setItems] = useState<RegistryItem[]>([])
  const [loaded, setLoaded] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [q, setQ] = useState("")
  const [ownerFilter, setOwnerFilter] = useState("all")
  const [tagFilter, setTagFilter] = useState("all")
  const [statusFilter, setStatusFilter] = useState("all")
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [actionNotice, setActionNotice] = useState<string | null>(null)

  async function loadRegistryItems() {
    setLoaded(false)
    setLoadError(null)
    const { data, error } = await cloudApi
      .from("registry_items")
      .select("*")
      .order("created_at", { ascending: false })
    setItems(error ? [] : ((data ?? []) as RegistryItem[]))
    setLoadError(error?.message ?? null)
    setLoaded(true)
  }

  useEffect(() => {
    void loadRegistryItems()
  }, [])

  const filtered = useMemo(() => {
    return registrySortItems(items.filter((i) => {
      if (activeKind && i.kind !== activeKind) return false
      if (ownerFilter !== "all" && i.owner !== ownerFilter) return false
      if (tagFilter !== "all" && !i.tags.includes(tagFilter)) return false
      if (statusFilter !== "all" && i.status !== statusFilter) return false
      if (q && !registrySearchText(i).includes(q.toLowerCase())) return false
      return true
    }))
  }, [items, activeKind, ownerFilter, q, statusFilter, tagFilter])

  const counts = useMemo(() => {
    const c: Record<string, number> = { all: items.length, agent: 0, benchmark: 0, mcp: 0 }
    items.forEach((i) => (c[i.kind] = (c[i.kind] ?? 0) + 1))
    return c
  }, [items])

  const allSelected = filtered.length > 0 && filtered.every((i) => selected.has(i.id))
  const owners = useMemo(
    () => filterOptions(items.map((i) => i.owner), "all owners"),
    [items],
  )
  const tags = useMemo(
    () => filterOptions(items.flatMap((i) => i.tags), "all tags"),
    [items],
  )
  const statuses = useMemo(() => statusFilterOptions(items), [items])
  const selectedItems = useMemo(
    () => items.filter((item) => selected.has(item.id)),
    [items, selected],
  )
  const selectedSummary = useMemo(() => registrySelectionSummary(selectedItems), [selectedItems])
  const inventory = useMemo(() => registryInventory(items), [items])
  const statusRows = useMemo(() => registryStatusByKind(items), [items])
  const freshnessRows = useMemo(() => registryFreshness(items), [items])
  const topOwners = useMemo(() => topRegistryValues(items.map((item) => item.owner), 5), [items])
  const topTags = useMemo(() => topRegistryValues(items.flatMap((item) => item.tags), 5), [items])
  const registryBrief = useMemo(() => registryDecisionBrief(items, inventory), [inventory, items])
  const unavailable = Boolean(loadError)

  function toggle(id: string) {
    setSelected((s) => {
      const next = new Set(s)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Registry"
        subtitle="Long-term resources you push, version, and reuse across experiments."
      />

      <div className="grid grid-cols-2 gap-px border-b border-border bg-border md:grid-cols-5">
        <InventoryStat
          icon={Boxes}
          label="Resources"
          value={!loaded ? "loading" : unavailable ? "-" : `${inventory.total}`}
          detail={unavailable ? "request failed" : `${inventory.ready} ready`}
        />
        <InventoryStat
          icon={CheckCircle2}
          label="Accepted"
          value={!loaded ? "loading" : unavailable ? "-" : `${inventory.ready}`}
          detail={unavailable ? "unavailable" : `${inventory.building} building`}
        />
        <InventoryStat
          icon={BarChart3}
          label="Kinds"
          value={!loaded ? "loading" : unavailable ? "-" : `${inventory.kinds}`}
          detail={unavailable ? "connect API" : `${inventory.owners} owners`}
        />
        <InventoryStat
          icon={Package}
          label="Bytes"
          value={!loaded ? "loading" : unavailable ? "-" : formatBytes(inventory.bytes)}
          detail={unavailable ? "request failed" : `${inventory.tagCount} tags`}
        />
        <InventoryStat
          icon={Clock}
          label="Latest push"
          value={!loaded ? "loading" : unavailable ? "-" : inventory.latestLabel}
          detail={unavailable ? "unavailable" : inventory.latestDetail}
          className="col-span-2 md:col-span-1"
        />
      </div>

      {loaded && !loadError ? (
        <RegistryBriefView
          brief={registryBrief}
          onAction={() => {
            if (registryBrief.actionType === "settings") navigate({ name: "settings" })
            if (registryBrief.actionType === "packages") {
              navigate({ name: "registry", kind: "experiment_package" })
              setStatusFilter("ready")
            }
            if (registryBrief.actionType === "status" && registryBrief.status) {
              setStatusFilter(registryBrief.status)
            }
          }}
        />
      ) : null}

      {loaded && !loadError ? (
        <div className="sticky top-11 z-10 flex flex-col gap-1.5 border-b border-border bg-background/95 px-3 py-1.5 backdrop-blur">
          <div className="flex flex-col gap-2 lg:flex-row lg:items-center lg:justify-between">
            <div className="flex max-w-full items-center overflow-x-auto scrollbar-thin">
              {KIND_TABS.map((t) => {
                const active = activeKind === t.kind
                const count =
                  t.kind === undefined ? counts.all : counts[t.kind] ?? 0
                const Icon = t.icon
                return (
                  <button
                    key={t.label}
                    onClick={() => navigate({ name: "registry", kind: t.kind })}
                    className={cn(
                      "flex items-center gap-1.5 rounded-md border border-transparent px-2 py-1 text-[12px]",
                      active
                        ? "border-border bg-secondary text-foreground"
                        : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    <Icon className="h-3 w-3" />
                    {t.label}
                    <span className="ml-0.5 rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground">
                      {count}
                    </span>
                  </button>
                )
              })}
            </div>
            <div className="relative min-w-0 lg:w-auto">
              <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={q}
                onChange={(e) => setQ(e.target.value)}
                placeholder="Filter by name, tag, owner"
                className="h-7 w-full pl-7 text-[12px] lg:w-72"
              />
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Filter className="h-3 w-3 text-muted-foreground" />
            <FilterStrip label="Status" value={statusFilter} options={statuses} onValueChange={setStatusFilter} max={4} />
            <FilterStrip label="Tag" value={tagFilter} options={tags} onValueChange={setTagFilter} max={6} />
            <FilterStrip label="Owner" value={ownerFilter} options={owners} onValueChange={setOwnerFilter} max={5} />
          </div>
        </div>
      ) : null}

      {actionNotice ? (
        <div className="flex items-center gap-2 border-b border-border bg-info/10 px-3 py-2 text-[12px] text-info">
          <Info className="h-3.5 w-3.5 shrink-0" />
          <span className="flex-1 text-foreground">{actionNotice}</span>
          <Button
            size="xs"
            variant="outline"
            className="h-6 text-[11px]"
            onClick={() => navigate({ name: "settings" })}
          >
            Settings
          </Button>
          <button
            className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:text-foreground"
            onClick={() => setActionNotice(null)}
            aria-label="Dismiss registry action notice"
          >
            <X className="h-3 w-3" />
          </button>
        </div>
      ) : null}

      {loaded && !loadError && items.length > 0 ? (
        <section className="grid grid-cols-1 gap-px border-b border-border bg-border xl:grid-cols-[minmax(0,1.1fr)_minmax(0,0.9fr)_320px]">
          <RegistryInsightPanel title="Status by kind" detail={`${inventory.queueable} queueable packages`}>
            <ChartContainer config={statusByKindCfg} className="h-40 w-full">
              <BarChart data={statusRows} layout="vertical" margin={{ left: 4, right: 12, top: 8, bottom: 4 }}>
                <CartesianGrid horizontal={false} stroke="var(--border)" strokeDasharray="2 4" />
                <XAxis type="number" hide allowDecimals={false} />
                <YAxis
                  type="category"
                  dataKey="kind"
                  width={92}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <Tooltip contentStyle={registryTooltipStyle} />
                <Bar dataKey="ready" stackId="status" fill="var(--color-ready)" isAnimationActive={false} />
                <Bar dataKey="building" stackId="status" fill="var(--color-building)" isAnimationActive={false} />
                <Bar dataKey="failed" stackId="status" fill="var(--color-failed)" radius={[0, 2, 2, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          </RegistryInsightPanel>

          <RegistryInsightPanel title="Freshness" detail="created_at distribution">
            <ChartContainer config={freshnessCfg} className="h-40 w-full">
              <BarChart data={freshnessRows}>
                <CartesianGrid stroke="var(--border)" strokeDasharray="2 4" vertical={false} />
                <XAxis
                  dataKey="bucket"
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10, fill: "var(--muted-foreground)" }}
                />
                <YAxis hide allowDecimals={false} />
                <Tooltip contentStyle={registryTooltipStyle} />
                <Bar dataKey="resources" fill="var(--color-resources)" radius={[2, 2, 0, 0]} isAnimationActive={false} />
              </BarChart>
            </ChartContainer>
          </RegistryInsightPanel>

          <RegistryInsightPanel title="Ownership and tags" detail={`${inventory.owners} owners / ${inventory.tagCount} tags`}>
            <div className="grid h-40 grid-cols-1 gap-2 p-3 sm:grid-cols-2 xl:grid-cols-1">
              <ValueBars label="Top owners" rows={topOwners} maxRows={2} />
              <ValueBars label="Top tags" rows={topTags} maxRows={2} />
            </div>
          </RegistryInsightPanel>
        </section>
      ) : null}

      {loaded && !loadError && filtered.length > 0 ? (
        <div className="overflow-x-auto text-[12px]">
          <div
            className="grid min-w-[1120px] items-center gap-2 border-b border-border bg-background px-3 py-1.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
            style={{
              gridTemplateColumns:
                "20px minmax(300px,1.35fr) minmax(150px,0.75fr) minmax(88px,110px) minmax(92px,110px) 80px minmax(140px,1fr) 90px 80px 82px",
            }}
          >
            <input
              type="checkbox"
              checked={allSelected}
              onChange={(e) => {
                if (e.target.checked) setSelected(new Set(filtered.map((i) => i.id)))
                else setSelected(new Set())
              }}
              className="h-3 w-3 accent-brand"
            />
            <span>Resource</span>
            <span>Readiness</span>
            <span>Kind</span>
            <span>Version</span>
            <span>Size</span>
            <span>Tags</span>
            <span>Owner</span>
            <span>Pushed</span>
            <span className="text-right">Action</span>
          </div>

          {filtered.map((it) => (
            <RegistryRow
              key={it.id}
              item={it}
              selected={selected.has(it.id)}
              onSelect={() => toggle(it.id)}
              onCopy={(value, label) => {
                copyToClipboard(value)
                setActionNotice(`${label} copied to clipboard.`)
              }}
              onCompose={() => {
                writeRegistryExperimentDraft(it, workspace.defaultRegion)
                navigate({ name: "experiment-new" })
              }}
            />
          ))}
        </div>
      ) : (
        <RegistryEmptyState
          loaded={loaded}
          error={loadError}
          hasItems={items.length > 0}
          onClear={() => {
            setQ("")
            setOwnerFilter("all")
            setTagFilter("all")
            setStatusFilter("all")
            navigate({ name: "registry", kind: undefined })
          }}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadRegistryItems()}
        />
      )}

      {selected.size > 0 ? (
        <div className="sticky bottom-0 z-20 grid gap-px border-t border-border bg-border text-[12px] shadow-[0_-8px_30px_rgba(0,0,0,0.22)] lg:grid-cols-[minmax(260px,1fr)_minmax(0,1.5fr)_auto]">
          <div className="min-w-0 bg-popover px-3 py-2">
            <div className="flex min-w-0 items-center gap-2">
              <span className={cn("h-2 w-2 shrink-0 rounded-full", selectionSummaryDot(selectedSummary.tone))} />
              <div className="min-w-0">
                <div className={cn("truncate font-medium", selectionSummaryText(selectedSummary.tone))}>
                  {selectedSummary.verdict}
                </div>
                <div className="truncate text-[10.5px] text-muted-foreground" title={selectedSummary.detail}>
                  {selectedSummary.detail}
                </div>
              </div>
            </div>
          </div>
          <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
            {selectedSummary.facts.map((fact) => (
              <div key={fact.label} className="min-w-0 bg-popover px-3 py-2">
                <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
                <div className={cn("truncate font-mono text-[11.5px]", fact.tone ? selectionSummaryText(fact.tone) : "")}>
                  {fact.value}
                </div>
              </div>
            ))}
          </div>
          <div className="flex flex-wrap items-center justify-end gap-1.5 bg-popover px-3 py-2">
            <Button
              size="sm"
              variant="outline"
              className="h-7 gap-1 text-[12px]"
              onClick={() => exportRegistryItems(selectedItems)}
            >
              <Download className="h-3 w-3" /> Export CSV
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 gap-1 text-[12px]"
              onClick={() => {
                copyToClipboard(selectedItems.map(resourceCoordinate).join("\n"))
                setActionNotice(`${selectedItems.length} registry coordinates copied to clipboard.`)
              }}
            >
              <Copy className="h-3 w-3" /> Copy refs
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-[12px]"
              onClick={() => setSelected(new Set())}
            >
              Clear
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  )
}

function RegistryRow({
  item,
  selected,
  onSelect,
  onCopy,
  onCompose,
}: {
  item: RegistryItem
  selected: boolean
  onSelect: () => void
  onCopy: (value: string, label: string) => void
  onCompose: () => void
}) {
  const brief = registryRowBrief(item)
  const coordinate = resourceCoordinate(item)
  return (
    <div
      className={cn(
        "group grid min-w-[1120px] items-center gap-2 border-b border-border px-3 py-1.5 hover:bg-accent/40",
        selected && "bg-accent/40",
      )}
      style={{
        gridTemplateColumns:
          "20px minmax(300px,1.35fr) minmax(150px,0.75fr) minmax(88px,110px) minmax(92px,110px) 80px minmax(140px,1fr) 90px 80px 82px",
      }}
    >
      <input
        type="checkbox"
        checked={selected}
        onChange={onSelect}
        className="h-3 w-3 accent-brand"
      />
      <div className="min-w-0">
        <div className="flex min-w-0 items-center gap-2">
          <Package className="h-3 w-3 shrink-0 text-muted-foreground" />
          <span className="truncate font-mono text-[12px] text-foreground">{formatReadableLabel(item.name)}</span>
        </div>
        <div className="mt-0.5 flex min-w-0 items-center gap-1.5">
          <span
            className="min-w-0 truncate font-mono text-[10.5px] text-muted-foreground"
            title={coordinate}
          >
            {resourceCoordinateLabel(item)}
          </span>
          <span className="shrink-0 rounded bg-muted px-1 font-mono text-[9.5px] text-muted-foreground">
            {item.tags.length} tag{item.tags.length === 1 ? "" : "s"}
          </span>
        </div>
      </div>
      <div className="min-w-0">
        <div className="flex min-w-0 items-center gap-1.5">
          <span className={cn("h-1.5 w-1.5 shrink-0 rounded-full", registryRowBriefDot(brief.tone))} />
          <span className={cn("truncate text-[11.5px] font-medium", registryRowBriefText(brief.tone))}>
            {brief.label}
          </span>
        </div>
        <div className="truncate text-[10.5px] text-muted-foreground" title={brief.detail}>
          {brief.detail}
        </div>
      </div>
      <div className="min-w-0"><KindBadge kind={item.kind} /></div>
      <span className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">{displayVersion(item)}</span>
      <span className="font-mono text-[11px] text-muted-foreground">{formatBytes(item.size_bytes)}</span>
      <div className="flex min-w-0 flex-wrap items-center gap-1">
        {item.tags.slice(0, 3).map((t) => (
          <span
            key={t}
            className="max-w-[7rem] truncate rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
            title={t}
          >
            {formatReadableToken(t)}
          </span>
        ))}
        {item.tags.length > 3 ? (
          <span className="rounded bg-muted/70 px-1 font-mono text-[10px] text-muted-foreground">
            +{item.tags.length - 3}
          </span>
        ) : null}
      </div>
      <span className="truncate font-mono text-[11px] text-muted-foreground" title={item.owner}>{formatReadableToken(item.owner)}</span>
      <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(item.created_at)}</span>
      <div className="flex items-center justify-end gap-1">
        {brief.canCompose ? (
          <button
            className="inline-flex h-6 items-center gap-1 rounded border border-brand/30 bg-brand/10 px-1.5 text-[10.5px] font-medium text-brand hover:bg-brand/15"
            onClick={onCompose}
            title="Compose a new experiment with this package"
          >
            <ArrowUpRight className="h-3 w-3" />
            Compose
          </button>
        ) : null}
        <button
          className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-secondary hover:text-foreground"
          onClick={() => onCopy(coordinate, "Resource coordinate")}
          aria-label={`Copy ${item.name} coordinate`}
          title="Copy resource coordinate"
        >
          <Copy className="h-3 w-3 text-muted-foreground" />
        </button>
      </div>
    </div>
  )
}

function RegistryInsightPanel({
  title,
  detail,
  children,
}: {
  title: string
  detail: string
  children: React.ReactNode
}) {
  return (
    <div className="bg-background">
      <header className="flex h-8 items-center justify-between border-b border-border px-3">
        <h3 className="text-[12px] font-semibold">{title}</h3>
        <span className="truncate text-[11px] text-muted-foreground">{detail}</span>
      </header>
      {children}
    </div>
  )
}

function RegistryBriefView({
  brief,
  onAction,
}: {
  brief: RegistryBrief
  onAction: () => void
}) {
  return (
    <section className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(280px,0.9fr)_minmax(0,1.5fr)]">
      <div className="min-w-0 bg-background p-3">
        <div className="flex items-center gap-2">
          <span className={cn("h-2 w-2 rounded-full", registryBriefDot(brief.tone))} />
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Registry brief</div>
            <div className={cn("mt-0.5 truncate text-[14px] font-medium", registryBriefText(brief.tone))}>
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
          <ArrowIcon actionType={brief.actionType} />
        </Button>
      </div>
      <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
        {brief.facts.map((fact) => (
          <div key={fact.label} className="min-w-0 bg-background px-3 py-2.5">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
            <div
              className={cn("mt-0.5 truncate font-mono text-[12px]", fact.tone ? registryBriefText(fact.tone) : "")}
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

function ArrowIcon({ actionType }: { actionType: RegistryBriefAction }) {
  if (actionType === "settings") return <ArrowUpRight className="h-3 w-3" />
  return <Filter className="h-3 w-3" />
}

function ValueBars({
  label,
  rows,
  maxRows = 5,
}: {
  label: string
  rows: { value: string; count: number }[]
  maxRows?: number
}) {
  const max = Math.max(1, ...rows.map((row) => row.count))
  const visibleRows = (rows.length ? rows : [{ value: "none", count: 0 }]).slice(0, maxRows)
  const hiddenCount = Math.max(0, rows.length - visibleRows.length)
  return (
    <div className="min-w-0">
      <div className="mb-1 text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="grid gap-0.5">
        {visibleRows.map((row) => (
          <div key={row.value} className="grid grid-cols-[minmax(72px,0.8fr)_minmax(0,1fr)_34px] items-center gap-2 text-[10.5px]">
            <span className="truncate font-mono text-muted-foreground">{formatReadableToken(row.value)}</span>
            <span className="h-1.5 overflow-hidden rounded bg-muted">
              <span
                className="block h-full rounded bg-brand"
                style={{ width: `${row.count ? Math.max(6, (row.count / max) * 100) : 0}%` }}
              />
            </span>
            <span className="text-right font-mono">{row.count}</span>
          </div>
        ))}
        {hiddenCount > 0 ? (
          <div className="truncate text-[10px] text-muted-foreground">
            +{hiddenCount} more
          </div>
        ) : null}
      </div>
    </div>
  )
}

function InventoryStat({
  icon: Icon,
  label,
  value,
  detail,
  className,
}: {
  icon: React.ComponentType<{ className?: string }>
  label: string
  value: string
  detail: string
  className?: string
}) {
  return (
    <div className={cn("bg-background p-2.5", className)}>
      <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wide text-muted-foreground">
        <Icon className="h-3 w-3" />
        {label}
      </div>
      <div className="mt-0.5 truncate font-mono text-[16px] font-medium">{value}</div>
      <div className="truncate text-[10.5px] text-muted-foreground">{detail}</div>
    </div>
  )
}

function RegistryEmptyState({
  loaded,
  error,
  hasItems,
  onClear,
  onSettings,
  onRetry,
}: {
  loaded: boolean
  error: string | null
  hasItems: boolean
  onClear: () => void
  onSettings: () => void
  onRetry: () => void
}) {
  if (!loaded) {
    return (
      <div className="grid grid-cols-1 gap-px bg-border md:grid-cols-3">
        {[0, 1, 2].map((i) => (
          <div key={i} className="bg-background p-3">
            <div className="h-3 w-24 rounded bg-muted" />
            <div className="mt-3 h-6 w-40 rounded bg-muted/70" />
            <div className="mt-2 h-3 w-32 rounded bg-muted/60" />
          </div>
        ))}
      </div>
    )
  }

  if (error) {
    return (
      <ConnectionIssue
        title="Registry request failed"
        detail={error}
        onSettings={onSettings}
        onRetry={onRetry}
      />
    )
  }

  return (
    <div className="flex flex-col items-center gap-2 px-3 py-12 text-center">
      <Package className="h-6 w-6 text-muted-foreground" />
      <div className="text-[13px] font-medium">
        {hasItems ? "No resources match these filters" : "No registry resources loaded"}
      </div>
      <div className="max-w-[260px] text-balance text-[12px] text-muted-foreground sm:max-w-md">
        {hasItems
          ? "Clear the current kind, tag, owner, and search filters to return to the full inventory."
          : "Accepted packages and reusable resources will appear here after they are published for this workspace."}
      </div>
      <div className="mt-1 flex items-center gap-1.5">
        {hasItems ? (
          <Button variant="outline" size="sm" className="h-7 text-[12px]" onClick={onClear}>
            Clear filters
          </Button>
        ) : null}
        <Button
          variant={hasItems ? "outline" : "default"}
          size="sm"
          className={cn("h-7 text-[12px]", hasItems ? "" : "bg-brand text-brand-foreground hover:bg-brand/90")}
          onClick={onSettings}
        >
          Settings
        </Button>
      </div>
    </div>
  )
}

function exportRegistryItems(items: RegistryItem[]) {
  downloadCsv(
    `registry-selection-${new Date().toISOString().slice(0, 10)}.csv`,
    items.map((item) => ({
      id: item.id,
      name: item.name,
      kind: item.kind,
      version: item.version,
      status: item.status,
      size_bytes: item.size_bytes,
      tags: item.tags.join("|"),
      owner: item.owner,
      created_at: item.created_at,
      description: item.description,
    })),
  )
}

function copyToClipboard(value: string) {
  if (!navigator.clipboard) return
  void navigator.clipboard.writeText(value).catch(() => undefined)
}

function displayVersion(item: RegistryItem) {
  if (!item.version) return formatShortId(item.id)
  if (item.version === item.id || item.version.length > 16) return formatReadableToken(item.version)
  return item.version
}

function resourceCoordinate(item: RegistryItem) {
  return `${item.name}@${item.version || item.id}`
}

function resourceCoordinateLabel(item: RegistryItem) {
  return `${formatReadableLabel(item.name)}@${displayVersion(item)}`
}

type RegistryRowBrief = {
  tone: RegistrySelectionTone
  label: string
  detail: string
  canCompose: boolean
}

function registryRowBrief(item: RegistryItem): RegistryRowBrief {
  const queueable = isQueueableRegistryItem(item)
  if (item.status === "failed") {
    return {
      tone: "danger",
      label: "Needs review",
      detail: queueable ? "Failed package cannot queue." : "Failed artifact retained for diagnosis.",
      canCompose: false,
    }
  }
  if (item.status === "building") {
    return {
      tone: "warning",
      label: "Still building",
      detail: queueable ? "Visible now; compose when ready." : "Registered, but not ready for workflows.",
      canCompose: false,
    }
  }
  if (queueable) {
    return {
      tone: "success",
      label: "Queueable",
      detail: "Ready package can seed a new experiment.",
      canCompose: true,
    }
  }
  return {
    tone: "success",
    label: "Reusable",
    detail: `${displayKind(item.kind)} ready for package workflows.`,
    canCompose: false,
  }
}

function registryRowBriefDot(tone: RegistrySelectionTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  return "bg-muted-foreground"
}

function registryRowBriefText(tone: RegistrySelectionTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  return "text-muted-foreground"
}

function registrySearchText(item: RegistryItem) {
  return [
    item.name,
    item.description,
    item.kind,
    displayKind(item.kind),
    item.status,
    item.owner,
    item.version,
    item.id,
    resourceCoordinate(item),
    resourceCoordinateLabel(item),
    ...item.tags,
  ].join(" ").toLowerCase()
}

function registrySortItems(items: RegistryItem[]) {
  return [...items].sort((a, b) =>
    Number(isQueueableRegistryItem(b)) - Number(isQueueableRegistryItem(a)) ||
    registryStatusRank(b.status) - registryStatusRank(a.status) ||
    Date.parse(b.created_at) - Date.parse(a.created_at) ||
    a.name.localeCompare(b.name),
  )
}

function registryStatusRank(status: RegistryItem["status"]) {
  if (status === "ready") return 3
  if (status === "building") return 2
  if (status === "failed") return 1
  return 0
}

function isQueueableRegistryItem(item: RegistryItem) {
  return item.kind === "benchmark" || item.kind === "experiment_package"
}

function writeRegistryExperimentDraft(item: RegistryItem, region: string) {
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

function registryInventory(items: RegistryItem[]) {
  const latest = [...items].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]
  return {
    total: items.length,
    ready: items.filter((item) => item.status === "ready").length,
    building: items.filter((item) => item.status === "building").length,
    failed: items.filter((item) => item.status === "failed").length,
    queueable: items.filter((item) => item.kind === "benchmark" || item.kind === "experiment_package").length,
    bytes: items.reduce((acc, item) => acc + item.size_bytes, 0),
    kinds: new Set(items.map((item) => item.kind)).size,
    owners: new Set(items.map((item) => item.owner)).size,
    tagCount: new Set(items.flatMap((item) => item.tags)).size,
    latestLabel: latest ? formatRelative(latest.created_at) : "none",
    latestDetail: latest ? formatReadableLabel(latest.name) : "waiting for resources",
  }
}

type RegistrySelectionTone = "success" | "warning" | "danger" | "muted"
type RegistrySelectionSummary = {
  tone: RegistrySelectionTone
  verdict: string
  detail: string
  facts: { label: string; value: string; tone?: RegistrySelectionTone }[]
}

function registrySelectionSummary(items: RegistryItem[]): RegistrySelectionSummary {
  const failed = items.filter((item) => item.status === "failed").length
  const building = items.filter((item) => item.status === "building").length
  const ready = items.filter((item) => item.status === "ready").length
  const bytes = items.reduce((acc, item) => acc + item.size_bytes, 0)
  const kinds = new Set(items.map((item) => item.kind))
  const owners = new Set(items.map((item) => item.owner))
  const queueable = items.filter((item) => item.kind === "benchmark" || item.kind === "experiment_package").length
  const latest = [...items].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]

  if (items.length === 0) {
    return {
      tone: "muted",
      verdict: "No selection",
      detail: "Select resources to export, copy, or audit coordinates.",
      facts: selectionSummaryFacts("Selected", "0", "Ready", "0", "Bytes", "0 B", "Kinds", "0", "muted"),
    }
  }

  if (failed > 0) {
    return {
      tone: "danger",
      verdict: "Selection includes failed artifacts",
      detail: "Export is available, but failed resources should be reviewed before copying refs into a workflow.",
      facts: selectionSummaryFacts("Selected", `${items.length}`, "Failed", `${failed}`, "Bytes", formatBytes(bytes), "Kinds", `${kinds.size}`, "danger"),
    }
  }

  if (building > 0) {
    return {
      tone: "warning",
      verdict: "Selection still building",
      detail: "Some selected resources are not ready yet. Copy refs only if the downstream workflow can wait.",
      facts: selectionSummaryFacts("Selected", `${items.length}`, "Building", `${building}`, "Bytes", formatBytes(bytes), "Queueable", `${queueable}`, "warning"),
    }
  }

  return {
    tone: "success",
    verdict: `${items.length} ready resource${items.length === 1 ? "" : "s"} selected`,
    detail: `${owners.size} owner${owners.size === 1 ? "" : "s"} · latest ${latest ? formatRelative(latest.created_at) : "unknown"} · ready to export or copy refs.`,
    facts: selectionSummaryFacts("Selected", `${items.length}`, "Ready", `${ready}`, "Bytes", formatBytes(bytes), "Kinds", `${kinds.size}`, "success"),
  }
}

function selectionSummaryFacts(
  firstLabel: string,
  firstValue: string,
  secondLabel: string,
  secondValue: string,
  thirdLabel: string,
  thirdValue: string,
  fourthLabel: string,
  fourthValue: string,
  tone: RegistrySelectionTone,
): RegistrySelectionSummary["facts"] {
  return [
    { label: firstLabel, value: firstValue, tone },
    { label: secondLabel, value: secondValue, tone: tone === "danger" || tone === "warning" ? tone : undefined },
    { label: thirdLabel, value: thirdValue },
    { label: fourthLabel, value: fourthValue },
  ]
}

function selectionSummaryDot(tone: RegistrySelectionTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  return "bg-muted-foreground"
}

function selectionSummaryText(tone: RegistrySelectionTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  return "text-muted-foreground"
}

function registryDecisionBrief(items: RegistryItem[], inventory: ReturnType<typeof registryInventory>): RegistryBrief {
  const latest = [...items].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))[0]
  const latestAgeDays = latest ? (Date.now() - Date.parse(latest.created_at)) / 86_400_000 : Infinity
  const readyPackages = items.filter((item) => item.kind === "experiment_package" && item.status === "ready").length
  const buildingPackages = items.filter((item) => item.kind === "experiment_package" && item.status === "building").length
  const failedPackages = items.filter((item) => item.kind === "experiment_package" && item.status === "failed").length
  const packageFact = `${readyPackages} ready / ${buildingPackages} building`

  if (items.length === 0) {
    return {
      tone: "info",
      verdict: "Registry is waiting for resources",
      detail: "Accepted packages and reusable resources will appear after this workspace publishes them.",
      action: "Check settings",
      actionType: "settings",
      facts: registryBriefFacts("Resources", "0", "Packages", "0 ready", "Owners", "none", "Latest", "none", "info"),
    }
  }

  if (inventory.failed > 0) {
    return {
      tone: "danger",
      verdict: "Failed resources need attention",
      detail: "Failed registry artifacts can block queueing or confuse package selection. Review them before promoting this inventory.",
      action: "Show failed",
      actionType: "status",
      status: "failed",
      facts: registryBriefFacts("Failed", `${inventory.failed}`, "Packages", `${failedPackages} failed`, "Ready", `${inventory.ready}`, "Latest", inventory.latestLabel, "danger"),
    }
  }

  if (inventory.queueable === 0) {
    return {
      tone: "warning",
      verdict: "No queueable packages",
      detail: "The registry has resources, but no benchmark or experiment package is ready to launch runs.",
      action: "Check settings",
      actionType: "settings",
      facts: registryBriefFacts("Queueable", "0", "Kinds", `${inventory.kinds}`, "Owners", `${inventory.owners}`, "Latest", inventory.latestLabel, "warning"),
    }
  }

  if (inventory.building > inventory.ready || buildingPackages > readyPackages) {
    return {
      tone: "warning",
      verdict: "Builds are still settling",
      detail: "Some registry resources are still building. Filter to building artifacts before choosing a package for a run.",
      action: "Show building",
      actionType: "status",
      status: "building",
      facts: registryBriefFacts("Building", `${inventory.building}`, "Packages", packageFact, "Ready", `${inventory.ready}`, "Latest", inventory.latestLabel, "warning"),
    }
  }

  if (latestAgeDays > 30) {
    return {
      tone: "warning",
      verdict: "Inventory looks stale",
      detail: "The latest registry update is over a month old. Confirm the workspace connection before relying on old resources for new experiments.",
      action: "Check settings",
      actionType: "settings",
      facts: registryBriefFacts("Latest", inventory.latestLabel, "Queueable", `${inventory.queueable}`, "Bytes", formatBytes(inventory.bytes), "Owners", `${inventory.owners}`, "warning"),
    }
  }

  return {
    tone: "success",
    verdict: "Ready resources available",
    detail: "Queueable packages are ready and the registry has enough ownership and tag signal for selection workflows.",
    action: "Show packages",
    actionType: "packages",
    facts: registryBriefFacts("Ready", `${inventory.ready}`, "Packages", packageFact, "Tags", `${inventory.tagCount}`, "Latest", inventory.latestLabel, "success"),
  }
}

function registryBriefFacts(
  firstLabel: string,
  firstValue: string,
  secondLabel: string,
  secondValue: string,
  thirdLabel: string,
  thirdValue: string,
  fourthLabel: string,
  fourthValue: string,
  tone: RegistryBriefTone,
): RegistryBrief["facts"] {
  return [
    { label: firstLabel, value: firstValue, tone },
    { label: secondLabel, value: secondValue, tone: tone === "danger" ? "danger" : undefined },
    { label: thirdLabel, value: thirdValue },
    { label: fourthLabel, value: fourthValue },
  ]
}

function registryBriefDot(tone: RegistryBriefTone) {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  if (tone === "info") return "bg-info"
  return "bg-muted-foreground"
}

function registryBriefText(tone: RegistryBriefTone) {
  if (tone === "success") return "text-success"
  if (tone === "warning") return "text-warning"
  if (tone === "danger") return "text-destructive"
  if (tone === "info") return "text-info"
  return "text-muted-foreground"
}

function registryStatusByKind(items: RegistryItem[]) {
  const rows = new Map<string, { kind: string; ready: number; building: number; failed: number }>()
  items.forEach((item) => {
    const kind = displayKind(item.kind)
    const row = rows.get(kind) ?? { kind, ready: 0, building: 0, failed: 0 }
    row[item.status] += 1
    rows.set(kind, row)
  })
  return Array.from(rows.values())
    .sort((a, b) => b.ready + b.building + b.failed - (a.ready + a.building + a.failed) || a.kind.localeCompare(b.kind))
    .slice(0, 7)
}

function registryFreshness(items: RegistryItem[]) {
  const now = Date.now()
  const rows = [
    { bucket: "24h", resources: 0, maxAgeDays: 1 },
    { bucket: "7d", resources: 0, maxAgeDays: 7 },
    { bucket: "30d", resources: 0, maxAgeDays: 30 },
    { bucket: "90d", resources: 0, maxAgeDays: 90 },
    { bucket: "old", resources: 0, maxAgeDays: Infinity },
  ]
  items.forEach((item) => {
    const ageDays = (now - Date.parse(item.created_at)) / 86_400_000
    if (!Number.isFinite(ageDays)) return
    const row = rows.find((bucket) => ageDays <= bucket.maxAgeDays) ?? rows[rows.length - 1]
    row.resources += 1
  })
  return rows.map(({ maxAgeDays: _maxAgeDays, ...row }) => row)
}

function statusFilterOptions(items: RegistryItem[]) {
  const counts = new Map<RegistryItem["status"], number>([
    ["ready", 0],
    ["building", 0],
    ["failed", 0],
  ])
  items.forEach((item) => counts.set(item.status, (counts.get(item.status) ?? 0) + 1))
  return [
    { value: "all", label: "all status", count: items.length },
    { value: "ready", label: "ready", count: counts.get("ready") ?? 0 },
    { value: "building", label: "building", count: counts.get("building") ?? 0 },
    { value: "failed", label: "failed", count: counts.get("failed") ?? 0 },
  ]
}

function topRegistryValues(values: string[], max: number) {
  const counts = new Map<string, number>()
  values.filter(Boolean).forEach((value) => {
    counts.set(value, (counts.get(value) ?? 0) + 1)
  })
  return Array.from(counts.entries())
    .map(([value, count]) => ({ value, count }))
    .sort((a, b) => b.count - a.count || a.value.localeCompare(b.value))
    .slice(0, max)
}

function displayKind(kind: RegistryItem["kind"]) {
  if (kind === "experiment_package") return "package"
  if (kind === "runtime_profile") return "runtime"
  if (kind === "task_boundary") return "boundary"
  if (kind === "trial_contract") return "contract"
  return kind
}

const registryTooltipStyle = {
  background: "var(--popover)",
  border: "1px solid var(--border)",
  borderRadius: 6,
  fontSize: 11,
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

export function PageHeader({
  title,
  subtitle,
  primaryAction,
  rightSlot,
}: {
  title: string
  subtitle?: string
  primaryAction?: React.ReactNode
  rightSlot?: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-2 border-b border-border px-3 py-3 sm:flex-row sm:items-center sm:justify-between">
      <div className="min-w-0 max-w-full">
        <h1 className="text-[16px] font-semibold tracking-tight">{title}</h1>
        {subtitle ? (
          <p className="max-w-[280px] text-balance text-[12px] text-muted-foreground sm:max-w-none">{subtitle}</p>
        ) : null}
      </div>
      <div className="flex max-w-full flex-wrap items-center gap-2">
        {rightSlot}
        {primaryAction}
      </div>
    </div>
  )
}

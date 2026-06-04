import { useEffect, useMemo, useState } from "react"
import {
  Boxes,
  Copy,
  Download,
  Filter,
  Info,
  Package,
  Plus,
  Search,
  Server,
  CircleDot,
  BarChart3,
  CheckCircle2,
  Clock,
  X,
  TerminalSquare,
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
import { StatusPill, KindBadge } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import { supabase, type RegistryItem } from "@/lib/supabase"
import { formatBytes, formatReadableLabel, formatReadableToken, formatRelative, formatShortId } from "@/lib/format"
import { downloadCsv } from "@/lib/export"
import { cn } from "@/lib/utils"
import { useWorkspacePreferences, type WorkspacePreferences } from "@/lib/workspace"

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
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [actionNotice, setActionNotice] = useState<string | null>(null)
  const [pushOpen, setPushOpen] = useState(false)

  async function loadRegistryItems() {
    setLoaded(false)
    setLoadError(null)
    const { data, error } = await supabase
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

  useEffect(() => {
    if (new URLSearchParams(window.location.search).get("push") === "1") {
      setPushOpen(true)
    }
  }, [])

  const filtered = useMemo(() => {
    return items.filter((i) => {
      if (activeKind && i.kind !== activeKind) return false
      if (ownerFilter !== "all" && i.owner !== ownerFilter) return false
      if (tagFilter !== "all" && !i.tags.includes(tagFilter)) return false
      if (q && !`${i.name} ${i.description} ${i.tags.join(" ")}`.toLowerCase().includes(q.toLowerCase())) return false
      return true
    })
  }, [items, activeKind, ownerFilter, q, tagFilter])

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
  const selectedItems = useMemo(
    () => items.filter((item) => selected.has(item.id)),
    [items, selected],
  )
  const inventory = useMemo(() => registryInventory(items), [items])
  const statusRows = useMemo(() => registryStatusByKind(items), [items])
  const freshnessRows = useMemo(() => registryFreshness(items), [items])
  const topOwners = useMemo(() => topRegistryValues(items.map((item) => item.owner), 5), [items])
  const topTags = useMemo(() => topRegistryValues(items.flatMap((item) => item.tags), 5), [items])
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
        primaryAction={
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={() => setPushOpen((open) => !open)}
          >
            <Plus className="h-3.5 w-3.5" /> Push resource
          </Button>
        }
      />

      {pushOpen ? (
        <PushResourcePanel
          workspace={workspace}
          onCopy={(value, label) => {
            copyToClipboard(value)
            setActionNotice(`${label} copied to clipboard.`)
          }}
          onSettings={() => navigate({ name: "settings" })}
        />
      ) : null}

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
            className="grid min-w-[980px] items-center gap-2 border-b border-border bg-background px-3 py-1.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
            style={{
              gridTemplateColumns:
                "20px minmax(240px,1fr) minmax(96px,120px) minmax(96px,110px) 100px 80px minmax(140px,1fr) 90px 80px 22px",
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
            <span>Name</span>
            <span>Kind</span>
            <span>Version</span>
            <span>Status</span>
            <span>Size</span>
            <span>Tags</span>
            <span>Owner</span>
            <span>Pushed</span>
            <span></span>
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
            navigate({ name: "registry", kind: undefined })
          }}
          onSettings={() => navigate({ name: "settings" })}
          onRetry={() => void loadRegistryItems()}
          onPush={() => setPushOpen(true)}
        />
      )}

      {selected.size > 0 ? (
        <div className="sticky bottom-0 flex items-center justify-between border-t border-border bg-popover px-3 py-1.5 text-[12px]">
          <div className="flex items-center gap-2 text-muted-foreground">
            <span className="font-mono text-foreground">{selected.size}</span>{" "}
            selected
          </div>
          <div className="flex items-center gap-1.5">
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
}: {
  item: RegistryItem
  selected: boolean
  onSelect: () => void
  onCopy: (value: string, label: string) => void
}) {
  return (
    <div
      className={cn(
        "group grid min-w-[980px] items-center gap-2 border-b border-border px-3 py-1.5 hover:bg-accent/40",
        selected && "bg-accent/40",
      )}
      style={{
        gridTemplateColumns:
          "20px minmax(240px,1fr) minmax(96px,120px) minmax(96px,110px) 100px 80px minmax(140px,1fr) 90px 80px 22px",
      }}
    >
      <input
        type="checkbox"
        checked={selected}
        onChange={onSelect}
        className="h-3 w-3 accent-brand"
      />
      <div className="flex min-w-0 items-center gap-2">
        <Package className="h-3 w-3 shrink-0 text-muted-foreground" />
        <span className="truncate font-mono text-[12px]">{formatReadableLabel(item.name)}</span>
        <button
          className="opacity-0 transition-opacity group-hover:opacity-100"
          onClick={() => onCopy(resourceCoordinate(item), "Resource coordinate")}
          aria-label={`Copy ${item.name} coordinate`}
          title="Copy resource coordinate"
        >
          <Copy className="h-3 w-3 text-muted-foreground" />
        </button>
      </div>
      <div className="min-w-0"><KindBadge kind={item.kind} /></div>
      <span className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">{displayVersion(item)}</span>
      <StatusPill status={item.status} />
      <span className="font-mono text-[11px] text-muted-foreground">{formatBytes(item.size_bytes)}</span>
      <div className="flex flex-wrap items-center gap-1">
        {item.tags.slice(0, 3).map((t) => (
          <span
            key={t}
            className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
          >
            {formatReadableToken(t)}
          </span>
        ))}
      </div>
      <span className="font-mono text-[11px] text-muted-foreground">{item.owner}</span>
      <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(item.created_at)}</span>
      <button
        className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-secondary hover:text-foreground"
        onClick={() => onCopy(resourceCoordinate(item), "Resource coordinate")}
        aria-label={`Copy ${item.name} coordinate`}
        title="Copy resource coordinate"
      >
        <Copy className="h-3 w-3" />
      </button>
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

function PushResourcePanel({
  workspace,
  onCopy,
  onSettings,
}: {
  workspace: WorkspacePreferences
  onCopy: (value: string, label: string) => void
  onSettings: () => void
}) {
  const apiLabel = workspace.apiBase ? workspace.apiBase : "default cloud API"
  const tokenLabel = workspace.userToken ? "token configured" : "anonymous"
  const baseCommand = [
    "buc registry push",
    "  --kind experiment_package",
    "  --name owner/package-name",
    "  --version sha256:<digest>",
    "  --file ./dist/package.tar.zst",
    `  --region ${workspace.defaultRegion}`,
  ].join(" \\\n")

  return (
    <section className="grid grid-cols-1 gap-px overflow-hidden border-b border-border bg-border lg:grid-cols-[280px_minmax(0,1fr)_260px]">
      <div className="min-w-0 bg-background p-3">
        <div className="flex items-center gap-2">
          <TerminalSquare className="h-4 w-4 text-brand" />
          <div>
            <div className="text-[12.5px] font-medium">Push workflow</div>
            <div className="text-[11px] text-muted-foreground">Get resources into the registry.</div>
          </div>
        </div>
        <div className="mt-3 grid grid-cols-2 gap-px bg-border">
          <PushFact label="API" value={apiLabel} />
          <PushFact label="Auth" value={tokenLabel} />
          <PushFact label="Region" value={workspace.defaultRegion} />
          <PushFact label="Owner" value={workspace.slug} />
        </div>
      </div>

      <div className="min-w-0 bg-background p-3">
        <div className="mb-2 grid gap-2 sm:flex sm:items-center sm:justify-between">
          <div className="text-[10px] uppercase tracking-wide text-muted-foreground">CLI template</div>
          <Button variant="outline" size="sm" className="h-7 w-fit gap-1 text-[12px]" onClick={() => onCopy(baseCommand, "Registry push command")}>
            <Copy className="h-3 w-3" /> Copy
          </Button>
        </div>
        <pre className="whitespace-pre-wrap break-words rounded-md border border-border bg-card p-2 font-mono text-[11px] leading-relaxed text-muted-foreground">
          {baseCommand}
        </pre>
      </div>

      <div className="min-w-0 bg-background p-3">
        <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Accepted kinds</div>
        <div className="mt-2 grid gap-1">
          {[
            ["experiment_package", "Queueable eval bundle"],
            ["agent", "Reusable execution agent"],
            ["benchmark", "Legacy queue target"],
            ["mcp", "Tool server definition"],
          ].map(([kind, detail]) => (
            <div key={kind} className="grid min-w-0 gap-0.5 border-b border-border py-1 text-[11px] last:border-b-0 sm:grid-cols-[minmax(120px,1fr)_minmax(0,1fr)] sm:items-center sm:gap-2">
              <span className="font-mono text-foreground">{formatReadableToken(kind)}</span>
              <span className="min-w-0 truncate text-muted-foreground sm:text-right">{detail}</span>
            </div>
          ))}
        </div>
        <Button variant="outline" size="sm" className="mt-3 h-7 w-full text-[12px]" onClick={onSettings}>
          Settings
        </Button>
      </div>
    </section>
  )
}

function PushFact({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-card p-2">
      <div className="text-[9.5px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className="truncate font-mono text-[11.5px] text-foreground">{value}</div>
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
  onPush,
}: {
  loaded: boolean
  error: string | null
  hasItems: boolean
  onClear: () => void
  onSettings: () => void
  onRetry: () => void
  onPush: () => void
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
          : "Connect an API token or push packages from the CLI; accepted packages and reusable resources will appear here."}
      </div>
      <div className="mt-1 flex items-center gap-1.5">
        {hasItems ? (
          <Button variant="outline" size="sm" className="h-7 text-[12px]" onClick={onClear}>
            Clear filters
          </Button>
        ) : null}
        {hasItems ? (
          <Button variant="outline" size="sm" className="h-7 text-[12px]" onClick={onSettings}>
            Settings
          </Button>
        ) : (
          <Button size="sm" className="h-7 bg-brand text-[12px] text-brand-foreground hover:bg-brand/90" onClick={onPush}>
            Push resource
          </Button>
        )}
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

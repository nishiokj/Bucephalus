import { useEffect, useMemo, useState } from "react"
import {
  Boxes,
  ChevronDown,
  Copy,
  Filter,
  MoreHorizontal,
  Package,
  Plus,
  Search,
  Server,
  CircleDot,
  BarChart3,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { StatusPill, KindBadge } from "@/components/status-pill"
import { useRouter } from "@/lib/router"
import { supabase, type RegistryItem } from "@/lib/supabase"
import { formatBytes, formatRelative } from "@/lib/format"
import { cn } from "@/lib/utils"

const KIND_TABS: {
  label: string
  kind: "agent" | "benchmark" | "mcp" | undefined
  icon: React.ComponentType<{ className?: string }>
}[] = [
  { label: "All", kind: undefined, icon: Boxes },
  { label: "Agents", kind: "agent", icon: CircleDot },
  { label: "Benchmarks", kind: "benchmark", icon: BarChart3 },
  { label: "MCP servers", kind: "mcp", icon: Server },
]

export function RegistryPage() {
  const { route, navigate } = useRouter()
  const activeKind = route.name === "registry" ? route.kind : undefined
  const [items, setItems] = useState<RegistryItem[]>([])
  const [q, setQ] = useState("")
  const [selected, setSelected] = useState<Set<string>>(new Set())

  useEffect(() => {
    void supabase
      .from("registry_items")
      .select("*")
      .order("created_at", { ascending: false })
      .then(({ data }) => setItems((data ?? []) as RegistryItem[]))
  }, [])

  const filtered = useMemo(() => {
    return items.filter((i) => {
      if (activeKind && i.kind !== activeKind) return false
      if (q && !`${i.name} ${i.description} ${i.tags.join(" ")}`.toLowerCase().includes(q.toLowerCase())) return false
      return true
    })
  }, [items, activeKind, q])

  const counts = useMemo(() => {
    const c: Record<string, number> = { all: items.length, agent: 0, benchmark: 0, mcp: 0 }
    items.forEach((i) => (c[i.kind] = (c[i.kind] ?? 0) + 1))
    return c
  }, [items])

  const allSelected = filtered.length > 0 && filtered.every((i) => selected.has(i.id))

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
          <Button size="sm" className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90">
            <Plus className="h-3.5 w-3.5" /> Push resource
          </Button>
        }
      />

      <div className="sticky top-11 z-10 flex items-center justify-between gap-3 border-b border-border bg-background/95 px-3 py-1.5 backdrop-blur">
        <div className="flex items-center">
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
        <div className="flex items-center gap-1.5">
          <div className="relative">
            <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="Filter by name, tag, owner"
              className="h-7 w-72 pl-7 text-[12px]"
            />
          </div>
          <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
            <Filter className="h-3 w-3" /> tag <ChevronDown className="h-3 w-3" />
          </Button>
          <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
            owner <ChevronDown className="h-3 w-3" />
          </Button>
        </div>
      </div>

      <div className="text-[12px]">
        <div
          className="sticky top-[5.25rem] z-10 grid items-center gap-2 border-b border-border bg-background px-3 py-1.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
          style={{
            gridTemplateColumns:
              "20px minmax(280px,1fr) 90px 80px 100px 80px minmax(160px,1fr) 90px 80px 22px",
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
          />
        ))}

        {filtered.length === 0 ? (
          <div className="flex flex-col items-center gap-2 px-3 py-12 text-center">
            <Package className="h-6 w-6 text-muted-foreground" />
            <div className="text-[13px] font-medium">Nothing here yet</div>
            <div className="text-[12px] text-muted-foreground">
              Push your first agent, benchmark, or MCP server to the registry.
            </div>
          </div>
        ) : null}
      </div>

      {selected.size > 0 ? (
        <div className="sticky bottom-0 flex items-center justify-between border-t border-border bg-popover px-3 py-1.5 text-[12px]">
          <div className="flex items-center gap-2 text-muted-foreground">
            <span className="font-mono text-foreground">{selected.size}</span>{" "}
            selected
          </div>
          <div className="flex items-center gap-1.5">
            <Button size="sm" variant="outline" className="h-7 text-[12px]">
              Tag
            </Button>
            <Button size="sm" variant="outline" className="h-7 text-[12px]">
              Move
            </Button>
            <Button size="sm" variant="outline" className="h-7 text-[12px]">
              Compare
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 text-[12px] text-destructive hover:text-destructive"
            >
              Delete
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
}: {
  item: RegistryItem
  selected: boolean
  onSelect: () => void
}) {
  return (
    <div
      className={cn(
        "grid items-center gap-2 border-b border-border px-3 py-1.5 hover:bg-accent/40",
        selected && "bg-accent/40",
      )}
      style={{
        gridTemplateColumns:
          "20px minmax(280px,1fr) 90px 80px 100px 80px minmax(160px,1fr) 90px 80px 22px",
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
        <span className="truncate font-mono text-[12px]">{item.name}</span>
        <button className="opacity-0 transition-opacity group-hover:opacity-100">
          <Copy className="h-3 w-3 text-muted-foreground" />
        </button>
      </div>
      <KindBadge kind={item.kind} />
      <span className="font-mono text-[11px] text-muted-foreground">{item.version}</span>
      <StatusPill status={item.status} />
      <span className="font-mono text-[11px] text-muted-foreground">{formatBytes(item.size_bytes)}</span>
      <div className="flex flex-wrap items-center gap-1">
        {item.tags.slice(0, 3).map((t) => (
          <span
            key={t}
            className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
          >
            {t}
          </span>
        ))}
      </div>
      <span className="font-mono text-[11px] text-muted-foreground">{item.owner}</span>
      <span className="font-mono text-[11px] text-muted-foreground">{formatRelative(item.created_at)}</span>
      <button className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-secondary">
        <MoreHorizontal className="h-3 w-3" />
      </button>
    </div>
  )
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
    <div className="flex items-center justify-between border-b border-border px-3 py-3">
      <div>
        <h1 className="text-[16px] font-semibold tracking-tight">{title}</h1>
        {subtitle ? (
          <p className="text-[12px] text-muted-foreground">{subtitle}</p>
        ) : null}
      </div>
      <div className="flex items-center gap-2">
        {rightSlot}
        {primaryAction}
      </div>
    </div>
  )
}

import {
  Activity,
  BarChart3,
  Boxes,
  CircleDot,
  CreditCard,
  GitCompare,
  Home,
  KeyRound,
  Package,
  Server,
  Settings,
  TerminalSquare,
  Users,
} from "lucide-react"
import { useRouter, type Route } from "@/lib/router"
import { cn } from "@/lib/utils"

type NavItem = {
  label: string
  icon: React.ComponentType<{ className?: string }>
  route: Route
  match: (r: Route) => boolean
  hint?: string
}

const PRIMARY: NavItem[] = [
  {
    label: "Overview",
    icon: Home,
    route: { name: "home" },
    match: (r) => r.name === "home",
    hint: "G H",
  },
  {
    label: "Registry",
    icon: Package,
    route: { name: "registry" },
    match: (r) => r.name === "registry",
    hint: "G R",
  },
  {
    label: "Experiments",
    icon: TerminalSquare,
    route: { name: "experiments" },
    match: (r) =>
      r.name === "experiments" ||
      r.name === "experiment-new" ||
      r.name === "experiment-detail",
    hint: "G E",
  },
  {
    label: "Runs",
    icon: Activity,
    route: { name: "runs" },
    match: (r) => r.name === "runs" || r.name === "run-detail",
    hint: "G U",
  },
  {
    label: "Compare",
    icon: GitCompare,
    route: { name: "compare" },
    match: (r) => r.name === "compare",
    hint: "G C",
  },
]

const REGISTRY_SUB = [
  { label: "All", kind: undefined, icon: Boxes },
  { label: "Agents", kind: "agent" as const, icon: CircleDot },
  { label: "Benchmarks", kind: "benchmark" as const, icon: BarChart3 },
  { label: "MCP servers", kind: "mcp" as const, icon: Server },
]

const ACCOUNT: NavItem[] = [
  {
    label: "Team",
    icon: Users,
    route: { name: "team" },
    match: (r) => r.name === "team",
  },
  {
    label: "Billing",
    icon: CreditCard,
    route: { name: "billing" },
    match: (r) => r.name === "billing",
  },
  {
    label: "Settings",
    icon: Settings,
    route: { name: "settings" },
    match: (r) => r.name === "settings",
  },
]

export function AppSidebar() {
  const { route, navigate } = useRouter()
  const showRegistrySub = route.name === "registry"

  return (
    <aside className="flex h-full w-56 shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      <div className="flex h-11 items-center gap-2 border-b border-sidebar-border px-3">
        <div className="grid h-6 w-6 place-items-center rounded-md bg-brand text-brand-foreground">
          <span className="font-mono text-[11px] font-bold">B</span>
        </div>
        <div className="flex flex-1 items-center justify-between">
          <div>
            <div className="text-xs font-semibold tracking-tight">Bucephalus</div>
            <div className="text-[10px] text-muted-foreground leading-none">kira/prod</div>
          </div>
          <div className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground">
            <span className="font-mono text-[10px]">/</span>
          </div>
        </div>
      </div>

      <nav className="flex-1 overflow-y-auto scrollbar-thin py-1.5">
        <div className="px-2">
          {PRIMARY.map((item) => {
            const Icon = item.icon
            const active = item.match(route)
            return (
              <button
                key={item.label}
                onClick={() => navigate(item.route)}
                className={cn(
                  "group flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-[12.5px] transition-colors",
                  active
                    ? "bg-sidebar-accent text-sidebar-accent-foreground"
                    : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground",
                )}
              >
                <Icon className="h-3.5 w-3.5 shrink-0" />
                <span className="flex-1 truncate">{item.label}</span>
                {item.hint ? (
                  <span className="font-mono text-[10px] text-muted-foreground/60 group-hover:text-muted-foreground">
                    {item.hint}
                  </span>
                ) : null}
              </button>
            )
          })}
        </div>

        {showRegistrySub ? (
          <div className="mx-2 mt-1 ml-5 border-l border-sidebar-border pl-1">
            {REGISTRY_SUB.map((s) => {
              const active =
                route.name === "registry" && route.kind === s.kind
              const Icon = s.icon
              return (
                <button
                  key={s.label}
                  onClick={() => navigate({ name: "registry", kind: s.kind })}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-[12px] transition-colors",
                    active
                      ? "bg-sidebar-accent text-sidebar-accent-foreground"
                      : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground",
                  )}
                >
                  <Icon className="h-3 w-3 shrink-0" />
                  <span className="flex-1 truncate">{s.label}</span>
                </button>
              )
            })}
          </div>
        ) : null}

        <div className="mt-4 px-3 pb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70">
          Account
        </div>
        <div className="px-2">
          {ACCOUNT.map((item) => {
            const Icon = item.icon
            const active = item.match(route)
            return (
              <button
                key={item.label}
                onClick={() => navigate(item.route)}
                className={cn(
                  "flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-[12.5px] transition-colors",
                  active
                    ? "bg-sidebar-accent text-sidebar-accent-foreground"
                    : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground",
                )}
              >
                <Icon className="h-3.5 w-3.5 shrink-0" />
                <span className="flex-1 truncate">{item.label}</span>
              </button>
            )
          })}
        </div>
      </nav>

      <div className="border-t border-sidebar-border p-2">
        <div className="flex items-center gap-2 rounded-md bg-sidebar-accent/50 px-2 py-1.5">
          <div className="grid h-6 w-6 place-items-center rounded-full bg-info/20 text-[10px] font-medium text-info">
            KS
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate text-[11.5px] font-medium leading-tight">
              kira@bucephalus.dev
            </div>
            <div className="text-[10px] text-muted-foreground leading-tight">
              Pro plan
            </div>
          </div>
          <KeyRound className="h-3.5 w-3.5 text-muted-foreground" />
        </div>
        <div className="mt-1.5 flex items-center justify-between px-1 text-[10px] text-muted-foreground">
          <div className="flex items-center gap-1">
            <span className="inline-block h-1.5 w-1.5 rounded-full bg-success" />
            <span>All systems normal</span>
          </div>
          <span className="font-mono">v0.7.4</span>
        </div>
      </div>
    </aside>
  )
}

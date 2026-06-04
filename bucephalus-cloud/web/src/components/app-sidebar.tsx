import { useEffect, useMemo, useState } from "react"
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
import { probeCloudConnection, type CloudProbeResult } from "@/lib/supabase"
import { cn } from "@/lib/utils"
import { useWorkspacePreferences } from "@/lib/workspace"

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
  { label: "Packages", kind: "experiment_package" as const, icon: Package },
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
  const workspace = useWorkspacePreferences()
  const [connectionResults, setConnectionResults] = useState<CloudProbeResult[]>([])
  const [checkingConnection, setCheckingConnection] = useState(false)
  const initials = workspace.name
    .split(/\s|\/|-/)
    .map((part) => part[0]?.toUpperCase())
    .join("")
    .slice(0, 2) || "B"
  const connection = useMemo(
    () => sidebarConnectionStatus(connectionResults, checkingConnection, Boolean(workspace.userToken)),
    [checkingConnection, connectionResults, workspace.userToken],
  )

  useEffect(() => {
    let alive = true
    setCheckingConnection(true)
    void probeCloudConnection({ apiBase: workspace.apiBase, userToken: workspace.userToken })
      .then((results) => {
        if (alive) setConnectionResults(results)
      })
      .finally(() => {
        if (alive) setCheckingConnection(false)
      })
    return () => {
      alive = false
    }
  }, [workspace.apiBase, workspace.userToken])

  return (
    <aside className="flex h-full w-12 shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground md:w-56">
      <div className="flex h-11 items-center justify-center gap-2 border-b border-sidebar-border px-2 md:justify-start md:px-3">
        <div className="grid h-6 w-6 place-items-center rounded-md bg-brand text-brand-foreground">
          <span className="font-mono text-[11px] font-bold">B</span>
        </div>
        <div className="hidden min-w-0 flex-1 md:block">
          <div className="text-xs font-semibold tracking-tight">Bucephalus</div>
          <div className="text-[10px] text-muted-foreground leading-none">{workspace.slug}</div>
        </div>
      </div>

      <nav className="flex-1 overflow-y-auto scrollbar-thin py-1.5">
        <div className="px-1.5 md:px-2">
          {PRIMARY.map((item) => {
            const Icon = item.icon
            const active = item.match(route)
            return (
              <button
                key={item.label}
                onClick={() => navigate(item.route)}
                className={cn(
                  "group flex w-full items-center justify-center gap-2 rounded-md px-2 py-1 text-left text-[12.5px] transition-colors md:justify-start",
                  active
                    ? "bg-sidebar-accent text-sidebar-accent-foreground"
                    : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground",
                )}
                title={item.label}
              >
                <Icon className="h-3.5 w-3.5 shrink-0" />
                <span className="hidden flex-1 truncate md:inline">{item.label}</span>
                {item.hint ? (
                  <span className="hidden font-mono text-[10px] text-muted-foreground/60 group-hover:text-muted-foreground md:inline">
                    {item.hint}
                  </span>
                ) : null}
              </button>
            )
          })}
        </div>

        {showRegistrySub ? (
          <div className="mx-1.5 mt-1 border-l border-sidebar-border pl-1 md:mx-2 md:ml-5">
            {REGISTRY_SUB.map((s) => {
              const active =
                route.name === "registry" && route.kind === s.kind
              const Icon = s.icon
              return (
                <button
                  key={s.label}
                  onClick={() => navigate({ name: "registry", kind: s.kind })}
                  className={cn(
                    "flex w-full items-center justify-center gap-2 rounded-md px-2 py-1 text-left text-[12px] transition-colors md:justify-start",
                    active
                      ? "bg-sidebar-accent text-sidebar-accent-foreground"
                      : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground",
                  )}
                  title={s.label}
                >
                  <Icon className="h-3 w-3 shrink-0" />
                  <span className="hidden flex-1 truncate md:inline">{s.label}</span>
                </button>
              )
            })}
          </div>
        ) : null}

        <div className="mt-4 hidden px-3 pb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70 md:block">
          Account
        </div>
        <div className="px-1.5 md:px-2">
          {ACCOUNT.map((item) => {
            const Icon = item.icon
            const active = item.match(route)
            return (
              <button
                key={item.label}
                onClick={() => navigate(item.route)}
                className={cn(
                  "flex w-full items-center justify-center gap-2 rounded-md px-2 py-1 text-left text-[12.5px] transition-colors md:justify-start",
                  active
                    ? "bg-sidebar-accent text-sidebar-accent-foreground"
                    : "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground",
                )}
                title={item.label}
              >
                <Icon className="h-3.5 w-3.5 shrink-0" />
                <span className="hidden flex-1 truncate md:inline">{item.label}</span>
              </button>
            )
          })}
        </div>
      </nav>

      <div className="border-t border-sidebar-border p-1.5 md:p-2">
        <div className="flex items-center justify-center gap-2 rounded-md bg-sidebar-accent/50 px-1 py-1.5 md:justify-start md:px-2">
          <div className="grid h-6 w-6 place-items-center rounded-full bg-info/20 text-[10px] font-medium text-info">
            {initials}
          </div>
          <div className="hidden min-w-0 flex-1 md:block">
            <div className="truncate text-[11.5px] font-medium leading-tight">
              {workspace.name}
            </div>
            <div className="text-[10px] text-muted-foreground leading-tight">
              {workspace.userToken ? "token configured" : "anonymous API"}
            </div>
          </div>
          <KeyRound className="hidden h-3.5 w-3.5 text-muted-foreground md:block" />
        </div>
        <button
          className="mt-1.5 hidden w-full items-center justify-between rounded px-1 py-0.5 text-left text-[10px] text-muted-foreground hover:bg-sidebar-accent/50 hover:text-sidebar-accent-foreground md:flex"
          onClick={() => navigate({ name: "settings" })}
          title={connection.title}
        >
          <div className="flex items-center gap-1">
            <span className={cn("inline-block h-1.5 w-1.5 rounded-full", connection.dot)} />
            <span>{connection.label}</span>
          </div>
          <span className="font-mono">{workspace.defaultRegion}</span>
        </button>
      </div>
    </aside>
  )
}

function sidebarConnectionStatus(results: CloudProbeResult[], checking: boolean, hasToken: boolean) {
  if (checking && results.length === 0) {
    return {
      label: "Checking API",
      title: "Checking cloud API reachability",
      dot: "bg-warning animate-pulse",
    }
  }
  if (results.length === 0) {
    return {
      label: "API unknown",
      title: "Open Settings to check the cloud API",
      dot: "bg-muted-foreground",
    }
  }
  const failures = results.filter((result) => !result.ok)
  if (failures.length === 0) {
    return {
      label: "API reachable",
      title: "Cloud API endpoints are reachable",
      dot: "bg-success",
    }
  }
  if (failures.length === results.length) {
    const authFailed = failures.some((result) => result.message.toLowerCase().includes("oauth"))
    return {
      label: authFailed ? "API auth required" : "API unavailable",
      title: authFailed
        ? hasToken
          ? "The configured token was rejected by the cloud API"
          : "Add an OAuth bearer token in Settings"
        : failures[0]?.message ?? "Cloud API request failed",
      dot: authFailed ? "bg-warning" : "bg-destructive",
    }
  }
  return {
    label: "API degraded",
    title: `${results.length - failures.length}/${results.length} API checks passed`,
    dot: "bg-warning",
  }
}

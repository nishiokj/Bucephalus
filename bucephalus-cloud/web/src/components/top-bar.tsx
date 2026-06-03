import {
  Bell,
  ChevronRight,
  Command,
  Plus,
  Search,
  Slash,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { useRouter } from "@/lib/router"
import { ModeToggle } from "@/components/mode-toggle"

function useCrumbs() {
  const { route } = useRouter()
  const crumbs: { label: string; mono?: boolean }[] = [
    { label: "kira" },
    { label: "prod" },
  ]
  switch (route.name) {
    case "home":
      crumbs.push({ label: "overview" })
      break
    case "registry":
      crumbs.push({ label: "registry" })
      if (route.kind) crumbs.push({ label: route.kind })
      break
    case "experiments":
      crumbs.push({ label: "experiments" })
      break
    case "experiment-new":
      crumbs.push({ label: "experiments" }, { label: "new", mono: true })
      break
    case "experiment-detail":
      crumbs.push({ label: "experiments" }, { label: route.id.slice(0, 8), mono: true })
      break
    case "runs":
      crumbs.push({ label: "runs" })
      break
    case "run-detail":
      crumbs.push({ label: "runs" }, { label: route.id.slice(0, 8), mono: true })
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

export function TopBar() {
  const crumbs = useCrumbs()
  const { navigate, route } = useRouter()

  const showNewBtn =
    route.name === "experiments" ||
    route.name === "experiment-detail" ||
    route.name === "experiment-new" ||
    route.name === "home"

  return (
    <header className="sticky top-0 z-20 flex h-11 shrink-0 items-center gap-2 border-b border-border bg-background/80 px-3 backdrop-blur">
      <nav className="flex items-center gap-1 text-[12.5px]">
        {crumbs.map((c, i) => (
          <span key={i} className="flex items-center gap-1">
            {i > 0 ? (
              <Slash className="h-3 w-3 -rotate-12 text-muted-foreground/60" />
            ) : null}
            <span
              className={
                i === crumbs.length - 1
                  ? "font-medium text-foreground"
                  : "text-muted-foreground hover:text-foreground"
              }
              style={c.mono ? { fontFamily: "var(--font-mono)" } : undefined}
            >
              {c.label}
            </span>
          </span>
        ))}
      </nav>

      <div className="ml-auto flex items-center gap-1.5">
        <button className="hidden items-center gap-2 rounded-md border border-border bg-card px-2 py-1 text-[12px] text-muted-foreground hover:bg-accent md:flex">
          <Search className="h-3.5 w-3.5" />
          <span>Search resources, runs, traces</span>
          <span className="ml-8 inline-flex items-center gap-0.5 rounded border border-border bg-muted px-1 font-mono text-[10px]">
            <Command className="h-2.5 w-2.5" />K
          </span>
        </button>
        <Button
          size="sm"
          variant="ghost"
          className="h-7 w-7 p-0 text-muted-foreground"
        >
          <Bell className="h-3.5 w-3.5" />
        </Button>
        <ModeToggle />
        {showNewBtn ? (
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={() => navigate({ name: "experiment-new" })}
          >
            <Plus className="h-3.5 w-3.5" />
            New experiment
            <ChevronRight className="h-3 w-3 opacity-60" />
          </Button>
        ) : null}
      </div>
    </header>
  )
}

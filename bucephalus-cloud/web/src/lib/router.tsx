import * as React from "react"

export type Route =
  | { name: "home" }
  | { name: "registry"; kind?: "agent" | "benchmark" | "mcp" | "experiment_package" }
  | { name: "experiments" }
  | { name: "experiment-new" }
  | { name: "experiment-detail"; id: string }
  | { name: "runs" }
  | { name: "run-detail"; id: string }
  | { name: "compare" }
  | { name: "billing" }
  | { name: "settings" }
  | { name: "team" }

type Ctx = {
  route: Route
  navigate: (r: Route) => void
}

const RouterCtx = React.createContext<Ctx | null>(null)

export function RouterProvider({ children }: { children: React.ReactNode }) {
  const [route, setRoute] = React.useState<Route>(() => routeFromHash())

  React.useEffect(() => {
    const onHashChange = () => setRoute(routeFromHash())
    window.addEventListener("hashchange", onHashChange)
    return () => window.removeEventListener("hashchange", onHashChange)
  }, [])

  const navigate = React.useCallback((next: Route) => {
    const hash = routeToHash(next)
    if (window.location.hash === hash) {
      setRoute(next)
      return
    }
    window.location.hash = hash
  }, [])

  const value = React.useMemo(() => ({ route, navigate }), [navigate, route])
  return <RouterCtx.Provider value={value}>{children}</RouterCtx.Provider>
}

export function useRouter() {
  const ctx = React.useContext(RouterCtx)
  if (!ctx) throw new Error("useRouter must be used within RouterProvider")
  return ctx
}

function routeFromHash(): Route {
  const raw = window.location.hash.replace(/^#\/?/, "")
  const parts = raw.split("/").filter(Boolean).map(decodeURIComponent)
  const [top, second] = parts

  if (top === "registry") {
    const kind = second === "agent" || second === "benchmark" || second === "mcp" || second === "experiment_package" ? second : undefined
    return { name: "registry", kind }
  }
  if (top === "experiments") {
    if (second === "new") return { name: "experiment-new" }
    if (second) return { name: "experiment-detail", id: second }
    return { name: "experiments" }
  }
  if (top === "runs") {
    if (second) return { name: "run-detail", id: second }
    return { name: "runs" }
  }
  if (top === "compare") return { name: "compare" }
  if (top === "billing") return { name: "billing" }
  if (top === "settings") return { name: "settings" }
  if (top === "team") return { name: "team" }
  return { name: "home" }
}

function routeToHash(route: Route): string {
  switch (route.name) {
    case "home":
      return "#/overview"
    case "registry":
      return route.kind ? `#/registry/${route.kind}` : "#/registry"
    case "experiments":
      return "#/experiments"
    case "experiment-new":
      return "#/experiments/new"
    case "experiment-detail":
      return `#/experiments/${encodeURIComponent(route.id)}`
    case "runs":
      return "#/runs"
    case "run-detail":
      return `#/runs/${encodeURIComponent(route.id)}`
    case "compare":
      return "#/compare"
    case "billing":
      return "#/billing"
    case "settings":
      return "#/settings"
    case "team":
      return "#/team"
  }
}

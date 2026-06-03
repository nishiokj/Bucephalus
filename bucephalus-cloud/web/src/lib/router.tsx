import * as React from "react"

export type Route =
  | { name: "home" }
  | { name: "registry"; kind?: "agent" | "benchmark" | "mcp" }
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
  const [route, setRoute] = React.useState<Route>({ name: "home" })
  const value = React.useMemo(() => ({ route, navigate: setRoute }), [route])
  return <RouterCtx.Provider value={value}>{children}</RouterCtx.Provider>
}

export function useRouter() {
  const ctx = React.useContext(RouterCtx)
  if (!ctx) throw new Error("useRouter must be used within RouterProvider")
  return ctx
}

import { AppSidebar } from "@/components/app-sidebar"
import { TopBar } from "@/components/top-bar"
import { Button } from "@/components/ui/button"
import { GoogleSignInButton, useAuth } from "@/lib/auth"
import { configuredCloudApiBase } from "@/lib/cloud-api"
import { RouterProvider, useRouter } from "@/lib/router"
import { HomePage } from "@/pages/home"
import { RegistryPage } from "@/pages/registry"
import {
  ExperimentsPage,
  NewExperimentPage,
  ExperimentDetailPage,
} from "@/pages/experiments"
import { RunsPage, RunDetailPage } from "@/pages/runs"
import { ComparePage } from "@/pages/compare"
import { SettingsPage, BillingPage, TeamPage } from "@/pages/account"
import { Activity, AlertCircle, CheckCircle2, Database, KeyRound, Server, ShieldCheck } from "lucide-react"
import { useWorkspacePreferences } from "@/lib/workspace"
import { cn } from "@/lib/utils"

function RoutedPage() {
  const { route } = useRouter()
  switch (route.name) {
    case "home":
      return <HomePage />
    case "registry":
      return <RegistryPage />
    case "experiments":
      return <ExperimentsPage />
    case "experiment-new":
      return <NewExperimentPage />
    case "experiment-detail":
      return <ExperimentDetailPage />
    case "runs":
      return <RunsPage />
    case "run-detail":
      return <RunDetailPage />
    case "compare":
      return <ComparePage />
    case "settings":
      return <SettingsPage />
    case "billing":
      return <BillingPage />
    case "team":
      return <TeamPage />
  }
}

export function App() {
  const auth = useAuth()

  if (auth.status !== "signed_in") {
    return <SignInScreen />
  }

  return (
    <RouterProvider>
      <div className="flex h-screen w-screen overflow-hidden bg-background text-foreground">
        <AppSidebar />
        <main className="flex w-0 min-w-0 flex-1 flex-col overflow-hidden">
          <TopBar />
          <div className="flex-1 overflow-x-hidden overflow-y-auto scrollbar-thin">
            <RoutedPage />
          </div>
        </main>
      </div>
    </RouterProvider>
  )
}

export default App

function SignInScreen() {
  const auth = useAuth()
  const workspace = useWorkspacePreferences()
  const apiHost = safeHost(configuredCloudApiBase(workspace.apiBase)) || "not configured"
  const authRows = [
    {
      label: "Google client",
      value: auth.configured ? "configured" : "missing",
      tone: auth.configured ? "success" : "danger",
      detail: auth.configured ? "Sign-in is available" : "Ask an operator to configure sign-in",
    },
    {
      label: "Identity script",
      value: auth.ready ? "ready" : auth.configured ? "loading" : "blocked",
      tone: auth.ready ? "success" : auth.configured ? "warning" : "muted",
      detail: auth.ready ? "Google prompt available" : auth.configured ? "waiting on accounts.google.com" : "client id required first",
    },
    {
      label: "Session storage",
      value: "browser tab",
      tone: "success",
      detail: "Credentials clear when this tab closes",
    },
  ] as const

  return (
    <main className="grid min-h-screen bg-background text-foreground lg:grid-cols-[minmax(0,1fr)_400px]">
      <section className="flex min-h-[55vh] flex-col border-b border-border bg-background lg:min-h-screen lg:border-b-0 lg:border-r">
        <header className="flex h-14 items-center justify-between border-b border-border px-4 lg:px-6">
          <div className="flex min-w-0 items-center gap-2">
            <div className="grid h-8 w-8 shrink-0 place-items-center rounded-md bg-brand text-brand-foreground">
              <span className="font-mono text-[13px] font-bold">B</span>
            </div>
            <div className="min-w-0">
              <div className="truncate text-sm font-semibold">Bucephalus Cloud</div>
              <div className="truncate text-[11px] text-muted-foreground">{workspace.slug} / sign-in</div>
            </div>
          </div>
          <div className="hidden items-center gap-1 rounded border border-border bg-card px-2 py-1 text-[11px] text-muted-foreground sm:inline-flex">
            <ShieldCheck className="h-3.5 w-3.5 text-success" />
            Google OAuth
          </div>
        </header>

        <div className="grid flex-1 grid-cols-1 gap-px bg-border lg:grid-cols-[minmax(0,1fr)_280px]">
          <div className="min-w-0 bg-background p-4 lg:p-6">
            <div className="mb-4 max-w-2xl">
              <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Workspace access</div>
              <h1 className="mt-1 text-2xl font-semibold leading-tight sm:text-3xl">
                Sign in before touching cloud experiments.
              </h1>
              <p className="mt-2 max-w-xl text-[13px] leading-5 text-muted-foreground">
                Sign in with an authorized Google account to load registry items, runs, and traces for this workspace.
              </p>
            </div>

            <div className="grid grid-cols-1 gap-px overflow-hidden rounded-md border border-border bg-border md:grid-cols-3">
              {authRows.map((row) => (
                <AuthGateFact key={row.label} {...row} />
              ))}
            </div>

            <div className="mt-4 overflow-hidden rounded-md border border-border bg-card">
              <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center border-b border-border px-3 py-2">
                <div className="min-w-0">
                  <div className="text-[10px] uppercase tracking-wide text-muted-foreground">API preview</div>
                  <div className="mt-0.5 truncate font-mono text-[12px]">{apiHost}</div>
                </div>
                <span className="rounded border border-warning/25 bg-warning/10 px-1.5 py-0.5 font-mono text-[10px] text-warning">
                  locked
                </span>
              </div>
              <div className="grid grid-cols-1 gap-px bg-border sm:grid-cols-3">
                <PreviewMetric icon={Database} label="Registry" value="packages" detail="requires token" />
                <PreviewMetric icon={Activity} label="Runs" value="telemetry" detail="requires token" />
                <PreviewMetric icon={Server} label="Runtime" value={workspace.defaultRegion} detail="workspace default" />
              </div>
              <div className="grid gap-px bg-border">
                {[
                  ["Sign-in provider", "Google"],
                  ["Access", "workspace-scoped"],
                  ["Session", "clears when this tab closes"],
                ].map(([label, value]) => (
                  <div key={label} className="grid grid-cols-[140px_minmax(0,1fr)] gap-3 bg-card px-3 py-2 text-[11px]">
                    <span className="text-muted-foreground">{label}</span>
                    <span className="truncate font-mono">{value}</span>
                  </div>
                ))}
              </div>
            </div>
          </div>

          <aside className="bg-card p-4 lg:p-5">
            <div className="mb-3 flex items-center gap-2">
              <KeyRound className="h-4 w-4 text-brand" />
              <div>
                <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Auth status</div>
                <div className="text-[13px] font-semibold">{auth.status === "loading" ? "Preparing Google" : "Signed out"}</div>
              </div>
            </div>
            <div className="grid gap-2">
              <GoogleSignInButton />
              <Button variant="outline" className="h-9 w-full justify-between text-[12px]" onClick={auth.prompt} disabled={!auth.ready}>
                Try One Tap
                <span className={cn("h-1.5 w-1.5 rounded-full", auth.ready ? "bg-success" : "bg-muted-foreground")} />
              </Button>
            </div>
            {auth.error ? (
              <div className="mt-4 flex items-start gap-2 rounded border border-warning/30 bg-warning/10 p-3 text-[12px] leading-relaxed text-warning">
                <AlertCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>{auth.error}</span>
              </div>
            ) : null}
            {!auth.configured ? (
              <div className="mt-4 rounded border border-destructive/25 bg-destructive/10 p-3">
                <div className="flex items-center gap-1.5 text-[12.5px] font-medium text-destructive">
                  <AlertCircle className="h-3.5 w-3.5" />
                  Missing client ID
                </div>
                <div className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
                  Google sign-in is not configured for this deployment. Ask an operator to finish the console setup.
                </div>
              </div>
            ) : null}
          </aside>
        </div>
      </section>

      <section className="flex min-h-[45vh] items-stretch bg-card lg:min-h-screen">
        <div className="flex w-full flex-col">
          <header className="border-b border-border px-5 py-4">
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Access contract</div>
            <h2 className="mt-1 text-lg font-semibold">No anonymous console mode</h2>
            <p className="mt-1 text-[12px] leading-5 text-muted-foreground">
              Sign-in is required before loading workspace data. This keeps the UI honest: no stale mock state, no guessed registry, no hidden demo data.
            </p>
          </header>
          <div className="grid gap-px bg-border">
            <AuthContractRow ok={auth.configured} label="OAuth client configured" value={auth.configured ? "ready" : "missing"} />
            <AuthContractRow ok={auth.ready} label="Google prompt ready" value={auth.ready ? "available" : auth.configured ? "loading" : "blocked"} />
            <AuthContractRow ok label="Token storage" value="session only" />
            <AuthContractRow ok label="API endpoint" value={apiHost} />
          </div>
        </div>
      </section>
    </main>
  )
}

function AuthGateFact({
  label,
  value,
  detail,
  tone,
}: {
  label: string
  value: string
  detail: string
  tone: "success" | "warning" | "danger" | "muted"
}) {
  return (
    <div className="min-w-0 bg-card p-3">
      <div className="flex items-center justify-between gap-2">
        <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
        <span className={cn("h-1.5 w-1.5 rounded-full", authToneDot(tone))} />
      </div>
      <div className={cn("mt-0.5 truncate font-mono text-[13px]", tone === "danger" ? "text-destructive" : tone === "warning" ? "text-warning" : "")}>{value}</div>
      <div className="truncate text-[10.5px] text-muted-foreground" title={detail}>{detail}</div>
    </div>
  )
}

function PreviewMetric({
  icon: Icon,
  label,
  value,
  detail,
}: {
  icon: React.ComponentType<{ className?: string }>
  label: string
  value: string
  detail: string
}) {
  return (
    <div className="min-w-0 bg-card p-3">
      <div className="flex items-center gap-1.5 text-muted-foreground">
        <Icon className="h-3.5 w-3.5" />
        <span className="text-[10px] uppercase tracking-wide">{label}</span>
      </div>
      <div className="mt-1 truncate font-mono text-[13px]">{value}</div>
      <div className="truncate text-[10.5px] text-muted-foreground">{detail}</div>
    </div>
  )
}

function AuthContractRow({ ok, label, value }: { ok: boolean; label: string; value: string }) {
  return (
    <div className="grid grid-cols-[18px_minmax(0,1fr)_minmax(80px,auto)] items-center gap-2 bg-card px-4 py-2.5 text-[12px]">
      {ok ? <CheckCircle2 className="h-3.5 w-3.5 text-success" /> : <AlertCircle className="h-3.5 w-3.5 text-warning" />}
      <span className="truncate text-muted-foreground">{label}</span>
      <span className="truncate text-right font-mono">{value}</span>
    </div>
  )
}

function authToneDot(tone: "success" | "warning" | "danger" | "muted") {
  if (tone === "success") return "bg-success"
  if (tone === "warning") return "bg-warning"
  if (tone === "danger") return "bg-destructive"
  return "bg-muted-foreground"
}

function safeHost(value: string) {
  if (!value) return ""
  try {
    return new URL(value).host
  } catch {
    return value
  }
}

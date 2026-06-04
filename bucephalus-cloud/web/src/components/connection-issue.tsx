import { CircleAlert, KeyRound, RefreshCw, Settings, WifiOff } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

export function ConnectionIssue({
  title = "Cloud API request failed",
  detail,
  onSettings,
  onRetry,
  compact = false,
}: {
  title?: string
  detail: string
  onSettings: () => void
  onRetry?: () => void
  compact?: boolean
}) {
  const diagnosis = diagnoseConnectionIssue(detail)
  const Icon = diagnosis.kind === "auth" ? KeyRound : diagnosis.kind === "network" ? WifiOff : CircleAlert

  return (
    <div
      className={cn(
        "border-b border-border bg-background px-3",
        compact ? "py-4" : "py-8",
      )}
    >
      <div
        className={cn(
          "mx-auto grid w-full max-w-5xl gap-px overflow-hidden rounded-md border border-border bg-border",
          compact ? "grid-cols-1 lg:grid-cols-[minmax(0,1fr)_260px]" : "grid-cols-1 lg:grid-cols-[minmax(0,1fr)_320px]",
        )}
      >
        <div className="min-w-0 bg-card p-3">
          <div className="flex min-w-0 items-start gap-2">
            <span className={cn("mt-0.5 grid h-6 w-6 shrink-0 place-items-center rounded", diagnosis.tone)}>
              <Icon className="h-3.5 w-3.5" />
            </span>
            <div className="min-w-0">
              <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{diagnosis.eyebrow}</div>
              <div className="mt-0.5 text-[14px] font-medium">{title}</div>
              <div className="mt-1 max-w-2xl text-[12px] leading-relaxed text-muted-foreground">
                {diagnosis.summary}
              </div>
            </div>
          </div>
          <div className="mt-3 grid grid-cols-1 gap-px bg-border sm:grid-cols-3">
            {diagnosis.facts.map((fact) => (
              <div key={fact.label} className="min-w-0 bg-background px-2 py-1.5">
                <div className="truncate text-[9.5px] uppercase tracking-wide text-muted-foreground">{fact.label}</div>
                <div className="truncate font-mono text-[11.5px] text-foreground">{fact.value}</div>
              </div>
            ))}
          </div>
          <div className="mt-3 max-h-20 overflow-auto rounded border border-border bg-background p-2 font-mono text-[10.5px] leading-relaxed text-muted-foreground">
            {detail}
          </div>
        </div>

        <div className="flex min-w-0 flex-col justify-between gap-3 bg-card p-3">
          <div>
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Next step</div>
            <div className="mt-1 text-[12.5px] font-medium">{diagnosis.actionTitle}</div>
            <div className="mt-1 text-[11px] leading-relaxed text-muted-foreground">{diagnosis.actionDetail}</div>
          </div>
          <div className="flex flex-wrap items-center gap-1.5">
            <Button size="sm" className="h-7 gap-1 bg-brand text-[12px] text-brand-foreground hover:bg-brand/90" onClick={onSettings}>
              <Settings className="h-3 w-3" />
              Settings
            </Button>
            {onRetry ? (
              <Button size="sm" variant="outline" className="h-7 gap-1 text-[12px]" onClick={onRetry}>
                <RefreshCw className="h-3 w-3" />
                Retry
              </Button>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  )
}

type ConnectionKind = "auth" | "network" | "server" | "runtime"

function diagnoseConnectionIssue(detail: string): {
  kind: ConnectionKind
  eyebrow: string
  summary: string
  actionTitle: string
  actionDetail: string
  tone: string
  facts: { label: string; value: string }[]
} {
  const normalized = detail.toLowerCase()
  const kind: ConnectionKind =
    /auth|oauth|bearer|token|401|403|unauthorized|forbidden/.test(normalized)
      ? "auth"
      : /runtime|metric observations|trace events|evidence/.test(normalized)
        ? "runtime"
      : /failed to fetch|network|dns|cors|timeout|enotfound|econnrefused|load failed/.test(normalized)
        ? "network"
        : "server"
  const baseFacts = [
    { label: "Class", value: kind },
    { label: "Recovery", value: kind === "network" ? "check host" : kind === "auth" ? "check token" : "retry safe" },
    { label: "Source", value: "cloud API" },
  ]

  if (kind === "auth") {
    return {
      kind,
      eyebrow: "Authentication",
      summary: "The cloud API is reachable, but this request needs a valid bearer token or refreshed credentials.",
      actionTitle: "Update the local token",
      actionDetail: "Open Settings, paste a valid user token, save, then retry this request.",
      tone: "bg-warning/10 text-warning",
      facts: baseFacts,
    }
  }
  if (kind === "network") {
    return {
      kind,
      eyebrow: "Connectivity",
      summary: "The browser could not reach the configured API host. This is usually a host, CORS, DNS, or local-network issue.",
      actionTitle: "Verify the API base URL",
      actionDetail: "Check the API host in Settings, then run the endpoint diagnostics before retrying.",
      tone: "bg-destructive/10 text-destructive",
      facts: baseFacts,
    }
  }
  if (kind === "runtime") {
    return {
      kind,
      eyebrow: "Runtime evidence",
      summary: "The core record loaded, but runtime metrics or trace evidence did not return for this view.",
      actionTitle: "Retry evidence, then inspect Settings",
      actionDetail: "Retry first. If it repeats, use Settings diagnostics to confirm the run endpoints are reachable.",
      tone: "bg-info/10 text-info",
      facts: baseFacts,
    }
  }
  return {
    kind,
    eyebrow: "API response",
    summary: "The cloud API returned an error for this request. The view is holding its state instead of showing guessed data.",
    actionTitle: "Retry or inspect diagnostics",
    actionDetail: "Retry the request. If the error persists, open Settings to check registry, package, and run endpoints.",
    tone: "bg-warning/10 text-warning",
    facts: baseFacts,
  }
}

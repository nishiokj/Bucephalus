import { cn } from "@/lib/utils"

type StatusType = "ready" | "building" | "failed" | "queued" | "running" | "succeeded"

const VARIANTS: Record<StatusType, { dot: string; text: string; bg: string }> = {
  ready: { dot: "bg-success", text: "text-success", bg: "bg-success/10" },
  succeeded: { dot: "bg-success", text: "text-success", bg: "bg-success/10" },
  building: { dot: "bg-warning animate-pulse", text: "text-warning", bg: "bg-warning/10" },
  running: { dot: "bg-info animate-pulse", text: "text-info", bg: "bg-info/10" },
  queued: { dot: "bg-muted-foreground", text: "text-muted-foreground", bg: "bg-muted/40" },
  failed: { dot: "bg-destructive", text: "text-destructive", bg: "bg-destructive/10" },
}

export function StatusPill({
  status,
  withDot = true,
  className,
}: {
  status: string
  withDot?: boolean
  className?: string
}) {
  const v = VARIANTS[status as StatusType] ?? VARIANTS.queued
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10.5px] font-medium",
        v.bg,
        v.text,
        className,
      )}
    >
      {withDot ? <span className={cn("h-1.5 w-1.5 rounded-full", v.dot)} /> : null}
      {status}
    </span>
  )
}

export function KindBadge({ kind, className }: { kind: string; className?: string }) {
  const map: Record<string, { label: string; tone: string }> = {
    agent: { label: "agent", tone: "bg-info/10 text-info border-info/20" },
    agent_app: { label: "agent", tone: "bg-info/10 text-info border-info/20" },
    benchmark: {
      label: "bench",
      tone: "bg-chart-3/10 text-chart-3 border-chart-3/20",
    },
    case: { label: "case", tone: "bg-chart-3/10 text-chart-3 border-chart-3/20" },
    dataset: { label: "data", tone: "bg-chart-2/10 text-chart-2 border-chart-2/20" },
    experiment_package: { label: "pkg", tone: "bg-brand/10 text-brand border-brand/20" },
    grader: { label: "grader", tone: "bg-chart-4/10 text-chart-4 border-chart-4/20" },
    mcp: { label: "mcp", tone: "bg-chart-2/10 text-chart-2 border-chart-2/20" },
    metric: { label: "metric", tone: "bg-success/10 text-success border-success/20" },
    runtime_profile: { label: "runtime", tone: "bg-warning/10 text-warning border-warning/20" },
    task_boundary: { label: "task", tone: "bg-muted text-muted-foreground border-border" },
    trial_contract: { label: "contract", tone: "bg-muted text-muted-foreground border-border" },
    variant: { label: "variant", tone: "bg-info/10 text-info border-info/20" },
  }
  const v = map[kind] ?? { label: kind, tone: "bg-muted text-muted-foreground border-border" }
  return (
    <span
      className={cn(
        "inline-flex max-w-full items-center truncate rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide",
        v.tone,
        className,
      )}
    >
      {v.label}
    </span>
  )
}

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
    benchmark: {
      label: "benchmark",
      tone: "bg-chart-3/10 text-chart-3 border-chart-3/20",
    },
    mcp: { label: "mcp", tone: "bg-chart-2/10 text-chart-2 border-chart-2/20" },
  }
  const v = map[kind] ?? { label: kind, tone: "bg-muted text-muted-foreground" }
  return (
    <span
      className={cn(
        "inline-flex items-center rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide",
        v.tone,
        className,
      )}
    >
      {v.label}
    </span>
  )
}

export function formatBytes(b: number) {
  if (b < 1024) return `${b} B`
  const u = ["KB", "MB", "GB", "TB"]
  let i = -1
  let v = b
  do {
    v /= 1024
    i++
  } while (v >= 1024 && i < u.length - 1)
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${u[i]}`
}

export function formatDuration(ms: number) {
  if (!ms) return "—"
  if (ms < 1000) return `${ms}ms`
  const s = Math.round(ms / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  const r = s % 60
  if (m < 60) return `${m}m ${r}s`
  const h = Math.floor(m / 60)
  const mr = m % 60
  return `${h}h ${mr}m`
}

export function formatRelative(iso: string | null) {
  if (!iso) return "—"
  const d = new Date(iso)
  const diff = (Date.now() - d.getTime()) / 1000
  if (diff < 60) return `${Math.round(diff)}s ago`
  if (diff < 3600) return `${Math.round(diff / 60)}m ago`
  if (diff < 86400) return `${Math.round(diff / 3600)}h ago`
  return `${Math.round(diff / 86400)}d ago`
}

export function formatNumber(n: number) {
  if (Math.abs(n) >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (Math.abs(n) >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return `${n}`
}

export function formatUsd(n: number) {
  return `$${n.toFixed(2)}`
}

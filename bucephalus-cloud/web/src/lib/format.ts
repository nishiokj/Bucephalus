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
  if (!Number.isFinite(diff)) return "—"
  const seconds = Math.abs(diff)
  const label =
    seconds < 60
      ? `${Math.round(seconds)}s`
      : seconds < 3600
        ? `${Math.round(seconds / 60)}m`
        : seconds < 86400
          ? `${Math.round(seconds / 3600)}h`
          : `${Math.round(seconds / 86400)}d`
  return diff < 0 ? `in ${label}` : `${label} ago`
}

export function formatNumber(n: number) {
  if (Math.abs(n) >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (Math.abs(n) >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return `${n}`
}

export function formatUsd(n: number) {
  return `$${n.toFixed(2)}`
}

export function formatShortId(value: string | null | undefined) {
  if (!value) return "—"
  const clean = value.replace(/^sha256:/i, "")
  if (clean.length <= 12) return clean
  return `${clean.slice(0, 6)}...${clean.slice(-4)}`
}

export function formatReadableToken(value: string | null | undefined) {
  if (!value) return "—"
  const clean = value.replace(/^sha256:/i, "")
  if (/^[a-f0-9]{16,}$/i.test(clean)) return formatShortId(clean)
  if (/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(clean)) {
    return formatShortId(clean)
  }
  if (clean.length >= 32 && /^[A-Za-z0-9_-]+$/.test(clean) && /\d/.test(clean)) return formatShortId(clean)
  return value
}

export function formatReadableLabel(value: string | null | undefined) {
  if (!value) return "—"
  return value
    .replace(/sha256:[a-f0-9]{20,}/gi, (token) => formatShortId(token))
    .replace(/\b[a-f0-9]{24,}\b/gi, (token) => formatShortId(token))
    .replace(/\b[A-Za-z0-9_-]{32,}\b/g, (token) => (/\d/.test(token) ? formatShortId(token) : token))
}

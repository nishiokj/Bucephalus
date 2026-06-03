import { createClient } from "@supabase/supabase-js"

const url = import.meta.env.VITE_SUPABASE_URL as string
const anon = import.meta.env.VITE_SUPABASE_ANON_KEY as string

export const supabase = createClient(url, anon, {
  auth: { persistSession: true, autoRefreshToken: true },
})

export type RegistryItem = {
  id: string
  name: string
  kind: "agent" | "benchmark" | "mcp"
  version: string
  description: string
  status: "ready" | "building" | "failed"
  size_bytes: number
  tags: string[]
  owner: string
  created_at: string
}

export type Experiment = {
  id: string
  name: string
  description: string
  config: Record<string, unknown>
  tags: string[]
  owner: string
  created_at: string
}

export type Run = {
  id: string
  experiment_id: string | null
  experiment_name: string
  status: "queued" | "running" | "succeeded" | "failed"
  variant: string
  started_at: string | null
  ended_at: string | null
  duration_ms: number
  region: string
  cost_usd: number
  created_at: string
}

export type RunMetric = {
  id: string
  run_id: string
  name: string
  value: number
  unit: string
  step: number
  recorded_at: string
}

export type Trace = {
  id: string
  run_id: string
  level: "info" | "warn" | "error" | "debug"
  span: string
  message: string
  latency_ms: number
  recorded_at: string
}

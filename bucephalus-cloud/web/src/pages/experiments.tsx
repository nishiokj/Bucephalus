import { useEffect, useMemo, useState } from "react"
import {
  ArrowUpRight,
  Beaker,
  ChevronDown,
  Copy,
  Eye,
  GitBranch,
  Play,
  Plus,
  Search,
  Tag,
  XCircle,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { useRouter } from "@/lib/router"
import { supabase, type Experiment, type RegistryItem } from "@/lib/supabase"
import { formatRelative } from "@/lib/format"
import { PageHeader } from "@/pages/registry"
import { cn } from "@/lib/utils"

export function ExperimentsPage() {
  const { navigate } = useRouter()
  const [items, setItems] = useState<Experiment[]>([])
  const [q, setQ] = useState("")

  useEffect(() => {
    void supabase
      .from("experiments")
      .select("*")
      .order("created_at", { ascending: false })
      .then(({ data }) => setItems((data ?? []) as Experiment[]))
  }, [])

  const filtered = useMemo(
    () =>
      items.filter((i) =>
        q
          ? `${i.name} ${i.description} ${i.tags.join(" ")}`
              .toLowerCase()
              .includes(q.toLowerCase())
          : true,
      ),
    [items, q],
  )

  return (
    <div className="flex flex-col">
      <PageHeader
        title="Experiments"
        subtitle="Author, queue, and reproduce evals across agents and benchmarks."
        primaryAction={
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={() => navigate({ name: "experiment-new" })}
          >
            <Plus className="h-3.5 w-3.5" /> New experiment
          </Button>
        }
      />

      <div className="sticky top-11 z-10 flex items-center justify-between gap-3 border-b border-border bg-background/95 px-3 py-1.5 backdrop-blur">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Filter experiments..."
            className="h-7 w-72 pl-7 text-[12px]"
          />
        </div>
        <div className="flex items-center gap-1.5">
          <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
            owner <ChevronDown className="h-3 w-3" />
          </Button>
          <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
            tag <ChevronDown className="h-3 w-3" />
          </Button>
          <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
            sort: recent <ChevronDown className="h-3 w-3" />
          </Button>
        </div>
      </div>

      <div className="text-[12px]">
        <div
          className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
          style={{
            gridTemplateColumns:
              "minmax(220px,1.6fr) minmax(220px,1fr) minmax(160px,0.8fr) 90px 90px 80px",
          }}
        >
          <span>Experiment</span>
          <span>Description</span>
          <span>Tags</span>
          <span>Owner</span>
          <span>Updated</span>
          <span>Actions</span>
        </div>

        {filtered.map((e) => (
          <div
            key={e.id}
            className="group grid items-center gap-2 border-b border-border px-3 py-1.5 hover:bg-accent/40"
            style={{
              gridTemplateColumns:
                "minmax(220px,1.6fr) minmax(220px,1fr) minmax(160px,0.8fr) 90px 90px 80px",
            }}
          >
            <button
              onClick={() => navigate({ name: "experiment-detail", id: e.id })}
              className="flex min-w-0 items-center gap-2 text-left"
            >
              <Beaker className="h-3 w-3 shrink-0 text-brand" />
              <span className="truncate font-mono text-[12.5px] font-medium">
                {e.name}
              </span>
            </button>
            <span className="truncate text-[11.5px] text-muted-foreground">
              {e.description}
            </span>
            <div className="flex flex-wrap items-center gap-1">
              {e.tags.slice(0, 3).map((t) => (
                <span
                  key={t}
                  className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
                >
                  {t}
                </span>
              ))}
            </div>
            <span className="font-mono text-[11px] text-muted-foreground">
              {e.owner}
            </span>
            <span className="font-mono text-[11px] text-muted-foreground">
              {formatRelative(e.created_at)}
            </span>
            <div className="flex items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
              <button
                title="Run"
                onClick={() => navigate({ name: "run-detail", id: e.id })}
                className="grid h-5 w-5 place-items-center rounded text-success hover:bg-success/10"
              >
                <Play className="h-3 w-3" />
              </button>
              <button
                title="Open"
                onClick={() => navigate({ name: "experiment-detail", id: e.id })}
                className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-secondary"
              >
                <ArrowUpRight className="h-3 w-3" />
              </button>
              <button
                title="Duplicate"
                className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-secondary"
              >
                <Copy className="h-3 w-3" />
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

export function NewExperimentPage() {
  const { navigate } = useRouter()
  const [name, setName] = useState("")
  const [description, setDescription] = useState("")
  const [benchmark, setBenchmark] = useState<string | null>(null)
  const [agents, setAgents] = useState<string[]>([])
  const [mcps, setMcps] = useState<string[]>([])
  const [seeds, setSeeds] = useState("7,11,42")
  const [region, setRegion] = useState("us-east-1")
  const [maxParallel, setMaxParallel] = useState(8)
  const [budget, setBudget] = useState(50)
  const [registry, setRegistry] = useState<RegistryItem[]>([])
  const [variantSweep, setVariantSweep] = useState<{ key: string; values: string }[]>([
    { key: "temperature", values: "0.0, 0.4, 0.8" },
  ])

  useEffect(() => {
    void supabase
      .from("registry_items")
      .select("*")
      .order("name")
      .then(({ data }) => setRegistry((data ?? []) as RegistryItem[]))
  }, [])

  const benchmarks = registry.filter((r) => r.kind === "benchmark")
  const agentsList = registry.filter((r) => r.kind === "agent")
  const mcpsList = registry.filter((r) => r.kind === "mcp")

  function toggle(list: string[], v: string) {
    return list.includes(v) ? list.filter((x) => x !== v) : [...list, v]
  }

  async function submit() {
    if (!name.trim()) return
    const { data } = await supabase
      .from("experiments")
      .insert({
        name,
        description,
        tags: [],
        owner: "kira",
        config: {
          benchmark,
          agents,
          mcps,
          seeds: seeds
            .split(",")
            .map((s) => Number(s.trim()))
            .filter((n) => !Number.isNaN(n)),
          region,
          maxParallel,
          budgetUsd: budget,
          sweep: Object.fromEntries(
            variantSweep.map((s) => [s.key, s.values.split(",").map((v) => v.trim())]),
          ),
        },
      })
      .select("*")
      .maybeSingle()
    if (data) navigate({ name: "experiment-detail", id: data.id })
    else navigate({ name: "experiments" })
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title="New experiment"
        subtitle="Author once, run many. Compose registry items into a reproducible eval."
        rightSlot={
          <Button
            variant="outline"
            size="sm"
            className="h-7 text-[12px]"
            onClick={() => navigate({ name: "experiments" })}
          >
            <XCircle className="h-3 w-3" /> Cancel
          </Button>
        }
        primaryAction={
          <Button
            size="sm"
            className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90"
            onClick={submit}
          >
            <Play className="h-3 w-3" /> Save and queue
          </Button>
        }
      />

      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[minmax(0,1fr)_360px]">
        <div className="flex flex-col gap-px bg-border">
          <Section title="Identity">
            <Field label="Name" hint="Lowercase, kebab, descriptive">
              <Input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="sonnet-vs-gpt5-swe-bench"
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Description">
              <Textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What are you measuring and why?"
                className="min-h-[64px] resize-none text-[12px]"
              />
            </Field>
          </Section>

          <Section title="Benchmark">
            <RadioGrid
              options={benchmarks.map((b) => ({
                value: b.name,
                label: b.name,
                hint: b.description,
                badge: b.version,
              }))}
              value={benchmark ?? ""}
              onChange={setBenchmark}
            />
          </Section>

          <Section title="Agents" hint="Pick one or many for head-to-head">
            <CheckGrid
              options={agentsList.map((a) => ({
                value: a.name,
                label: a.name,
                hint: a.description,
                badge: a.version,
              }))}
              values={agents}
              onChange={(v) => setAgents(toggle(agents, v))}
            />
          </Section>

          <Section title="MCP servers" hint="Tools available to agents">
            <CheckGrid
              options={mcpsList.map((m) => ({
                value: m.name,
                label: m.name,
                hint: m.description,
                badge: m.version,
              }))}
              values={mcps}
              onChange={(v) => setMcps(toggle(mcps, v))}
            />
          </Section>

          <Section title="Variant sweep" hint="Cartesian over keys">
            <div className="flex flex-col gap-1.5">
              {variantSweep.map((s, i) => (
                <div key={i} className="grid grid-cols-[140px_1fr_24px] items-center gap-2">
                  <Input
                    value={s.key}
                    onChange={(e) =>
                      setVariantSweep((vs) =>
                        vs.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)),
                      )
                    }
                    placeholder="parameter"
                    className="h-7 font-mono text-[12px]"
                  />
                  <Input
                    value={s.values}
                    onChange={(e) =>
                      setVariantSweep((vs) =>
                        vs.map((x, j) => (j === i ? { ...x, values: e.target.value } : x)),
                      )
                    }
                    placeholder="0.0, 0.4, 0.8"
                    className="h-7 font-mono text-[12px]"
                  />
                  <button
                    onClick={() =>
                      setVariantSweep((vs) => vs.filter((_, j) => j !== i))
                    }
                    className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-secondary"
                  >
                    <XCircle className="h-3 w-3" />
                  </button>
                </div>
              ))}
              <Button
                variant="outline"
                size="sm"
                className="h-7 self-start text-[12px]"
                onClick={() =>
                  setVariantSweep((vs) => [...vs, { key: "", values: "" }])
                }
              >
                <Plus className="h-3 w-3" /> Add dimension
              </Button>
            </div>
          </Section>
        </div>

        <aside className="flex flex-col gap-px bg-border">
          <Section title="Compute">
            <Field label="Region">
              <Select value={region} onValueChange={setRegion}>
                <SelectTrigger className="h-8 text-[12px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="us-east-1">us-east-1</SelectItem>
                  <SelectItem value="us-west-2">us-west-2</SelectItem>
                  <SelectItem value="eu-west-1">eu-west-1</SelectItem>
                  <SelectItem value="ap-southeast-1">ap-southeast-1</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field label="Max parallel">
              <Input
                type="number"
                value={maxParallel}
                onChange={(e) => setMaxParallel(Number(e.target.value))}
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Budget cap (USD)">
              <Input
                type="number"
                value={budget}
                onChange={(e) => setBudget(Number(e.target.value))}
                className="h-8 font-mono text-[12px]"
              />
            </Field>
            <Field label="Seeds">
              <Input
                value={seeds}
                onChange={(e) => setSeeds(e.target.value)}
                placeholder="7,11,42"
                className="h-8 font-mono text-[12px]"
              />
            </Field>
          </Section>

          <Section title="Summary">
            <Summary
              label="Variants"
              value={`${
                Math.max(1, agents.length) *
                Math.max(
                  1,
                  variantSweep.reduce(
                    (acc, s) =>
                      acc *
                      Math.max(
                        1,
                        s.values.split(",").map((v) => v.trim()).filter(Boolean).length,
                      ),
                    1,
                  ),
                ) *
                Math.max(1, seeds.split(",").map((s) => s.trim()).filter(Boolean).length)
              } runs`}
            />
            <Summary
              label="Budget"
              value={`$${budget.toFixed(2)} cap`}
            />
            <Summary label="Region" value={region} />
            <Summary label="Owner" value="kira" />
          </Section>

          <Section title="Reproducibility">
            <div className="rounded border border-border bg-muted/30 p-2 font-mono text-[10.5px] text-muted-foreground">
              <div className="flex items-center justify-between">
                <span>bucephalus run create</span>
                <Copy className="h-3 w-3" />
              </div>
              <div className="mt-1 leading-tight">
                {"--name " + (name || "<name>")}
                <br />
                {"--bench " + (benchmark || "<bench>")}
                <br />
                {agents.map((a) => `--agent ${a} `).join("")}
              </div>
            </div>
          </Section>
        </aside>
      </div>
    </div>
  )
}

function Section({
  title,
  hint,
  children,
}: {
  title: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <section className="bg-background">
      <header className="flex h-8 items-center justify-between border-b border-border px-3">
        <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
          {title}
        </div>
        {hint ? (
          <span className="text-[11px] text-muted-foreground">{hint}</span>
        ) : null}
      </header>
      <div className="flex flex-col gap-2 p-3">{children}</div>
    </section>
  )
}

function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <div className="grid grid-cols-1 gap-1.5 md:grid-cols-[180px_1fr] md:items-start md:gap-3">
      <Label className="pt-1.5 text-[12px] font-medium">
        {label}
        {hint ? (
          <span className="ml-1 text-[10.5px] font-normal text-muted-foreground">
            {hint}
          </span>
        ) : null}
      </Label>
      <div>{children}</div>
    </div>
  )
}

function Summary({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between text-[12px]">
      <span className="text-muted-foreground">{label}</span>
      <span className="font-mono">{value}</span>
    </div>
  )
}

function CheckGrid({
  options,
  values,
  onChange,
}: {
  options: { value: string; label: string; hint?: string; badge?: string }[]
  values: string[]
  onChange: (v: string) => void
}) {
  return (
    <div className="grid grid-cols-1 gap-px overflow-hidden rounded-md border border-border bg-border md:grid-cols-2">
      {options.map((o) => {
        const checked = values.includes(o.value)
        return (
          <button
            key={o.value}
            onClick={() => onChange(o.value)}
            className={cn(
              "flex items-start gap-2 bg-card p-2 text-left transition-colors",
              checked ? "ring-1 ring-inset ring-brand" : "hover:bg-accent/50",
            )}
          >
            <span
              className={cn(
                "mt-0.5 grid h-3.5 w-3.5 place-items-center rounded border",
                checked ? "border-brand bg-brand" : "border-border",
              )}
            >
              {checked ? (
                <span className="h-1.5 w-1.5 rounded-sm bg-brand-foreground" />
              ) : null}
            </span>
            <span className="min-w-0 flex-1">
              <span className="flex items-center gap-1">
                <span className="truncate font-mono text-[12px]">{o.label}</span>
                {o.badge ? (
                  <span className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground">
                    {o.badge}
                  </span>
                ) : null}
              </span>
              {o.hint ? (
                <span className="block truncate text-[10.5px] leading-tight text-muted-foreground">
                  {o.hint}
                </span>
              ) : null}
            </span>
          </button>
        )
      })}
    </div>
  )
}

function RadioGrid({
  options,
  value,
  onChange,
}: {
  options: { value: string; label: string; hint?: string; badge?: string }[]
  value: string
  onChange: (v: string) => void
}) {
  return (
    <div className="grid grid-cols-1 gap-px overflow-hidden rounded-md border border-border bg-border md:grid-cols-2">
      {options.map((o) => {
        const checked = value === o.value
        return (
          <button
            key={o.value}
            onClick={() => onChange(o.value)}
            className={cn(
              "flex items-start gap-2 bg-card p-2 text-left transition-colors",
              checked ? "ring-1 ring-inset ring-brand" : "hover:bg-accent/50",
            )}
          >
            <span
              className={cn(
                "mt-0.5 grid h-3.5 w-3.5 place-items-center rounded-full border",
                checked ? "border-brand" : "border-border",
              )}
            >
              {checked ? <span className="h-1.5 w-1.5 rounded-full bg-brand" /> : null}
            </span>
            <span className="min-w-0 flex-1">
              <span className="flex items-center gap-1">
                <span className="truncate font-mono text-[12px]">{o.label}</span>
                {o.badge ? (
                  <span className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground">
                    {o.badge}
                  </span>
                ) : null}
              </span>
              {o.hint ? (
                <span className="block truncate text-[10.5px] leading-tight text-muted-foreground">
                  {o.hint}
                </span>
              ) : null}
            </span>
          </button>
        )
      })}
    </div>
  )
}

export function ExperimentDetailPage() {
  const { route, navigate } = useRouter()
  const id = route.name === "experiment-detail" ? route.id : ""
  const [exp, setExp] = useState<Experiment | null>(null)
  const [runs, setRuns] = useState<{ id: string; status: string; variant: string; duration_ms: number; cost_usd: number; created_at: string }[]>([])

  useEffect(() => {
    if (!id) return
    void (async () => {
      const { data: expData } = await supabase
        .from("experiments")
        .select("*")
        .eq("id", id)
        .maybeSingle()
      const experiment = (expData as Experiment) ?? null
      setExp(experiment)
      if (!experiment) return
      const { data: runsData } = await supabase
        .from("runs")
        .select("id, status, variant, duration_ms, cost_usd, created_at, experiment_id, experiment_name")
        .eq("experiment_name", experiment.name)
        .order("created_at", { ascending: false })
        .limit(40)
      setRuns((runsData ?? []) as typeof runs)
    })()
  }, [id])

  if (!exp) {
    return (
      <div className="p-6 text-[12px] text-muted-foreground">Loading experiment…</div>
    )
  }

  return (
    <div className="flex flex-col">
      <PageHeader
        title={exp.name}
        subtitle={exp.description}
        rightSlot={
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1 text-[12px]"
            onClick={() => navigate({ name: "experiments" })}
          >
            <ArrowUpRight className="h-3 w-3 -rotate-180" /> All experiments
          </Button>
        }
        primaryAction={
          <Button size="sm" className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90">
            <Play className="h-3 w-3" /> Queue run
          </Button>
        }
      />

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-4">
        <KV label="Owner" value={exp.owner} mono />
        <KV
          label="Tags"
          value={
            <div className="flex flex-wrap items-center gap-1">
              {exp.tags.map((t) => (
                <span
                  key={t}
                  className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground"
                >
                  {t}
                </span>
              ))}
            </div>
          }
        />
        <KV label="Created" value={formatRelative(exp.created_at)} mono />
        <KV label="Latest variants" value={`${runs.length}`} mono />
      </div>

      <div className="grid grid-cols-1 gap-px border-b border-border bg-border lg:grid-cols-[minmax(0,1fr)_minmax(0,420px)]">
        <div className="bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <div className="flex items-center gap-2">
              <h3 className="text-[12px] font-semibold">Run history</h3>
              <span className="text-[11px] text-muted-foreground">
                {runs.length} runs
              </span>
            </div>
            <Button variant="outline" size="sm" className="h-6 gap-1 text-[11px]">
              <GitBranch className="h-3 w-3" /> Compare
            </Button>
          </header>

          <div
            className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
            style={{
              gridTemplateColumns:
                "minmax(120px,1fr) 100px 90px 70px 90px 24px",
            }}
          >
            <span>Variant</span>
            <span>Status</span>
            <span>Duration</span>
            <span>Cost</span>
            <span>Started</span>
            <span></span>
          </div>
          {runs.map((r) => (
            <button
              key={r.id}
              onClick={() => navigate({ name: "run-detail", id: r.id })}
              className="grid w-full items-center gap-2 border-b border-border px-3 py-1.5 text-left hover:bg-accent/40"
              style={{
                gridTemplateColumns:
                  "minmax(120px,1fr) 100px 90px 70px 90px 24px",
              }}
            >
              <span className="truncate font-mono text-[11.5px]">{r.variant}</span>
              <span className="text-[11.5px]">
                <span
                  className={cn(
                    "inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10.5px] font-medium",
                    statusToneBg(r.status),
                    statusToneText(r.status),
                  )}
                >
                  <span
                    className={cn(
                      "h-1.5 w-1.5 rounded-full",
                      statusToneDot(r.status),
                    )}
                  />
                  {r.status}
                </span>
              </span>
              <span className="font-mono text-[11px] text-muted-foreground">
                {r.duration_ms ? `${Math.round(r.duration_ms / 1000)}s` : "—"}
              </span>
              <span className="font-mono text-[11px] text-muted-foreground">
                ${Number(r.cost_usd).toFixed(2)}
              </span>
              <span className="font-mono text-[11px] text-muted-foreground">
                {formatRelative(r.created_at)}
              </span>
              <ArrowUpRight className="h-3 w-3 text-muted-foreground" />
            </button>
          ))}
        </div>

        <div className="bg-background">
          <header className="flex h-9 items-center justify-between border-b border-border px-3">
            <h3 className="text-[12px] font-semibold">Config</h3>
            <Button variant="ghost" size="sm" className="h-6 gap-1 text-[11px]">
              <Eye className="h-3 w-3" /> Diff
            </Button>
          </header>
          <pre className="overflow-auto p-3 font-mono text-[11px] leading-relaxed text-muted-foreground">
            {JSON.stringify(exp.config, null, 2)}
          </pre>
        </div>
      </div>

      <div className="border-b border-border bg-background">
        <header className="flex h-9 items-center justify-between border-b border-border px-3">
          <h3 className="text-[12px] font-semibold">Notes</h3>
          <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
            <Tag className="h-3 w-3" /> kira
          </span>
        </header>
        <div className="p-3 text-[12px] text-muted-foreground leading-relaxed">
          Three seeds, head-to-head. Sonnet beats GPT-5 on the SWE-Bench Verified split by{" "}
          <span className="font-mono text-foreground">+5.4pp</span> on pass@1, with comparable token usage.
        </div>
      </div>
    </div>
  )
}

function KV({
  label,
  value,
  mono,
}: {
  label: string
  value: React.ReactNode
  mono?: boolean
}) {
  return (
    <div className="bg-background p-3">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div
        className={cn(
          "mt-0.5 text-[12.5px]",
          mono ? "font-mono text-[12px]" : "",
        )}
      >
        {value}
      </div>
    </div>
  )
}

function statusToneBg(s: string) {
  return s === "succeeded"
    ? "bg-success/10"
    : s === "running"
      ? "bg-info/10"
      : s === "failed"
        ? "bg-destructive/10"
        : "bg-muted/40"
}
function statusToneText(s: string) {
  return s === "succeeded"
    ? "text-success"
    : s === "running"
      ? "text-info"
      : s === "failed"
        ? "text-destructive"
        : "text-muted-foreground"
}
function statusToneDot(s: string) {
  return s === "succeeded"
    ? "bg-success"
    : s === "running"
      ? "bg-info animate-pulse"
      : s === "failed"
        ? "bg-destructive"
        : "bg-muted-foreground"
}

import {
  CreditCard,
  Download,
  Globe,
  Key,
  Lock,
  Plus,
  Settings,
  Shield,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Switch } from "@/components/ui/switch"
import { PageHeader } from "@/pages/registry"
import { cn } from "@/lib/utils"

export function SettingsPage() {
  return (
    <div className="flex flex-col">
      <PageHeader
        title="Settings"
        subtitle="Workspace, compute, and integration preferences."
      />
      <div className="grid grid-cols-1 gap-px bg-border lg:grid-cols-[200px_minmax(0,1fr)]">
        <SideNav
          items={[
            { label: "Workspace", icon: Settings, hint: "kira/prod" },
            { label: "API keys", icon: Key, hint: "3 active" },
            { label: "Compute", icon: Globe, hint: "4 regions" },
            { label: "Security", icon: Shield },
          ]}
          activeIndex={0}
        />
        <div className="flex flex-col gap-px bg-border">
          <Section title="Workspace">
            <Field label="Name">
              <Input defaultValue="kira/prod" className="h-8 font-mono text-[12px]" />
            </Field>
            <Field label="Slug">
              <Input defaultValue="kira-prod" className="h-8 font-mono text-[12px]" />
            </Field>
            <Field label="Default region">
              <Input defaultValue="us-east-1" className="h-8 font-mono text-[12px]" />
            </Field>
            <Field label="Auto-tear-down" hint="Stop idle sandboxes after">
              <div className="flex items-center gap-2">
                <Input defaultValue="20" className="h-8 w-20 font-mono text-[12px]" />
                <span className="text-[12px] text-muted-foreground">min</span>
              </div>
            </Field>
          </Section>

          <Section title="API keys">
            <div
              className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
              style={{ gridTemplateColumns: "minmax(160px,1fr) 110px 100px 90px 24px" }}
            >
              <span>Name</span>
              <span>Prefix</span>
              <span>Created</span>
              <span>Last used</span>
              <span></span>
            </div>
            {[
              { name: "ci/runner", prefix: "buc_pk_4f9", created: "12d ago", last: "2m ago" },
              { name: "kira-laptop", prefix: "buc_pk_a1c", created: "30d ago", last: "1h ago" },
              { name: "tom-cli", prefix: "buc_pk_12d", created: "3mo ago", last: "5d ago" },
            ].map((k) => (
              <div
                key={k.name}
                className="grid items-center gap-2 border-b border-border px-3 py-1.5"
                style={{ gridTemplateColumns: "minmax(160px,1fr) 110px 100px 90px 24px" }}
              >
                <span className="font-mono text-[12px]">{k.name}</span>
                <span className="font-mono text-[11px] text-muted-foreground">{k.prefix}…</span>
                <span className="font-mono text-[11px] text-muted-foreground">{k.created}</span>
                <span className="font-mono text-[11px] text-muted-foreground">{k.last}</span>
                <Lock className="h-3 w-3 text-muted-foreground" />
              </div>
            ))}
            <div className="px-3 py-2">
              <Button variant="outline" size="sm" className="h-7 gap-1 text-[12px]">
                <Plus className="h-3 w-3" /> Create key
              </Button>
            </div>
          </Section>

          <Section title="Compute regions">
            <div className="grid grid-cols-2 gap-px bg-border">
              {[
                { code: "us-east-1", gpu: "12 H100", up: true },
                { code: "us-west-2", gpu: "6 H100", up: true },
                { code: "eu-west-1", gpu: "4 A100", up: true },
                { code: "ap-southeast-1", gpu: "off", up: false },
              ].map((r) => (
                <div key={r.code} className="bg-card p-3">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <span
                        className={cn(
                          "h-1.5 w-1.5 rounded-full",
                          r.up ? "bg-success" : "bg-muted-foreground",
                        )}
                      />
                      <span className="font-mono text-[12px]">{r.code}</span>
                    </div>
                    <Switch defaultChecked={r.up} />
                  </div>
                  <div className="mt-1 text-[11px] text-muted-foreground">{r.gpu} available</div>
                </div>
              ))}
            </div>
          </Section>

          <Section title="Security">
            <div className="flex items-center justify-between">
              <div>
                <div className="text-[12.5px] font-medium">Single sign-on</div>
                <div className="text-[11px] text-muted-foreground">
                  SAML for kira.dev workspace.
                </div>
              </div>
              <Switch defaultChecked />
            </div>
            <div className="flex items-center justify-between">
              <div>
                <div className="text-[12.5px] font-medium">2FA required</div>
                <div className="text-[11px] text-muted-foreground">
                  All members must use 2FA.
                </div>
              </div>
              <Switch defaultChecked />
            </div>
            <div className="flex items-center justify-between">
              <div>
                <div className="text-[12.5px] font-medium">Audit logs</div>
                <div className="text-[11px] text-muted-foreground">Stream to S3.</div>
              </div>
              <Switch />
            </div>
          </Section>
        </div>
      </div>
    </div>
  )
}

export function BillingPage() {
  return (
    <div className="flex flex-col">
      <PageHeader
        title="Billing"
        subtitle="Plan, spend, and invoices."
        primaryAction={
          <Button size="sm" variant="outline" className="h-7 gap-1 text-[12px]">
            <Download className="h-3 w-3" /> Download CSV
          </Button>
        }
      />
      <div className="grid grid-cols-1 gap-px border-b border-border bg-border md:grid-cols-4">
        <Stat label="Plan" value="Pro" hint="$0 base + usage" />
        <Stat label="Spend (mtd)" value="$1,284.30" hint="-$182 vs last cycle" />
        <Stat label="Forecast" value="$2,900" hint="Trending high" />
        <Stat label="Credits" value="$140.00" hint="Expires Jul 2026" />
      </div>

      <Section title="Usage by category">
        {[
          { name: "GPU compute (H100)", value: 824.4, pct: 64 },
          { name: "GPU compute (A100)", value: 188.1, pct: 14 },
          { name: "CPU sandbox", value: 122.6, pct: 9 },
          { name: "Storage (registry)", value: 84.2, pct: 6 },
          { name: "Network egress", value: 64.9, pct: 5 },
        ].map((u) => (
          <div key={u.name} className="grid grid-cols-[minmax(220px,1fr)_70px_minmax(120px,1fr)] items-center gap-2 border-b border-border py-1.5">
            <span className="font-mono text-[12px]">{u.name}</span>
            <span className="font-mono text-[12px]">${u.value.toFixed(2)}</span>
            <div className="h-2 overflow-hidden rounded-full bg-muted">
              <div className="h-full rounded-full bg-brand" style={{ width: `${u.pct}%` }} />
            </div>
          </div>
        ))}
      </Section>

      <Section title="Invoices">
        <div
          className="grid items-center gap-2 border-b border-border py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
          style={{ gridTemplateColumns: "120px minmax(160px,1fr) 100px 110px 24px" }}
        >
          <span>Invoice</span>
          <span>Period</span>
          <span>Amount</span>
          <span>Status</span>
          <span></span>
        </div>
        {[
          { id: "INV-1238", period: "May 1 - May 31, 2026", amount: 2104.4, status: "paid" },
          { id: "INV-1199", period: "Apr 1 - Apr 30, 2026", amount: 1840.2, status: "paid" },
          { id: "INV-1162", period: "Mar 1 - Mar 31, 2026", amount: 1620.1, status: "paid" },
        ].map((i) => (
          <div
            key={i.id}
            className="grid items-center gap-2 border-b border-border py-1.5"
            style={{ gridTemplateColumns: "120px minmax(160px,1fr) 100px 110px 24px" }}
          >
            <span className="font-mono text-[12px]">{i.id}</span>
            <span className="text-[12px] text-muted-foreground">{i.period}</span>
            <span className="font-mono text-[12px]">${i.amount.toFixed(2)}</span>
            <span className="rounded bg-success/10 px-1.5 py-0.5 font-mono text-[10.5px] text-success w-fit">
              {i.status}
            </span>
            <Download className="h-3 w-3 text-muted-foreground" />
          </div>
        ))}
      </Section>

      <Section title="Payment method">
        <div className="flex items-center gap-3 rounded-md border border-border bg-card p-3">
          <CreditCard className="h-4 w-4 text-muted-foreground" />
          <div className="flex-1">
            <div className="text-[12.5px] font-medium">Visa ending 4242</div>
            <div className="text-[11px] text-muted-foreground">Expires 09/27</div>
          </div>
          <Button variant="outline" size="sm" className="h-7 text-[12px]">
            Update
          </Button>
        </div>
      </Section>
    </div>
  )
}

export function TeamPage() {
  return (
    <div className="flex flex-col">
      <PageHeader
        title="Team"
        subtitle="Members of kira/prod."
        primaryAction={
          <Button size="sm" className="h-7 gap-1 bg-brand text-brand-foreground hover:bg-brand/90">
            <Plus className="h-3 w-3" /> Invite
          </Button>
        }
      />
      <div className="text-[12px]">
        <div
          className="grid items-center gap-2 border-b border-border px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground"
          style={{ gridTemplateColumns: "minmax(220px,1fr) 100px 90px 110px 90px" }}
        >
          <span>Member</span>
          <span>Role</span>
          <span>2FA</span>
          <span>Last active</span>
          <span>Joined</span>
        </div>
        {[
          { name: "kira sundström", email: "kira@bucephalus.dev", role: "Owner", twofa: true, last: "2m", joined: "1y" },
          { name: "tom kerr", email: "tom@bucephalus.dev", role: "Admin", twofa: true, last: "12m", joined: "8mo" },
          { name: "noah park", email: "noah@bucephalus.dev", role: "Member", twofa: true, last: "3h", joined: "5mo" },
          { name: "adi rao", email: "adi@bucephalus.dev", role: "Member", twofa: false, last: "1d", joined: "2mo" },
          { name: "ci runner", email: "ci@bucephalus.dev", role: "Service", twofa: true, last: "1m", joined: "11mo" },
        ].map((m) => (
          <div
            key={m.email}
            className="grid items-center gap-2 border-b border-border px-3 py-1.5"
            style={{ gridTemplateColumns: "minmax(220px,1fr) 100px 90px 110px 90px" }}
          >
            <div className="flex items-center gap-2">
              <span className="grid h-5 w-5 place-items-center rounded-full bg-info/15 text-[10px] font-medium text-info">
                {m.name.split(" ").map((p) => p[0]?.toUpperCase()).join("").slice(0, 2)}
              </span>
              <div className="min-w-0 leading-tight">
                <div className="truncate text-[12.5px]">{m.name}</div>
                <div className="truncate font-mono text-[10.5px] text-muted-foreground">
                  {m.email}
                </div>
              </div>
            </div>
            <span className="rounded border border-border px-1.5 py-0.5 font-mono text-[10.5px] text-muted-foreground w-fit">
              {m.role}
            </span>
            <span
              className={cn(
                "rounded px-1.5 py-0.5 font-mono text-[10.5px] w-fit",
                m.twofa ? "bg-success/10 text-success" : "bg-warning/10 text-warning",
              )}
            >
              {m.twofa ? "on" : "off"}
            </span>
            <span className="font-mono text-[11px] text-muted-foreground">{m.last} ago</span>
            <span className="font-mono text-[11px] text-muted-foreground">{m.joined} ago</span>
          </div>
        ))}
      </div>
    </div>
  )
}

function Stat({
  label,
  value,
  hint,
}: {
  label: string
  value: string
  hint?: string
}) {
  return (
    <div className="bg-background p-3">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </div>
      <div className="mt-0.5 font-mono text-[18px] font-medium">{value}</div>
      {hint ? <div className="text-[10.5px] text-muted-foreground">{hint}</div> : null}
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="border-b border-border bg-background">
      <header className="flex h-9 items-center justify-between border-b border-border px-3">
        <h3 className="text-[12px] font-semibold">{title}</h3>
      </header>
      <div className="flex flex-col gap-2 px-3 py-3">{children}</div>
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
    <div className="grid grid-cols-1 gap-1.5 md:grid-cols-[180px_minmax(0,360px)] md:items-center">
      <Label className="text-[12px] font-medium">
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

function SideNav({
  items,
  activeIndex,
}: {
  items: { label: string; icon: React.ComponentType<{ className?: string }>; hint?: string }[]
  activeIndex: number
}) {
  return (
    <nav className="flex flex-col gap-px bg-border">
      {items.map((it, i) => {
        const Icon = it.icon
        const active = i === activeIndex
        return (
          <button
            key={it.label}
            className={cn(
              "flex items-center justify-between gap-2 px-3 py-2 text-left text-[12.5px]",
              active
                ? "bg-secondary text-foreground"
                : "bg-background text-muted-foreground hover:text-foreground",
            )}
          >
            <span className="flex items-center gap-2">
              <Icon className="h-3.5 w-3.5" />
              {it.label}
            </span>
            {it.hint ? (
              <span className="font-mono text-[10.5px] text-muted-foreground">
                {it.hint}
              </span>
            ) : null}
          </button>
        )
      })}
    </nav>
  )
}

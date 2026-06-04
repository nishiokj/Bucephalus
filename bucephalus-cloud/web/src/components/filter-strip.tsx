import * as React from "react"
import { Check, ChevronDown, ListFilter, RotateCcw, X } from "lucide-react"
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command"
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover"
import { formatReadableLabel, formatReadableToken } from "@/lib/format"
import { cn } from "@/lib/utils"

export type FilterOption = {
  value: string
  label: string
  count?: number
  detail?: string
}

type NormalizedFilterOption = FilterOption & {
  displayLabel: string
  searchValue: string
}

export function FilterStrip({
  label,
  value,
  options,
  onValueChange,
  max = 6,
  className,
}: {
  label: string
  value: string
  options: FilterOption[]
  onValueChange: (value: string) => void
  max?: number
  className?: string
}) {
  const [open, setOpen] = React.useState(false)
  const normalized = React.useMemo(() => options.map(normalizeOption), [options])
  const visible = visibleOptions(normalized, value, max)
  const overflow = normalized.filter((option) => !visible.some((shown) => shown.value === option.value))
  const overflowActive = overflow.some((option) => option.value === value)
  const selectedOverflow = overflow.find((option) => option.value === value)
  const resetOption = normalized[0]
  const selected = normalized.find((option) => option.value === value) ?? resetOption
  const hasSelection = Boolean(resetOption && selected && selected.value !== resetOption.value)
  const selectedCount = selected?.count ?? null
  const totalCount = resetOption?.count ?? normalized.reduce((sum, option) => sum + (option.count ?? 0), 0)
  const ranked = React.useMemo(
    () =>
      normalized
        .filter((option) => option.value !== resetOption?.value && (option.count ?? 0) > 0)
        .sort((a, b) => (b.count ?? 0) - (a.count ?? 0))
        .slice(0, 3),
    [normalized, resetOption?.value],
  )
  const maxCount = ranked[0]?.count ?? normalized.reduce((max, option) => Math.max(max, option.count ?? 0), 0)
  const activeRank = React.useMemo(() => {
    if (!selected || selected.value === resetOption?.value) return null
    const ordered = normalized
      .filter((option) => option.value !== resetOption?.value && (option.count ?? 0) > 0)
      .sort((a, b) => (b.count ?? 0) - (a.count ?? 0))
    const index = ordered.findIndex((option) => option.value === selected.value)
    return index >= 0 ? index + 1 : null
  }, [normalized, resetOption?.value, selected])
  const selectedShare = selectedCount != null && totalCount > 0
    ? Math.round((selectedCount / totalCount) * 100)
    : null

  return (
    <div className={cn("flex min-w-0 flex-col gap-1 sm:flex-row sm:items-center sm:gap-1.5", className)}>
      <div className="flex min-w-0 items-center justify-between gap-2 sm:justify-start">
        <span className="inline-flex shrink-0 items-center gap-1 text-[10px] uppercase tracking-wide text-muted-foreground">
          <ListFilter className="h-3 w-3" />
          {label}
        </span>
        {hasSelection && resetOption ? (
          <button
            className="inline-flex h-5 shrink-0 items-center gap-1 rounded px-1.5 text-[10.5px] text-muted-foreground hover:bg-accent hover:text-foreground"
            onClick={() => onValueChange(resetOption.value)}
            title={`Reset ${label.toLowerCase()} to ${resetOption.displayLabel}`}
          >
            <RotateCcw className="h-3 w-3" />
            Reset
          </button>
        ) : null}
      </div>
      <div className="flex min-w-0 items-center gap-1 overflow-x-auto pb-0.5 scrollbar-thin sm:pb-0">
        {visible.map((option) => {
          const active = option.value === value
          return (
            <button
              key={option.value}
              onClick={() => onValueChange(option.value)}
              className={cn(
                "inline-flex h-6 shrink-0 items-center gap-1 rounded-md border px-2 text-[11.5px] transition-colors",
                active
                  ? "border-brand/40 bg-brand/10 text-foreground"
                  : "border-transparent text-muted-foreground hover:bg-secondary hover:text-foreground",
              )}
              title={filterOptionTitle(label, option)}
            >
              <span className="max-w-[9.5rem] truncate">{option.displayLabel}</span>
              {option.count != null ? (
                <span className={cn(
                  "rounded px-1 font-mono text-[10px]",
                  active ? "bg-brand/15 text-foreground" : "bg-muted/70 text-muted-foreground",
                )}>
                  {compactCount(option.count)}
                </span>
              ) : null}
            </button>
          )
        })}
      </div>
      {overflow.length > 0 ? (
        <Popover open={open} onOpenChange={setOpen}>
          <PopoverTrigger asChild>
            <button
              className={cn(
                "inline-flex h-6 shrink-0 items-center gap-1 rounded-md border px-2 text-[11.5px] transition-colors",
                overflowActive
                  ? "border-brand/40 bg-brand/10 text-foreground"
                  : "border-border bg-card text-muted-foreground hover:bg-accent/50 hover:text-foreground",
              )}
              aria-label={`${label} more filters`}
            >
              <span className="max-w-[88px] truncate">
                {overflowActive && selectedOverflow ? selectedOverflow.displayLabel : `+${overflow.length}`}
              </span>
              <ChevronDown className="h-3 w-3 opacity-60" />
            </button>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-[min(22rem,calc(100vw-2rem))] p-0">
            <Command>
              <div className="grid gap-0.5 border-b border-border px-2 py-2">
                <div className="flex items-center justify-between gap-2">
                  <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
                    {label}
                  </span>
                  {resetOption && resetOption.value !== value ? (
                    <button
                      className="inline-flex h-5 items-center gap-1 rounded px-1.5 text-[10.5px] text-muted-foreground hover:bg-accent hover:text-foreground"
                      onClick={() => {
                        onValueChange(resetOption.value)
                        setOpen(false)
                      }}
                    >
                      <X className="h-3 w-3" />
                      {resetOption.displayLabel}
                    </button>
                  ) : null}
                </div>
                <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-end gap-3 text-[11px]">
                  <div className="min-w-0">
                    <span className="block truncate text-foreground">
                      {selected ? selected.displayLabel : "No selection"}
                    </span>
                    <span className="block truncate text-[10px] text-muted-foreground">
                      {activeRank ? `rank #${activeRank}` : "all values"}
                      {selectedShare != null ? ` · ${selectedShare}% of rows` : ""}
                    </span>
                  </div>
                  <div className="grid justify-items-end gap-1">
                    {selectedCount != null || totalCount ? (
                      <span className="shrink-0 font-mono text-muted-foreground">
                        {selectedCount != null ? compactCount(selectedCount) : "all"} / {compactCount(totalCount)}
                      </span>
                    ) : null}
                    {selectedCount != null && totalCount > 0 ? (
                      <span className="h-1 w-16 overflow-hidden rounded-full bg-muted">
                        <span
                          className="block h-full rounded-full bg-brand"
                          style={{ width: `${Math.min(100, Math.max(3, selectedShare ?? 0))}%` }}
                        />
                      </span>
                    ) : null}
                  </div>
                </div>
              </div>
              {ranked.length > 0 ? (
                <div className="grid gap-1 border-b border-border px-2 py-1.5">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-[9.5px] uppercase tracking-wide text-muted-foreground">Top values</span>
                    <span className="font-mono text-[9.5px] text-muted-foreground">{compactCount(totalCount)} rows</span>
                  </div>
                  <div className="flex min-w-0 gap-1 overflow-x-auto pb-0.5 scrollbar-thin">
                    {ranked.map((option) => {
                      const active = option.value === value
                      return (
                        <button
                          key={option.value}
                          className={cn(
                            "inline-flex h-6 max-w-[9.5rem] shrink-0 items-center gap-1 rounded-md border px-2 text-[11px] transition-colors",
                            active
                              ? "border-brand/40 bg-brand/10 text-foreground"
                              : "border-border bg-card text-muted-foreground hover:bg-accent hover:text-foreground",
                          )}
                          title={filterOptionTitle(label, option)}
                          onClick={() => {
                            onValueChange(option.value)
                            setOpen(false)
                          }}
                        >
                          <span className="truncate">{option.displayLabel}</span>
                          <span className="rounded bg-muted px-1 font-mono text-[9.5px] text-muted-foreground">
                            {compactCount(option.count ?? 0)}
                          </span>
                        </button>
                      )
                    })}
                  </div>
                </div>
              ) : null}
              <CommandInput placeholder={`Search ${label.toLowerCase()}`} className="h-8 py-2 text-[12px]" />
              <CommandList className="max-h-64">
                <CommandEmpty className="py-6 text-center text-[12px] text-muted-foreground">
                  No matches
                </CommandEmpty>
                <CommandGroup className="p-1">
                  {normalized.map((option) => {
                    const active = option.value === value
                    return (
                      <CommandItem
                        key={option.value}
                        value={option.searchValue}
                        onSelect={() => {
                          onValueChange(option.value)
                          setOpen(false)
                        }}
                        className="grid grid-cols-[14px_minmax(0,1fr)_minmax(3.5rem,auto)] gap-2 rounded px-2 py-1.5 text-[12px]"
                      >
                        <Check className={cn("mt-0.5 h-3.5 w-3.5", active ? "opacity-100" : "opacity-0")} />
                        <span className="min-w-0">
                          <span className="block truncate">{option.displayLabel}</span>
                          {option.detail ? (
                            <span className="block truncate text-[10.5px] text-muted-foreground">
                              {option.detail}
                            </span>
                          ) : null}
                        </span>
                        {option.count != null ? (
                          <FilterCountMeter count={option.count} max={maxCount || option.count} active={active} />
                        ) : null}
                      </CommandItem>
                    )
                  })}
                </CommandGroup>
              </CommandList>
            </Command>
          </PopoverContent>
        </Popover>
      ) : null}
    </div>
  )
}

function FilterCountMeter({ count, max, active }: { count: number; max: number; active: boolean }) {
  const width = max > 0 ? Math.min(100, Math.max(4, (count / max) * 100)) : 0
  return (
    <span className="grid min-w-14 justify-items-end gap-1">
      <span
        className={cn(
          "rounded px-1 font-mono text-[10px]",
          active ? "bg-brand/15 text-foreground" : "bg-muted text-muted-foreground",
        )}
      >
        {compactCount(count)}
      </span>
      <span className="h-1 w-12 overflow-hidden rounded-full bg-muted">
        <span
          className={cn("block h-full rounded-full", active ? "bg-brand" : "bg-muted-foreground/45")}
          style={{ width: `${width}%` }}
        />
      </span>
    </span>
  )
}

export function SegmentedControl({
  label,
  value,
  options,
  onValueChange,
  className,
}: {
  label?: string
  value: string
  options: FilterOption[]
  onValueChange: (value: string) => void
  className?: string
}) {
  return (
    <div className={cn("flex min-w-0 items-center gap-1.5", className)}>
      {label ? (
        <span className="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground">
          {label}
        </span>
      ) : null}
      <div className="inline-flex max-w-full min-w-0 overflow-x-auto rounded-md border border-border bg-card p-0.5 scrollbar-thin">
        {options.map((option) => {
          const active = option.value === value
          return (
            <button
              key={option.value}
              onClick={() => onValueChange(option.value)}
              className={cn(
                "inline-flex h-6 shrink-0 items-center gap-1 rounded px-2 text-[11.5px] transition-colors",
                active
                  ? "bg-brand/10 text-foreground shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--brand)_35%,transparent)]"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              <span>{formatReadableLabel(option.label)}</span>
              {option.count != null ? (
                <span className="font-mono text-[10px] text-muted-foreground">
                  {compactCount(option.count)}
                </span>
              ) : null}
            </button>
          )
        })}
      </div>
    </div>
  )
}

function visibleOptions(options: NormalizedFilterOption[], value: string, max: number) {
  const pinned = options.slice(0, max)
  const selected = options.find((option) => option.value === value)
  if (!selected || pinned.some((option) => option.value === selected.value)) return pinned
  return [...pinned.slice(0, Math.max(0, max - 1)), selected]
}

function normalizeOption(option: FilterOption): NormalizedFilterOption {
  const token = formatReadableToken(option.label)
  const displayLabel = token !== option.label
    ? token
    : formatReadableLabel(option.label)
      .replace(/[_-]+/g, " ")
      .replace(/\s+/g, " ")
      .trim()
  return {
    ...option,
    displayLabel,
    searchValue: `${option.label} ${option.value} ${displayLabel} ${option.detail ?? ""}`,
  }
}

function compactCount(count: number) {
  if (Math.abs(count) >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`
  if (Math.abs(count) >= 1_000) return `${(count / 1_000).toFixed(1)}k`
  return `${count}`
}

function filterOptionTitle(label: string, option: NormalizedFilterOption) {
  const count = option.count == null ? "" : ` (${compactCount(option.count)})`
  return `${label}: ${option.displayLabel}${count}`
}

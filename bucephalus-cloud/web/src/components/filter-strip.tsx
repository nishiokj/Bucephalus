import * as React from "react"
import { Check, ChevronDown, X } from "lucide-react"
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
import { cn } from "@/lib/utils"

export type FilterOption = {
  value: string
  label: string
  count?: number
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
  const visible = visibleOptions(options, value, max)
  const overflow = options.filter((option) => !visible.some((shown) => shown.value === option.value))
  const overflowActive = overflow.some((option) => option.value === value)
  const selectedOverflow = overflow.find((option) => option.value === value)
  const resetOption = options[0]

  return (
    <div className={cn("flex min-w-0 items-center gap-1.5", className)}>
      <span className="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
      <div className="flex min-w-0 items-center gap-1 overflow-x-auto scrollbar-thin">
        {visible.map((option) => {
          const active = option.value === value
          return (
            <button
              key={option.value}
              onClick={() => onValueChange(option.value)}
              className={cn(
                "inline-flex h-6 shrink-0 items-center gap-1 rounded-md border px-2 text-[11.5px] transition-colors",
                active
                  ? "border-border bg-secondary text-foreground"
                  : "border-transparent text-muted-foreground hover:bg-secondary hover:text-foreground",
              )}
            >
              <span className="truncate">{option.label}</span>
              {option.count != null ? (
                <span className="font-mono text-[10px] text-muted-foreground">
                  {option.count}
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
                  ? "border-border bg-secondary text-foreground"
                  : "border-border bg-card text-muted-foreground hover:bg-accent/50 hover:text-foreground",
              )}
              aria-label={`${label} more filters`}
            >
              <span className="max-w-[88px] truncate">
                {overflowActive && selectedOverflow ? selectedOverflow.label : `+${overflow.length}`}
              </span>
              <ChevronDown className="h-3 w-3 opacity-60" />
            </button>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-[min(19rem,calc(100vw-2rem))] p-0">
            <Command>
              <div className="flex h-8 items-center justify-between border-b border-border px-2">
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
                    {resetOption.label}
                  </button>
                ) : null}
              </div>
              <CommandInput placeholder={`Search ${label.toLowerCase()}`} className="h-8 py-2 text-[12px]" />
              <CommandList className="max-h-64">
                <CommandEmpty className="py-6 text-center text-[12px] text-muted-foreground">
                  No matches
                </CommandEmpty>
                <CommandGroup className="p-1">
                  {options.map((option) => {
                    const active = option.value === value
                    return (
                      <CommandItem
                        key={option.value}
                        value={`${option.label} ${option.value}`}
                        onSelect={() => {
                          onValueChange(option.value)
                          setOpen(false)
                        }}
                        className="grid grid-cols-[14px_minmax(0,1fr)_auto] gap-2 rounded px-2 py-1.5 text-[12px]"
                      >
                        <Check className={cn("h-3.5 w-3.5", active ? "opacity-100" : "opacity-0")} />
                        <span className="min-w-0 truncate">{option.label}</span>
                        {option.count != null ? (
                          <span className="rounded bg-muted px-1 font-mono text-[10px] text-muted-foreground">
                            {option.count}
                          </span>
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
                  ? "bg-secondary text-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              <span>{option.label}</span>
              {option.count != null ? (
                <span className="font-mono text-[10px] text-muted-foreground">
                  {option.count}
                </span>
              ) : null}
            </button>
          )
        })}
      </div>
    </div>
  )
}

function visibleOptions(options: FilterOption[], value: string, max: number) {
  const pinned = options.slice(0, max)
  const selected = options.find((option) => option.value === value)
  if (!selected || pinned.some((option) => option.value === selected.value)) return pinned
  return [...pinned.slice(0, Math.max(0, max - 1)), selected]
}

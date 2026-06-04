import { Monitor, Moon, Sun } from "lucide-react"

import { Button } from "@/components/ui/button"
import { useTheme } from "@/components/theme-provider"

export function ModeToggle() {
  const { theme, setTheme } = useTheme()
  const nextTheme = theme === "dark" ? "light" : theme === "light" ? "system" : "dark"
  const Icon = theme === "dark" ? Moon : theme === "light" ? Sun : Monitor

  return (
    <Button
      variant="ghost"
      size="sm"
      className="h-7 w-7 p-0 text-muted-foreground"
      onClick={() => setTheme(nextTheme)}
      aria-label={`Theme is ${theme}. Switch to ${nextTheme}.`}
      title={`Theme: ${theme}. Click for ${nextTheme}.`}
    >
      <Icon className="h-3.5 w-3.5" />
      <span className="sr-only">Toggle theme</span>
    </Button>
  )
}

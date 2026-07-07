import { SunMoonIcon } from "lucide-react"

import { Button } from "@/components/ui/button"

export function AppHeader({ onToggleTheme }: { onToggleTheme: () => void }) {
  return (
    <header className="flex items-center justify-between py-4">
      <h1 className="flex items-center gap-3 text-2xl font-bold tracking-tight">
        Groundhog
      </h1>
      <Button
        variant="outline"
        size="icon"
        onClick={onToggleTheme}
        aria-label="Toggle theme"
      >
        <SunMoonIcon data-icon="icon" />
      </Button>
    </header>
  )
}

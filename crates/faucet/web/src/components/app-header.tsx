import { ZapIcon } from "lucide-react"

export function AppHeader() {
  return (
    <header className="border-b bg-background">
      <div className="mx-auto flex min-h-20 max-w-5xl items-center px-4 py-3 sm:px-6 lg:min-h-22">
        <a
          href="/"
          aria-label="Thruput Faucet homepage"
          className="flex min-h-12 items-center gap-2.5"
        >
          <ZapIcon
            className="size-6 shrink-0 fill-primary text-primary"
            aria-hidden="true"
          />
          <span className="text-xl font-medium tracking-tight">THRUPUT</span>
          <span className="ml-0.5 border-l pl-3 font-mono text-xs text-muted-foreground">
            Faucet
          </span>
        </a>
      </div>
    </header>
  )
}

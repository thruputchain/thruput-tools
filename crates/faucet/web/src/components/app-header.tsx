import { buttonVariants } from "@/components/ui/button"

export function AppHeader() {
  return (
    <header className="border-b bg-background">
      <nav
        aria-label="Main navigation"
        className="mx-auto flex min-h-20 max-w-5xl flex-wrap items-center justify-between gap-3 px-4 py-3 sm:px-6 lg:min-h-22"
      >
        <a
          href="/"
          aria-label="Thruput Faucet homepage"
          className="flex min-h-12 items-center gap-2.5"
        >
          <img
            src="/brand/stellar-dark.svg"
            width="100"
            height="25"
            alt="Stellar"
            className="w-20 shrink-0 sm:w-25"
          />
          <div className="border-l pl-3 text-lg font-medium sm:text-xl">
            Thruput Faucet
          </div>
        </a>
        <a
          href="https://thruput.network/docs/"
          className={buttonVariants({
            variant: "secondary",
            className: "ml-auto max-sm:hidden",
          })}
        >
          Read Thruput Docs
        </a>
      </nav>
    </header>
  )
}

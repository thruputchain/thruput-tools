import { useEffect, useState, useSyncExternalStore } from "react"
import { toast } from "sonner"

import {
  type NetworkDescriptor,
  fetchNetworkDescriptor,
  formatHogAmount,
  requestAirdrop,
} from "@/api"
import { AirdropForm } from "@/components/airdrop-form"
import { AppHeader } from "@/components/app-header"
import { FaucetHero } from "@/components/faucet-hero"
import { Toaster } from "@/components/ui/sonner"
import {
  getThemePreference,
  setThemePreference,
  subscribeToThemePreference,
} from "@/theme"

const ADDRESS_REGEX = /^0x[0-9a-fA-F]{64}$/

export function App() {
  const themePreference = useSyncExternalStore(
    subscribeToThemePreference,
    getThemePreference,
    () => "system",
  )
  const [address, setAddress] = useState("")
  const [fieldError, setFieldError] = useState<string | null>(null)
  const [networkDescriptor, setNetworkDescriptor] =
    useState<NetworkDescriptor | null>(null)
  const [isSubmitting, setIsSubmitting] = useState(false)

  useEffect(() => {
    let isCurrent = true

    fetchNetworkDescriptor()
      .then((descriptor) => {
        if (isCurrent) {
          setNetworkDescriptor(descriptor)
        }
      })
      .catch((error) => {
        if (isCurrent) {
          toast.error(
            error instanceof Error
              ? error.message
              : "Unable to load faucet metadata.",
          )
        }
      })

    return () => {
      isCurrent = false
    }
  }, [])

  function toggleTheme() {
    const resolvedTheme =
      themePreference === "system"
        ? window.matchMedia("(prefers-color-scheme: dark)").matches
          ? "dark"
          : "light"
        : themePreference
    setThemePreference(resolvedTheme === "dark" ? "light" : "dark")
  }

  function handleAddressChange(nextAddress: string) {
    setAddress(nextAddress)
    if (fieldError) {
      setFieldError(null)
    }
  }

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()

    const normalized = address.trim().toLowerCase()
    if (!networkDescriptor) {
      toast.error("Faucet metadata is still loading.")
      return
    }
    if (!ADDRESS_REGEX.test(normalized)) {
      setFieldError("Enter a 0x-prefixed 64-character Groundhog address.")
      return
    }

    setIsSubmitting(true)
    setFieldError(null)

    try {
      const result = await requestAirdrop(normalized)
      const amount = formatHogAmount(
        result.rawAmount,
        networkDescriptor.nativeHog.decimals,
      )
      showSuccessToast(amount, result.txHash)
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Faucet request failed.",
      )
    } finally {
      setIsSubmitting(false)
    }
  }

  return (
    <main className="min-h-dvh bg-background text-foreground">
      <div className="mx-auto w-full max-w-5xl px-4">
        <AppHeader onToggleTheme={toggleTheme} />

        <div className="mx-auto flex max-w-2xl flex-col items-center gap-8 py-8">
          <FaucetHero />
          <AirdropForm
            address={address}
            fieldError={fieldError}
            isLoadingMetadata={!networkDescriptor}
            isSubmitting={isSubmitting}
            onAddressChange={handleAddressChange}
            onSubmit={handleSubmit}
          />
        </div>
      </div>
      <Toaster position="bottom-right" />
    </main>
  )
}

function shortHash(value: string) {
  return `${value.slice(0, 6)}...${value.slice(-4)}`
}

function showSuccessToast(amountHog: string, txHash: string) {
  toast.success(`Sent ${amountHog} HOG`, {
    description: `Transaction ${shortHash(txHash)}`,
  })
}

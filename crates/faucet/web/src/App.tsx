import { type FormEvent, useEffect, useState } from "react"
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
import { Button } from "@/components/ui/button"
import { Toaster } from "@/components/ui/sonner"

const ADDRESS_REGEX = /^0x[0-9a-fA-F]{64}$/

export function App() {
  const [address, setAddress] = useState("")
  const [fieldError, setFieldError] = useState<string | null>(null)
  const [networkDescriptor, setNetworkDescriptor] =
    useState<NetworkDescriptor | null>(null)
  const [metadataError, setMetadataError] = useState<string | null>(null)
  const [isLoadingMetadata, setIsLoadingMetadata] = useState(true)
  const [isSubmitting, setIsSubmitting] = useState(false)

  useEffect(() => {
    let isCurrent = true
    loadMetadata(() => isCurrent)

    return () => {
      isCurrent = false
    }
  }, [])

  async function loadMetadata(isCurrent: () => boolean = () => true) {
    setIsLoadingMetadata(true)
    setMetadataError(null)
    try {
      const descriptor = await fetchNetworkDescriptor()
      if (isCurrent()) {
        setNetworkDescriptor(descriptor)
      }
    } catch (error) {
      if (isCurrent()) {
        setNetworkDescriptor(null)
        setMetadataError(
          error instanceof Error
            ? error.message
            : "Unable to load faucet metadata.",
        )
      }
    } finally {
      if (isCurrent()) {
        setIsLoadingMetadata(false)
      }
    }
  }

  function handleAddressChange(nextAddress: string) {
    setAddress(nextAddress)
    if (fieldError) {
      setFieldError(null)
    }
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()

    const normalized = address.trim().toLowerCase()
    if (!networkDescriptor) {
      toast.error("Faucet metadata is still loading.")
      return
    }
    if (!ADDRESS_REGEX.test(normalized)) {
      setFieldError("Enter a 0x-prefixed 64-character Thruput address.")
      return
    }

    setIsSubmitting(true)
    setFieldError(null)

    try {
      const result = await requestAirdrop(normalized, networkDescriptor)
      showSuccessToast(
        formatAirdropAmount(result.rawAmount, networkDescriptor),
        result.txHash,
      )
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Faucet request failed.",
      )
    } finally {
      setIsSubmitting(false)
    }
  }

  return (
    <div className="min-h-dvh bg-background text-foreground">
      <AppHeader />
      <main className="mx-auto w-full max-w-5xl px-4 sm:px-6">
        <div className="mx-auto flex max-w-2xl flex-col items-center gap-8 py-12 sm:py-16">
          <FaucetHero />
          {metadataError ? (
            <div className="w-full rounded-xl border border-destructive/30 bg-destructive/10 p-4 text-sm text-destructive">
              <div className="font-medium">Faucet metadata unavailable</div>
              <p className="mt-1 text-destructive/80">{metadataError}</p>
              <Button
                type="button"
                variant="outline"
                className="mt-3 h-9 px-3"
                onClick={() => void loadMetadata()}
                disabled={isLoadingMetadata}
              >
                {isLoadingMetadata ? "Retrying..." : "Retry"}
              </Button>
            </div>
          ) : null}
          <AirdropForm
            address={address}
            fieldError={fieldError}
            isLoadingMetadata={isLoadingMetadata}
            isMetadataUnavailable={!isLoadingMetadata && !networkDescriptor}
            isSubmitting={isSubmitting}
            onAddressChange={handleAddressChange}
            onSubmit={handleSubmit}
          />
        </div>
      </main>
      <Toaster position="bottom-right" />
    </div>
  )
}

function shortHash(value: string) {
  return `${value.slice(0, 6)}...${value.slice(-4)}`
}

function showSuccessToast(amountHog: string | null, txHash: string) {
  toast.success(amountHog ? `Sent ${amountHog} HOG` : "Airdrop sent", {
    description: `Transaction ${shortHash(txHash)}`,
  })
}

function formatAirdropAmount(rawAmount: string, descriptor: NetworkDescriptor) {
  try {
    return formatHogAmount(rawAmount, descriptor.nativeHog.decimals)
  } catch {
    return null
  }
}

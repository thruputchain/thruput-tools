import { type FormEvent, useEffect, useState } from "react"
import { toast } from "sonner"

import { AirdropForm } from "@/components/airdrop-form"
import { AppHeader } from "@/components/app-header"
import { FaucetHero } from "@/components/faucet-hero"
import { Button } from "@/components/ui/button"
import { Toaster } from "@/components/ui/sonner"
import {
  type FaucetClient,
  airdrop,
  connectFaucet,
  describeFaucetError,
  formatNativeTokens,
  parseRecipient,
} from "@/faucet"
import { toBase58 } from "@thruputchain/client"

export function App() {
  const [address, setAddress] = useState("")
  const [fieldError, setFieldError] = useState<string | null>(null)
  const [client, setClient] = useState<FaucetClient | null>(null)
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
      const connected = await connectFaucet()
      if (isCurrent()) {
        setClient(connected)
      }
    } catch (error) {
      if (isCurrent()) {
        setClient(null)
        setMetadataError(
          error instanceof Error
            ? error.message
            : "Unable to reach the faucet.",
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

    if (!client) {
      toast.error("The faucet is still connecting.")
      return
    }
    const recipient = parseRecipient(address)
    if (!recipient) {
      setFieldError("Enter a Thruput address (C... StrKey or base58).")
      return
    }

    setIsSubmitting(true)
    setFieldError(null)

    try {
      const result = await airdrop(client, recipient)
      toast.success(
        `Sent ${formatNativeTokens(result.rawAmount)} native tokens`,
        {
          description: `Transaction ${shortHash(toBase58(result.transactionHash))}`,
        },
      )
    } catch (error) {
      toast.error(describeFaucetError(error))
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
              <div className="font-medium">Faucet unavailable</div>
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
            isMetadataUnavailable={!isLoadingMetadata && !client}
            isSubmitting={isSubmitting}
            onAddressChange={handleAddressChange}
            onSubmit={handleSubmit}
          />
          <p className="text-center text-sm text-muted-foreground">
            Request airdrops directly from the CLI -{" "}
            <a
              href="https://docs.thruput.network/tools/faucet/"
              className="text-primary underline-offset-4 hover:underline"
            >
              Learn more
            </a>
          </p>
        </div>
      </main>
      <Toaster position="bottom-right" />
    </div>
  )
}

function shortHash(value: string) {
  return `${value.slice(0, 6)}...${value.slice(-4)}`
}

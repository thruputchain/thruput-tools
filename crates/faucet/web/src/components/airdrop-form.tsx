import type { FormEvent } from "react"

import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Field, FieldError, FieldGroup } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"

export function AirdropForm(props: {
  address: string
  fieldError: string | null
  isLoadingMetadata: boolean
  isMetadataUnavailable: boolean
  isSubmitting: boolean
  onAddressChange: (address: string) => void
  onSubmit: (event: FormEvent<HTMLFormElement>) => void
}) {
  const isInvalid = Boolean(props.fieldError)

  return (
    <Card className="w-full">
      <CardHeader className="sr-only">
        <CardTitle>Fund a wallet</CardTitle>
        <CardDescription>
          Request native tokens for a Thruput address.
        </CardDescription>
      </CardHeader>
      <form onSubmit={props.onSubmit} noValidate>
        <CardContent>
          <FieldGroup>
            <Field data-invalid={isInvalid || undefined}>
              <div className="flex flex-col items-center gap-3 sm:flex-row">
                <Input
                  id="address"
                  aria-label="Wallet address"
                  aria-describedby={isInvalid ? "address-error" : undefined}
                  value={props.address}
                  onChange={(event) =>
                    props.onAddressChange(event.target.value)
                  }
                  placeholder="Wallet address (C...)"
                  autoComplete="off"
                  spellCheck={false}
                  aria-invalid={isInvalid || undefined}
                  disabled={
                    props.isLoadingMetadata ||
                    props.isMetadataUnavailable ||
                    props.isSubmitting
                  }
                />
                <Button
                  type="submit"
                  disabled={
                    props.isLoadingMetadata ||
                    props.isMetadataUnavailable ||
                    props.isSubmitting
                  }
                  className="w-full sm:w-auto"
                >
                  {props.isLoadingMetadata || props.isSubmitting ? (
                    <Spinner data-icon="inline-start" />
                  ) : null}
                  {props.isLoadingMetadata
                    ? "Connecting..."
                    : props.isMetadataUnavailable
                      ? "Faucet unavailable"
                      : props.isSubmitting
                        ? "Requesting..."
                        : "Request airdrop"}
                </Button>
              </div>
              {props.fieldError ? (
                <FieldError id="address-error">{props.fieldError}</FieldError>
              ) : null}
            </Field>
          </FieldGroup>
        </CardContent>
      </form>
    </Card>
  )
}

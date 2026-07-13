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
        <CardDescription>Request HOG for a Groundhog address.</CardDescription>
      </CardHeader>
      <form onSubmit={props.onSubmit} noValidate>
        <CardContent>
          <FieldGroup>
            <Field data-invalid={isInvalid || undefined}>
              <div className="flex flex-col items-center gap-3 sm:flex-row">
                <Input
                  id="address"
                  value={props.address}
                  onChange={(event) =>
                    props.onAddressChange(event.target.value)
                  }
                  placeholder="0x0000"
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
                    ? "Loading..."
                    : props.isMetadataUnavailable
                      ? "Metadata unavailable"
                      : props.isSubmitting
                        ? "Requesting..."
                        : "Request airdrop"}
                </Button>
              </div>
              {props.fieldError ? (
                <FieldError>{props.fieldError}</FieldError>
              ) : null}
            </Field>
          </FieldGroup>
        </CardContent>
      </form>
    </Card>
  )
}

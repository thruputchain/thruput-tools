// Faucet HTTP payloads. Kept free of Vite globals so `pnpm test` can check
// them against fixtures the faucet's Rust tests capture from the real server.

import { isBase58Identifier } from "./address.ts"

/** `NETWORK_DESCRIPTOR_SCHEMA_VERSION` in `thruput-json-rpc-types`. */
export const NETWORK_DESCRIPTOR_SCHEMA_VERSION = 7
/** `NETWORK_DESCRIPTOR_KIND` in `thruput-json-rpc-types`. */
export const NETWORK_DESCRIPTOR_KIND = "groundhog-network"

export type NetworkDescriptor = {
  networkId: string
  networkHash: string
  genesisHash: string
  endpoints: {
    rpcUrl: string
  }
  nativeToken: {
    decimals: number
  }
  airdrop: {
    rawAmount: string
  }
}

export type AirdropRequestBody = {
  recipient: string
  clientNonce: string
  networkId: string
  networkHash: string
  genesisHash: string
}

export type AirdropSuccess = {
  attemptStatus: "succeeded"
  recipient: string
  rawAmount: string
  txHash: string
  height: string
  status: { result: string }
}

export type AirdropPending = {
  attemptStatus: "pending"
  retryAfterSeconds: number
}

export function parseNetworkDescriptor(payload: unknown): NetworkDescriptor {
  if (!isRecord(payload)) {
    throw new Error("Faucet metadata had an unexpected shape.")
  }
  if (
    payload.schemaVersion !== NETWORK_DESCRIPTOR_SCHEMA_VERSION ||
    payload.kind !== NETWORK_DESCRIPTOR_KIND
  ) {
    throw new Error("Faucet metadata uses an unsupported schema.")
  }
  const endpoints = payload.endpoints
  const nativeToken = payload.nativeToken
  const airdrop = payload.airdrop
  if (!isRecord(endpoints) || !isRecord(nativeToken) || !isRecord(airdrop)) {
    throw new Error("Faucet metadata is missing required sections.")
  }
  return {
    networkId: requireString(payload.networkId, "networkId"),
    networkHash: requireIdentifier(payload.networkHash, "networkHash"),
    genesisHash: requireIdentifier(payload.genesisHash, "genesisHash"),
    endpoints: {
      rpcUrl: requireString(endpoints.rpcUrl, "endpoints.rpcUrl"),
    },
    nativeToken: {
      decimals: requireNonNegativeInteger(
        nativeToken.decimals,
        "nativeToken.decimals",
      ),
    },
    airdrop: {
      rawAmount: requireDecimalString(airdrop.rawAmount, "airdrop.rawAmount"),
    },
  }
}

/** Build the `POST /v1/airdrop` body for a base58 recipient. */
export function airdropRequestBody(
  recipient: string,
  clientNonce: string,
  descriptor: NetworkDescriptor,
): AirdropRequestBody {
  return {
    recipient,
    clientNonce,
    networkId: descriptor.networkId,
    networkHash: descriptor.networkHash,
    genesisHash: descriptor.genesisHash,
  }
}

export function parseAirdropAttempt(
  payload: unknown,
  expectedRecipient: string,
  expectedRawAmount: string,
): AirdropSuccess | AirdropPending {
  if (!isRecord(payload)) {
    throw new Error("The airdrop response had an unexpected shape.")
  }
  const attemptStatus = requireString(payload.attemptStatus, "attemptStatus")
  const recipient = requireIdentifier(payload.recipient, "recipient")
  const rawAmount = requireDecimalString(payload.rawAmount, "rawAmount")
  if (recipient !== expectedRecipient) {
    throw new Error("The airdrop response recipient did not match the request.")
  }
  if (BigInt(rawAmount) !== BigInt(expectedRawAmount)) {
    throw new Error(
      "The airdrop response amount did not match faucet metadata.",
    )
  }
  if (attemptStatus === "pending") {
    if (payload.txHash !== undefined) {
      requireIdentifier(payload.txHash, "txHash")
    }
    return {
      attemptStatus,
      retryAfterSeconds: requireSeconds(
        payload.retryAfterSeconds,
        "retryAfterSeconds",
      ),
    }
  }
  if (attemptStatus !== "succeeded") {
    throw new Error("The airdrop response had an unknown attempt status.")
  }
  const status = payload.status
  if (!isRecord(status)) {
    throw new Error("The airdrop response omitted transaction status.")
  }
  const result = requireString(status.result, "status.result")
  if (result !== "ok") {
    throw new Error("The airdrop transaction did not commit successfully.")
  }
  return {
    attemptStatus,
    recipient,
    rawAmount,
    txHash: requireIdentifier(payload.txHash, "txHash"),
    height: requireDecimalString(payload.height, "height"),
    status: { result },
  }
}

export function hasErrorEnvelope(payload: unknown) {
  return isRecord(payload) && isRecord(payload.error)
}

export function errorMessageFromPayload(payload: unknown, fallback: string) {
  if (!isRecord(payload) || !isRecord(payload.error)) {
    return fallback
  }
  const message =
    typeof payload.error.message === "string" ? payload.error.message : fallback
  const retryAfterSeconds = payload.error.retryAfterSeconds
  const retryAfter =
    typeof retryAfterSeconds === "string" && /^[0-9]+$/.test(retryAfterSeconds)
      ? ` Try again in ${retryAfterSeconds}s.`
      : ""
  return `${message}${retryAfter}`
}

export function formatNativeTokenAmount(rawAmount: string, decimals: number) {
  const raw = BigInt(rawAmount)
  const scale = 10n ** BigInt(decimals)
  const whole = raw / scale
  const fractional = raw % scale
  if (fractional === 0n) {
    return whole.toString()
  }
  const padded = fractional.toString().padStart(decimals, "0")
  return `${whole}.${padded.replace(/0+$/, "")}`
}

function requireString(value: unknown, field: string) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`Field ${field} is invalid.`)
  }
  return value
}

function requireIdentifier(value: unknown, field: string) {
  const text = requireString(value, field)
  if (!isBase58Identifier(text)) {
    throw new Error(`Field ${field} must be a base58 32-byte value.`)
  }
  return text
}

function requireDecimalString(value: unknown, field: string) {
  const text = requireString(value, field)
  if (!/^[0-9]+$/.test(text)) {
    throw new Error(`Field ${field} must be a decimal string.`)
  }
  return text
}

function requireSeconds(value: unknown, field: string) {
  const seconds = Number(requireDecimalString(value, field))
  if (!Number.isSafeInteger(seconds)) {
    throw new Error(`Field ${field} is out of range.`)
  }
  return seconds
}

function requireNonNegativeInteger(value: unknown, field: string) {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new Error(`Field ${field} must be a non-negative integer.`)
  }
  return value
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

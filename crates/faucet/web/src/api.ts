const API_BASE = normalizeApiBase(import.meta.env.VITE_GROUNDHOG_FAUCET_API_BASE)

export type NetworkDescriptor = {
  networkId: string
  genesisHash: string
  endpoints: {
    rpcUrl: string
  }
  nativeHog: {
    decimals: number
  }
  walletCreate: {
    prefundRawAmount: string
  }
}

export type AirdropSuccess = {
  recipient: string
  rawAmount: string
  txHash: string
  height: number
  status: { result: string }
}

export type FaucetError = {
  error?: {
    code?: string
    message?: string
    retryAfterSeconds?: number
  }
}

export async function fetchNetworkDescriptor() {
  ensureApiBase()
  const response = await fetch(apiUrl("/v1/network"))
  const payload = await readJsonResponse(response, "faucet metadata")
  if (!response.ok) {
    throw new Error(errorMessageFromPayload(payload, "Unable to load faucet metadata."))
  }
  return parseNetworkDescriptor(payload)
}

export async function requestAirdrop(
  recipient: string,
  descriptor: NetworkDescriptor,
) {
  ensureApiBase()
  const response = await fetch(apiUrl("/v1/airdrop"), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      recipient,
      clientNonce: randomClientNonce(),
      networkId: descriptor.networkId,
      genesisHash: descriptor.genesisHash,
    }),
  })
  const payload = await readJsonResponse(response, "airdrop response")
  if (!response.ok) {
    throw new Error(errorMessageFromPayload(payload, "Faucet request failed."))
  }
  return parseAirdropSuccess(payload)
}

export function formatHogAmount(rawAmount: string, decimals: number) {
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

function apiUrl(path: string) {
  return `${API_BASE}${path}`
}

function normalizeApiBase(value: unknown) {
  if (typeof value !== "string") {
    return ""
  }
  return value.trim().replace(/\/+$/, "")
}

function ensureApiBase() {
  if (API_BASE || import.meta.env.DEV) {
    return
  }
  throw new Error(
    "Faucet API base is not configured. Set VITE_GROUNDHOG_FAUCET_API_BASE for this deployment.",
  )
}

async function readJsonResponse(response: Response, label: string) {
  const contentType = response.headers.get("content-type") ?? ""
  const text = await response.text()
  if (!isJsonContentType(contentType)) {
    throw new Error(nonJsonError(label, response.status, text))
  }
  if (!text.trim()) {
    throw new Error(`The ${label} was empty.`)
  }
  try {
    const payload: unknown = JSON.parse(text)
    if (payload === null) {
      throw new Error(`The ${label} was JSON null.`)
    }
    return payload
  } catch (error) {
    if (error instanceof SyntaxError) {
      throw new Error(`The ${label} was not valid JSON.`)
    }
    throw error
  }
}

function nonJsonError(label: string, status: number, text: string) {
  const trimmed = text.trim().slice(0, 120).toLowerCase()
  if (trimmed.startsWith("<!doctype") || trimmed.startsWith("<html")) {
    return `The ${label} returned HTML instead of JSON. Check VITE_GROUNDHOG_FAUCET_API_BASE.`
  }
  return `The ${label} returned non-JSON content${status ? ` (HTTP ${status})` : ""}.`
}

function parseNetworkDescriptor(payload: unknown): NetworkDescriptor {
  if (!isRecord(payload)) {
    throw new Error("Faucet metadata had an unexpected shape.")
  }
  const endpoints = payload.endpoints
  const nativeHog = payload.nativeHog
  const walletCreate = payload.walletCreate
  if (!isRecord(endpoints) || !isRecord(nativeHog) || !isRecord(walletCreate)) {
    throw new Error("Faucet metadata is missing required sections.")
  }
  const descriptor: NetworkDescriptor = {
    networkId: requireString(payload.networkId, "networkId"),
    genesisHash: requireHexHash(payload.genesisHash, "genesisHash"),
    endpoints: {
      rpcUrl: requireString(endpoints.rpcUrl, "endpoints.rpcUrl"),
    },
    nativeHog: {
      decimals: requireNonNegativeInteger(nativeHog.decimals, "nativeHog.decimals"),
    },
    walletCreate: {
      prefundRawAmount: requireDecimalString(
        walletCreate.prefundRawAmount,
        "walletCreate.prefundRawAmount",
      ),
    },
  }
  return descriptor
}

function parseAirdropSuccess(payload: unknown): AirdropSuccess {
  if (!isRecord(payload)) {
    throw new Error("The airdrop response had an unexpected shape.")
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
    recipient: requireHexHash(payload.recipient, "recipient"),
    rawAmount: requireDecimalString(payload.rawAmount, "rawAmount"),
    txHash: requireHexHash(payload.txHash, "txHash"),
    height: requireNonNegativeInteger(payload.height, "height"),
    status: {
      result,
    },
  }
}

function isJsonContentType(contentType: string) {
  const mediaType = contentType.toLowerCase().split(";", 1)[0]?.trim() ?? ""
  return mediaType === "application/json" || mediaType.endsWith("+json")
}

function errorMessageFromPayload(payload: unknown, fallback: string) {
  if (!isRecord(payload) || !isRecord(payload.error)) {
    return fallback
  }
  const message = typeof payload.error.message === "string" ? payload.error.message : fallback
  const retryAfterSeconds = payload.error.retryAfterSeconds
  const retryAfter =
    typeof retryAfterSeconds === "number" && Number.isFinite(retryAfterSeconds)
      ? ` Try again in ${retryAfterSeconds}s.`
      : ""
  return `${message}${retryAfter}`
}

function requireString(value: unknown, field: string) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`Field ${field} is invalid.`)
  }
  return value
}

function requireHexHash(value: unknown, field: string) {
  const text = requireString(value, field)
  if (!/^0x[0-9a-fA-F]{64}$/.test(text)) {
    throw new Error(`Field ${field} must be a 0x-prefixed 32-byte hex value.`)
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

function requireNonNegativeInteger(value: unknown, field: string) {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new Error(`Field ${field} must be a non-negative integer.`)
  }
  return value
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function randomClientNonce() {
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  return `0x${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`
}

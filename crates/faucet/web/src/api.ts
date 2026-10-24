import {
  type NetworkDescriptor,
  airdropRequestBody,
  errorMessageFromPayload,
  hasErrorEnvelope,
  parseAirdropAttempt,
  parseNetworkDescriptor,
} from "./lib/faucet-payloads.ts"

export {
  type NetworkDescriptor,
  formatNativeTokenAmount,
} from "./lib/faucet-payloads.ts"

const API_BASE = normalizeApiBase(
  import.meta.env.VITE_GROUNDHOG_FAUCET_API_BASE,
)
const AIRDROP_POLL_TIMEOUT_MS = 10 * 60 * 1000
const pendingAirdropNonces = new Map<string, string>()

export async function fetchNetworkDescriptor() {
  ensureApiBase()
  const response = await fetch(apiUrl("/v1/network"))
  const payload = await readJsonResponse(response, "faucet metadata")
  if (!response.ok) {
    throw new Error(
      errorMessageFromPayload(payload, "Unable to load faucet metadata."),
    )
  }
  return parseNetworkDescriptor(payload)
}

export async function requestAirdrop(
  recipient: string,
  descriptor: NetworkDescriptor,
) {
  ensureApiBase()
  const attemptKey = `${descriptor.networkId}:${descriptor.genesisHash}:${recipient}`
  const clientNonce = retainedClientNonce(attemptKey)
  const deadline = Date.now() + AIRDROP_POLL_TIMEOUT_MS

  while (true) {
    const remainingMs = deadline - Date.now()
    if (remainingMs <= 0) {
      throw pendingTimeoutError()
    }
    let response: Response
    let payload: unknown
    try {
      response = await fetch(apiUrl("/v1/airdrop"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(
          airdropRequestBody(recipient, clientNonce, descriptor),
        ),
        signal: AbortSignal.timeout(remainingMs),
      })
      payload = await readJsonResponse(response, "airdrop response")
    } catch (error) {
      if (
        error instanceof DOMException &&
        (error.name === "TimeoutError" || error.name === "AbortError")
      ) {
        throw pendingTimeoutError()
      }
      throw error
    }
    if (!response.ok || hasErrorEnvelope(payload)) {
      throw new Error(
        errorMessageFromPayload(payload, "Faucet request failed."),
      )
    }
    const result = parseAirdropAttempt(
      payload,
      recipient,
      descriptor.airdrop.rawAmount,
    )
    if (result.attemptStatus === "succeeded") {
      clearRetainedClientNonce(attemptKey)
      return result
    }
    const retryDelayMs = Math.max(result.retryAfterSeconds * 1000, 250)
    if (Date.now() + retryDelayMs > deadline) {
      throw pendingTimeoutError()
    }
    await sleep(retryDelayMs)
  }
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

function isJsonContentType(contentType: string) {
  const mediaType = contentType.toLowerCase().split(";", 1)[0]?.trim() ?? ""
  return mediaType === "application/json" || mediaType.endsWith("+json")
}

function randomClientNonce() {
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  return `0x${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`
}

function retainedClientNonce(attemptKey: string) {
  const existing = pendingAirdropNonces.get(attemptKey)
  if (existing) {
    return existing
  }
  const storageKey = nonceStorageKey(attemptKey)
  try {
    const stored = sessionStorage.getItem(storageKey)
    if (stored && /^0x(?:[0-9a-fA-F]{2}){1,32}$/.test(stored)) {
      pendingAirdropNonces.set(attemptKey, stored)
      return stored
    }
  } catch {
    // In-memory retention still protects retries when storage is unavailable.
  }
  const nonce = randomClientNonce()
  pendingAirdropNonces.set(attemptKey, nonce)
  try {
    sessionStorage.setItem(storageKey, nonce)
  } catch {
    // In-memory retention still protects retries when storage is unavailable.
  }
  return nonce
}

function clearRetainedClientNonce(attemptKey: string) {
  pendingAirdropNonces.delete(attemptKey)
  try {
    sessionStorage.removeItem(nonceStorageKey(attemptKey))
  } catch {
    // The successful attempt is still cleared from in-memory state.
  }
}

function nonceStorageKey(attemptKey: string) {
  // Keep pending request identities stable across tool branding changes.
  return `groundhog-faucet:airdrop:${attemptKey}`
}

function pendingTimeoutError() {
  return new Error("The airdrop is still pending. Try again shortly.")
}

function sleep(milliseconds: number) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds))
}

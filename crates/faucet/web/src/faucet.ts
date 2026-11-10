import {
  type AirdropSucceeded,
  FundingObservationError,
  NativeRpcError,
  createThruputClient,
  futurenet,
  formatUnits,
  fromBase58,
  parseAddress,
  toBase58,
} from "@thruputchain/client"

// Protocol-defined on every network (thruput_signing::NATIVE_TOKEN_DECIMALS).
const NATIVE_TOKEN_DECIMALS = 7
const AIRDROP_TIMEOUT_MS = 10 * 60 * 1000
const pendingNonces = new Map<string, Uint8Array>()

/** Connect to the RPC gateway; fails if it does not serve `faucet_requestAirdrop`. */
export function connectFaucet() {
  const rpcUrl = import.meta.env.VITE_THRUPUT_RPC_URL?.trim() || undefined
  return createThruputClient({ cluster: futurenet({ rpcUrl }) })
}

export type FaucetClient = Awaited<ReturnType<typeof connectFaucet>>

/** Parse a Thruput address, or return null when it is invalid. */
export function parseRecipient(input: string) {
  try {
    return parseAddress(input.trim())
  } catch {
    return null
  }
}

/**
 * Request one airdrop and wait for it to commit. A failed attempt keeps its
 * client nonce, so retrying the same recipient cannot fund it twice.
 */
export async function airdrop(
  client: FaucetClient,
  recipient: Uint8Array,
): Promise<AirdropSucceeded> {
  const attemptKey = `${toBase58(client.network.networkHash)}:${toBase58(recipient)}`
  const clientNonce = retainedNonce(attemptKey)
  const result = await client.requestAirdrop({
    recipient,
    clientNonce,
    timeoutMs: AIRDROP_TIMEOUT_MS,
  })
  clearRetainedNonce(attemptKey)
  return result
}

export function formatNativeTokens(rawAmount: bigint) {
  return formatUnits(rawAmount, NATIVE_TOKEN_DECIMALS)
}

/** A user-facing message, including the faucet's retry delay when it gives one. */
export function describeFaucetError(error: unknown): string {
  const cause = error instanceof FundingObservationError ? error.cause : error
  if (cause instanceof NativeRpcError) {
    const retryAfterMs =
      cause.decoded.classification === "known" &&
      "retryAfterMs" in cause.decoded.data
        ? cause.decoded.data.retryAfterMs
        : undefined
    const retry =
      typeof retryAfterMs === "bigint"
        ? ` Try again in ${Math.ceil(Number(retryAfterMs) / 1000)}s.`
        : ""
    return `${cause.rpcMessage}${retry}`
  }
  if (error instanceof FundingObservationError) {
    return "The airdrop is still pending. Try again shortly."
  }
  return error instanceof Error ? error.message : "Faucet request failed."
}

function retainedNonce(attemptKey: string) {
  const existing = pendingNonces.get(attemptKey)
  if (existing) {
    return existing
  }
  const storageKey = nonceStorageKey(attemptKey)
  try {
    const stored = sessionStorage.getItem(storageKey)
    if (stored) {
      const nonce = fromBase58(stored, 32)
      pendingNonces.set(attemptKey, nonce)
      return nonce
    }
  } catch {
    // In-memory retention still protects retries when storage is unavailable.
  }
  const nonce = crypto.getRandomValues(new Uint8Array(16))
  pendingNonces.set(attemptKey, nonce)
  try {
    sessionStorage.setItem(storageKey, toBase58(nonce))
  } catch {
    // In-memory retention still protects retries when storage is unavailable.
  }
  return nonce
}

function clearRetainedNonce(attemptKey: string) {
  pendingNonces.delete(attemptKey)
  try {
    sessionStorage.removeItem(nonceStorageKey(attemptKey))
  } catch {
    // The successful attempt is still cleared from in-memory state.
  }
}

function nonceStorageKey(attemptKey: string) {
  return `thruput-faucet:airdrop:${attemptKey}`
}

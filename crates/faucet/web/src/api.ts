const API_BASE = import.meta.env.VITE_GROUNDHOG_FAUCET_API_BASE ?? ""

export type NetworkDescriptor = {
  networkId: string
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
  }
}

export async function fetchNetworkDescriptor() {
  const response = await fetch(apiUrl("/v1/network"))
  if (!response.ok) {
    throw new Error("Unable to load faucet metadata.")
  }
  return (await response.json()) as NetworkDescriptor
}

export async function requestAirdrop(recipient: string) {
  const response = await fetch(apiUrl("/v1/airdrop"), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ recipient }),
  })
  const payload = (await response.json()) as AirdropSuccess | FaucetError
  if (!response.ok) {
    const error = "error" in payload ? payload.error : null
    throw new Error(error?.message ?? "Faucet request failed.")
  }
  return payload as AirdropSuccess
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

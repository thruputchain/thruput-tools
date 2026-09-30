// Wallet addresses as the faucet accepts them: a C StrKey (Stellar SEP-23)
// or the legacy 0x-prefixed 64-character hex form of the same 32 bytes.

const BASE32_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
const CONTRACT_VERSION_BYTE = 2 << 3
const CONTRACT_STRKEY = /^C[A-Z2-7]{55}$/
const LEGACY_HEX = /^0x[0-9a-f]{64}$/

/** Parse a C StrKey or 0x-prefixed hex wallet address into its 32 bytes. */
export function parseWalletAddress(text: string): Uint8Array | null {
  const trimmed = text.trim()
  const lowercase = trimmed.toLowerCase()
  if (LEGACY_HEX.test(lowercase)) {
    return hexToBytes(lowercase.slice(2))
  }
  return decodeContractStrKey(trimmed.toUpperCase())
}

/** Render a wallet address as its C StrKey. */
export function formatWalletAddress(address: Uint8Array): string {
  const data = new Uint8Array(35)
  data[0] = CONTRACT_VERSION_BYTE
  data.set(address, 1)
  const checksum = crc16Xmodem(data.subarray(0, 33))
  data[33] = checksum & 0xff
  data[34] = checksum >> 8
  return base32Encode(data)
}

/** Render a wallet address as 0x-prefixed lowercase hex. */
export function hexWalletAddress(address: Uint8Array): string {
  return `0x${Array.from(address, (byte) => byte.toString(16).padStart(2, "0")).join("")}`
}

export function sameWalletAddress(left: Uint8Array, right: Uint8Array) {
  return (
    left.length === right.length &&
    left.every((byte, index) => byte === right[index])
  )
}

function decodeContractStrKey(text: string): Uint8Array | null {
  if (!CONTRACT_STRKEY.test(text)) {
    return null
  }
  const data = base32Decode(text)
  if (data.length !== 35 || data[0] !== CONTRACT_VERSION_BYTE) {
    return null
  }
  const checksum = crc16Xmodem(data.subarray(0, 33))
  if (data[33] !== (checksum & 0xff) || data[34] !== checksum >> 8) {
    return null
  }
  return data.slice(1, 33)
}

function hexToBytes(hex: string) {
  const bytes = new Uint8Array(hex.length / 2)
  for (let index = 0; index < bytes.length; index++) {
    bytes[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16)
  }
  return bytes
}

function base32Encode(bytes: Uint8Array) {
  let output = ""
  let value = 0
  let bits = 0
  for (const byte of bytes) {
    value = ((value << 8) | byte) & 0xfff
    bits += 8
    while (bits >= 5) {
      bits -= 5
      output += BASE32_ALPHABET[(value >>> bits) & 31]
    }
  }
  if (bits > 0) {
    output += BASE32_ALPHABET[(value << (5 - bits)) & 31]
  }
  return output
}

// Callers pass only text that matches CONTRACT_STRKEY, so every character is
// in the alphabet and 56 characters decode to exactly 35 bytes.
function base32Decode(text: string) {
  const output: number[] = []
  let value = 0
  let bits = 0
  for (const char of text) {
    value = ((value << 5) | BASE32_ALPHABET.indexOf(char)) & 0xfff
    bits += 5
    if (bits >= 8) {
      bits -= 8
      output.push((value >>> bits) & 0xff)
    }
  }
  return Uint8Array.from(output)
}

function crc16Xmodem(bytes: Uint8Array) {
  let crc = 0
  for (const byte of bytes) {
    crc ^= byte << 8
    for (let bit = 0; bit < 8; bit++) {
      crc = crc & 0x8000 ? ((crc << 1) ^ 0x1021) & 0xffff : (crc << 1) & 0xffff
    }
  }
  return crc
}

// Thruput address text, mirroring `thruput_encoding::parse_address`: people may
// paste a C StrKey or base58, but the faucet API accepts base58 only.

const BASE58_ALPHABET =
  "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
const BASE32_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
const ADDRESS_BYTES = 32
// Stellar StrKey version byte for contract ("C") addresses.
const CONTRACT_STRKEY_VERSION = 2 << 3
const STRKEY_LENGTH = 56

/** Encode bytes with the Bitcoin/Solana base58 alphabet, keeping leading zeros. */
export function encodeBase58(bytes: Uint8Array): string {
  const digits: number[] = []
  for (const byte of bytes) {
    let carry = byte
    for (let i = 0; i < digits.length; i++) {
      carry += digits[i] << 8
      digits[i] = carry % 58
      carry = Math.floor(carry / 58)
    }
    while (carry > 0) {
      digits.push(carry % 58)
      carry = Math.floor(carry / 58)
    }
  }
  let leadingZeros = 0
  while (leadingZeros < bytes.length && bytes[leadingZeros] === 0) {
    leadingZeros++
  }
  return (
    "1".repeat(leadingZeros) +
    digits
      .reverse()
      .map((digit) => BASE58_ALPHABET[digit])
      .join("")
  )
}

/** Decode base58 text, or return null when it contains a character outside the alphabet. */
export function decodeBase58(text: string): Uint8Array | null {
  const bytes: number[] = []
  for (const char of text) {
    let carry = BASE58_ALPHABET.indexOf(char)
    if (carry < 0) {
      return null
    }
    for (let i = 0; i < bytes.length; i++) {
      carry += bytes[i] * 58
      bytes[i] = carry & 0xff
      carry >>= 8
    }
    while (carry > 0) {
      bytes.push(carry & 0xff)
      carry >>= 8
    }
  }
  let leadingZeros = 0
  while (leadingZeros < text.length && text[leadingZeros] === "1") {
    leadingZeros++
  }
  const result = new Uint8Array(leadingZeros + bytes.length)
  result.set(bytes.reverse(), leadingZeros)
  return result
}

/** Whether text is base58 that decodes to exactly 32 bytes, as API identifiers must. */
export function isBase58Identifier(text: string): boolean {
  return decodeBase58(text)?.length === ADDRESS_BYTES
}

/**
 * Parse a human-entered address (C StrKey or 32-byte base58) into the
 * base58 form the faucet API accepts, or null when it is neither.
 */
export function parseThruputAddress(input: string): string | null {
  const text = input.trim()
  const strkeyBytes = decodeContractStrKey(text)
  if (strkeyBytes) {
    return encodeBase58(strkeyBytes)
  }
  const bytes = decodeBase58(text)
  return bytes?.length === ADDRESS_BYTES ? encodeBase58(bytes) : null
}

function decodeContractStrKey(text: string): Uint8Array | null {
  if (text.length !== STRKEY_LENGTH) {
    return null
  }
  const data = decodeBase32(text)
  if (!data || data[0] !== CONTRACT_STRKEY_VERSION) {
    return null
  }
  const body = data.subarray(0, 1 + ADDRESS_BYTES)
  const checksum = data[33] | (data[34] << 8)
  if (crc16Xmodem(body) !== checksum) {
    return null
  }
  return data.slice(1, 1 + ADDRESS_BYTES)
}

// Unpadded RFC 4648 base32; 56 characters carry exactly 35 bytes.
function decodeBase32(text: string): Uint8Array | null {
  const output = new Uint8Array((text.length * 5) / 8)
  let buffer = 0
  let bits = 0
  let index = 0
  for (const char of text) {
    const value = BASE32_ALPHABET.indexOf(char)
    if (value < 0) {
      return null
    }
    buffer = (buffer << 5) | value
    bits += 5
    if (bits >= 8) {
      bits -= 8
      output[index++] = (buffer >> bits) & 0xff
    }
  }
  return output
}

function crc16Xmodem(bytes: Uint8Array): number {
  let crc = 0
  for (const byte of bytes) {
    crc ^= byte << 8
    for (let bit = 0; bit < 8; bit++) {
      crc = crc & 0x8000 ? ((crc << 1) ^ 0x1021) & 0xffff : (crc << 1) & 0xffff
    }
  }
  return crc
}

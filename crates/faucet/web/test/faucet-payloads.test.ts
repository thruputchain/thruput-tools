// Checks the client against payloads captured from the faucet server by
// `web_client_fixtures_match_server_responses` in `crates/faucet/tests/http.rs`.

import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { test } from "node:test"

import {
  decodeBase58,
  encodeBase58,
  parseThruputAddress,
} from "../src/lib/address.ts"
import {
  airdropRequestBody,
  errorMessageFromPayload,
  formatNativeTokenAmount,
  hasErrorEnvelope,
  parseAirdropAttempt,
  parseNetworkDescriptor,
} from "../src/lib/faucet-payloads.ts"

function fixture(name: string): any {
  return JSON.parse(
    readFileSync(new URL(`./fixtures/${name}`, import.meta.url), "utf8"),
  )
}

const network = fixture("network.json")
const request = fixture("airdrop-request.json")
const descriptor = parseNetworkDescriptor(network)

test("parses the server's network descriptor", () => {
  assert.equal(descriptor.networkId, network.networkId)
  assert.equal(descriptor.networkHash, network.networkHash)
  assert.equal(descriptor.genesisHash, network.genesisHash)
  assert.equal(descriptor.nativeToken.decimals, network.nativeToken.decimals)
  assert.equal(descriptor.airdrop.rawAmount, network.airdrop.rawAmount)
})

test("rejects a descriptor from another schema version", () => {
  assert.throws(
    () => parseNetworkDescriptor({ ...network, schemaVersion: 6 }),
    /unsupported schema/,
  )
})

test("builds the airdrop request the server accepts", () => {
  assert.deepEqual(
    airdropRequestBody(request.recipient, request.clientNonce, descriptor),
    request,
  )
})

test("parses a succeeded airdrop", () => {
  const payload = fixture("airdrop-succeeded.json")
  const result = parseAirdropAttempt(
    payload,
    request.recipient,
    descriptor.airdrop.rawAmount,
  )
  assert.equal(result.attemptStatus, "succeeded")
  assert.ok(result.attemptStatus === "succeeded")
  assert.equal(result.txHash, payload.txHash)
  assert.equal(result.height, payload.height)
})

test("parses a pending airdrop's retry delay", () => {
  const payload = fixture("airdrop-pending.json")
  const result = parseAirdropAttempt(
    payload,
    request.recipient,
    descriptor.airdrop.rawAmount,
  )
  assert.deepEqual(result, {
    attemptStatus: "pending",
    retryAfterSeconds: Number(payload.retryAfterSeconds),
  })
})

test("rejects a response for another recipient", () => {
  assert.throws(
    () =>
      parseAirdropAttempt(
        fixture("airdrop-succeeded.json"),
        fixture("address.json").base58,
        descriptor.airdrop.rawAmount,
      ),
    /recipient did not match/,
  )
})

test("reports a rate-limited error with its retry delay", () => {
  const payload = fixture("airdrop-rate-limited.json")
  assert.ok(hasErrorEnvelope(payload))
  assert.equal(
    errorMessageFromPayload(payload, "fallback"),
    `${payload.error.message} Try again in ${payload.error.retryAfterSeconds}s.`,
  )
})

test("accepts the same address as a C StrKey or base58", () => {
  const address = fixture("address.json")
  assert.equal(parseThruputAddress(address.strkey), address.base58)
  assert.equal(parseThruputAddress(` ${address.base58} `), address.base58)
})

test("rejects malformed addresses", () => {
  const address = fixture("address.json")
  const corrupted = `${address.strkey.slice(0, -1)}${
    address.strkey.endsWith("A") ? "B" : "A"
  }`
  assert.equal(parseThruputAddress(corrupted), null)
  assert.equal(parseThruputAddress(`0x${"11".repeat(32)}`), null)
  assert.equal(parseThruputAddress(address.base58.slice(1)), null)
  assert.equal(parseThruputAddress(""), null)
})

test("rejects oversized address text without decoding it", () => {
  const started = performance.now()
  assert.equal(parseThruputAddress("z".repeat(1_000_000)), null)
  assert.ok(performance.now() - started < 1000)
})

test("base58 round-trips leading zero bytes", () => {
  const bytes = new Uint8Array([0, 0, 1, 2, 255])
  assert.deepEqual(decodeBase58(encodeBase58(bytes)), bytes)
})

test("formats raw amounts with the descriptor's decimals", () => {
  assert.equal(formatNativeTokenAmount("100000000", 7), "10")
  assert.equal(formatNativeTokenAmount("15000000", 7), "1.5")
})

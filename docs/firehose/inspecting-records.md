# Inspecting firehose records

`hog-firehose-store` archives each committed block as one XDR-encoded
`EventIndexerPulledBlock` file. The `inspect_record` example in
[`crates/firehose`](../../crates/firehose/README.md) decodes those files offline
and reports what is in them.

## Get a record

A firehose subscription with `{"from":"height","height":H}` returns the exact
stored record: the notification's `record` field is the archived XDR,
hex-encoded. This works only for heights inside serve's `--request-window`
(default 42,428 blocks) and records at or below `--max-record-bytes`
(default 4 MiB).

Larger or older retained records must be copied from the store's archive
directory on the host, where each block is `<archive>/<20-digit height>.xdr`.

## Run the inspector

Records decode only with the XDR of the protocol that produced them. Build the
example from the tools release the network runs, which is the same version the
firehose binaries are pinned to.

```sh
cargo run -p thruput-firehose --example inspect_record --offline -- /path/to/00000000000000000001.xdr
# Use --text for an indented report; --json (one object per file) is the default.
```

## What the report contains

- Encoded byte sizes split into consensus value, event logs and transaction
  bodies, with transaction size distribution and histogram.
- Per transaction: invoked contract and selector, auth claim, signature scheme,
  gas limit, nonce, expiry and execution outcome.
- Senders: pubkey-signature senders are derived offline from their auth
  preimages, with counts, shares of all transactions, nonce ranges, and
  self-invocation counts. Auth variants without a derivable preimage report a
  null sender and their auth variant.
- Event counts by emitting contract and topic, and success and failure counts.

Transactions have no operation-kind union, and selectors and topics are
reported as hexadecimal identifiers without inferred names.

The inspector is a decoder and analyser, not a commitment audit; use the store's
`verify` command to audit an archive.

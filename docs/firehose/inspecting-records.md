# Inspect firehose records

`hog-firehose-store` archives committed blocks as XDR files. The `inspect_record`
example decodes them offline and reports sizes, transactions, senders, and events.

## Get a record

A firehose subscription with `{"from":"height","height":H}` returns the archived
XDR as hex in the notification's `record` field. Requests are limited by serve's
`--request-window` and `--max-record-bytes` options. For larger or older retained
records, copy `<archive>/<20-digit height>.xdr` from the store host.

See the [firehose guide](../../crates/firehose/README.md) for subscriptions and
archive operations.

## Run the inspector

Use a tools checkout with XDR definitions compatible with the producer. From its
repository root:

```sh
cargo run --locked -p thruput-firehose --example inspect_record -- \
  /path/to/00000000000000000001.xdr
```

The default output is one JSON object per file. Add `--text` for an indented
report. Selectors and topics are reported as hex identifiers without inferred
names. Auth variants without a derivable sender report a null sender.

The inspector decodes records; it does not verify commitments or signatures.
Use `hog-firehose-store verify` to check archive consistency. See the
[inspector source](../../crates/firehose/examples/inspect_record.rs) for report
fields and the firehose guide for the store's verification limits.

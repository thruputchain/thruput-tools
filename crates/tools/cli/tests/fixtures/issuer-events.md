# Issuer token events

`issuer-events.json` contains actual SDK RV32 runtime events. Regenerate from
`example-contracts/` in the SDK:

```sh
ISSUER_TOKEN_RV32_ARTIFACT="$PWD/../target/groundhog/erc20_issuer.rv32a" \
ISSUER_TOKEN_CLI_EVENTS_OUT=/tmp/issuer-events.json \
  cargo test -p erc20_issuer --locked export_cli_event_fixture -- --ignored
```

The HTTP test checks raw preservation of all nine events and the four canonical
movements. Clawback emits a standard burn plus context; only the burn contributes
to movement history. This needs no new CLI dependency or JSON schema version.
Transaction hashes and block heights in the HTTP test are synthetic. It does
not test live indexer ingestion.

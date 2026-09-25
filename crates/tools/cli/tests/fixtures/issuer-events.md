# Issuer token events

`issuer-events.json` contains actual SDK RV32 runtime events. Regenerate from
`example-contracts/` in the SDK:

```sh
ISSUER_TOKEN_RV32_ARTIFACT="$PWD/../target/groundhog/erc20_issuer.rv32a" \
ISSUER_TOKEN_CLI_EVENTS_OUT=/tmp/issuer-events.json \
  cargo test -p erc20_issuer --locked export_cli_event_fixture -- --ignored
```

Clawback emits a standard burn plus context; only the burn contributes to
movement history. HTTP transaction hashes and block heights are synthetic.

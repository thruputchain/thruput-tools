# Issuer token events

`issuer-events.json` contains RV32 runtime events from
[SDK 0.56.0](https://github.com/thruputchain/thruput-sdk-rs/tree/afcbd97b081a15ac5eddda33cd4492682182bd82).
Regenerate from `example-contracts/` in that checkout:

```sh
thruput build -p erc20_issuer -- --locked -Zbuild-std=core,alloc
ISSUER_TOKEN_RV32_ARTIFACT="$PWD/../target/thruput/erc20_issuer.rv32a" \
ISSUER_TOKEN_CLI_EVENTS_OUT=/tmp/issuer-events.json \
  cargo test -p erc20_issuer --locked export_cli_event_fixture -- --ignored
```

Clawback emits a standard burn plus context; only the burn contributes to
movement history. HTTP transaction hashes and block heights are synthetic.

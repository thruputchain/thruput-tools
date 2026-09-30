# Issuer token events

`issuer-events.json` contains RV32 runtime events from
[SDK 0.42.1](https://github.com/groundhogchain/sdk_rs/tree/7b96330dcfc2a7240cfb052bcb0d47fa07ec51b4).
Regenerate from `example-contracts/` in that checkout:

```sh
groundhog build -p erc20_issuer -- --locked -Zbuild-std=core,alloc
ISSUER_TOKEN_RV32_ARTIFACT="$PWD/../target/groundhog/erc20_issuer.rv32a" \
ISSUER_TOKEN_CLI_EVENTS_OUT=/tmp/issuer-events.json \
  cargo test -p erc20_issuer --locked export_cli_event_fixture -- --ignored
```

Clawback emits a standard burn plus context; only the burn contributes to
movement history. HTTP transaction hashes and block heights are synthetic.

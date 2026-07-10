# Groundhog Faucet

`groundhog-faucet` serves the hosted Devnet network descriptor, airdrops, and
sponsored wallet creation. The signing key is loaded once at startup and never
belongs in the TOML config, process arguments, or logs.

## Local key source

Use an environment variable for local `hog-devnode` development:

```toml
[airdrop.secret_key_source]
type = "environment"
variable = "GROUNDHOG_FAUCET_SECRET_KEY"
```

Set the variable to the faucet account's 32-byte Ed25519 secret key as 64 hex
characters, with an optional `0x` prefix. Its derived public key must equal
`network.faucet.public_key`; startup fails before serving if they differ. The
key must control the configured `network.faucet.wallet_address`. Local flows may
use the seed-derived `dev-0` wallet that `groundhog` already treats as the
fallback local faucet sponsor.

## Hosted AWS key source

Hosted Devnet loads the same hex string from AWS Secrets Manager:

```toml
[airdrop.secret_key_source]
type = "aws_secrets_manager"
secret_id = "groundhog/devnet/faucet"
region = "us-west-2"
```

The secret must contain a `SecretString` whose entire value is the hex-encoded
32-byte key. `SecretBinary` and JSON-wrapped values are rejected. The hosted
unit should receive an instance/task role granting only
`secretsmanager:GetSecretValue` on this secret. The SDK gets those credentials
from the environment, shared AWS config, or EC2 instance metadata. Do not put
static AWS credentials in faucet config.

Set `network.faucet.public_key` and `network.faucet.wallet_address` from the
non-secret hosted-bootstrap faucet metadata. Only the secret ID/ARN and region
belong in the hosted runtime config; the secret payload remains in Secrets
Manager.

Startup fails before serving requests when the source is incomplete,
inaccessible, binary, or malformed. Errors identify the source and operation
but never include the secret payload.

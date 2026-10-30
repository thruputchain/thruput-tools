# Security policy

Report suspected vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/thruputchain/thruput-tools/security/advisories/new).
Include the affected version or commit, reproduction steps, expected impact,
and a minimal example. Keep signing keys, access tokens, and personal data out
of reports and attachments.

The `thruputchain/core-maintainers` team handles reports and coordinates fixes
and disclosure. Please use a public issue for ordinary bugs and support
questions, and the private reporting form for vulnerabilities.

## Supported versions

Security fixes target the latest published release and the current default
branch. Reports affecting older releases are welcome, but maintainers may ask
users to upgrade rather than provide a backport. Release and upgrade notes
are recorded in [CHANGELOG.md](CHANGELOG.md).

## Scope

This repository contains off-chain tools, including signing and key storage,
runtime acquisition, JSON-RPC services, the faucet, and generated applications.
The node runtime is maintained in the separate
[`hogd` repository](https://github.com/thruputchain/hogd). Identify the component
in your report so maintainers can coordinate across repositories when needed.

Deterministic development accounts and keys in test vectors are public fixtures.
Do not use them to secure real funds or a deployed network. A public source
release is not a claim that every component has received an independent audit.

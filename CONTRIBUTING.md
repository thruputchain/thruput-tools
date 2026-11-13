# Contributing

Start with the [README development instructions](README.md#contributing) and
the [architecture guide](docs/architecture.md). The repository pins its Rust
toolchain in [rust-toolchain.toml](rust-toolchain.toml). The
[CI workflow](.github/workflows/ci.yml) is the source for required checks,
including generated-project and TypeScript validation.

Contributors without repository write access should fork the repository and
open a pull request against `main`. Repository writers can use feature branches
in this repository. Fork PRs run on GitHub-hosted runners; same-repository PRs,
main pushes, and maintainer workflows use Namespace. Both paths run the same
checks. Outside-contributor workflows may need a repository writer's approval
before running; workflow approval and approval to merge are separate.

Reviewers are chosen manually. Merging normally requires approval from someone
with repository write access. Members of `core-maintainers` may explicitly
bypass review approval when merging a pull request; required CI checks and the other branch
protections still apply.

Describe the concrete problem, resulting behavior, and checks you ran. Include a
regression test when it verifies a meaningful behavior change. Keep unrelated
changes separate, preserve public wire encodings, and follow the owning crate's
guidance.

Use [issues](https://github.com/thruputchain/thruput-tools/issues) for bug reports
and feature proposals. For a substantial API or architectural change, explain
the intended behavior in an issue before implementing it. Maintainers review
contributions as capacity permits; an open issue does not guarantee a timeline.

Follow the [code of conduct](CODE_OF_CONDUCT.md). Report vulnerabilities through
the private channel in [SECURITY.md](SECURITY.md).

Unless explicitly stated otherwise, contributions are submitted under this
repository's [Apache-2.0 license](LICENSE). Submit only material you have the
right to contribute and preserve third-party notices and attribution.

## Dependency updates

[Dependabot](.github/dependabot.yml) checks Rust, npm, and GitHub Actions
dependencies weekly. Routine third-party version updates wait until a release
is at least eight days old. First-party packages and shared actions are exempt
so coordinated Thruput releases can proceed without that delay.

The cooldown controls Dependabot's version selection. Security-update PRs can
open immediately and still need the usual review and CI checks. See
[GitHub's cooldown documentation](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference#cooldown).

For manual Rust updates, use
[cargo-cooldown](https://crates.io/crates/cargo-cooldown) instead of
`cargo update`. It takes the same arguments and only selects releases old
enough for the policy:

```sh
cargo install cargo-cooldown --version 0.3.5 --locked
cargo cooldown update
```

CI's `cooldown` job checks `Cargo.lock` against the policy with
[cargo-cooldown-check](https://github.com/NomicFoundation/cargo-cooldown-check)
before any job builds dependency code. To run the same check locally:

```sh
cargo install --locked --git https://github.com/NomicFoundation/cargo-cooldown-check \
    --rev d519cf72eed8dde4301f83298ee08366cf3b1cea
cargo metadata --locked >/dev/null # fail on stale lockfile
cargo cooldown-check
```

The policy lives in [cooldown.toml](cooldown.toml) for `cargo cooldown`, and in
[.cargo/cooldown.toml](.cargo/cooldown.toml) and
[.cargo/cooldown-allowlist.toml](.cargo/cooldown-allowlist.toml) for
`cargo cooldown-check`. Keep them and [.github/dependabot.yml](.github/dependabot.yml)
in sync. For an urgent security fix, add the release as an `[[allow.exact]]`
entry in both `cooldown.toml` and `.cargo/cooldown-allowlist.toml`, with the
reason and a removal date, and give the reason in the PR.

The faucet web app sets the same policy for pnpm in
[crates/faucet/web/pnpm-workspace.yaml](crates/faucet/web/pnpm-workspace.yaml):
`pnpm install` and `pnpm update` only select releases old enough, and CI's
`pnpm install --frozen-lockfile` rejects a lockfile entry that is too new.
Manual updates in [scripts/scaffold-app](scripts/scaffold-app), which uses
npm, should follow the same waiting period; CI does not check them.

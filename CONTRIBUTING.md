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

Describe the concrete problem, resulting behavior, and checks you ran. Include a
regression test when it verifies a meaningful behavior change. Keep unrelated changes separate, preserve public
wire encodings, and follow the owning crate's guidance.

Use [issues](https://github.com/thruputchain/thruput-tools/issues) for bug reports
and feature proposals. For a substantial API or architectural change, explain
the intended behavior in an issue before implementing it. Maintainers review
contributions as capacity permits; an open issue does not guarantee a timeline.

Follow the [code of conduct](CODE_OF_CONDUCT.md). Report vulnerabilities through
the private channel in [SECURITY.md](SECURITY.md).

Unless explicitly stated otherwise, contributions are submitted under this
repository's [Apache-2.0 license](LICENSE). Submit only material you have the
right to contribute and preserve third-party notices and attribution.

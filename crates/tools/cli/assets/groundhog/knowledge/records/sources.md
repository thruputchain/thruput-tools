+++
schema_version = 1
id = "groundhog.sources"
owner = "groundhog-rpc"
kind = "reference"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 70
+++
## Sources and Escalation

Use this source order:

1. Project `AGENTS.md`, manifests, generated artifacts, and current code.
2. Installed `groundhog` help and structured output.
3. Public SDK Rustdoc, TypeScript declarations, generated bindings, and
   Groundhog developer documentation matching the installed versions.
4. Current blessed examples.
5. Public implementation source when a documented contract is incomplete.
6. Dependency internals only when debugging the dependency itself or when the
   public contract cannot answer the question.

When sources disagree, stop and identify the versions and exact disagreement.
Do not silently combine APIs from different Groundhog releases. Prefer the
installed scaffold and the public dependencies selected by that project.

Never print, log, store, or paste private signing material. Use public
addresses, transaction hashes, and redacted configuration when reporting a
problem.

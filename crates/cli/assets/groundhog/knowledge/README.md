# Agent knowledge bundle

This directory is the canonical source for the knowledge embedded in the
Groundhog CLI. Do not edit files under `generated/` directly.

Build and verify the bundle from the workspace root:

```console
cargo run -p groundhog-agent-knowledge -- build
cargo run -p groundhog-agent-knowledge -- check
```

The build is deterministic. It emits:

- `generated/records.jsonl`: normalized records sorted by stable ID;
- `generated/manifest.json`: owners, compatibility, record IDs, and artifact
  SHA-256 hashes;
- `generated/SKILL.md`: bootstrap records ordered by `bootstrap_order`;
- `generated/agent-guide.md`: the same bootstrap with the concrete CLI version,
  ready for a pre-install web entry point. Its hash is included in the manifest.

## Source contract

`bundle.toml` declares the bundle ID, allowed record owners, compatibility
components, and skill identity. The generated manifest records the concrete
Cargo package version. `{{GROUNDHOG_CLI_VERSION}}` remains in the generated
skill template and is replaced by the released CLI.

Local records live in `records/*.md`. Each starts with TOML frontmatter:

```toml
+++
schema_version = 1
id = "groundhog.example"
owner = "groundhog-rpc"
kind = "guide"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 10
+++
```

The Markdown after the frontmatter is the record content. A record without
`bootstrap_order` remains available to the normalized bundle but is omitted
from the compact skill.

Future repository producers emit one normalized JSON object per line under
`imports/*.jsonl`. Imported records use the fields in `generated/records.jsonl`
and are validated against the same owners, components, stable IDs, and schema
version. Their `source` is an owner-qualified canonical relative path such as
`sdk_rs:crates/sdk/src/lib.rs`. The assembly step rejects duplicate IDs and
bootstrap order values.

Each fact has one owning record. Other documentation and delivery adapters
should link to or render that record instead of maintaining another copy.

## Release and hosting handoff

The release workflow checks these snapshots and compares `agent-guide.md`
byte-for-byte with the release binary's `groundhog --skill` output. The guide
ships inside each CLI archive and as a standalone release asset.

A docs publisher should retain each released guide at
`/agents/<groundhog-cli-version>/agent-guide.md` and promote the selected,
qualified release to `/agent-guide.md`. Copy the release artifact unchanged;
do not regenerate it with a newer checkout or choose the newest prerelease
implicitly. Historical version URLs must retain their original bytes.
This repository prepares the artifact; publishing those public URLs belongs
to the developer site's deployment.

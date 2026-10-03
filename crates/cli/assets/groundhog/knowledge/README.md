# Bundled agent guidance

The maintained source is
[groundhogchain/agent-skills](https://github.com/groundhogchain/agent-skills).
Edit `skills/groundhog/SKILL.md` and its references there. This directory
contains an imported snapshot and generated CLI artifacts.

## Select a skill revision

Check out the intended commit in the skill repository, then run:

```sh
cargo run -p groundhog-agent-knowledge -- import /path/to/agent-skills
cargo run -p groundhog-agent-knowledge -- check
cargo test -p groundhog-cli skill
cargo test -p groundhog-agent-knowledge
```

Import requires a clean checkout whose origin is the maintained repository.
`--allow-dirty` permits a local preview, recording `dirty: true` and a null
revision for an unborn repository. Before release, commit and publish the
source and import it again without that option.
The release workflow runs `check-release`, which rejects preview snapshots.

`skill-source.json` records the source repository, revision, dirty state,
content digest, and complete skill files. Do not edit this imported file.
Ordinary CLI builds use these bytes without network or repository access.

## Generate release artifacts

```sh
cargo run -p groundhog-agent-knowledge -- build
cargo run -p groundhog-agent-knowledge -- check
```

The build emits the full guide (`generated/SKILL.md`), its
concrete CLI-version form (`generated/agent-guide.md`), source-file records,
and a manifest with artifact hashes. Generation is deterministic. The CLI's
`skill show` and `skill list` read the imported files;
`skill show --all` prints the full guide.

Installed skills update independently through `npx skills`. They can be newer
than this CLI bundle. Neither source overrides the project's installed APIs.

## Hosting handoff

The release workflow compares `agent-guide.md` byte-for-byte with the release
binary's `groundhog skill show --all` output. It ships with the CLI archives and as a
standalone release asset.

A docs publisher should retain each released guide at
`/agents/<groundhog-cli-version>/agent-guide.md` and promote the selected,
qualified release to `/agent-guide.md`. Copy the release artifact unchanged.
Historical version URLs must retain their original bytes.

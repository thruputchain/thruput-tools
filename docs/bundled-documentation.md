# Bundled documentation maintenance

The CLI embeds a pinned developer-site snapshot for `groundhog docs`.
Ordinary source builds use the checked-in snapshot; they do not require a
`developers-site` checkout. The procedure below is for maintainers selecting a
new snapshot.

1. In `developers-site`, build the selected clean commit with
   `pnpm package:snapshot`, or download `docs-snapshot-<commit>` from its successful
   CI run. It contains `snapshot.tar.gz` and `snapshot-lock.json`.
2. Run `python3 scripts/import-docs-snapshot.py /path/to/artifact` here. This
   validates the archive's checksum, size, clean source identity and contents,
   then copies both files into `crates/tools/cli/assets/docs/`.
3. Review the source revision, documentation/version notes, and archive pin;
   commit both files with the CLI change. Never select the latest docs implicitly.
4. Run the snapshot checks and CLI tests. Normal CLI packaging embeds those exact
   bytes, so consumers need no build-time docs checkout or separate access token.

`python3 scripts/import-docs-snapshot.py` without an argument verifies the current
pin. The Rust bundle test checks the same pin/content contract and cache recovery.
The full documentation set can evolve independently of CLI releases; changing
what a CLI ships is an explicit selection. A guide version in the manifest does
not assert that every SDK example or deployed Devnet matches that version.

# Update the CLI's bundled documentation

`thruput docs` serves a pinned developer-site snapshot embedded in the CLI.
Ordinary builds use the checked-in archive and need no site checkout. This
snapshot is separate from the repository documentation in `docs/`.

## Select and import a snapshot

1. In [thruput-docs](https://github.com/thruputchain/thruput-docs), build a clean
   selected commit with `pnpm package:snapshot`, or download
   `docs-snapshot-<commit>` from its successful CI run. The artifact contains
   `snapshot.tar.gz` and `snapshot-lock.json`.
2. From this repository's root, run:

   ```sh
   python3 scripts/import-docs-snapshot.py /path/to/artifact
   ```

   The importer verifies source identity, checksum, size, and required content.
   It removes historical skill downloads and adds missing bundled font notices.
3. Review and commit both updated files under `crates/cli/assets/docs/` with the
   CLI change. Select a specific source revision; do not track latest implicitly.
4. Verify the pin and run the focused checks:

   ```sh
   python3 scripts/import-docs-snapshot.py
   python3 -m unittest discover -s scripts -p test_import_docs_snapshot.py
   cargo test --locked -p thruput --lib docs_snapshot::tests
   ```

The archive contains the browser site and a self-contained Markdown tree for
`thruput docs export`. Both must identify the same source snapshot. Keep generated
content together; the CLI does not derive Markdown from HTML.

The [importer](../scripts/import-docs-snapshot.py) defines archive validation and
filtering. [docs_snapshot.rs](../crates/cli/src/docs_snapshot.rs) verifies the
embedded bundle and its cache. See [docs.rs](../crates/cli/src/docs.rs) and
[export.rs](../crates/cli/src/docs/export.rs) for external snapshot and export
requirements. Font notices come from the checked-in
[license texts](../crates/cli/assets/docs/font-licenses/).

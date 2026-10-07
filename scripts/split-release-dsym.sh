#!/usr/bin/env bash
# Split a staged macOS binary and restore its ad-hoc signature after stripping.
set -euo pipefail

binary=${1:?staged binary path required}
debug_root=${2:?debuginfo staging root required}
archive=${3:?debuginfo archive path required}

app=$(basename "$binary")
mkdir -p "$debug_root"
dsymutil "$binary" -o "$debug_root/$app.dSYM"
strip -S "$binary"
codesign --force --sign - "$binary"
codesign --verify --strict "$binary"
tar -czf "$archive" -C "$debug_root" "$app.dSYM"

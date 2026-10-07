#!/usr/bin/env bash
# Split a staged Linux ELF binary; retain its symbol table for profilers.
set -euo pipefail

binary=${1:?staged binary path required}
debug_root=${2:?debuginfo staging root required}
archive=${3:?debuginfo archive path required}

build_id=$(readelf -n "$binary" | awk '/Build ID:/ { print $3 }')
if [[ ! "$build_id" =~ ^[0-9a-f]{40}$ ]]; then
  echo "expected a SHA-1 GNU build-id in $binary, got: $build_id" >&2
  exit 1
fi
debug_dir="$debug_root/usr/lib/debug/.build-id/${build_id:0:2}"
debug_file="$debug_dir/${build_id:2}.debug"
mkdir -p "$debug_dir"
objcopy --only-keep-debug "$binary" "$debug_file"
objcopy --strip-debug --add-gnu-debuglink="$debug_file" "$binary"
readelf -SW "$binary" | grep '\.symtab ' > /dev/null
readelf -SW "$binary" | grep '\.gnu_debuglink ' > /dev/null
tar -czf "$archive" -C "$debug_root" usr

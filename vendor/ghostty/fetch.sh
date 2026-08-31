#!/bin/bash
# ninja q0: fetch the pinned ghostty source (embed API) into vendor/ghostty/src.
# Reproducibility: pinned commit + sha256 of the codeload tarball.
# Alternatives for the source (checked in order):
#   1. already-extracted vendor/ghostty/src (no-op)
#   2. GHOSTTY_EMBED_TARBALL env (pre-downloaded tarball, checksum still verified)
#   3. download from codeload.github.com
set -euo pipefail
cd "$(dirname "$0")"

COMMIT=a887df42c56f6de86c0fe6da9c4eeca37931e083
SHA256=fb4b2f9ffa0af125983041fdbe4ef94d3fa79fb9f2d22b9c213c0e3847a866b6
URL="https://codeload.github.com/ghostty-org/ghostty/tar.gz/${COMMIT}"

if [ -f src/build.zig ] && [ -f src/include/ghostty.h ]; then
  exit 0
fi

tarball="${GHOSTTY_EMBED_TARBALL:-}"
if [ -z "${tarball}" ]; then
  echo "fetch: downloading ghostty @ ${COMMIT}" >&2
  tarball="$(mktemp -t ghostty-a887df42).tar.gz"
  curl -sL --fail --retry 3 -o "${tarball}" "${URL}"
fi

actual="$(shasum -a 256 "${tarball}" | cut -d' ' -f1)"
if [ "${actual}" != "${SHA256}" ]; then
  echo "fetch: sha256 mismatch for ghostty ${COMMIT}" >&2
  echo "  expected ${SHA256}" >&2
  echo "  got      ${actual}" >&2
  exit 1
fi

rm -rf src
mkdir -p src
tar -xzf "${tarball}" -C src --strip-components=1
test -f src/include/ghostty.h
echo "fetch: ghostty ${COMMIT} (sha256 ok) -> src/" >&2

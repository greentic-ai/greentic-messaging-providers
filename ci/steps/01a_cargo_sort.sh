#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

# cargo-sort is a standalone binary, not a rustup component. Install on demand
# so fresh runners (and contributors) don't have to do it by hand.
if ! command -v cargo-sort >/dev/null 2>&1; then
  if command -v cargo-binstall >/dev/null 2>&1; then
    cargo binstall cargo-sort --no-confirm --locked
  else
    cargo install cargo-sort --locked
  fi
fi

cargo sort --workspace --check

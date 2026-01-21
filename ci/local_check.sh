#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

PACK_VERSION="${PACK_VERSION:-$(python3 - <<'PY'
from pathlib import Path
import tomllib
data = tomllib.loads(Path("Cargo.toml").read_text())
print(data.get("workspace", {}).get("package", {}).get("version", "0.0.0"))
PY
)}"
export PACK_VERSION

echo "==> cargo fmt --check"
cargo fmt --check

echo "==> tools/build_components.sh"
./tools/build_components.sh

echo "==> tools/sync_packs.sh (PACK_VERSION=${PACK_VERSION})"
./tools/sync_packs.sh

run_publish_packs="${RUN_PUBLISH_PACKS:-${CI:-0}}"
case "${run_publish_packs}" in
  1|true|TRUE|yes|YES) run_publish_packs=1 ;;
  *) run_publish_packs=0 ;;
esac

if [ "${run_publish_packs}" -eq 1 ]; then
  if ! command -v cargo-binstall >/dev/null 2>&1; then
    echo "==> Installing cargo-binstall"
    cargo install cargo-binstall --locked
  fi
  if ! command -v greentic-messaging-test >/dev/null 2>&1; then
    echo "==> Installing greentic-messaging-test"
    cargo binstall greentic-messaging-test --no-confirm --locked
  fi
  echo "==> tools/publish_packs_oci.sh (dry-run, PACK_VERSION=${PACK_VERSION})"
  DRY_RUN=1 ./tools/publish_packs_oci.sh
  if compgen -G "dist/packs/messaging-*.gtpack" >/dev/null; then
    echo "==> greentic-pack doctor --validate (dist/packs)"
    for p in dist/packs/messaging-*.gtpack; do
      greentic-pack doctor --validate --pack "$p"
    done
    echo "==> python3 tools/validate_pack_fixtures.py"
    python3 tools/validate_pack_fixtures.py
    echo "==> greentic-messaging-test packs conformance (dry-run)"
    for p in dist/packs/messaging-*.gtpack; do
      greentic-messaging-test packs conformance \
        --setup-only \
        --public-base-url "${PUBLIC_BASE_URL:-https://example.com}" \
        --pack-path "$p" \
        --env "${CONFORMANCE_ENV:-dev}" \
        --tenant "${CONFORMANCE_TENANT:-example}" \
        --team "${CONFORMANCE_TEAM:-default}"
    done
  fi
else
  echo "==> tools/publish_packs_oci.sh (dry-run; rebuild dist/packs)"
  DRY_RUN=1 PACKC_BUILD_FLAGS="--offline" ./tools/publish_packs_oci.sh
fi

echo "==> cargo test --workspace"
cargo test --workspace

echo "All checks completed."

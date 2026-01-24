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
export GREENTIC_RUNNER_SMOKE=1

echo "==> cargo fmt --check"
if ! cargo fmt --check; then
  if command -v rustup >/dev/null 2>&1; then
    toolchain="$(rustup show active-toolchain | awk '{print $1}')"
    rustup component add --toolchain "${toolchain}" rustfmt clippy
    cargo fmt --check
  else
    exit 1
  fi
fi

echo "==> tools/build_components.sh"
./tools/build_components.sh

echo "==> tools/sync_packs.sh (PACK_VERSION=${PACK_VERSION})"
./tools/sync_packs.sh

if ! command -v greentic-runner >/dev/null 2>&1; then
  echo "==> Installing greentic-runner"
  cargo binstall greentic-runner --no-confirm --locked
fi

echo "==> greentic-flow doctor --validate (packs/*/flows)"
if ! command -v greentic-flow >/dev/null 2>&1; then
  echo "greentic-flow is required for flow validation" >&2
  exit 1
fi
if compgen -G "packs/*/flows/*.ygtc" >/dev/null; then
  for f in packs/*/flows/*.ygtc; do
    greentic-flow doctor "$f"
  done
fi

echo "==> greentic-component doctor --validate (components manifests)"
if ! command -v greentic-component >/dev/null 2>&1; then
  echo "greentic-component is required for component validation" >&2
  exit 1
fi
if compgen -G "packs/*/components/*.manifest.json" >/dev/null; then
  for c in packs/*/components/*.manifest.json; do
    greentic-component doctor "$c"
  done
fi

echo "==> greentic-component test (questions emit/validate)"
if ! command -v greentic-component >/dev/null 2>&1; then
  echo "greentic-component is required for component tests" >&2
  exit 1
fi
tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT
cp components/questions/questions.wasm "${tmpdir}/questions.wasm"
cp components/questions/component.manifest.json "${tmpdir}/component.manifest.json"
cp packs/messaging-dummy/assets/setup.yaml "${tmpdir}/setup.yaml"
(
  cd "${tmpdir}"
  greentic-component test \
    --wasm questions.wasm \
    --manifest component.manifest.json \
    --op emit \
    --input-json '{"id":"dummy-setup","spec_ref":"assets/setup.yaml","context":{"tenant_id":"t1","env":"dev"}}' \
    --pretty
)
python3 - <<'PY' "${tmpdir}"
import json
import sys
from pathlib import Path

tmpdir = Path(sys.argv[1])
spec = {
    "provider_id": "dummy",
    "version": 1,
    "title": "Dummy provider setup",
    "questions": [],
    "id": "dummy-setup",
}
payload = {
    "id": "dummy-setup",
    "spec_json": json.dumps(spec),
    "answers_json": json.dumps({}),
}
(tmpdir / "validate_input.json").write_text(json.dumps(payload))
PY
(
  cd "${tmpdir}"
  greentic-component test \
    --wasm questions.wasm \
    --manifest component.manifest.json \
    --op validate \
    --input validate_input.json \
    --pretty
)
trap - EXIT
rm -rf "${tmpdir}"

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
  echo "==> tools/publish_packs_oci.sh (dry-run, PACK_VERSION=${PACK_VERSION})"
  DRY_RUN=1 ./tools/publish_packs_oci.sh
  if compgen -G "dist/packs/messaging-*.gtpack" >/dev/null; then
    echo "==> provider pack must pass greentic-pack doctor (messaging validator)"
    for p in dist/packs/messaging-*.gtpack; do
      greentic-pack doctor --validate --validator-pack "oci://ghcr.io/greentic-ai/validators/messaging:latest" --pack "$p"
    done
  fi
else
  echo "==> tools/publish_packs_oci.sh (dry-run; rebuild dist/packs)"
  PACKC_BUILD_FLAGS="${PACKC_BUILD_FLAGS:-}"
  DRY_RUN=1 PACKC_BUILD_FLAGS="${PACKC_BUILD_FLAGS}" ./tools/publish_packs_oci.sh
fi

echo "==> cargo test --workspace"
cargo test --workspace

echo "All checks completed."

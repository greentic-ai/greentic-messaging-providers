#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

run_fmt() {
  if [ -n "${COMPONENT_MANIFESTS_JSON:-}" ]; then
    python3 - <<'PY' "${COMPONENT_MANIFESTS_JSON}"
import json
import sys
for manifest in json.loads(sys.argv[1]):
    print(manifest)
PY
    return
  fi
  echo "__workspace__"
}

fmt_failed=0
while IFS= read -r manifest; do
  [ -z "${manifest}" ] && continue
  if [ "${manifest}" = "__workspace__" ]; then
    cargo fmt --check || fmt_failed=1
  else
    cargo fmt --check --manifest-path "${manifest}" || fmt_failed=1
  fi
done < <(run_fmt)

if [ "${fmt_failed}" -ne 0 ]; then
  if command -v rustup >/dev/null 2>&1; then
    toolchain="$(rustup show active-toolchain | awk '{print $1}')"
    rustup component add --toolchain "${toolchain}" rustfmt clippy
    fmt_failed=0
    while IFS= read -r manifest; do
      [ -z "${manifest}" ] && continue
      if [ "${manifest}" = "__workspace__" ]; then
        cargo fmt --check || fmt_failed=1
      else
        cargo fmt --check --manifest-path "${manifest}" || fmt_failed=1
      fi
    done < <(run_fmt)
    [ "${fmt_failed}" -eq 0 ] || exit 1
  else
    exit 1
  fi
fi

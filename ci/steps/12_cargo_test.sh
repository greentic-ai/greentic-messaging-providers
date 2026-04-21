#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

# Build/test scope is provided by the detect-changes job via env vars:
#   BUILD_ALL              "true" / "false"
#   VERSION_ONLY           "true" / "false"
#   AFFECTED_PROVIDERS     JSON array of provider names
# When BUILD_ALL=false, only provider-scoped tests run; otherwise the full
# workspace is tested. VERSION_ONLY=true short-circuits because source code
# did not change.

BUILD_ALL="${BUILD_ALL:-true}"
VERSION_ONLY="${VERSION_ONLY:-false}"
export AFFECTED_PROVIDERS="${AFFECTED_PROVIDERS:-[]}"

if [ "${VERSION_ONLY}" = "true" ]; then
  echo "cargo-test: skipped (version_only bump, no source changes)"
  exit 0
fi

if [ "${BUILD_ALL}" != "false" ]; then
  echo "cargo-test: running full workspace test"
  exec cargo test --workspace
fi

mapfile -t providers < <(python3 -c '
import json, os, sys
raw = os.environ.get("AFFECTED_PROVIDERS", "[]")
try:
    for name in json.loads(raw):
        print(name)
except Exception as exc:
    print(f"failed to parse AFFECTED_PROVIDERS: {exc}", file=sys.stderr)
    sys.exit(1)
')

if [ "${#providers[@]}" -eq 0 ]; then
  echo "cargo-test: no affected providers, falling back to workspace test"
  exec cargo test --workspace
fi

# Shared crates that are always tested when anything below them changed.
shared_args=(
  "-p" "messaging-core"
  "-p" "provider-common"
  "-p" "provider-runtime-config"
  "-p" "greentic-messaging-renderer"
  "-p" "greentic-messaging-packgen"
  "-p" "messaging-cardkit"
)

provider_args=()
integration_args=()
for provider in "${providers[@]}"; do
  # component crates named like `messaging-provider-<provider>` plus any extras
  for dir in "components/messaging-provider-${provider}" "components/${provider}" "components/messaging-ingress-${provider}" "components/${provider}-webhook"; do
    if [ -f "${dir}/Cargo.toml" ]; then
      pkg=$(CARGO_TOML_PATH="${dir}/Cargo.toml" python3 -c '
import os, tomllib, pathlib
data = tomllib.loads(pathlib.Path(os.environ["CARGO_TOML_PATH"]).read_text())
print(data.get("package", {}).get("name", ""))
')
      if [ -n "${pkg}" ]; then
        provider_args+=("-p" "${pkg}")
      fi
    fi
  done

  # per-provider integration test file under provider-tests
  test_file="crates/provider-tests/tests/provider_core_${provider}.rs"
  if [ -f "${test_file}" ]; then
    integration_args+=("--test" "provider_core_${provider}")
  fi
done

echo "cargo-test: scoped run"
echo "  shared crates: ${shared_args[*]}"
echo "  provider crates: ${provider_args[*]}"
echo "  integration tests: ${integration_args[*]}"

cargo test "${shared_args[@]}" "${provider_args[@]}"

if [ "${#integration_args[@]}" -gt 0 ]; then
  cargo test -p provider-tests "${integration_args[@]}"
fi

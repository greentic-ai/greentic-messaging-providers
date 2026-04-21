#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

# Env contract:
#   VERSION_ONLY    "true" => no source change, skip everything
#   TEST_SCOPE      "shared" | "provider" | "all" (default "all" = legacy workspace)
#   TEST_PROVIDER   required when TEST_SCOPE=provider (single provider name)
#
# This script is called from two matrix jobs:
#   cargo-test-shared: library crates + universal integration tests (one runner)
#   cargo-test-provider: per-provider component crates + provider_core_<name> (matrix)

VERSION_ONLY="${VERSION_ONLY:-false}"
TEST_SCOPE="${TEST_SCOPE:-all}"
TEST_PROVIDER="${TEST_PROVIDER:-}"

if [ "${VERSION_ONLY}" = "true" ]; then
  echo "cargo-test: skipped (version_only bump, no source changes)"
  exit 0
fi

run_shared_tests() {
  # Package names as declared in each crate's Cargo.toml (NOT directory names).
  # crates/component_questions has `name = "questions"`.
  local -a library_crates=(
    messaging-core
    provider-common
    provider-runtime-config
    greentic-messaging-cardkit
    greentic-messaging-packgen
    greentic-messaging-planned
    greentic-messaging-renderer
    greentic-messaging-tester
    messaging-cardkit
    questions
    questions-cli
    webchat-directline-core
  )
  local -a universal_tests=(
    universal_ops_conformance
    universal_ops_render_plan
    universal_ops_email
    packs_consistency
    registry_fixtures
    instantiation_providers
    provider_harness
    pack_doctor_loads_validator
    http_client_world_guard
    provider_ingress_components
  )

  # Collect package names from crates/*/Cargo.toml as a single whitespace
  # string (bash 3.2-compatible; no associative arrays).
  local workspace_pkgs
  workspace_pkgs=$(python3 - <<'PY'
import pathlib, tomllib
names = []
for cargo in sorted(pathlib.Path("crates").glob("*/Cargo.toml")):
    data = tomllib.loads(cargo.read_text())
    name = data.get("package", {}).get("name")
    if name:
        names.append(name)
print(" ".join(names))
PY
)

  local -a pkg_args=()
  local pkg_name
  for pkg_name in "${library_crates[@]}"; do
    if [[ " ${workspace_pkgs} " == *" ${pkg_name} "* ]]; then
      pkg_args+=("-p" "${pkg_name}")
    else
      echo "cargo-test: warning - crate ${pkg_name} not found in workspace" >&2
    fi
  done

  echo "cargo-test shared libraries: ${pkg_args[*]}"
  cargo test "${pkg_args[@]}"

  local -a test_args=()
  local t
  for t in "${universal_tests[@]}"; do
    if [ -f "crates/provider-tests/tests/${t}.rs" ]; then
      test_args+=("--test" "${t}")
    fi
  done

  if [ "${#test_args[@]}" -gt 0 ]; then
    echo "cargo-test universal integration: ${test_args[*]}"
    cargo test -p provider-tests "${test_args[@]}"
  fi
}

run_provider_tests() {
  local provider="$1"
  local -a pkg_args=()
  local dir pkg
  for dir in \
    "components/messaging-provider-${provider}" \
    "components/${provider}" \
    "components/messaging-ingress-${provider}" \
    "components/${provider}-webhook"
  do
    if [ -f "${dir}/Cargo.toml" ]; then
      pkg=$(CARGO_TOML_PATH="${dir}/Cargo.toml" python3 -c '
import os, tomllib, pathlib
data = tomllib.loads(pathlib.Path(os.environ["CARGO_TOML_PATH"]).read_text())
print(data.get("package", {}).get("name", ""))
')
      if [ -n "${pkg}" ]; then
        pkg_args+=("-p" "${pkg}")
      fi
    fi
  done

  if [ "${#pkg_args[@]}" -gt 0 ]; then
    echo "cargo-test provider=${provider} crates: ${pkg_args[*]}"
    cargo test "${pkg_args[@]}"
  else
    echo "cargo-test provider=${provider}: no component crates found"
  fi

  local -a test_args=()
  local t
  for t in "provider_core_${provider}" "provider_core_${provider}_interactive_mcp"; do
    if [ -f "crates/provider-tests/tests/${t}.rs" ]; then
      test_args+=("--test" "${t}")
    fi
  done

  if [ "${#test_args[@]}" -gt 0 ]; then
    echo "cargo-test provider=${provider} integration: ${test_args[*]}"
    cargo test -p provider-tests "${test_args[@]}"
  fi
}

case "${TEST_SCOPE}" in
  shared)
    run_shared_tests
    ;;
  provider)
    if [ -z "${TEST_PROVIDER}" ]; then
      echo "TEST_SCOPE=provider requires TEST_PROVIDER" >&2
      exit 1
    fi
    run_provider_tests "${TEST_PROVIDER}"
    ;;
  all | *)
    echo "cargo-test: running full workspace (TEST_SCOPE=${TEST_SCOPE})"
    exec cargo test --workspace
    ;;
esac

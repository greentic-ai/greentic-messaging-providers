#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${ROOT_DIR}/target/components"
BUILD_TARGET="wasm32-wasip2"
TARGET_DIR_OVERRIDE="${ROOT_DIR}/target/${BUILD_TARGET}"
PACKAGES=("provision" "questions" "secrets-probe" "slack" "teams" "telegram" "webchat" "webex" "whatsapp" "messaging-ingress-slack" "messaging-ingress-teams" "messaging-ingress-telegram" "messaging-ingress-whatsapp" "messaging-provider-dummy" "messaging-provider-telegram" "messaging-provider-teams" "messaging-provider-email" "messaging-provider-slack" "messaging-provider-webex" "messaging-provider-whatsapp" "messaging-provider-webchat")
WASM_TOOLS_BIN="${WASM_TOOLS_BIN:-wasm-tools}"
# Skip WASI preview 2 validation checks when requested.
SKIP_WASM_TOOLS_VALIDATION="${SKIP_WASM_TOOLS_VALIDATION:-0}"
HAS_WASM_TOOLS=0
# Keep tool caches inside the workspace to avoid sandbox write issues.
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-${ROOT_DIR}/.cache}"
mkdir -p "${XDG_CACHE_HOME}"

if ! rustup target list --installed | grep -q "${BUILD_TARGET}"; then
  echo "Installing Rust target ${BUILD_TARGET}..."
  rustup target add "${BUILD_TARGET}"
fi

if ! command -v cargo-component >/dev/null 2>&1; then
  echo "cargo-component not found; installing..."
  cargo install cargo-component --locked
fi

if command -v "${WASM_TOOLS_BIN}" >/dev/null 2>&1; then
  HAS_WASM_TOOLS=1
else
  echo "wasm-tools not found; installing via cargo-binstall (fallback to cargo install if needed)..."
  if command -v cargo-binstall >/dev/null 2>&1; then
    cargo binstall --no-confirm --locked wasm-tools || true
  fi
  if command -v "${WASM_TOOLS_BIN}" >/dev/null 2>&1; then
    HAS_WASM_TOOLS=1
  else
    echo "cargo-binstall not available or wasm-tools still missing; attempting cargo install..."
    cargo install wasm-tools --locked || true
    if command -v "${WASM_TOOLS_BIN}" >/dev/null 2>&1; then
      HAS_WASM_TOOLS=1
    else
      echo "wasm-tools still not found; skipping WASI preview 2 validation checks (install wasm-tools to enable)" >&2
    fi
  fi
fi

mkdir -p "${TARGET_DIR}"
mkdir -p "${TARGET_DIR_OVERRIDE}"

for PACKAGE_NAME in "${PACKAGES[@]}"; do
  ARTIFACT_NAME="${PACKAGE_NAME//-/_}.wasm"
  ARTIFACT_PATH="${TARGET_DIR_OVERRIDE}/release/${ARTIFACT_NAME}"
  NESTED_ARTIFACT_PATH="${TARGET_DIR_OVERRIDE}/${BUILD_TARGET}/release/${ARTIFACT_NAME}"

  cargo component build --release --package "${PACKAGE_NAME}" --target "${BUILD_TARGET}" --target-dir "${TARGET_DIR_OVERRIDE}"

  if [ ! -f "${ARTIFACT_PATH}" ] && [ -f "${NESTED_ARTIFACT_PATH}" ]; then
    ARTIFACT_PATH="${NESTED_ARTIFACT_PATH}"
  fi

  if [ ! -f "${ARTIFACT_PATH}" ]; then
    echo "Expected artifact not found: ${ARTIFACT_PATH}" >&2
    exit 1
  fi

  cp "${ARTIFACT_PATH}" "${TARGET_DIR}/${PACKAGE_NAME}.wasm"
  if [ "${PACKAGE_NAME}" = "provision" ]; then
    cp "${ARTIFACT_PATH}" "${ROOT_DIR}/components/provision/provision.wasm"
  fi
  if [ "${PACKAGE_NAME}" = "questions" ]; then
    cp "${ARTIFACT_PATH}" "${ROOT_DIR}/components/questions/questions.wasm"
  fi
  if [ "${HAS_WASM_TOOLS}" -eq 1 ] && [ "${SKIP_WASM_TOOLS_VALIDATION}" -eq 0 ]; then
    if ! "${WASM_TOOLS_BIN}" component wit "${TARGET_DIR}/${PACKAGE_NAME}.wasm" | grep -q "wasi:cli/"; then
      echo "Artifact ${PACKAGE_NAME} does not appear to target WASI preview 2 (missing wasi:cli import)" >&2
      exit 1
    fi
    "${WASM_TOOLS_BIN}" validate "${TARGET_DIR}/${PACKAGE_NAME}.wasm" >/dev/null
  fi
  echo "Built ${TARGET_DIR}/${PACKAGE_NAME}.wasm"
done

# Clean nested target triples produced by cargo-component to keep output tidy.
rm -rf "${TARGET_DIR_OVERRIDE}/wasm32-wasip2" || true

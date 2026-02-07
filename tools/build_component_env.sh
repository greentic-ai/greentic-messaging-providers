#!/usr/bin/env bash
set -euo pipefail

if [ "${COMPONENT_BUILD_ENV_READY:-0}" = "1" ]; then
  return 0
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${ROOT_DIR}/target/components"
BUILD_TARGET="wasm32-wasip2"
TARGET_DIR_OVERRIDE="${ROOT_DIR}/target/${BUILD_TARGET}"
WASM_TOOLS_BIN="${WASM_TOOLS_BIN:-wasm-tools}"
SKIP_WASM_TOOLS_VALIDATION="${SKIP_WASM_TOOLS_VALIDATION:-0}"
HAS_WASM_TOOLS=0

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
mkdir -p "${TARGET_DIR_OVERRIDE}/wasm32-wasip1/release/deps"
mkdir -p "${TARGET_DIR_OVERRIDE}/wasm32-wasip1/debug/deps"
mkdir -p "${TARGET_DIR_OVERRIDE}/wasm32-wasip2/release/deps"
mkdir -p "${TARGET_DIR_OVERRIDE}/wasm32-wasip2/debug/deps"

export ROOT_DIR TARGET_DIR BUILD_TARGET TARGET_DIR_OVERRIDE WASM_TOOLS_BIN SKIP_WASM_TOOLS_VALIDATION HAS_WASM_TOOLS
export COMPONENT_BUILD_ENV_READY=1

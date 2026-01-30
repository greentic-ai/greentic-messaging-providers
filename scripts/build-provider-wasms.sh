#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_TARGET="${BUILD_TARGET:-wasm32-wasip2}"
TARGET_DIR="${ROOT_DIR}/target/provider-wasms"
DIST_DIR="${ROOT_DIR}/dist/wasms"
PACKAGES=(
  "messaging-provider-dummy"
  "messaging-provider-telegram"
  "messaging-provider-teams"
  "messaging-provider-email"
  "messaging-provider-slack"
  "messaging-provider-webex"
  "messaging-provider-whatsapp"
  "messaging-provider-webchat"
)

# Ensure toolchains/tools are available.
if ! rustup target list --installed | grep -q "${BUILD_TARGET}"; then
  echo "Installing Rust target ${BUILD_TARGET}..."
  rustup target add "${BUILD_TARGET}"
fi

if ! command -v cargo-component >/dev/null 2>&1; then
  echo "cargo-component not found; installing..."
  cargo install cargo-component --locked
fi

mkdir -p "${TARGET_DIR}"
mkdir -p "${DIST_DIR}"

for PACKAGE_NAME in "${PACKAGES[@]}"; do
  ARTIFACT_NAME="${PACKAGE_NAME//-/_}.wasm"
  ARTIFACT_PATH="${TARGET_DIR}/release/${ARTIFACT_NAME}"
  NESTED_ARTIFACT_PATH="${TARGET_DIR}/${BUILD_TARGET}/release/${ARTIFACT_NAME}"

  cargo component build \
    --release \
    --package "${PACKAGE_NAME}" \
    --target "${BUILD_TARGET}" \
    --target-dir "${TARGET_DIR}"

  if [ ! -f "${ARTIFACT_PATH}" ] && [ -f "${NESTED_ARTIFACT_PATH}" ]; then
    ARTIFACT_PATH="${NESTED_ARTIFACT_PATH}"
  fi

  if [ ! -f "${ARTIFACT_PATH}" ]; then
    echo "Expected artifact not found: ${ARTIFACT_PATH}" >&2
    exit 1
  fi

  cp "${ARTIFACT_PATH}" "${DIST_DIR}/${PACKAGE_NAME}.wasm"
  echo "Published ${DIST_DIR}/${PACKAGE_NAME}.wasm"
done

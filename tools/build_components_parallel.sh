#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JOBS="${BUILD_COMPONENTS_JOBS:-}"
if [ -z "${JOBS}" ]; then
  if command -v nproc >/dev/null 2>&1; then
    JOBS="$(nproc)"
  else
    JOBS="2"
  fi
fi

PACKAGES=(provision questions secrets-probe slack teams telegram webchat webex webex-webhook whatsapp messaging-ingress-slack messaging-ingress-teams messaging-ingress-telegram messaging-ingress-whatsapp messaging-provider-dummy messaging-provider-telegram messaging-provider-teams messaging-provider-email messaging-provider-slack messaging-provider-webex messaging-provider-whatsapp messaging-provider-webchat)

if xargs -P 1 -n 1 echo >/dev/null 2>&1; then
  printf '%s\n' "${PACKAGES[@]}" | xargs -n 1 -P "${JOBS}" bash -c 'bash "$1/tools/build_components/$2.sh"' _ "${ROOT_DIR}"
else
  echo "xargs -P not available; running builds sequentially" >&2
  for package in "${PACKAGES[@]}"; do
    bash "${ROOT_DIR}/tools/build_components/${package}.sh"
  done
fi

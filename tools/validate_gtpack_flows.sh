#!/usr/bin/env bash
set -euo pipefail

PACKS_DIR="${PACKS_DIR:-dist/packs}"
PACK_GLOB="${PACK_GLOB:-messaging-*.gtpack}"
PUBLIC_BASE_URL="${PUBLIC_BASE_URL:-https://example.com}"
CONFORMANCE_ENV="${CONFORMANCE_ENV:-dev}"
CONFORMANCE_TENANT="${CONFORMANCE_TENANT:-example}"
CONFORMANCE_TEAM="${CONFORMANCE_TEAM:-default}"

if ! command -v greentic-messaging-test >/dev/null 2>&1; then
  echo "greentic-messaging-test is required for gtpack flow validation" >&2
  exit 1
fi

if ! compgen -G "${PACKS_DIR}/${PACK_GLOB}" >/dev/null; then
  echo "No gtpack files found at ${PACKS_DIR}/${PACK_GLOB}" >&2
  exit 1
fi

for p in "${PACKS_DIR}"/${PACK_GLOB}; do
  greentic-messaging-test packs conformance \
    --setup-only \
    --public-base-url "${PUBLIC_BASE_URL}" \
    --pack-path "$p" \
    --env "${CONFORMANCE_ENV}" \
    --tenant "${CONFORMANCE_TENANT}" \
    --team "${CONFORMANCE_TEAM}"
done

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACK_VERSION="${PACK_VERSION:-}"

if [ -z "${PACK_VERSION}" ]; then
  PACK_VERSION="$(python3 - <<'PY'
from pathlib import Path
import tomllib

data = tomllib.loads(Path("Cargo.toml").read_text())
print(data.get("workspace", {}).get("package", {}).get("version", "0.0.0-dev"))
PY
)"
fi

cd "${ROOT_DIR}"

./tools/build_components.sh
./tools/sync_packs.sh

DRY_RUN=1 PACK_VERSION="${PACK_VERSION}" ./tools/publish_packs_oci.sh

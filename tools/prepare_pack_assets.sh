#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKS_DIR="${ROOT_DIR}/packs"

if [ -x "${ROOT_DIR}/tools/import_webchat_gui_assets.sh" ]; then
  "${ROOT_DIR}/tools/import_webchat_gui_assets.sh"
fi

for dir in "${PACKS_DIR}"/messaging-*; do
  [ -d "${dir}" ] || continue
  secrets_out="${dir}/.secret_requirements.json"
  python3 "${ROOT_DIR}/tools/generate_pack_metadata.py" \
    --pack-dir "${dir}" \
    --components-dir "${ROOT_DIR}/components" \
    --secrets-out "${secrets_out}" >/dev/null

  dest_root="${dir}/secret-requirements.json"
  rm -f "${dir}/assets/secret-requirements.json"
  if [ -f "${secrets_out}" ]; then
    cp "${secrets_out}" "${dest_root}"
  else
    printf '%s\n' "[]" > "${dest_root}"
  fi

  pack_yaml="${dir}/pack.yaml"
  if [ -f "${pack_yaml}" ]; then
    python3 - "${pack_yaml}" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
lines = path.read_text().splitlines()
asset_line = "- path: secret-requirements.json"
if any(line.strip() == asset_line for line in lines):
    raise SystemExit(0)

insert_at = None
for idx, line in enumerate(lines):
    if line.startswith("assets:"):
        insert_at = idx + 1
        if line.strip() == "assets: []":
            lines[idx] = "assets:"
        break

if insert_at is None:
    if lines and lines[-1].strip():
        lines.append("")
    lines.extend(["assets:", asset_line])
else:
    lines.insert(insert_at, asset_line)

path.write_text("\n".join(lines) + "\n")
PY
  fi
done

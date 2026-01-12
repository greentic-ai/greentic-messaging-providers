#!/usr/bin/env bash
set -euo pipefail

# Regenerates pack manifests, syncs schemas, bumps versions, and stages WASM artifacts from target/components.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKS_DIR="${PACKS_DIR:-${ROOT_DIR}/packs}"
TARGET_COMPONENTS="${ROOT_DIR}/target/components"
VERSION="${PACK_VERSION:-}"

if [ -z "${VERSION}" ]; then
  VERSION="$(python3 - <<'PY'
from pathlib import Path
import tomllib
data = tomllib.loads(Path("Cargo.toml").read_text())
print(data.get("workspace", {}).get("package", {}).get("version", "0.0.0"))
PY
)"
fi

echo "Using version: ${VERSION}"

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 is required" >&2; exit 1; }
command -v oras >/dev/null 2>&1 || { echo "oras is required for fetching OCI components" >&2; exit 1; }

if [ ! -d "${TARGET_COMPONENTS}" ]; then
  echo "Building components..."
  "${ROOT_DIR}/tools/build_components.sh"
fi

update_pack_yaml_version() {
  local yaml_path="$1"
  [ -f "${yaml_path}" ] || return 0
  python3 - "$yaml_path" "$VERSION" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
version = sys.argv[2]
lines = path.read_text().splitlines()
updated = False
out = []
for line in lines:
    stripped = line.lstrip()
    indent = len(line) - len(stripped)
    if indent == 0 and stripped.startswith("version:"):
        prefix = line.split("version:")[0] + "version: "
        out.append(f"{prefix}{version}")
        updated = True
    else:
        out.append(line)
if not updated:
    out.append(f"version: {version}")
path.write_text("\n".join(out) + "\n")
PY
}

copy_schema() {
  local pack_dir="$1"
  local schema_path="$2"
  local src="${ROOT_DIR}/${schema_path}"
  local dest="${pack_dir}/${schema_path}"
  if [ -f "${src}" ]; then
    mkdir -p "$(dirname "${dest}")"
    cp "${src}" "${dest}"
  else
    echo "Warning: schema not found at ${src}" >&2
  fi
}

fetch_oci_component() {
  local image="$1"
  local digest="$2"
  local artifact="$3"
  local dest_wasm="$4"
  local manifest_name="$5"
  local dest_manifest="$6"

  local ref="${image}"
  if [ -n "${digest}" ]; then
    ref="${image}@${digest}"
  fi

  local tmpdir
  tmpdir="$(mktemp -d)"
  echo "Fetching OCI component ${ref}..."
  oras pull --output "${tmpdir}" "${ref}"
  local src_path="${tmpdir}/${artifact}"
  if [ ! -f "${src_path}" ]; then
    echo "OCI component artifact ${artifact} not found in ${tmpdir}" >&2
    rm -rf "${tmpdir}"
    exit 1
  fi
  mkdir -p "$(dirname "${dest_wasm}")"
  cp "${src_path}" "${dest_wasm}"

  if [ -n "${manifest_name:-}" ] && [ -n "${dest_manifest:-}" ]; then
    local manifest_src="${tmpdir}/${manifest_name}"
    if [ -f "${manifest_src}" ]; then
      mkdir -p "$(dirname "${dest_manifest}")"
      cp "${manifest_src}" "${dest_manifest}"
    fi
  fi
  rm -rf "${tmpdir}"
}

for dir in "${PACKS_DIR}"/*; do
  [ -d "${dir}" ] || continue
  if [ ! -f "${dir}/pack.manifest.json" ]; then
    echo "Skipping ${dir}: no pack.manifest.json"
    continue
  fi

  echo "Syncing $(basename "${dir}")..."
  update_pack_yaml_version "${dir}/pack.yaml"
  python3 "${ROOT_DIR}/tools/generate_pack_metadata.py" \
    --pack-dir "${dir}" \
    --components-dir "${ROOT_DIR}/components" \
    --version "${VERSION}" \
    --include-capabilities-cache

  mkdir -p "${dir}/components"
  while IFS=$'\t' read -r comp wasm_path oci_image oci_digest oci_artifact manifest_rel oci_manifest; do
    [ -z "${comp}" ] && continue
    wasm_rel="${wasm_path:-components/${comp}.wasm}"
    wasm_file="$(basename "${wasm_rel}")"
    src="${TARGET_COMPONENTS}/${wasm_file}"
    dest="${dir}/${wasm_rel}"
    manifest_src=""
    manifest_dest=""
    if [ -n "${manifest_rel}" ]; then
      manifest_dest="${dir}/${manifest_rel}"
      manifest_src="${TARGET_COMPONENTS}/$(basename "${manifest_rel}")"
    fi
    mkdir -p "$(dirname "${dest}")"
    if [ ! -f "${src}" ] || { [ -n "${manifest_rel}" ] && [ ! -f "${manifest_src}" ]; }; then
      # Prefer OCI fetch when metadata is available.
      if [ -n "${oci_image}" ] && [ -n "${oci_artifact}" ]; then
        fetch_oci_component "${oci_image}" "${oci_digest}" "${oci_artifact}" "${src}" "${oci_manifest}" "${manifest_src}"
      # Fallback: reuse component artifacts already bundled under the pack directory.
      elif [ -f "${dir}/${wasm_rel}" ]; then
        mkdir -p "$(dirname "${src}")"
        cp "${dir}/${wasm_rel}" "${src}"
        if [ -n "${manifest_rel}" ] && [ -f "${dir}/${manifest_rel}" ]; then
          mkdir -p "$(dirname "${manifest_src}")"
          cp "${dir}/${manifest_rel}" "${manifest_src}"
        fi
      else
        echo "Missing component artifact: ${src}" >&2
        exit 1
      fi
    fi
    cp "${src}" "${dest}"
    if [ -n "${manifest_rel}" ] && [ -f "${manifest_src}" ]; then
      mkdir -p "$(dirname "${manifest_dest}")"
      cp "${manifest_src}" "${manifest_dest}"
    fi
  done < <(jq -r '(.component_sources // .components // [])[] | if type=="string" then {id: ., wasm: ("components/" + . + ".wasm")} else {id: .id, wasm: (.wasm // ("components/" + .id + ".wasm")), manifest: (.manifest // ""), oci: (.oci // {})} end | [.id, .wasm, (.oci.image // ""), (.oci.digest // ""), (.oci.artifact // ""), (.manifest // ""), (.oci.manifest // "")] | @tsv' "${dir}/pack.manifest.json")

  while IFS= read -r schema; do
    [ -z "${schema}" ] && continue
    copy_schema "${dir}" "${schema}"
  done < <(jq -r '
    [
      (.extensions["greentic.provider-extension.v1"].inline.providers[]?.config_schema_ref // empty),
      (.config_schema.provider_config.path // empty)
    ] | flatten | .[]? ' "${dir}/pack.manifest.json")
done

echo "Pack sync complete."

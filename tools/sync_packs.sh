#!/usr/bin/env bash
set -euo pipefail

# Regenerates pack manifests, syncs schemas, bumps versions, and stages WASM artifacts from target/components.

die() {
  echo "ERROR: $*" >&2
  exit 1
}

trap 'die "sync_packs failed."' ERR

if [ -z "${BASH_VERSION:-}" ]; then
  die "This script requires bash. Run: bash tools/sync_packs.sh"
fi


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

if [ -x "${ROOT_DIR}/tools/prepare_pack_assets.sh" ]; then
  "${ROOT_DIR}/tools/prepare_pack_assets.sh"
fi

# Default OCI location for the shared templates component used by many packs.
DEFAULT_TEMPLATES_IMAGE="ghcr.io/greentic-ai/components/templates"
DEFAULT_TEMPLATES_DIGEST="sha256:0904bee6ecd737506265e3f38f3e4fe6b185c20fd1b0e7c06ce03cdeedc00340"
DEFAULT_TEMPLATES_ARTIFACT="component_templates.wasm"
DEFAULT_TEMPLATES_MANIFEST="component.publish.manifest.json"

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
        out.append(line.replace("__PACK_VERSION__", version))
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

ensure_secret_requirements_asset() {
  local pack_dir="$1"
  local secrets_out="$2"
  local dest_assets="${pack_dir}/assets/secret-requirements.json"
  local dest_root="${pack_dir}/secret-requirements.json"
  mkdir -p "$(dirname "${dest_assets}")"
  if [ -f "${secrets_out}" ]; then
    cp "${secrets_out}" "${dest_assets}"
    cp "${secrets_out}" "${dest_root}"
  else
    printf '%s\n' "[]" > "${dest_assets}"
    printf '%s\n' "[]" > "${dest_root}"
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
  oras_pull "${ref}" "${tmpdir}"
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

OCI_CACHE_KEYS=()
OCI_CACHE_DIRS=()
OCI_CACHE_TMPDIRS=()

oci_cache_get() {
  local key="$1"
  local idx=0
  for existing in "${OCI_CACHE_KEYS[@]:-}"; do
    if [ "${existing}" = "${key}" ]; then
      echo "${OCI_CACHE_DIRS[$idx]}"
      return 0
    fi
    idx=$((idx + 1))
  done
  return 1
}

oci_cache_set() {
  local key="$1"
  local value="$2"
  OCI_CACHE_KEYS+=("${key}")
  OCI_CACHE_DIRS+=("${value}")
}

cleanup_oci_cache() {
  for dir in "${OCI_CACHE_TMPDIRS[@]:-}"; do
    rm -rf "${dir}"
  done
}

trap cleanup_oci_cache EXIT

fetch_locked_component() {
  local ref="$1"
  local digest="$2"
  local dest_wasm="$3"

  if [[ "${ref}" == file://* ]]; then
    local src_path="${ref#file://}"
    if [ ! -f "${src_path}" ]; then
      echo "Local component file not found for ${ref}" >&2
      exit 1
    fi
    mkdir -p "$(dirname "${dest_wasm}")"
    cp "${src_path}" "${dest_wasm}"
    return
  fi

  local ref_clean="${ref#oci://}"
  local cache_key="${digest:-${ref_clean}}"
  local tmpdir=""
  tmpdir="$(oci_cache_get "${cache_key}")" || tmpdir=""

  if [ -z "${tmpdir}" ]; then
    tmpdir="$(mktemp -d)"
    oci_cache_set "${cache_key}" "${tmpdir}"
    OCI_CACHE_TMPDIRS+=("${tmpdir}")
    echo "Fetching OCI component ${ref_clean}..."
    oras_pull "${ref_clean}" "${tmpdir}"
  fi

  local manifest="${tmpdir}/component.publish.manifest.json"
  local artifact=""
  if [ -f "${manifest}" ]; then
    artifact="$(jq -r '.artifacts.component_wasm // empty' "${manifest}")"
  fi
  if [ -z "${artifact}" ]; then
    artifact="$(ls "${tmpdir}"/*.wasm 2>/dev/null | head -n 1)"
    artifact="${artifact##*/}"
  fi
  if [ -z "${artifact}" ] || [ ! -f "${tmpdir}/${artifact}" ]; then
    echo "OCI component artifact not found for ${ref_clean}" >&2
    exit 1
  fi
  mkdir -p "$(dirname "${dest_wasm}")"
  cp "${tmpdir}/${artifact}" "${dest_wasm}"
}

oras_pull() {
  local ref="$1"
  local out_dir="$2"

  if [ -n "${GHCR_TOKEN:-}" ]; then
    local ghcr_user="${GHCR_USERNAME:-${GHCR_USER:-${USER:-}}}"
    if [ -z "${ghcr_user}" ]; then
      die "GHCR_TOKEN is set but no username found. Set GHCR_USERNAME."
    fi
    printf '%s' "${GHCR_TOKEN}" | oras pull --output "${out_dir}" --username "${ghcr_user}" --password-stdin "${ref}"
  else
    oras pull --output "${out_dir}" "${ref}"
  fi
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
    --secrets-out "${dir}/.secret_requirements.json" \
    --include-capabilities-cache
  ensure_secret_requirements_asset "${dir}" "${dir}/.secret_requirements.json"

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
    # Fill in default OCI metadata for template components when missing.
    if [ -z "${oci_image}" ] && { [ "${comp}" = "templates" ] || [ "${comp}" = "ai.greentic.component-templates" ]; }; then
      oci_image="${DEFAULT_TEMPLATES_IMAGE}"
      oci_digest="${DEFAULT_TEMPLATES_DIGEST}"
      oci_artifact="${DEFAULT_TEMPLATES_ARTIFACT}"
      oci_manifest="${DEFAULT_TEMPLATES_MANIFEST}"
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

  lock_file="${dir}/pack.lock.json"
  if [ -f "${lock_file}" ]; then
    while IFS=$'\t' read -r name ref digest; do
      [ -z "${name}" ] && continue
      wasm_rel="components/${name}.wasm"
      dest="${dir}/${wasm_rel}"
      if [ ! -f "${dest}" ]; then
        if [[ "${ref}" == *"components/questions"* ]] && [ -f "${ROOT_DIR}/components/questions/questions.wasm" ]; then
          cp "${ROOT_DIR}/components/questions/questions.wasm" "${dest}"
        else
          fetch_locked_component "${ref}" "${digest}" "${dest}"
        fi
      fi
    done < <(jq -r '.components[]? | [.name, .ref, .digest] | @tsv' "${lock_file}")
  fi
done

echo "Pack sync complete."

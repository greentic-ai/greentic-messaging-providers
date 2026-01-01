#!/usr/bin/env bash
set -euo pipefail

# Publishes *.gtpack archives to an OCI registry (GHCR by default) and writes packs.lock.json.
# DRY_RUN=1 builds packs and writes the lockfile but does not push.

OCI_REGISTRY="${OCI_REGISTRY:-ghcr.io}"
OCI_ORG="${OCI_ORG:-${GITHUB_REPOSITORY_OWNER:-greentic}}"
OCI_REPO="${OCI_REPO:-greentic-packs}"
PACK_VERSION="${PACK_VERSION:-${GITHUB_REF_NAME:-0.0.0}}"
PACK_VERSION="${PACK_VERSION#v}"
PACKS_DIR="${PACKS_DIR:-packs}"
OUT_DIR="${OUT_DIR:-dist/packs}"
DRY_RUN="${DRY_RUN:-0}"
PACKC_BIN="${PACKC_BIN:-packc}"
PACKC_BUILD_FLAGS="${PACKC_BUILD_FLAGS:-}"
MEDIA_TYPE="${MEDIA_TYPE:-application/vnd.greentic.gtpack.v1+zip}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "${ROOT_DIR}/${OUT_DIR}"

timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
git_sha="$(cd "${ROOT_DIR}" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")"

command -v jq >/dev/null 2>&1 || { echo "jq is required"; exit 1; }
command -v zip >/dev/null 2>&1 || { echo "zip is required"; exit 1; }
if [ "${DRY_RUN}" -eq 0 ]; then
  command -v oras >/dev/null 2>&1 || { echo "oras is required"; exit 1; }
fi
command -v python3 >/dev/null 2>&1 || { echo "python3 is required"; exit 1; }
if ! command -v "${PACKC_BIN}" >/dev/null 2>&1; then
  echo "packc (from greentic-pack) is required for building gtpack artifacts" >&2
  exit 1
fi
packc_version="$("${PACKC_BIN}" --version 2>/dev/null || true)"
required_packc="0.4.28"
if [ -z "${packc_version}" ]; then
  echo "packc is required (expected >= ${required_packc})" >&2
  exit 1
fi
echo "Using ${PACKC_BIN}: ${packc_version}" >&2
python3 - "${packc_version}" "${required_packc}" <<'PY'
import sys
def parse(ver: str):
    for token in ver.split():
        if token[0].isdigit():
            parts = token.split(".")
            return tuple(int(p) for p in parts[:3])
    return (0, 0, 0)
current = parse(sys.argv[1])
required = parse(sys.argv[2])
if current < required:
    sys.stderr.write(f"packc {sys.argv[2]} or newer is required; found {sys.argv[1]}\n")
    sys.exit(1)
PY

if [ ! -d "${ROOT_DIR}/${PACKS_DIR}" ]; then
  echo "Packs directory ${PACKS_DIR} not found" >&2
  exit 1
fi

packs_json="[]"

generate_pack_manifest() {
  local pack_dir="$1"
  local secrets_out="$2"
  python3 "${ROOT_DIR}/tools/generate_pack_metadata.py" \
    --pack-dir "${pack_dir}" \
    --components-dir "${ROOT_DIR}/components" \
    --version "${PACK_VERSION}" \
    --secrets-out "${secrets_out}"
}

update_pack_yaml_version() {
  local pack_dir="$1"
  local yaml_path="${pack_dir}/pack.yaml"
  [ -f "${yaml_path}" ] || return 0
  python3 - "$yaml_path" "$PACK_VERSION" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
version = sys.argv[2]
lines = path.read_text().splitlines()
updated = False
out = []
for line in lines:
    if line.strip().startswith("version:"):
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

ensure_components_artifacts() {
  local -a wasm_files=("$@")
  local missing=0
  for wasm in "${wasm_files[@]}"; do
    local fname
    fname="$(basename "${wasm}")"
    if [ ! -f "${ROOT_DIR}/target/components/${fname}" ]; then
      missing=1
      break
    fi
  done
  if [ "${missing}" -eq 1 ]; then
    echo "Building components because wasm artifacts are missing..."
    "${ROOT_DIR}/tools/build_components.sh"
  fi
}

stage_components_into_pack() {
  local pack_dir="$1"
  shift
  local -a wasm_files=("$@")
  mkdir -p "${pack_dir}/components"
  for wasm in "${wasm_files[@]}"; do
    local fname
    fname="$(basename "${wasm}")"
    local src="${ROOT_DIR}/target/components/${fname}"
    local dest="${pack_dir}/${wasm}"
    if [ ! -f "${src}" ]; then
      echo "Missing component artifact: ${src}" >&2
      exit 1
    fi
    mkdir -p "$(dirname "${dest}")"
    cp "${src}" "${dest}"
  done
}

for dir in "${ROOT_DIR}/${PACKS_DIR}/"*; do
  [ -d "${dir}" ] || continue
  pack_name="$(basename "${dir}")"
  pack_out_rel="${OUT_DIR}/${pack_name}.gtpack"
  pack_out="${ROOT_DIR}/${pack_out_rel}"
  secrets_out="${dir}/.secret_requirements.json"

  generate_pack_manifest "${dir}" "${secrets_out}"
  update_pack_yaml_version "${dir}"

  IFS=$'\n' read -r -d '' -a wasm_paths < <(jq -r '.components[] | if type=="object" then (.wasm // ("components/"+((.id // "")+".wasm"))) else ("components/"+(. + ".wasm")) end' "${dir}/pack.manifest.json" && printf '\0')
  ensure_components_artifacts "${wasm_paths[@]}"
  stage_components_into_pack "${dir}" "${wasm_paths[@]}"

  if [ ! -f "${dir}/pack.yaml" ]; then
    echo "Missing pack.yaml in ${dir}; packc requires pack.yaml inputs" >&2
    exit 1
  fi

  local_out_dir="${dir}/build"
  mkdir -p "${local_out_dir}"
  declare -a packc_flags=()
  if [ -n "${PACKC_BUILD_FLAGS:-}" ]; then
    IFS=' ' read -r -a packc_flags <<< "${PACKC_BUILD_FLAGS}"
  fi
  if [ "${#packc_flags[@]}" -gt 0 ]; then
    (cd "${dir}" && "${PACKC_BIN}" build "${packc_flags[@]}" \
      --in "." \
      --gtpack-out "build/${pack_name}.gtpack" \
      --secrets-req ".secret_requirements.json")
  else
    (cd "${dir}" && "${PACKC_BIN}" build \
      --in "." \
      --gtpack-out "build/${pack_name}.gtpack" \
      --secrets-req ".secret_requirements.json")
  fi
  mv "${local_out_dir}/${pack_name}.gtpack" "${pack_out}"

  pack_version="$("${PACKC_BIN}" inspect --json --pack "${pack_out}" | jq -r '.meta.packVersion // ""')"
  if [ "${pack_version}" = "1" ] || [ -z "${pack_version}" ]; then
    echo "warning: packc produced pack-v1 manifest for ${pack_name}; proceed anyway (upgrade packc for newer schema) " >&2
  fi

  oci_ref="${OCI_REGISTRY}/${OCI_ORG}/${OCI_REPO}/${pack_name}:${PACK_VERSION}"
  digest="DRY_RUN"

  if [ "${DRY_RUN}" -eq 0 ]; then
    digest="$(
      oras push \
        --artifact-type "${MEDIA_TYPE}" \
        --disable-path-validation \
        --annotation "org.opencontainers.image.source=${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-unknown}" \
        --annotation "org.opencontainers.image.revision=${git_sha}" \
        --annotation "org.opencontainers.image.version=${PACK_VERSION}" \
        "${oci_ref}" \
        "${pack_out}:${MEDIA_TYPE}" \
        | awk '/Digest:/{print $2}' | tail -n1
    )"
  else
    echo "[DRY RUN] Would push ${pack_out} to ${oci_ref}"
  fi

  pack_entry=$(jq -n \
    --arg name "${pack_name}" \
    --arg file "${OUT_DIR}/${pack_name}.gtpack" \
    --arg ref "${oci_ref}" \
    --arg digest "${digest}" \
    --arg timestamp "${timestamp}" \
    '{
      name: $name,
      file: $file,
      ref: $ref,
      digest: $digest,
      built_at: $timestamp
    }')

  packs_json=$(echo "${packs_json}" | jq --argjson entry "${pack_entry}" '. + [$entry]')
done

echo "${packs_json}" | jq '{ packs: . }' > "${ROOT_DIR}/packs.lock.json"
echo "Wrote packs.lock.json"

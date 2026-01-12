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
PACKC_BIN="${PACKC_BIN:-greentic-pack}"
PACKC_BUILD_FLAGS="${PACKC_BUILD_FLAGS:-}"
MEDIA_TYPE="${MEDIA_TYPE:-application/vnd.greentic.gtpack.v1+zip}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "${ROOT_DIR}/${OUT_DIR}"

timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
git_sha="$(cd "${ROOT_DIR}" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")"

command -v jq >/dev/null 2>&1 || { echo "jq is required"; exit 1; }
command -v zip >/dev/null 2>&1 || { echo "zip is required"; exit 1; }
command -v oras >/dev/null 2>&1 || { echo "oras is required"; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 is required"; exit 1; }
if ! command -v "${PACKC_BIN}" >/dev/null 2>&1; then
  echo "greentic-pack is required for building gtpack artifacts" >&2
  exit 1
fi
packc_version="$("${PACKC_BIN}" --version 2>/dev/null || true)"
required_packc="0.4.28"
if [ -z "${packc_version}" ]; then
  echo "greentic-pack is required (expected >= ${required_packc})" >&2
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
    sys.stderr.write(f"greentic-pack {sys.argv[2]} or newer is required; found {sys.argv[1]}\n")
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

read_components() {
  local manifest="$1"
  jq -c '(.component_sources // .components // [])[] | if type=="string" then {id: ., wasm: ("components/" + . + ".wasm"), manifest: "", oci: {}} else {id: .id, wasm: (.wasm // ("components/" + .id + ".wasm")), manifest: (.manifest // ""), oci: (.oci // {})} end' "${manifest}"
}

for dir in "${ROOT_DIR}/${PACKS_DIR}/"*; do
  [ -d "${dir}" ] || continue
  pack_name="$(basename "${dir}")"
  pack_out_rel="${OUT_DIR}/${pack_name}.gtpack"
  pack_out="${ROOT_DIR}/${pack_out_rel}"
  secrets_out="${dir}/.secret_requirements.json"

  generate_pack_manifest "${dir}" "${secrets_out}"
  update_pack_yaml_version "${dir}"

  components=()
  while IFS= read -r comp_line; do
    [ -z "${comp_line}" ] && continue
    components+=("${comp_line}")
  done < <(read_components "${dir}/pack.manifest.json")

  missing_local=0
  for comp_json in "${components[@]}"; do
    oci_image="$(jq -r '.oci.image // empty' <<<"${comp_json}")"
    wasm_path="$(jq -r '.wasm' <<<"${comp_json}")"
    fname="$(basename "${wasm_path}")"
    if [ -z "${oci_image}" ] && [ ! -f "${ROOT_DIR}/target/components/${fname}" ]; then
      missing_local=1
      break
    fi
  done
  if [ "${missing_local}" -eq 1 ]; then
    echo "Building components because wasm artifacts are missing..."
    "${ROOT_DIR}/tools/build_components.sh"
  fi

  mkdir -p "${dir}/components"
  for comp_json in "${components[@]}"; do
    comp_id="$(jq -r '.id' <<<"${comp_json}")"
    wasm_path="$(jq -r '.wasm' <<<"${comp_json}")"
    fname="$(basename "${wasm_path}")"
    oci_image="$(jq -r '.oci.image // empty' <<<"${comp_json}")"
    oci_digest="$(jq -r '.oci.digest // empty' <<<"${comp_json}")"
    oci_artifact="$(jq -r '.oci.artifact // empty' <<<"${comp_json}")"
    manifest_rel="$(jq -r '.manifest // empty' <<<"${comp_json}")"
    oci_manifest="$(jq -r '.oci.manifest // empty' <<<"${comp_json}")"

    manifest_src=""
    manifest_dest=""
    if [ -n "${manifest_rel}" ]; then
      manifest_src="${ROOT_DIR}/target/components/$(basename "${manifest_rel}")"
      manifest_dest="${dir}/${manifest_rel}"
    fi

    src="${ROOT_DIR}/target/components/${fname}"
    dest="${dir}/${wasm_path}"
    if [ ! -f "${src}" ] || { [ -n "${manifest_rel}" ] && [ ! -f "${manifest_src}" ]; }; then
      if [ -n "${oci_image}" ] && [ -n "${oci_artifact}" ]; then
        fetch_oci_component "${oci_image}" "${oci_digest}" "${oci_artifact}" "${src}" "${oci_manifest}" "${manifest_src}"
      else
        echo "Missing component artifact: ${src} (component ${comp_id})" >&2
        exit 1
      fi
    fi
    mkdir -p "$(dirname "${dest}")"
    cp "${src}" "${dest}"
    if [ -n "${manifest_rel}" ] && [ -f "${manifest_src}" ]; then
      mkdir -p "$(dirname "${manifest_dest}")"
      cp "${manifest_src}" "${manifest_dest}"
    fi
  done

  if [ ! -f "${dir}/pack.yaml" ]; then
    echo "Missing pack.yaml in ${dir}; greentic-pack requires pack.yaml inputs" >&2
    exit 1
  fi

  local_out_dir="${dir}/build"
  mkdir -p "${local_out_dir}"
  declare -a packc_flags=()
  if [ -n "${PACKC_BUILD_FLAGS:-}" ]; then
    IFS=' ' read -r -a packc_flags <<< "${PACKC_BUILD_FLAGS}"
  fi
  # Avoid greentic-pack mutating pack.yaml during CI runs.
  packc_flags+=("--no-update")
  (cd "${dir}" && "${PACKC_BIN}" build "${packc_flags[@]}" \
    --in "." \
    --gtpack-out "build/${pack_name}.gtpack" \
    --secrets-req ".secret_requirements.json")
  mv "${local_out_dir}/${pack_name}.gtpack" "${pack_out}"

  python3 "${ROOT_DIR}/tools/validate_pack_extensions.py" "${pack_out}"

  pack_version="$("${PACKC_BIN}" doctor --json --pack "${pack_out}" | jq -r '.meta.packVersion // ""')"
  if [ "${pack_version}" = "1" ] || [ -z "${pack_version}" ]; then
    echo "warning: greentic-pack produced pack-v1 manifest for ${pack_name}; proceed anyway (upgrade greentic-pack for newer schema) " >&2
  fi

  oci_ref="${OCI_REGISTRY}/${OCI_ORG}/${OCI_REPO}/${pack_name}:${PACK_VERSION}"
  # Compute local content digest (used for dry-run and lockfile regardless of push).
  digest="$(python3 - <<'PY' "${pack_out}"
import hashlib, sys
path = sys.argv[1]
h = hashlib.sha256()
with open(path, "rb") as f:
    for chunk in iter(lambda: f.read(8192), b""):
        h.update(chunk)
print("sha256:" + h.hexdigest())
PY
)"

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

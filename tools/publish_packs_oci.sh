#!/usr/bin/env bash
set -euo pipefail

# Publishes *.gtpack archives to an OCI registry (GHCR by default) and writes packs.lock.json.
# DRY_RUN=1 builds packs and writes the lockfile but does not push.

OCI_REGISTRY="${OCI_REGISTRY:-ghcr.io}"
OCI_NAMESPACE="${OCI_NAMESPACE:-}"
if [ -z "${OCI_NAMESPACE}" ] && [ -n "${OCI_ORG:-}" ]; then
  OCI_NAMESPACE="${OCI_ORG}"
fi
if [ -z "${OCI_NAMESPACE}" ] && [ -n "${GITHUB_REPOSITORY_OWNER:-}" ]; then
  OCI_NAMESPACE="${GITHUB_REPOSITORY_OWNER}/greentic-messaging-providers"
fi
OCI_NAMESPACE="${OCI_NAMESPACE:-greenticai/greentic-messaging-providers}"
OCI_ORG="${OCI_ORG:-${OCI_NAMESPACE%%/*}}"
OCI_REPO="${OCI_REPO:-packs}"
PACK_VERSION="${PACK_VERSION:-}"
if [ -z "${PACK_VERSION}" ]; then
  command -v python3 >/dev/null 2>&1 || { echo "python3 is required"; exit 1; }
  PACK_VERSION="$(python3 - <<'PY'
from pathlib import Path
import tomllib
data = tomllib.loads(Path("Cargo.toml").read_text())
print(data.get("workspace", {}).get("package", {}).get("version", ""))
PY
)"
fi
PACK_VERSION="${PACK_VERSION:-${GITHUB_REF_NAME:-0.0.0}}"
PACK_VERSION="${PACK_VERSION#v}"
PACKS_DIR="${PACKS_DIR:-packs}"
OUT_DIR="${OUT_DIR:-dist/packs}"
DRY_RUN="${DRY_RUN:-0}"
PUBLISH_LATEST="${PUBLISH_LATEST:-0}"
PUBLISH_LATEST_TAG="${PUBLISH_LATEST_TAG:-latest}"
PACKC_BIN="${PACKC_BIN:-greentic-pack}"
PACKC_BUILD_FLAGS="${PACKC_BUILD_FLAGS:-}"
PACK_FILTER="${PACK_FILTER:-}"
MEDIA_TYPE="${MEDIA_TYPE:-application/vnd.greentic.gtpack.v1+zip}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_COMPONENTS="${TARGET_COMPONENTS:-${ROOT_DIR}/target/components}"
mkdir -p "${ROOT_DIR}/${OUT_DIR}"

if [ -f "${ROOT_DIR}/.env" ]; then
  set -a
  # shellcheck disable=SC1091
  source "${ROOT_DIR}/.env"
  set +a
fi

if [ -x "${ROOT_DIR}/tools/prepare_pack_assets.sh" ]; then
  "${ROOT_DIR}/tools/prepare_pack_assets.sh"
fi

timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
git_sha="$(cd "${ROOT_DIR}" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")"

# Default OCI location for the shared templates component used by many packs.
TEMPLATES_REGISTRY="${TEMPLATES_REGISTRY:-${OCI_REGISTRY:-ghcr.io}}"
TEMPLATES_NAMESPACE="${TEMPLATES_NAMESPACE:-${GHCR_NAMESPACE:-${OCI_ORG:-greenticai}}}"
DEFAULT_TEMPLATES_IMAGE="${TEMPLATES_IMAGE:-${TEMPLATES_REGISTRY}/${TEMPLATES_NAMESPACE}/components/templates:latest}"
DEFAULT_TEMPLATES_DIGEST="${TEMPLATES_DIGEST:-}"
DEFAULT_TEMPLATES_ARTIFACT="component_templates.wasm"
DEFAULT_TEMPLATES_MANIFEST="component.publish.manifest.json"

components_lockfile="${ROOT_DIR}/components.lock.json"
if [ -f "${components_lockfile}" ] && { [ -z "${TEMPLATES_IMAGE:-}" ] || [ -z "${TEMPLATES_DIGEST:-}" ]; }; then
  locked_templates_ref="$(jq -r '.components[]? | select(.name=="templates") | .reference // empty' "${components_lockfile}" | head -n1)"
  locked_templates_digest="$(jq -r '.components[]? | select(.name=="templates") | .digest // empty' "${components_lockfile}" | head -n1)"
  if [ -n "${locked_templates_ref}" ] && [ -z "${TEMPLATES_IMAGE:-}" ]; then
    DEFAULT_TEMPLATES_IMAGE="${locked_templates_ref}"
  fi
  if [ -n "${locked_templates_digest}" ] && [ -z "${TEMPLATES_DIGEST:-}" ]; then
    DEFAULT_TEMPLATES_DIGEST="${locked_templates_digest}"
  fi
fi
echo "Using templates image: ${DEFAULT_TEMPLATES_IMAGE}"

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

# Generated flow helper components are still in the migration window where
# some metadata is sourced from pack.yaml instead of sibling manifests.
# Keep the pack build on the compatible path until those components are
# fully materialized with component.manifest.json files.
case " ${PACKC_BUILD_FLAGS} " in
  *" --allow-pack-schema "*) ;;
  *) PACKC_BUILD_FLAGS="--allow-pack-schema ${PACKC_BUILD_FLAGS}" ;;
esac

if [ ! -d "${ROOT_DIR}/${PACKS_DIR}" ]; then
  echo "Packs directory ${PACKS_DIR} not found" >&2
  exit 1
fi

packs_json="[]"

pack_selected() {
  local pack_name="$1"
  if [ -z "${PACK_FILTER}" ]; then
    return 0
  fi
  python3 - <<'PY' "${PACK_FILTER}" "${pack_name}"
import sys

raw = sys.argv[1]
name = sys.argv[2]
items = [part.strip() for chunk in raw.split(",") for part in chunk.split() if part.strip()]
raise SystemExit(0 if name in items else 1)
PY
}

generate_pack_manifest() {
  local pack_dir="$1"
  local secrets_out="$2"
  python3 "${ROOT_DIR}/tools/generate_pack_metadata.py" \
    --pack-dir "${pack_dir}" \
    --components-dir "${ROOT_DIR}/components" \
    --version "${PACK_VERSION}" \
    --secrets-out "${secrets_out}"
}

run_pack_doctor_json_tolerant() {
  local pack_path=""
  local prev=""
  for arg in "$@"; do
    if [ "${prev}" = "--pack" ]; then
      pack_path="${arg}"
      break
    fi
    prev="${arg}"
  done
  local raw_json
  local filtered_json
  local raw_stderr
  local doctor_rc=0
  local filtered_content
  local has_errors
  local filter_rc=0
  raw_json="$(mktemp)"
  filtered_json="$(mktemp)"
  raw_stderr="$(mktemp)"
  if ! "${PACKC_BIN}" doctor --json "$@" >"${raw_json}" 2>"${raw_stderr}"; then
    doctor_rc=$?
  fi
  if ! python3 "${ROOT_DIR}/tools/filter_pack_doctor_json.py" "${raw_json}" >"${filtered_json}"; then
    filter_rc=$?
  fi
  if [ "${filter_rc}" -ne 0 ]; then
    echo "greentic-pack doctor did not return valid JSON for ${pack_path:-pack}" >&2
    cat "${raw_stderr}" >&2
    rm -f "${raw_json}" "${filtered_json}" "${raw_stderr}"
    if [ "${doctor_rc}" -ne 0 ]; then
      return "${doctor_rc}"
    fi
    return "${filter_rc}"
  fi
  local raw_errors
  local filtered_errors
  raw_errors="$(jq '(.validation.diagnostics // []) | map(select(.severity == "error")) | length' "${raw_json}")"
  filtered_errors="$(jq '(.validation.diagnostics // []) | map(select(.severity == "error")) | length' "${filtered_json}")"
  if [ "${raw_errors}" -gt "${filtered_errors}" ]; then
    echo "warning: ignored legacy helper doctor diagnostics for ${pack_path:-pack}" >&2
  fi
  filtered_content="$(cat "${filtered_json}")"
  printf '%s\n' "${filtered_content}"
  has_errors="$(jq -r '.validation.has_errors // false' <<<"${filtered_content}")"
  if [ "${has_errors}" = "true" ]; then
    echo "Filtered pack doctor diagnostics for ${pack_path:-pack}:" >&2
    jq '.validation.diagnostics // []' <<<"${filtered_content}" >&2 || printf '%s\n' "${filtered_content}" >&2
    cat "${raw_stderr}" >&2
    rm -f "${raw_json}" "${filtered_json}" "${raw_stderr}"
    if [ "${doctor_rc}" -ne 0 ]; then
      return "${doctor_rc}"
    fi
    return 1
  fi
  rm -f "${raw_json}" "${filtered_json}" "${raw_stderr}"
  return 0
}

doctor_supports_validator_pack() {
  local output
  output="$("${PACKC_BIN}" doctor --validator-pack oci://example.invalid/validator:latest --pack /nonexistent 2>&1 || true)"
  if printf '%s' "${output}" | grep -Fq "unexpected argument '--validator-pack'"; then
    return 1
  fi
  return 0
}

doctor_supports_validator_wasm() {
  local output
  output="$("${PACKC_BIN}" doctor --validator-wasm greentic.validators.messaging=/nonexistent --pack /nonexistent 2>&1 || true)"
  if printf '%s' "${output}" | grep -Fq "unexpected argument '--validator-wasm'"; then
    return 1
  fi
  return 0
}

doctor_supports_validate() {
  local output
  output="$("${PACKC_BIN}" doctor --validate --pack /nonexistent 2>&1 || true)"
  if printf '%s' "${output}" | grep -Fq "unexpected argument '--validate'"; then
    return 1
  fi
  return 0
}

ensure_secret_requirements_asset() {
  local pack_dir="$1"
  local secrets_out="$2"
  local dest_root="${pack_dir}/secret-requirements.json"
  rm -f "${pack_dir}/assets/secret-requirements.json"
  if [ -f "${secrets_out}" ]; then
    cp "${secrets_out}" "${dest_root}"
  else
    printf '%s\n' "[]" > "${dest_root}"
  fi
}

ensure_secret_requirements_asset_entry() {
  local pack_dir="$1"
  local yaml_path="${pack_dir}/pack.yaml"
  [ -f "${yaml_path}" ] || return 0
  python3 - "${yaml_path}" <<'PY'
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
}

ensure_pack_readme() {
  local pack_dir="$1"
  local manifest_path="${pack_dir}/pack.manifest.json"
  local readme_path="${pack_dir}/README.md"
  [ -f "${readme_path}" ] && return 0
  [ -f "${manifest_path}" ] || return 0
  python3 - "${manifest_path}" "${readme_path}" <<'PY'
import json
import sys
from pathlib import Path

def titleize(name: str) -> str:
    return " ".join([part.capitalize() for part in name.replace("_", "-").split("-") if part])

manifest = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
out_path = Path(sys.argv[2])

name = manifest.get("name", out_path.parent.name)
desc = (manifest.get("description") or "").strip()
title = f"{titleize(name)} Pack"

lines = [f"# {title}", ""]
if desc:
    lines.append(desc)
    lines.append("")

lines.extend(["## Pack ID", f"- `{name}`", ""])

providers = (
    (manifest.get("extensions") or {})
    .get("greentic.provider-extension.v1", {})
    .get("inline", {})
    .get("providers", [])
)
if providers:
    lines.append("## Providers")
    for provider in providers:
        ptype = provider.get("provider_type", "")
        caps = provider.get("capabilities") or []
        ops = provider.get("ops") or []
        details = []
        if caps:
            details.append(f"capabilities: {', '.join(caps)}")
        if ops:
            details.append(f"ops: {', '.join(ops)}")
        suffix = f" ({'; '.join(details)})" if details else ""
        lines.append(f"- `{ptype}`{suffix}")
    lines.append("")

components = manifest.get("components") or []
if components:
    lines.append("## Components")
    for comp in components:
        lines.append(f"- `{comp}`")
    lines.append("")

secrets = manifest.get("secret_requirements") or []
lines.append("## Secrets")
if secrets:
    for item in secrets:
        name = (item.get("name") or "").strip()
        scope = (item.get("scope") or "").strip()
        desc = (item.get("description") or "").strip()
        scope_part = f" ({scope})" if scope else ""
        desc_part = f": {desc}" if desc else ""
        lines.append(f"- `{name}`{scope_part}{desc_part}")
else:
    lines.append("- None.")
lines.append("")

flows = manifest.get("flows") or []
if flows:
    lines.append("## Flows")
    for flow in flows:
        fid = flow.get("id", "")
        lines.append(f"- `{fid}`")
    lines.append("")

out_path.write_text("\n".join(lines), encoding="utf-8")
PY
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

# Detect current root version so we can replace all matching occurrences.
old_version = None
for line in lines:
    stripped = line.lstrip()
    indent = len(line) - len(stripped)
    if indent == 0 and stripped.startswith("version:"):
        old_version = stripped.split(":", 1)[1].strip().strip("'\"")
        break

out = []
updated = False
for line in lines:
    stripped = line.lstrip()
    # Replace all version: fields that match the old pack version.
    if stripped.startswith("version:"):
        current = stripped.split(":", 1)[1].strip().strip("'\"")
        if current == old_version or current == version:
            prefix = line.split("version:")[0] + "version: "
            out.append(f"{prefix}{version}")
            updated = True
            continue
    out.append(line.replace("__PACK_VERSION__", version))

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
  if [ "${pack_name}" = "messaging-provider-bundle" ]; then
    echo "Skipping deprecated pack ${pack_name}"
    continue
  fi
  if ! pack_selected "${pack_name}"; then
    echo "Skipping filtered-out pack ${pack_name}"
    continue
  fi
  pack_out_rel="${OUT_DIR}/${pack_name}.gtpack"
  pack_out="${ROOT_DIR}/${pack_out_rel}"
  secrets_out="${dir}/.secret_requirements.json"

  update_pack_yaml_version "${dir}"
  generate_pack_manifest "${dir}" "${secrets_out}"
  ensure_secret_requirements_asset "${dir}" "${secrets_out}"
  ensure_secret_requirements_asset_entry "${dir}"
  ensure_pack_readme "${dir}"
  (cd "${dir}" && "${PACKC_BIN}" config)

  components=()
  while IFS= read -r comp_line; do
    [ -z "${comp_line}" ] && continue
    components+=("${comp_line}")
  done < <(read_components "${dir}/pack.manifest.json")

  missing_local=0
  for comp_json in "${components[@]}"; do
    oci_image="$(jq -r '.oci.image // empty' <<<"${comp_json}")"
    comp_id="$(jq -r '.id' <<<"${comp_json}")"
    wasm_path="$(jq -r '.wasm' <<<"${comp_json}")"
    fname="$(basename "${wasm_path}")"
    is_templates_component=0
    if [ "${comp_id}" = "templates" ] || [ "${comp_id}" = "ai.greentic.component-templates" ] || [ "${fname}" = "templates.wasm" ]; then
      is_templates_component=1
    fi
    if [ -z "${oci_image}" ] && [ "${is_templates_component}" -eq 1 ]; then
      oci_image="${DEFAULT_TEMPLATES_IMAGE}"
    fi
    if [ -z "${oci_image}" ] && [ ! -f "${TARGET_COMPONENTS}/${fname}" ]; then
      missing_local=1
      break
    fi
  done
  if [ "${missing_local}" -eq 1 ]; then
    echo "Building components because wasm artifacts are missing..."
    "${ROOT_DIR}/tools/build_components.sh"
  fi

  mkdir -p "${dir}/components"
  rm -f "${dir}/components/provision.wasm" "${dir}/components/qa.wasm"
  for comp_json in "${components[@]}"; do
    comp_id="$(jq -r '.id' <<<"${comp_json}")"
    wasm_path="$(jq -r '.wasm' <<<"${comp_json}")"
    fname="$(basename "${wasm_path}")"
    oci_image="$(jq -r '.oci.image // empty' <<<"${comp_json}")"
    oci_digest="$(jq -r '.oci.digest // empty' <<<"${comp_json}")"
    oci_artifact="$(jq -r '.oci.artifact // empty' <<<"${comp_json}")"
    manifest_rel="$(jq -r '.manifest // empty' <<<"${comp_json}")"
    oci_manifest="$(jq -r '.oci.manifest // empty' <<<"${comp_json}")"
    is_templates_component=0
    if [ "${comp_id}" = "templates" ] || [ "${comp_id}" = "ai.greentic.component-templates" ] || [ "${fname}" = "templates.wasm" ]; then
      is_templates_component=1
    fi

    if [ "${is_templates_component}" -eq 1 ] && [ -z "${oci_image}" ]; then
      oci_image="${DEFAULT_TEMPLATES_IMAGE}"
    fi
    if [ "${is_templates_component}" -eq 1 ] && [ -z "${oci_digest}" ]; then
      oci_digest="${DEFAULT_TEMPLATES_DIGEST}"
    fi
    if [ "${is_templates_component}" -eq 1 ] && [ -z "${oci_artifact}" ]; then
      oci_artifact="${DEFAULT_TEMPLATES_ARTIFACT}"
    fi
    if [ "${is_templates_component}" -eq 1 ] && [ -z "${oci_manifest}" ]; then
      oci_manifest="${DEFAULT_TEMPLATES_MANIFEST}"
    fi

    manifest_src=""
    manifest_dest=""
    root_manifest_src=""
    if [ -n "${manifest_rel}" ]; then
      # Use component-specific filename to avoid collisions when multiple components
      # have manifests with the same basename (e.g., component.manifest.json)
      manifest_src="${TARGET_COMPONENTS}/${comp_id}.manifest.json"
      manifest_dest="${dir}/${manifest_rel}"
      # Also check the root component directory as the canonical source
      root_manifest_src="${ROOT_DIR}/components/${comp_id}/component.manifest.json"
    fi

    src="${TARGET_COMPONENTS}/${fname}"
    dest="${dir}/${wasm_path}"
    if [ ! -f "${src}" ] || { [ -n "${manifest_rel}" ] && [ ! -f "${manifest_src}" ]; }; then
      if [ -n "${oci_image}" ] && [ -n "${oci_artifact}" ]; then
        fetch_oci_component "${oci_image}" "${oci_digest}" "${oci_artifact}" "${src}" "${oci_manifest}" "${manifest_src}"
      elif [ -f "${dir}/${wasm_path}" ]; then
        mkdir -p "$(dirname "${src}")"
        cp "${dir}/${wasm_path}" "${src}"
        if [ -n "${manifest_rel}" ] && [ -f "${dir}/${manifest_rel}" ]; then
          mkdir -p "$(dirname "${manifest_src}")"
          cp "${dir}/${manifest_rel}" "${manifest_src}"
        fi
      else
        echo "Missing component artifact: ${src} (component ${comp_id})" >&2
        exit 1
      fi
    fi
    mkdir -p "$(dirname "${dest}")"
    cp "${src}" "${dest}"
    # Copy manifest to pack directory, preferring root component manifest as canonical source
    if [ -n "${manifest_rel}" ]; then
      mkdir -p "$(dirname "${manifest_dest}")"
      if [ -n "${root_manifest_src}" ] && [ -f "${root_manifest_src}" ]; then
        # Use root component manifest as canonical source
        cp "${root_manifest_src}" "${manifest_dest}"
      elif [ -f "${manifest_src}" ]; then
        # Fallback to TARGET_COMPONENTS cache
        cp "${manifest_src}" "${manifest_dest}"
      fi
      # Stamp version, world, and profiles on component manifests
      python3 - "${manifest_dest}" "${PACK_VERSION}" "${dir}/pack.yaml" "${comp_id}" <<'PY'
from pathlib import Path
import json
import sys
import yaml

manifest_path = Path(sys.argv[1])
version = sys.argv[2]
pack_yaml_path = Path(sys.argv[3])
comp_id = sys.argv[4]

if manifest_path.exists():
    data = json.loads(manifest_path.read_text())
    data["version"] = version

    # Extract component config from pack.yaml
    if pack_yaml_path.exists():
        pack_data = yaml.safe_load(pack_yaml_path.read_text())
        for comp in pack_data.get("components", []):
            if comp.get("id") == comp_id:
                if "world" in comp:
                    data["world"] = comp["world"]
                if "profiles" in comp:
                    data["profiles"] = comp["profiles"]
                break

    manifest_path.write_text(json.dumps(data, indent=2) + "\n")
PY
    fi
  done

  # Stamp version on all component.manifest.json files inside the pack components directory.
  while IFS= read -r comp_id; do
    [ -z "${comp_id}" ] && continue
    local_manifest="${dir}/components/${comp_id}/component.manifest.json"
    [ -f "${local_manifest}" ] || continue
    python3 - "${local_manifest}" "${PACK_VERSION}" "${dir}/pack.yaml" "${comp_id}" <<'PY'
from pathlib import Path
import json
import sys
import yaml

manifest_path = Path(sys.argv[1])
version = sys.argv[2]
pack_yaml_path = Path(sys.argv[3])
comp_id = sys.argv[4]

if manifest_path.exists():
    data = json.loads(manifest_path.read_text())
    data["version"] = version
    if pack_yaml_path.exists():
        pack_data = yaml.safe_load(pack_yaml_path.read_text())
        for comp in pack_data.get("components", []):
            if comp.get("id") == comp_id:
                if "world" in comp:
                    data["world"] = comp["world"]
                if "profiles" in comp:
                    data["profiles"] = comp["profiles"]
                break
    manifest_path.write_text(json.dumps(data, indent=2) + "\n")
PY
  done < <(jq -r '(.component_sources // .components // [])[] | if type=="string" then . else (.id // "") end' "${dir}/pack.manifest.json")

  if [ ! -f "${dir}/pack.yaml" ]; then
    echo "Missing pack.yaml in ${dir}; greentic-pack requires pack.yaml inputs" >&2
    exit 1
  fi

  # Resolve after staging component artifacts so pack.lock digests match the
  # exact bytes that will be packaged into the final gtpack.
  (cd "${dir}" && "${PACKC_BIN}" resolve)

  local_out_dir="${ROOT_DIR}/.tmp/packs"
  mkdir -p "${local_out_dir}"
  tmp_pack_out="${local_out_dir}/${pack_name}.gtpack"
  rm -f "${tmp_pack_out}"
  declare -a packc_flags=()
  if [ -n "${PACKC_BUILD_FLAGS:-}" ]; then
    IFS=' ' read -r -a packc_flags <<< "${PACKC_BUILD_FLAGS}"
  fi
  # Avoid greentic-pack mutating pack.yaml during CI runs.
  packc_flags+=("--no-update")
  (cd "${dir}" && "${PACKC_BIN}" build "${packc_flags[@]}" \
    --in "." \
    --gtpack-out "${tmp_pack_out}" \
    --secrets-req ".secret_requirements.json")
  mv "${tmp_pack_out}" "${pack_out}"

  python3 "${ROOT_DIR}/tools/validate_pack_extensions.py" "${pack_out}"

  doctor_json="$(run_pack_doctor_json_tolerant --pack "${pack_out}")"
  pack_version="$(jq -r '.manifest.meta.packVersion // ""' <<<"${doctor_json}")"
  if [ "${pack_version}" = "1" ] || [ -z "${pack_version}" ]; then
    echo "warning: greentic-pack produced pack-v1 manifest for ${pack_name}; proceed anyway (upgrade greentic-pack for newer schema) " >&2
  fi
  doctor_version="$(jq -r '.manifest.meta.version // ""' <<<"${doctor_json}")"
  manifest_version="$(jq -r '.version // ""' "${dir}/pack.manifest.json")"
  if [ -z "${doctor_version}" ] || [ -z "${manifest_version}" ]; then
    echo "Missing pack version metadata for ${pack_name} (doctor=${doctor_version:-empty}, manifest=${manifest_version:-empty})" >&2
    exit 1
  fi
  if [ "${doctor_version}" != "${manifest_version}" ]; then
    echo "Pack version drift for ${pack_name}: gtpack=${doctor_version} manifest=${manifest_version}" >&2
    exit 1
  fi
  if [ -n "${PACK_VERSION}" ] && [ "${doctor_version}" != "${PACK_VERSION}" ]; then
    echo "Pack version mismatch for ${pack_name}: gtpack=${doctor_version} expected=${PACK_VERSION}" >&2
    exit 1
  fi

  python3 "${ROOT_DIR}/tools/validate_pack_fixtures.py"

  oci_ref="${OCI_REGISTRY}/${OCI_ORG}/${OCI_REPO}/messaging/${pack_name}:${PACK_VERSION}"
  latest_ref="${OCI_REGISTRY}/${OCI_ORG}/${OCI_REPO}/messaging/${pack_name}:${PUBLISH_LATEST_TAG}"
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
    readme_path="${dir}/README.md"
    pack_desc="$(jq -r '.description // empty' "${dir}/pack.manifest.json")"
    pack_title="$(jq -r '.name // empty' "${dir}/pack.manifest.json")"
    # Use relative paths to avoid absolute paths being embedded in OCI artifacts.
    # This prevents "path traversal disallowed" errors when pulling.
    pack_basename="$(basename "${pack_out}")"
    pack_dir="$(dirname "${pack_out}")"
    oras_files=("${pack_basename}:${MEDIA_TYPE}")
    if [ -f "${readme_path}" ]; then
      # Copy README to pack output directory for relative path push
      cp "${readme_path}" "${pack_dir}/README.md"
      oras_files+=("README.md:text/markdown")
    fi
    digest="$(
      cd "${pack_dir}" && oras push \
        --artifact-type "${MEDIA_TYPE}" \
        --annotation "org.opencontainers.image.source=${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-unknown}" \
        --annotation "org.opencontainers.image.revision=${git_sha}" \
        --annotation "org.opencontainers.image.version=${PACK_VERSION}" \
        --annotation "org.opencontainers.image.title=${pack_title}" \
        --annotation "org.opencontainers.image.description=${pack_desc}" \
        "${oci_ref}" \
        "${oras_files[@]}" \
        | awk '/Digest:/{print $2}' | tail -n1
    )"
    if [[ "${PUBLISH_LATEST}" =~ ^(1|true|TRUE|yes|YES)$ ]]; then
      (
        cd "${pack_dir}" && oras push \
          --artifact-type "${MEDIA_TYPE}" \
          --annotation "org.opencontainers.image.source=${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-unknown}" \
          --annotation "org.opencontainers.image.revision=${git_sha}" \
          --annotation "org.opencontainers.image.version=${PACK_VERSION}" \
          --annotation "org.opencontainers.image.title=${pack_title}" \
          --annotation "org.opencontainers.image.description=${pack_desc}" \
          "${latest_ref}" \
          "${oras_files[@]}"
      ) >/dev/null
    fi
    # Clean up copied README from pack directory
    rm -f "${pack_dir}/README.md"
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

bundle_pack="${ROOT_DIR}/${OUT_DIR}/messaging-provider-bundle.gtpack"
if [ -f "${bundle_pack}" ]; then
  echo "Removing deprecated pack artifact ${bundle_pack}"
  rm -f "${bundle_pack}"
fi

if compgen -G "${ROOT_DIR}/${OUT_DIR}/messaging-*.gtpack" >/dev/null; then
  validator_ref="${VALIDATOR_PACK_REF:-oci://ghcr.io/greenticai/validators/messaging:latest}"
  validator_root="${ROOT_DIR}/.greentic/validators"
  validator_wasm="${VALIDATOR_WASM:-${validator_root}/greentic.validators.messaging.wasm}"
  mkdir -p "${validator_root}"
  if [ ! -f "${validator_wasm}" ] && command -v greentic-dev >/dev/null 2>&1; then
    if greentic-dev store fetch "${validator_ref}" --out "${validator_wasm}" >/dev/null 2>&1; then
      echo "Validator cached at ${validator_wasm}" >&2
    else
      echo "warning: validator fetch skipped; using validator ref/path if already available" >&2
    fi
  fi

  if doctor_supports_validate; then
    doctor_supports_validate=1
  else
    doctor_supports_validate=0
  fi
  if doctor_supports_validator_wasm; then
    doctor_supports_validator_wasm=1
  else
    doctor_supports_validator_wasm=0
  fi
  if doctor_supports_validator_pack; then
    doctor_supports_validator_pack=1
  else
    doctor_supports_validator_pack=0
  fi
  validator_warning_printed=0
  for pack in "${ROOT_DIR}/${OUT_DIR}"/messaging-*.gtpack; do
    pack_name="$(basename "${pack}" .gtpack)"
    if ! pack_selected "${pack_name}"; then
      continue
    fi
    if [ -f "${validator_wasm}" ] && [ "${doctor_supports_validator_wasm}" -eq 1 ]; then
      if [ "${doctor_supports_validate}" -eq 1 ]; then
        run_pack_doctor_json_tolerant --validate --validator-wasm "greentic.validators.messaging=${validator_wasm}" --validator-policy required --pack "${pack}" >/dev/null
      else
        run_pack_doctor_json_tolerant --validator-wasm "greentic.validators.messaging=${validator_wasm}" --validator-policy required --pack "${pack}" >/dev/null
      fi
    elif [ "${doctor_supports_validator_pack}" -eq 1 ]; then
      if [ "${doctor_supports_validate}" -eq 1 ]; then
        run_pack_doctor_json_tolerant --validate --validator-pack "${validator_ref}" --pack "${pack}" >/dev/null
      else
        run_pack_doctor_json_tolerant --validator-pack "${validator_ref}" --pack "${pack}" >/dev/null
      fi
    else
      if [ "${validator_warning_printed}" -eq 0 ]; then
        echo "warning: ${PACKC_BIN} doctor lacks validator flags; running basic doctor only" >&2
        validator_warning_printed=1
      fi
      if [ "${doctor_supports_validate}" -eq 1 ]; then
        run_pack_doctor_json_tolerant --validate --pack "${pack}" >/dev/null
      else
        run_pack_doctor_json_tolerant --pack "${pack}" >/dev/null
      fi
    fi
  done
fi

echo "${packs_json}" | jq '{ packs: . }' > "${ROOT_DIR}/packs.lock.json"
echo "Wrote packs.lock.json"

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

if [ -f "${ROOT_DIR}/.env" ]; then
  echo "==> Loading local environment from .env"
  set -a
  # shellcheck disable=SC1091
  source "${ROOT_DIR}/.env"
  set +a
fi

# Keep authentication identity (GHCR_USERNAME) decoupled from namespace
# selection. Namespace resolution is handled in tools/sync_packs.sh via
# TEMPLATES_NAMESPACE/GHCR_NAMESPACE/OCI_ORG.

PACK_VERSION="${PACK_VERSION:-$(python3 - <<'PY'
from pathlib import Path
import tomllib
data = tomllib.loads(Path("Cargo.toml").read_text())
print(data.get("workspace", {}).get("package", {}).get("version", "0.0.0"))
PY
)}"
export PACK_VERSION
export GREENTIC_RUNNER_SMOKE=1

LOCAL_CHECK_PLAN_JSON="$(mktemp)"
python3 - <<'PY' >"${LOCAL_CHECK_PLAN_JSON}"
import json
import os
import subprocess
from pathlib import Path

ROOT = Path.cwd()
matrix = json.loads(Path("ci/provider-matrix.json").read_text())

def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True, stderr=subprocess.DEVNULL)

def git_ok(*args: str) -> bool:
    return subprocess.run(["git", *args], cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0

def lines(text: str) -> list[str]:
    return [line.strip() for line in text.splitlines() if line.strip()]

override = os.environ.get("LOCAL_CHECK_CHANGED_FILES_JSON", "").strip()
if override:
    changed_files = sorted(set(json.loads(override)))
else:
    base = ""
    for candidate in (
        "origin/main",
        "origin/master",
        "main",
        "master",
    ):
        if git_ok("rev-parse", "--verify", "--quiet", f"{candidate}^{{commit}}"):
            base = candidate
            break

    changed: set[str] = set()
    if base:
        try:
            merge_base = git("merge-base", "HEAD", base).strip()
            changed.update(lines(git("diff", "--name-only", f"{merge_base}..HEAD")))
        except subprocess.CalledProcessError:
            changed.update(lines(git("diff", "--name-only", f"{base}..HEAD")))

    changed.update(lines(git("diff", "--name-only")))
    changed.update(lines(git("diff", "--cached", "--name-only")))
    changed.update(lines(git("ls-files", "--others", "--exclude-standard")))
    changed_files = sorted(changed)

def matches(path: str, candidate: str) -> bool:
    return path.startswith(candidate) if candidate.endswith("/") else path == candidate

provider_hits: dict[str, set[str]] = {name: set() for name in matrix["providers"]}
global_provider_metadata = {
    "Cargo.lock",
    "packs.lock.json",
    "ci/provider-matrix.json",
}
shared_reasons: list[str] = []

for path in changed_files:
    # Provider pack archives are global outputs, but each archive belongs to one provider.
    gtpack_matches = [
        name
        for name, provider in matrix["providers"].items()
        if path == f"dist/packs/{provider['pack']}.gtpack"
    ]
    if len(gtpack_matches) == 1:
        provider_hits[gtpack_matches[0]].add(path)
        continue

    matched = [
        name
        for name, provider in matrix["providers"].items()
        if any(matches(path, candidate) for candidate in provider.get("paths", []))
    ]
    if len(matched) == 1:
        provider_hits[matched[0]].add(path)
        continue
    if len(matched) > 1:
        shared_reasons.append(f"multi-provider path changed: {path}")
        continue

    if path in global_provider_metadata:
        continue

    if any(matches(path, candidate) for candidate in matrix.get("shared_paths", [])):
        shared_reasons.append(f"shared path changed: {path}")
        continue

    shared_reasons.append(f"unmapped path changed: {path}")

affected = sorted(name for name, paths in provider_hits.items() if paths)
if not changed_files:
    mode = "full"
    reason = "no changed files detected"
elif shared_reasons:
    mode = "full"
    reason = shared_reasons[0]
elif not affected:
    mode = "full"
    reason = "only global provider metadata changed"
else:
    mode = "provider"
    reason = "provider-scoped changes only"

components: list[str] = []
manifests: list[str] = []
component_manifests: list[str] = []
packs: list[str] = []
provider_test_files: list[str] = []
for name in affected:
    provider = matrix["providers"][name]
    components.extend(provider.get("components", []))
    manifests.extend(provider.get("manifests", []))
    for component in provider.get("components", []):
        candidate = Path("components") / component / "component.manifest.json"
        if candidate.exists():
            component_manifests.append(str(candidate))
    packs.append(provider["pack"])
    for path in provider.get("paths", []):
        prefix = "crates/provider-tests/tests/"
        if path.startswith(prefix) and path.endswith(".rs"):
            provider_test_files.append(Path(path).stem)

plan = {
    "mode": mode,
    "reason": reason,
    "changed_files": changed_files,
    "providers": affected,
    "components": sorted(dict.fromkeys(components)),
    "manifests": sorted(dict.fromkeys(manifests)),
    "component_manifests": sorted(dict.fromkeys(component_manifests)),
    "packs": sorted(dict.fromkeys(packs)),
    "provider_test_files": sorted(dict.fromkeys(provider_test_files)),
}
print(json.dumps(plan))
PY

plan_value() {
  python3 - <<'PY' "${LOCAL_CHECK_PLAN_JSON}" "$1"
import json
import sys

data = json.loads(open(sys.argv[1]).read())
value = data.get(sys.argv[2])
if isinstance(value, list):
    print(" ".join(value))
elif isinstance(value, (dict, tuple)):
    print(json.dumps(value))
elif value is not None:
    print(value)
PY
}

plan_json_value() {
  python3 - <<'PY' "${LOCAL_CHECK_PLAN_JSON}" "$1"
import json
import sys

data = json.loads(open(sys.argv[1]).read())
print(json.dumps(data.get(sys.argv[2], [])))
PY
}

LOCAL_CHECK_MODE="$(plan_value mode)"
LOCAL_CHECK_REASON="$(plan_value reason)"
if [ "${LOCAL_CHECK_PRINT_PLAN_ONLY:-0}" = "1" ]; then
  cat "${LOCAL_CHECK_PLAN_JSON}"
  echo
  exit 0
fi
if [ "${LOCAL_CHECK_MODE}" = "provider" ]; then
  AFFECTED_PROVIDERS="$(plan_value providers)"
  AFFECTED_COMPONENTS="$(plan_value components)"
  AFFECTED_PACKS="$(plan_value packs)"
  AFFECTED_MANIFESTS_JSON="$(plan_json_value manifests)"
  AFFECTED_COMPONENT_MANIFESTS_JSON="$(plan_json_value component_manifests)"
  AFFECTED_PACK_FILTER="$(printf '%s' "${AFFECTED_PACKS}" | tr ' ' ',')"
  AFFECTED_COMPONENT_FILTER="$(printf '%s' "${AFFECTED_COMPONENTS}" | tr ' ' ',')"
  AFFECTED_PROVIDER_TESTS="$(plan_value provider_test_files)"

  echo "==> Local check mode: provider-scoped (${LOCAL_CHECK_REASON})"
  echo "    providers: ${AFFECTED_PROVIDERS}"
  echo "    components: ${AFFECTED_COMPONENTS}"
  echo "    packs: ${AFFECTED_PACKS}"

  echo "==> Syncing component dependency WIT from greentic-interfaces"
  ./tools/sync_wit_deps_from_greentic_interfaces.sh

  echo "==> WIT policy guard"
  ./ci/steps/00_wit_policy.sh

  echo "==> cargo fmt --check (affected components)"
  COMPONENT_MANIFESTS_JSON="${AFFECTED_MANIFESTS_JSON}" ./ci/steps/01_fmt.sh

  echo "==> cargo sort --workspace --check"
  ./ci/steps/01a_cargo_sort.sh

  echo "==> cargo clippy --all-targets (affected components)"
  COMPONENT_MANIFESTS_JSON="${AFFECTED_MANIFESTS_JSON}" ./ci/steps/02_clippy.sh

  echo "==> tools/build_components.sh (affected components)"
  COMPONENT_FILTER="${AFFECTED_COMPONENT_FILTER}" ./ci/steps/03_build_components.sh

  echo "==> ensuring shared templates component is available for affected packs"
  PACK_FILTER="${AFFECTED_PACK_FILTER}" ./ci/steps/04_ensure_templates.sh

  echo "==> tools/check_op_schemas.py (affected components)"
  COMPONENT_MANIFESTS_JSON="${AFFECTED_COMPONENT_MANIFESTS_JSON}" ./ci/steps/05_check_op_schemas.sh

  echo "==> ci/gen_flows.sh"
  ./ci/steps/06_gen_flows.sh

  echo "==> scripts/build_providers.sh (affected providers)"
  for provider in ${AFFECTED_PROVIDERS}; do
    bash scripts/build_providers.sh "${provider}"
  done

  if ! command -v greentic-runner >/dev/null 2>&1; then
    echo "==> Installing greentic-runner"
    cargo binstall greentic-runner --no-confirm --locked
  fi

  echo "==> greentic-flow doctor --validate (affected packs)"
  PACK_FILTER="${AFFECTED_PACK_FILTER}" ./ci/steps/08_flow_doctor.sh

  echo "==> greentic-component doctor --validate (affected packs)"
  PACK_FILTER="${AFFECTED_PACK_FILTER}" ./ci/steps/09_component_doctor.sh

  if command -v greentic-pack >/dev/null 2>&1; then
    echo "Using existing greentic-pack: $(greentic-pack --version)"
  else
    echo "greentic-pack is required; run gtc install before local checks" >&2
    exit 1
  fi

  echo "==> cargo test (affected components)"
  for component in ${AFFECTED_COMPONENTS}; do
    cargo test -p "${component}"
  done

  if [ -n "${AFFECTED_PROVIDER_TESTS}" ]; then
    echo "==> provider-tests (affected provider test files)"
    for test_name in ${AFFECTED_PROVIDER_TESTS}; do
      cargo test -p provider-tests --test "${test_name}"
    done
  fi

  echo "==> provider-tests (generic pack/provider checks)"
  cargo test -p provider-tests --test packs_consistency
  cargo test -p provider-tests --test provider_build_answers
  cargo test -p provider-tests --test registry_fixtures

  echo "==> pack fixture validation"
  python3 tools/validate_pack_fixtures.py

  echo "Provider-scoped checks completed."
  exit 0
fi

echo "==> Local check mode: full (${LOCAL_CHECK_REASON})"

echo "==> Syncing component dependency WIT from greentic-interfaces"
./tools/sync_wit_deps_from_greentic_interfaces.sh

echo "==> WIT policy guard"
# If this fails, re-run only:
#   ./ci/steps/00_wit_policy.sh
./ci/steps/00_wit_policy.sh

echo "==> cargo fmt --check"
# If this fails, re-run only:
#   ./ci/steps/01_fmt.sh
./ci/steps/01_fmt.sh

echo "==> cargo sort --workspace --check"
# If this fails, re-run only:
#   ./ci/steps/01a_cargo_sort.sh
./ci/steps/01a_cargo_sort.sh

echo "==> cargo clippy --workspace --all-targets"
# If this fails, re-run only:
#   ./ci/steps/02_clippy.sh
./ci/steps/02_clippy.sh

echo "==> tools/build_components.sh"
# If this fails, re-run only:
#   ./ci/steps/03_build_components.sh
./ci/steps/03_build_components.sh

echo "==> ensuring shared templates component is available for each pack"
# If this fails, re-run only:
#   ./ci/steps/04_ensure_templates.sh
./ci/steps/04_ensure_templates.sh

echo "==> tools/check_op_schemas.py"
# If this fails, re-run only:
#   ./ci/steps/05_check_op_schemas.sh
./ci/steps/05_check_op_schemas.sh

echo "==> ci/gen_flows.sh"
# If this fails, re-run only:
#   ./ci/steps/06_gen_flows.sh
./ci/steps/06_gen_flows.sh

if [ -n "${GHCR_TOKEN:-}" ] && [ "${LOCAL_CHECK_FETCH_TEMPLATES_FROM_OCI:-0}" = "1" ]; then
  echo "==> Forcing templates refresh from OCI (LOCAL_CHECK_FETCH_TEMPLATES_FROM_OCI=1)"
  rm -f "${ROOT_DIR}/target/components/templates.wasm"
  rm -f "${ROOT_DIR}/target/components/templates.manifest.json"
  rm -f "${ROOT_DIR}"/packs/*/components/templates.wasm
  rm -f "${ROOT_DIR}"/packs/*/components/templates.manifest.json
else
  # Local/offline path: seed template artifacts from already-synced pack copies
  # so sync_packs won't require OCI fetch for template components.
  templates_src="$(find "${ROOT_DIR}"/packs/*/components -maxdepth 1 -type f -name 'ai.greentic.component-templates.wasm' | head -n 1 || true)"
  if [ -n "${templates_src}" ]; then
    mkdir -p "${ROOT_DIR}/target/components"
    cp "${templates_src}" "${ROOT_DIR}/target/components/ai.greentic.component-templates.wasm"
    cp "${templates_src}" "${ROOT_DIR}/target/components/templates.wasm"
  fi
fi

echo "==> tools/sync_packs.sh (PACK_VERSION=${PACK_VERSION})"
# If this fails, re-run only:
#   ./ci/steps/07_sync_packs.sh
./ci/steps/07_sync_packs.sh

if ! command -v greentic-runner >/dev/null 2>&1; then
  echo "==> Installing greentic-runner"
  cargo binstall greentic-runner --no-confirm --locked
fi

echo "==> greentic-flow doctor --validate (packs/*/flows)"
# If this fails, re-run only:
#   ./ci/steps/08_flow_doctor.sh
./ci/steps/08_flow_doctor.sh

echo "==> greentic-component doctor --validate (components manifests)"
# If this fails, re-run only:
#   ./ci/steps/09_component_doctor.sh
./ci/steps/09_component_doctor.sh

echo "==> greentic-component test (questions emit/validate)"
# If this fails, re-run only:
#   ./ci/steps/10_questions_component_test.sh
./ci/steps/10_questions_component_test.sh

echo "==> tools/build_packs_only.sh (dry-run; rebuild dist/packs)"
# If this fails, re-run only:
#   ./ci/steps/11_build_packs.sh
pack_build_log="$(mktemp)"
set +e
./ci/steps/11_build_packs.sh 2>&1 | tee "${pack_build_log}"
pack_build_rc=${PIPESTATUS[0]}
set -e
if [ "${pack_build_rc}" -ne 0 ]; then
  if rg -q "expected record of 19 fields, found 18 fields" "${pack_build_log}" \
    && rg -q "component imports instance \`greentic:state/state-store@1.0.0\`" "${pack_build_log}"; then
    echo "warning: skipping pack build gate due to external greentic-pack/runner linker bug (state-store tenant-ctx ABI skew)." >&2
    echo "warning: reproduce with ./ci/steps/11_build_packs.sh and report against greentic-pack/greentic-runner." >&2
  else
    echo "pack build failed; see log at ${pack_build_log}" >&2
    exit "${pack_build_rc}"
  fi
fi
rm -f "${pack_build_log}"

if command -v greentic-pack >/dev/null 2>&1; then
  echo "Using existing greentic-pack: $(greentic-pack --version)"
else
  echo "greentic-pack is required; run gtc install before local checks" >&2
  exit 1
fi

echo "==> cargo test --workspace"
# If this fails, re-run only:
#   ./ci/steps/12_cargo_test.sh
cargo_test_log="$(mktemp)"
set +e
./ci/steps/12_cargo_test.sh 2>&1 | tee "${cargo_test_log}"
cargo_test_rc=${PIPESTATUS[0]}
set -e
if [ "${cargo_test_rc}" -ne 0 ]; then
  echo "cargo test failed; see log at ${cargo_test_log}" >&2
  exit "${cargo_test_rc}"
fi
rm -f "${cargo_test_log}"

provider_version_overrides="$(python3 - <<'PY'
import json
import os
from pathlib import Path

workspace_version = os.environ["PACK_VERSION"]
matrix = json.loads(Path("ci/provider-matrix.json").read_text())
for provider, spec in matrix.get("providers", {}).items():
    version = spec.get("version")
    pack = spec.get("pack")
    if pack and version and version != workspace_version:
        print(provider)
PY
)"
if [ -n "${provider_version_overrides}" ]; then
  echo "==> Restoring provider-specific pack versions"
  while IFS= read -r provider; do
    [ -n "${provider}" ] || continue
    bash scripts/build_providers.sh "${provider}"
  done <<EOF
${provider_version_overrides}
EOF
fi

echo "All checks completed."

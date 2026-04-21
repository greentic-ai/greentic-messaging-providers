#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

mkdir -p dist dist/packs .tmp
REPORT_PATH="${ROOT_DIR}/dist/pack_validation_report.json"
LOG_PATH="${ROOT_DIR}/dist/pack_validation.log"
PACK_FILTER_RAW="${PACK_FILTER:-}"
PACK_LIST_JSON="$(python3 - <<'PY' "${PACK_FILTER_RAW}"
import json
import sys
from pathlib import Path

pack_filter = sys.argv[1].strip()
packs = sorted(
    p.name
    for p in Path("packs").iterdir()
    if p.is_dir() and p.name != "messaging-provider-bundle"
)
if pack_filter:
    selected = {part.strip() for chunk in pack_filter.split(",") for part in chunk.split() if part.strip()}
    packs = [pack for pack in packs if pack in selected]
print(json.dumps(packs))
PY
)"

write_report() {
  local status="$1"
  local exit_code="$2"
  python3 - <<'PY' "${REPORT_PATH}" "${status}" "${exit_code}" "${LOG_PATH}" "${PACK_LIST_JSON}" "${ROOT_DIR}"
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

report_path = Path(sys.argv[1])
status = sys.argv[2]
exit_code = int(sys.argv[3])
log_path = sys.argv[4]
packs = json.loads(sys.argv[5])
root_dir = Path(sys.argv[6])
log_text = Path(log_path).read_text(encoding="utf-8") if Path(log_path).exists() else ""

synced = set(re.findall(r"^Syncing ([^.:\n]+)\.\.\.$", log_text, flags=re.MULTILINE))
built = set(re.findall(r"^wrote .*/\.tmp/packs/([^.]+)\.gtpack$", log_text, flags=re.MULTILINE))

def classify_failure(text: str) -> tuple[str | None, str | None]:
    patterns = [
        ("remote_fetch_disabled", r"Remote (?:locked )?component fetch disabled"),
        ("digest_mismatch", r"PACK_LOCK_COMPONENT_DIGEST_MISMATCH"),
        ("doctor_failed", r"pack validation failed"),
        ("resolve_failed", r"resolved_digest does not match component bytes"),
        ("network_fetch_failed", r"Fetching OCI component .*?\nError: Get "),
        ("missing_component_artifact", r"Missing component artifact:"),
        ("pack_yaml_invalid", r"is not a valid pack\.yaml"),
    ]
    for code, pattern in patterns:
        if re.search(pattern, text, flags=re.MULTILINE):
            match = re.search(pattern, text, flags=re.MULTILINE)
            detail = match.group(0).strip() if match else None
            return code, detail
    return None, None

failed_pack = None
for pack in packs:
    if pack not in built:
        failed_pack = pack
        break

failure_code, failure_detail = classify_failure(log_text)

pack_entries = []
for pack in packs:
    gtpack_path = root_dir / "dist" / "packs" / f"{pack}.gtpack"
    if pack in built:
        pack_status = "validated"
        stage = "build_packs_only"
        failure = None
        detail = None
    elif pack in synced:
        pack_status = "failed"
        stage = "build_packs_only" if failed_pack == pack else "pending_after_failure"
        failure = failure_code if failed_pack == pack else None
        detail = failure_detail if failed_pack == pack else None
    else:
        pack_status = "pending"
        stage = "sync_packs" if failed_pack == pack else "not_started"
        failure = failure_code if failed_pack == pack and stage == "sync_packs" else None
        detail = failure_detail if failed_pack == pack and stage == "sync_packs" else None
    pack_entries.append(
        {
            "pack": pack,
            "status": pack_status,
            "stage": stage,
            "failure": failure,
            "detail": detail,
            "artifact": str(gtpack_path.relative_to(root_dir)) if gtpack_path.exists() else None,
        }
    )

report = {
    "schema_version": 1,
    "status": status,
    "exit_code": exit_code,
    "generated_at_utc": datetime.now(timezone.utc).isoformat(),
    "log_path": str(Path(log_path).relative_to(Path.cwd())),
    "failed_pack": failed_pack,
    "failure": failure_code,
    "detail": failure_detail,
    "packs": pack_entries,
}

report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY
}

on_exit() {
  local exit_code=$?
  if [ "${exit_code}" -eq 0 ]; then
    write_report "ok" 0
  else
    write_report "error" "${exit_code}"
  fi
}

trap on_exit EXIT

"${ROOT_DIR}/ci/lib/stage_local_components.sh"

echo "== Sync packs from checked-in specs and downloaded component artifacts =="
ALLOW_REMOTE_COMPONENT_FETCH=0 ./ci/steps/07_sync_packs.sh 2>&1 | tee "${LOG_PATH}"

echo "== Hermetic pack-spec lint =="
# Schema + wasm-reference existence check. Replaces the expensive DRY_RUN
# build previously used here; the full build (pack doctor, manifest checks,
# .gtpack zip) still runs in build-packs on the synced state.
mapfile -t pack_list < <(python3 -c "
import json
import sys
print('\n'.join(json.loads(sys.argv[1])))
" "${PACK_LIST_JSON}")

if [ "${#pack_list[@]}" -eq 0 ]; then
  echo "No packs in filter; nothing to lint"
else
  python3 "${ROOT_DIR}/ci/lib/hermetic_pack_lint.py" \
    --packs "${pack_list[@]}" \
    --packs-dir "${ROOT_DIR}/packs" \
    --components-dir "${ROOT_DIR}/target/components" \
    --report "${ROOT_DIR}/dist/hermetic_lint_report.json" \
    2>&1 | tee -a "${LOG_PATH}"
fi

echo "Early pack validation passed"

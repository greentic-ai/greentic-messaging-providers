#!/usr/bin/env bash
# Publish a single provider fast-path end-to-end.
#
# Runs a targeted local check (fmt + clippy + test for the provider's
# crates only), then dispatches the `publish-provider.yml` workflow on
# the current branch with `dry_run=false`, then tails the run to
# completion.
#
# Usage:
#   scripts/publish_provider.sh <provider> [--skip-local-check] [--dry-run]
#
# Providers (from ci/provider-matrix.json):
#   dummy, email, slack, teams, telegram, webchat, webchat-gui, webex, whatsapp
#
# Requirements: gh CLI authenticated, Rust toolchain matching repo.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

PROVIDER="${1:-}"
SKIP_LOCAL_CHECK=0
DRY_RUN=false
PUBLISH_LATEST=false

shift 2>/dev/null || true
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-local-check) SKIP_LOCAL_CHECK=1 ;;
    --dry-run) DRY_RUN=true ;;
    --publish-latest) PUBLISH_LATEST=true ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ -z "${PROVIDER}" ]; then
  echo "Usage: $0 <provider> [--skip-local-check] [--dry-run] [--publish-latest]" >&2
  echo "Providers:" >&2
  python3 -c "
import json
with open('ci/provider-matrix.json') as h:
    providers = json.load(h).get('providers', {})
for name in providers:
    print(f'  {name}')
" >&2
  exit 2
fi

# Resolve the provider into pack + components + manifests via the shared
# matrix script. This fails fast if the name is unknown.
RESOLVED_JSON="$(python3 ci/provider_matrix.py resolve-provider "${PROVIDER}")"
PACK="$(echo "${RESOLVED_JSON}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pack"])')"
COMPONENTS_CSV="$(echo "${RESOLVED_JSON}" | python3 -c 'import json,sys; print(",".join(json.load(sys.stdin)["components"]))')"
MANIFESTS_JSON="$(echo "${RESOLVED_JSON}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["manifests"]))')"

echo "== provider fast-path =="
echo "  provider   : ${PROVIDER}"
echo "  pack       : ${PACK}"
echo "  components : ${COMPONENTS_CSV}"
echo "  dry_run    : ${DRY_RUN}"
echo

if [ "${SKIP_LOCAL_CHECK}" -ne 1 ]; then
  # Run the same targeted fmt + clippy scripts CI uses, scoped to this
  # provider. Same manifests list the workflow consumes.
  echo "-- local check: fmt --"
  COMPONENT_MANIFESTS_JSON="${MANIFESTS_JSON}" ./ci/steps/01_fmt.sh

  echo "-- local check: clippy --"
  COMPONENT_MANIFESTS_JSON="${MANIFESTS_JSON}" ./ci/steps/02_clippy.sh

  # Native unit tests for the component crates. Per-crate, not workspace,
  # so a pure webchat push doesn't wait on whatsapp tests.
  echo "-- local check: cargo test (per component) --"
  IFS=',' read -ra COMPONENTS <<< "${COMPONENTS_CSV}"
  for component in "${COMPONENTS[@]}"; do
    echo ">> cargo test -p ${component} --lib"
    cargo test -p "${component}" --lib
  done
else
  echo "-- local check: SKIPPED (--skip-local-check) --"
fi

# Workflow dispatches need the branch name. Default to current branch;
# that matches how Maarten would use it from a feature branch or main.
BRANCH="$(git rev-parse --abbrev-ref HEAD)"

echo
echo "-- dispatching publish-provider.yml --"
echo "  ref=${BRANCH} provider=${PROVIDER} dry_run=${DRY_RUN} publish_latest=${PUBLISH_LATEST}"
gh workflow run publish-provider.yml \
  --ref "${BRANCH}" \
  -f "provider=${PROVIDER}" \
  -f "dry_run=${DRY_RUN}" \
  -f "publish_latest=${PUBLISH_LATEST}"

# Poll for the run that was just created — `gh workflow run` doesn't print
# a run ID, so we grab the latest queued run for this workflow on this
# branch. A short sleep lets GitHub register the dispatch before we query.
sleep 5
RUN_ID="$(gh run list \
  --workflow publish-provider.yml \
  --branch "${BRANCH}" \
  --limit 1 \
  --json databaseId \
  --jq '.[0].databaseId')"

if [ -z "${RUN_ID}" ]; then
  echo "warning: could not locate the run just dispatched; check https://github.com/$(gh repo view --json nameWithOwner -q .nameWithOwner)/actions" >&2
  exit 0
fi

echo "-- watching run ${RUN_ID} --"
echo "  https://github.com/$(gh repo view --json nameWithOwner -q .nameWithOwner)/actions/runs/${RUN_ID}"
gh run watch "${RUN_ID}" --exit-status

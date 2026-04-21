#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="${GREENTIC_WEBCHAT_SITE_DIR:-/projects/ai/greentic-ng/greentic-webchat/site/app}"
DEST_DIR="${ROOT_DIR}/packs/messaging-webchat-gui/assets/webchat-gui"

if [ ! -d "${SRC_DIR}" ]; then
  echo "Skipping WebChat GUI asset import; source not found: ${SRC_DIR}" >&2
  exit 0
fi

mkdir -p "${DEST_DIR}" "${DEST_DIR}/config" "${DEST_DIR}/i18n" "${DEST_DIR}/js" "${DEST_DIR}/skins"

# Import SPA build artifacts (JS/CSS bundles, config, i18n, js)
rsync -a --delete "${SRC_DIR}/assets/" "${DEST_DIR}/assets/"
rsync -a --delete "${SRC_DIR}/config/" "${DEST_DIR}/config/"
rsync -a --delete "${SRC_DIR}/i18n/" "${DEST_DIR}/i18n/"
rsync -a --delete "${SRC_DIR}/js/" "${DEST_DIR}/js/"

# Import skins from SPA without --delete to preserve pack-specific
# customizations (e.g. _template/fullpage/page.css, _template/skin.json).
rsync -a "${SRC_DIR}/skins/" "${DEST_DIR}/skins/"

js_bundle="$(basename "$(find "${DEST_DIR}/assets" -maxdepth 1 -type f -name 'index-*.js' | sort | head -n 1)")"
css_bundle="$(basename "$(find "${DEST_DIR}/assets" -maxdepth 1 -type f -name 'index-*.css' | sort | head -n 1)")"

if [ -z "${js_bundle}" ] || [ -z "${css_bundle}" ]; then
  echo "Unable to locate greentic-webchat app bundles in ${DEST_DIR}/assets" >&2
  exit 1
fi

# Generate index.html with correct bundle filenames.
# runtime-bootstrap.js is maintained in-repo and NOT overwritten here.
cat > "${DEST_DIR}/index.html" <<EOF
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Greentic WebChat</title>
    <script src="./runtime-bootstrap.js"></script>
    <script type="module" crossorigin src="./assets/${js_bundle}"></script>
    <link rel="stylesheet" crossorigin href="./assets/${css_bundle}">
  </head>
  <body>
    <div id="root"></div>
  </body>
</html>
EOF

cp "${DEST_DIR}/index.html" "${DEST_DIR}/404.html"

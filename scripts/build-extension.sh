#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# 1. Build the WASM app with Trunk, no content hashes so filenames are stable
#    and the side panel HTML can reference them directly.
trunk build --release --filehash false

# 2. Assemble the extension bundle under extension/dist/.
EXT=extension
DIST="$EXT/dist"
rm -rf "$DIST"
mkdir -p "$DIST"

# Copy to the stable names sidepanel.html imports. With `--filehash false`
# Trunk emits these exact names. We copy the wasm-bindgen glue + module
# explicitly (NOT a *.js glob — that would also grab sw.js, the service
# worker, which the extension doesn't use).
cp dist/ocean-surface-ui.js     "$DIST/ocean-surface-ui.js"
cp dist/ocean-surface-ui_bg.wasm "$DIST/ocean-surface-ui_bg.wasm"
cp dist/style.css               "$DIST/style.css"

# Carry over any static assets the UI references (icons, etc.).
for f in dist/*.png dist/*.webmanifest; do
  [ -e "$f" ] && cp "$f" "$DIST/" || true
done

echo "Extension built at $EXT/"
echo "Load it unpacked at chrome://extensions, or copy to the daemon's"
echo "extension dir so it auto-loads with Ocean's Chrome:"
echo "  cp -r $EXT \"\${XDG_CONFIG_HOME:-\$HOME/.config}/ocean/chrome-extension\""

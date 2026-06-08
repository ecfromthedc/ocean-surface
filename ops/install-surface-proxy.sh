#!/usr/bin/env bash
# Install + supervise the Ocean Surface proxy under launchd (OCEAN-161, SEV1).
#
# Idempotent. Safe to re-run after a pull/rebuild. What it does:
#   1. Builds the proxy binary (release) and ensures a valid wasm bundle exists.
#   2. Copies the LaunchAgent plist into ~/Library/LaunchAgents/.
#   3. Bootstraps + enables + kickstarts the job in the per-user GUI domain.
#
# This DOES touch the live launchd on this box — run it intentionally.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="dev.risingtides.ocean-surface-proxy"
PLIST_SRC="$REPO/deploy/$LABEL.plist"
PLIST_DST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"

export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"

echo "==> [1/3] building proxy + wasm bundle (release)"
# Build the binary the service runs.
( cd "$REPO" && cargo build -p ocean-surface-proxy --release )
# Ensure a servable bundle exists. If trunk is available and no bundle is present,
# build it; otherwise assume run-surface.sh / CI already produced dist/.
shopt -s nullglob
wasm_files=( "$REPO"/dist/*_bg.wasm )
shopt -u nullglob
if (( ${#wasm_files[@]} == 0 )); then
  if command -v trunk >/dev/null 2>&1; then
    echo "    no dist/*_bg.wasm — running 'trunk build --release'"
    ( cd "$REPO" && trunk build --release )
  else
    echo "FATAL: no dist/*_bg.wasm and 'trunk' not on PATH. Build the bundle first." >&2
    exit 1
  fi
fi

echo "==> [2/3] installing plist -> $PLIST_DST"
mkdir -p "$HOME/Library/LaunchAgents"
cp "$PLIST_SRC" "$PLIST_DST"
plutil -lint "$PLIST_DST"

echo "==> [3/3] (re)bootstrapping launchd job $LABEL in $DOMAIN"
# Tear down any previous instance so this is a clean (re)install.
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$PLIST_DST"
launchctl enable "$DOMAIN/$LABEL"
# Force an immediate (re)start so we don't wait for the next event.
launchctl kickstart -k "$DOMAIN/$LABEL"

echo
echo "==> done. status:"
launchctl print "$DOMAIN/$LABEL" 2>/dev/null | grep -E 'state|pid|program|path =' | sed 's/^/    /' || true
echo
echo "    Check it's listening:   lsof -nP -iTCP:8790 -sTCP:LISTEN"
echo "    Tail logs:              tail -f /private/tmp/ocean-surface-proxy.log"

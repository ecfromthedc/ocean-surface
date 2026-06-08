#!/usr/bin/env bash
# Stop + unsupervise the Ocean Surface proxy (reverse of install-surface-proxy.sh).
# Boots the job out of launchd and removes the installed plist. Leaves the repo,
# the built binary, and the dist/ bundle untouched.
set -euo pipefail

LABEL="dev.risingtides.ocean-surface-proxy"
PLIST_DST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"

echo "==> booting out $DOMAIN/$LABEL"
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || echo "    (job was not loaded)"

echo "==> removing $PLIST_DST"
rm -f "$PLIST_DST"

echo "==> done. The proxy is no longer supervised."
echo "    Verify it stopped:  lsof -nP -iTCP:8790 -sTCP:LISTEN   (should print nothing)"

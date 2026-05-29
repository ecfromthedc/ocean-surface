#!/usr/bin/env bash
# Launch Ocean Surface: build the wasm bundle, then serve it + the xAI
# proxy from one binary. Point your browser (or phone) at the printed URL.
#
# Auth is preconfigured via the environment — export your key before running:
#   export XAI_API_KEY=sk-...           # enables voice (STT/TTS)
# Optional overrides:
#   export OCEAN_DAEMON_URL=http://<host>:4780   # default 127.0.0.1:4780
#   export OCEAN_VOICE_PROFILE=leo                # xAI voice
#   export OCEAN_SURFACE_BIND=0.0.0.0:8790        # LAN/tailnet; default below
set -euo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"
export RUSTUP_HOME="$HOME/.rustup"
export CARGO_HOME="$HOME/.cargo"

BIND="${OCEAN_SURFACE_BIND:-0.0.0.0:8790}"
DAEMON="${OCEAN_DAEMON_URL:-http://127.0.0.1:4780}"

echo "==> building wasm bundle (trunk)"
( cd "$REPO" && trunk build --release )

echo "==> auth: XAI_API_KEY is ${XAI_API_KEY:+SET}${XAI_API_KEY:-UNSET (voice disabled until you export it)}"
echo "==> daemon: $DAEMON"
echo "==> serving on http://$BIND   (open this in your browser)"

exec env \
  OCEAN_SURFACE_BIND="$BIND" \
  OCEAN_SURFACE_DIST="$REPO/dist" \
  OCEAN_DAEMON_URL="$DAEMON" \
  "$REPO/target/release/ocean-surface-proxy"

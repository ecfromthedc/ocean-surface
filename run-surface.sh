#!/usr/bin/env bash
# Launch Ocean Surface: build the wasm bundle, then serve it + the xAI
# proxy from one binary. Point your browser (or phone) at the printed URL.
#
# Auth (voice STT/TTS) is preconfigured. The proxy resolves the xAI key from,
# in order: env XAI_API_KEY → ~/.config/ocean-surface/xai.key → pi settings.
# The persistent, recommended way (set once, works every launch):
#   mkdir -p ~/.config/ocean-surface && printf '%s' "sk-..." > ~/.config/ocean-surface/xai.key
# Or per-run via env:
#   export XAI_API_KEY=sk-...
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

# Build-guard (OCEAN-121): trunk can report "✅ success" yet leave a corrupt,
# all-zero wasm in dist/ (e.g. when its wasm-opt step fails). Shipping that
# serves a blank page. Assert every dist *_bg.wasm starts with the WebAssembly
# magic word "00 61 73 6d" and fail loudly otherwise — never ship a dead bundle.
echo "==> verifying dist wasm magic word"
shopt -s nullglob
wasm_files=( "$REPO"/dist/*_bg.wasm )
shopt -u nullglob
if (( ${#wasm_files[@]} == 0 )); then
  echo "FATAL: no dist/*_bg.wasm produced by trunk build --release" >&2
  exit 1
fi
for w in "${wasm_files[@]}"; do
  # Read the first 4 bytes as hex; must equal the wasm magic "0061736d".
  magic="$(head -c 4 "$w" | xxd -p)"
  if [[ "$magic" != "0061736d" ]]; then
    echo "FATAL: $w is corrupt — first 4 bytes are '$magic', expected '0061736d' (wasm magic)." >&2
    echo "       trunk likely failed wasm-opt while reporting success; refusing to serve a blank bundle." >&2
    exit 1
  fi
  echo "    ok: $(basename "$w") ($magic)"
done

echo "==> auth: XAI_API_KEY is ${XAI_API_KEY:+SET}${XAI_API_KEY:-UNSET (voice disabled until you export it)}"
echo "==> daemon: $DAEMON"
echo "==> serving on http://$BIND   (open this in your browser)"

exec env \
  OCEAN_SURFACE_BIND="$BIND" \
  OCEAN_SURFACE_DIST="$REPO/dist" \
  OCEAN_DAEMON_URL="$DAEMON" \
  "$REPO/target/release/ocean-surface-proxy"

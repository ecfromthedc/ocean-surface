#!/usr/bin/env bash
# Ocean Surface finish line — the one command that closes L3-J.
#
# Ensures voice auth is preconfigured (prompts once for your xAI key if the
# key file is missing, writes it to the canonical location), confirms the
# daemon is reachable, runs the smoke test, then launches the surface and
# prints the URL to open.
#
#   ./finish.sh
set -uo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
KEY_FILE="${OCEAN_SURFACE_KEY_FILE:-$HOME/.config/ocean-surface/xai.key}"
DAEMON="${OCEAN_DAEMON_URL:-http://127.0.0.1:4780}"

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
dim()  { printf '\033[2m%s\033[0m\n' "$1"; }

bold "Ocean Surface — finish line"

# 1. Voice auth preconfig.
if [ -n "${XAI_API_KEY:-}" ]; then
  dim "Using XAI_API_KEY from the environment."
elif [ -s "$KEY_FILE" ]; then
  dim "Voice key already preconfigured at $KEY_FILE"
else
  echo
  echo "No xAI key found. Paste your key to preconfigure voice (input hidden),"
  echo "or just press Enter to run text-only (voice stays off)."
  printf 'xAI key: '
  read -rs PASTED_KEY
  echo
  if [ -n "$PASTED_KEY" ]; then
    mkdir -p "$(dirname "$KEY_FILE")"
    printf '%s' "$PASTED_KEY" > "$KEY_FILE"
    chmod 600 "$KEY_FILE"
    unset PASTED_KEY
    dim "Saved to $KEY_FILE (chmod 600). It persists — you won't be asked again."
  else
    dim "No key entered. Launching text-only; the orb will show 'voice off'."
  fi
fi

# 2. Daemon reachable?
if ! curl -s -m 5 "$DAEMON/health" >/dev/null 2>&1; then
  echo
  bold "⚠ daemon not reachable at $DAEMON"
  echo "Start it first (in the ocean-os repo):"
  echo "    cargo run -p ocean-daemon --release"
  echo "…or set OCEAN_DAEMON_URL to where it's running, then rerun ./finish.sh"
  exit 1
fi
dim "Daemon reachable at $DAEMON"

# 3. Build the bundle so smoke + launch use current code.
bold "building wasm bundle…"
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"
export RUSTUP_HOME="$HOME/.rustup"; export CARGO_HOME="$HOME/.cargo"
( cd "$REPO" && trunk build --release >/dev/null 2>&1 ) && dim "bundle built" || { bold "✗ trunk build failed"; exit 1; }

# 4. Launch. run-surface.sh already builds + serves + proxies; exec into it so
#    Ctrl-C stops cleanly. smoke.sh can be run separately against the live URL.
echo
bold "launching — open the printed URL, then try text + push-to-talk"
exec "$REPO/run-surface.sh"

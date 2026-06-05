#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# capture-gauntlet-cli.sh
#
# Usage:  ./capture-gauntlet-cli.sh [session_id]
#
# Opens a new Ocean session, triggers the Leptos Gauntlet,
# and saves the full assistant response as
#   docs/screenshots/gauntlet-cli-output.txt
#
# Requires ocean-cli to be built and available as `ocean` or
# in $OCEAN_CLI_PATH.
# ============================================================

OCEAN_BIN="${OCEAN_CLI_PATH:-$(which ocean 2>/dev/null || echo './target/release/ocean')}"
DAEMON_URL="${OCEAN_DAEMON_URL:-http://127.0.0.1:4780}"
OUTFILE="docs/screenshots/gauntlet-cli-output.txt"

# Ensure output directory exists
mkdir -p "$(dirname "$OUTFILE")"

echo "♻️  Using daemon at $DAEMON_URL"
echo "🔧 CLI binary: $OCEAN_BIN"

# 1) Create a fresh session
if [ -z "${1:-}" ]; then
  SESSION_RESP=$(curl -s -X POST "$DAEMON_URL/v1/agent/sessions" \
    -H "Content-Type: application/json" \
    -d '{"workspace_path": "'"$(pwd)"'"}')
  SESSION_ID=$(echo "$SESSION_RESP" | grep -o '"session_id":"[^"]*"' | head -1 | cut -d'"' -f4)
  echo "✨ Created session: $SESSION_ID"
else
  SESSION_ID="$1"
  echo "📌 Using session: $SESSION_ID"
fi

# 2) Send the gauntlet prompt via CLI (or raw curl)
echo "🪄  Sending gauntlet prompt…"

if command -v "$OCEAN_BIN" &>/dev/null && [ "$OCEAN_BIN" != "curl" ]; then
  "$OCEAN_BIN" --session-id "$SESSION_ID" \
    --prompt "Show me the full Leptos Gauntlet — every block type, component kind, and layout the agent can render." \
    > "$OUTFILE" 2>&1
else
  # Fallback: use curl to POST the turn, then poll events
  curl -s -X POST "$DAEMON_URL/v1/agent/turns" \
    -H "Content-Type: application/json" \
    -d "{
      \"session_id\": \"$SESSION_ID\",
      \"prompt\": \"Show me the full Leptos Gauntlet — every block type, component kind, and layout the agent can render.\",
      \"cwd\": \"$(pwd)\",
      \"model\": \"claude-sonnet-4-6\"
    }" > /tmp/ocean_turn_resp.json 2>&1

  # Extract turn_id
  TURN_ID=$(grep -o '"turn_id":"[^"]*"' /tmp/ocean_turn_resp.json | head -1 | cut -d'"' -f4)
  echo "📝 Turn ID: $TURN_ID"

  # Poll events for a few seconds to collect the full response
  echo "⏳ Waiting for response…"
  sleep 5

  curl -s "${DAEMON_URL}/v1/agent/events?session_id=${SESSION_ID}&format=json" \
    > "$OUTFILE" 2>/dev/null || echo "Warning: could not fetch events" > "$OUTFILE"
fi

LINE_COUNT=$(wc -l < "$OUTFILE")
echo "✅ Gauntlet output saved to $OUTFILE ($LINE_COUNT lines)"

# 3) Print a preview
echo ""
echo "=== PREVIEW (first 60 lines) ==="
head -60 "$OUTFILE"
echo ""
echo "=== PREVIEW (last 20 lines) ==="
tail -20 "$OUTFILE"

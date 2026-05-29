#!/usr/bin/env bash
# Ocean Surface end-to-end smoke test.
# Exercises every wired path so you get a one-command pass/fail before
# opening the browser. Voice checks only run when a key is preconfigured.
#
#   ./smoke.sh                 # uses defaults below
#   PROXY=http://mac:8790 DAEMON=http://mac:4780 ./smoke.sh
set -uo pipefail

PROXY="${PROXY:-http://127.0.0.1:8790}"
DAEMON="${DAEMON:-http://127.0.0.1:4780}"

pass=0; fail=0
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=$((fail+1)); }
info() { printf '  \033[2m·\033[0m %s\n' "$1"; }
hdr()  { printf '\n\033[1m%s\033[0m\n' "$1"; }

hdr "1. proxy reachable"
PH="$(curl -s -m 5 "$PROXY/health" 2>/dev/null)"
if echo "$PH" | grep -q '"ok":true'; then ok "GET /health"; else bad "GET /health → $PH"; fi

hdr "2. zero-config bootstrap"
CFG="$(curl -s -m 5 "$PROXY/api/config" 2>/dev/null)"
if echo "$CFG" | grep -q '"daemon_url"'; then ok "GET /api/config served"; info "$CFG"; else bad "GET /api/config → $CFG"; fi
HAS_AUTH="$(echo "$CFG" | grep -o '"has_auth":[a-z]*' | cut -d: -f2)"
if [ "$HAS_AUTH" = "true" ]; then ok "auth preconfigured (xAI key resolved)"; else info "auth NOT configured — voice checks will be skipped (drop key in ~/.config/ocean-surface/xai.key)"; fi

hdr "3. daemon chat round-trip (SSE)"
SSE_LOG="$(mktemp)"
curl -sN -m 20 "$DAEMON/v1/agent/events" > "$SSE_LOG" 2>&1 &
SSE_PID=$!
sleep 1
TURN="$(curl -s -m 20 -X POST "$DAEMON/v1/agent/turns" -H 'Content-Type: application/json' \
  -d '{"prompt":"reply with exactly: smoke ok","cwd":"/tmp"}' 2>/dev/null)"
sleep 8
kill "$SSE_PID" 2>/dev/null
if echo "$TURN" | grep -q '"ok":true'; then ok "POST /v1/agent/turns accepted"; else bad "turn POST → $TURN"; fi
if grep -q 'assistant_text_delta' "$SSE_LOG"; then
  REPLY="$(grep assistant_text_delta "$SSE_LOG" | sed 's/^data: //' | python3 -c "import sys,json;print(''.join(json.loads(l).get('delta','') for l in sys.stdin if l.strip().startswith('{')))" 2>/dev/null)"
  ok "assistant streamed deltas"; info "reply: ${REPLY:0:60}"
else bad "no assistant_text_delta on SSE stream"; fi
grep -q 'turn_finished' "$SSE_LOG" && ok "turn_finished seen" || bad "no turn_finished"
rm -f "$SSE_LOG"

if [ "$HAS_AUTH" = "true" ]; then
  hdr "4. live TTS (xAI grok-tts)"
  TTS_OUT="$(mktemp).mp3"
  TCODE="$(curl -s -m 30 -o "$TTS_OUT" -w '%{http_code}' -X POST "$PROXY/api/tts" \
    -H 'Content-Type: application/json' -d '{"text":"smoke test"}' 2>/dev/null)"
  SIZE="$(wc -c < "$TTS_OUT" 2>/dev/null | tr -d ' ')"
  if [ "$TCODE" = "200" ] && [ "${SIZE:-0}" -gt 500 ]; then ok "POST /api/tts → ${SIZE} bytes mp3"; else bad "tts http $TCODE size ${SIZE:-0}"; fi
  rm -f "$TTS_OUT"

  hdr "5. live STT (xAI grok-stt)"
  if command -v ffmpeg >/dev/null 2>&1; then
    CLIP="$(mktemp).webm"
    ffmpeg -f lavfi -i "sine=frequency=440:duration=1" -ac 1 -c:a libopus "$CLIP" -y >/dev/null 2>&1
    SRESP="$(curl -s -m 30 -X POST "$PROXY/api/stt" --data-binary "@$CLIP" -H 'Content-Type: audio/webm' 2>/dev/null)"
    if echo "$SRESP" | grep -q '"ok":true'; then ok "POST /api/stt accepted (round-trip to xAI)"; info "$SRESP"; else bad "stt → $SRESP"; fi
    rm -f "$CLIP"
  else info "ffmpeg not found — skipping synthetic STT clip; test in browser with a real mic"; fi
else
  hdr "4-5. voice checks SKIPPED (no key)"
fi

hdr "result"
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] && { printf '  \033[32mSMOKE OK — open %s in a browser for the final UI/voice check\033[0m\n' "$PROXY"; exit 0; } || exit 1

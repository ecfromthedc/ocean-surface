// ocean-client.mjs
//
// The single point of contact between the ocean-voice surface and the Rust
// Ocean runtime (ocean-daemon). ocean-voice is a thin steering client: it owns
// no agent logic, no provider keys, and no sessions of its own. It only speaks
// the daemon's product-shaped agent API:
//
//   POST /v1/agent/turns   { prompt, cwd, session_id?, guidance? }
//   GET  /v1/agent/events  (SSE stream of AgentTurnEvent)
//
// The turn POST blocks until the turn finishes and returns metadata only
// (turn_id, session_id, status) — the assistant's reply text arrives solely as
// `assistant_text_delta` events on the SSE stream, so we always consume the
// stream to recover the answer and to drive live status speech on tool calls.

import { setTimeout as delay } from 'node:timers/promises';

export const OCEAN_DAEMON_URL = (process.env.OCEAN_DAEMON_URL || 'http://127.0.0.1:4780').replace(/\/$/, '');

export async function oceanHealth(url = OCEAN_DAEMON_URL) {
  try {
    const r = await fetch(`${url}/health`, { signal: AbortSignal.timeout(4000) });
    if (!r.ok) return { ok: false, status: r.status };
    return await r.json();
  } catch (error) {
    return { ok: false, error: String(error?.message || error) };
  }
}

// Parse a fetch Response body as a Server-Sent Events stream, yielding the
// decoded JSON payload of each `data:` block.
async function* readSse(response, signal) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  while (!signal.aborted) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const blocks = buffer.split('\n\n');
    buffer = blocks.pop() || '';
    for (const block of blocks) {
      const dataLines = [];
      for (const line of block.split('\n')) {
        if (line.startsWith('data:')) dataLines.push(line.slice(5).trim());
      }
      if (dataLines.length === 0) continue;
      try {
        yield JSON.parse(dataLines.join('\n'));
      } catch {
        // ignore keep-alives and non-JSON frames
      }
    }
  }
}

/**
 * Run one agent turn against the Ocean runtime.
 *
 * @param {object} opts
 * @param {string} opts.prompt        The instruction to send.
 * @param {string} opts.cwd           Working directory for the turn (required by the daemon).
 * @param {?string} opts.sessionId    Existing session to continue, or null for a fresh one.
 * @param {?string[]} opts.guidance   Optional guidance hints.
 * @param {string} opts.clientType    Surface identity reported to the daemon so the
 *                                     agent personalizes for this surface (OCEAN-27).
 * @param {string} opts.url           Daemon base URL.
 * @param {number} opts.timeoutMs     Abort the turn POST after this long.
 * @param {(event:object)=>void} opts.onEvent  Fires live for each AgentTurnEvent.
 * @returns {Promise<{ok:boolean, text:string, error:?string, sessionId:?string, turnId:?string, status:string}>}
 */
export async function runOceanTurn({
  prompt,
  cwd,
  sessionId = null,
  guidance = null,
  clientType = 'surface-voice',
  url = OCEAN_DAEMON_URL,
  timeoutMs = Number(process.env.OCEAN_PROMPT_TIMEOUT_MS || 300_000),
  onEvent = () => {},
  // Internal: set when this call is the post-recovery retry, so a second
  // "session not found" can't loop. Callers never pass this (OCEAN-37).
  _isRetry = false,
}) {
  const eventsAc = new AbortController();
  const events = []; // every AgentTurnEvent seen during this turn
  let liveSessionId = sessionId;
  let liveTurnId = null;
  // Set when the daemon rejects our session_id as stale (e.g. it restarted and
  // dropped in-memory sessions). We drop the id and retry once with an implicit
  // fresh session after this call's SSE subscription is torn down (OCEAN-37) —
  // mirrors the strict-resume recovery in ocean-surface-ui/src/daemon.rs.
  let staleSessionRetry = false;

  // Background consumer of the daemon event stream. Scope to our session when
  // we already have one (continuing a session) so we never receive another
  // surface's events. For a fresh session the id doesn't exist yet, so we opt
  // into the firehose with `?all=1` to catch our own `session_created` — safe
  // here because ocean-voice runs turns single-flight, so any event seen during
  // this call belongs to this turn. The daemon omits session-bearing events
  // entirely without one of these params, so a bare URL would receive nothing.
  const eventsUrl = sessionId
    ? `${url}/v1/agent/events?session_id=${encodeURIComponent(sessionId)}`
    : `${url}/v1/agent/events?all=1`;
  const sseDone = (async () => {
    try {
      const resp = await fetch(eventsUrl, { signal: eventsAc.signal });
      if (!resp.ok || !resp.body) return;
      for await (const evt of readSse(resp, eventsAc.signal)) {
        events.push(evt);
        if (evt.type === 'session_created' && evt.session_id) liveSessionId = evt.session_id;
        if (evt.type === 'turn_started' && evt.turn_id) liveTurnId = evt.turn_id;
        try { onEvent(evt); } catch { /* callback errors must not break the stream */ }
      }
    } catch (error) {
      if (!eventsAc.signal.aborted) console.error('[ocean-client] event stream error', error);
    }
  })();

  // The SSE subscription must be torn down on every exit path — including a
  // failed/timed-out turn POST — or the daemon connection and its callback leak.
  try {
    // Small head start so the subscription is live before the turn emits events.
    await delay(120);

    const turnAc = new AbortController();
    const timer = setTimeout(() => turnAc.abort(), timeoutMs);
    let response;
    try {
      const body = {
        prompt,
        cwd,
        // Report the surface identity so the daemon applies voice-surface agent
        // personalization to this turn (OCEAN-27). Previously omitted, so voice
        // turns arrived with client_type: null and got no personalization.
        ...(clientType ? { client_type: clientType } : {}),
        ...(sessionId ? { session_id: sessionId } : {}),
        ...(guidance ? { guidance } : {}),
      };
      const r = await fetch(`${url}/v1/agent/turns`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
        signal: turnAc.signal,
      });
      const raw = await r.text();
      try { response = raw ? JSON.parse(raw) : {}; } catch { response = {}; }
      if (!r.ok) {
        // Strict-resume recovery: the daemon doesn't know our session_id (it
        // restarted, or the session was evicted). Drop the stale id and retry
        // once with an implicit fresh session rather than dead-ending the turn.
        // `_isRetry` guards against an infinite loop on a persistent failure.
        const errText = String(response?.error || raw || '');
        const staleSession =
          (r.status === 404 || /session not found/i.test(errText)) &&
          Boolean(sessionId) &&
          !_isRetry;
        if (staleSession) {
          staleSessionRetry = true;
        } else {
          throw new Error(`ocean-daemon ${r.status}: ${response?.error || raw}`);
        }
      }
    } finally {
      clearTimeout(timer);
    }

    // Stale-session recovery hands control back to a fresh run with no id.
    // Done outside the inner try so the failed turn's SSE is fully torn down
    // (in the outer finally) before the retry opens its own subscription.
    if (staleSessionRetry) {
      eventsAc.abort();
      await sseDone.catch(() => {});
      return runOceanTurn({
        prompt,
        cwd,
        sessionId: null,
        guidance,
        clientType,
        url,
        timeoutMs,
        onEvent,
        _isRetry: true,
      });
    }

    const turnId = response.turn_id || liveTurnId;
    const finalSessionId = response.session_id || liveSessionId;

    // NOTE: the daemon's /v1/agent/events stream regenerates a fresh turn_id on
    // every event, so it can't be matched against the response's turn_id. We run
    // turns single-flight with a fresh subscription per call, so every event in
    // this window belongs to this turn — accumulate them all rather than filter.

    // The turn POST may return just before the SSE flushes the final delta;
    // wait briefly for turn_finished before closing the stream.
    const deadline = Date.now() + 1500;
    while (Date.now() < deadline) {
      if (events.some((e) => e.type === 'turn_finished')) break;
      await delay(50);
    }

    const text = events
      .filter((e) => e.type === 'assistant_text_delta')
      .map((e) => e.delta)
      .join('')
      .trim();

    const status = response.status || 'completed';
    const ok = response.ok !== false && status !== 'failed';
    return {
      ok,
      text,
      error: response.error || null,
      sessionId: finalSessionId,
      turnId,
      status,
    };
  } finally {
    eventsAc.abort();
    await sseDone.catch(() => {});
  }
}

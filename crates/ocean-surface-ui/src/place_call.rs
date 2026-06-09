//! Place-call control — the outbound-call front door (OCEAN-261).
//!
//! The daemon registers `POST /v1/calls/place` (ocean-daemon `call_place`), but
//! nothing in the surface ever triggered it, so the outbound path was untestable
//! end-to-end without curl. This component is that trigger: a phone-number input
//! + a "Call" button that POSTs `{ "to": "<number>" }` to the daemon.
//!
//! ## What happens on each daemon response
//!
//! - **200 `{ ok: true, dialed, room, .. }`** — the dial was accepted. The daemon
//!   has already emitted `CallStarted` on its `/v1/events` control stream, which
//!   [`crate::call::CallPanel`] is independently subscribed to; that panel flips
//!   `active` and takes over the live view (transcript, summary, wake orb). So on
//!   success this control simply clears its input and steps back — it does NOT
//!   render the call itself, it just opens the door. (We surface a brief
//!   "dialing…" status so the click has immediate feedback before the first
//!   `call_started` frame lands.)
//! - **503 `{ blocked_on, missing, needed_env, note }`** — telephony isn't
//!   configured (no LiveKit Cloud + Twilio SIP trunk). This is NOT a code failure;
//!   the daemon names exactly which env vars are unset. We render a calm
//!   "telephony not set up" notice listing `needed_env` so the operator knows the
//!   one remaining step is provisioning creds, not a bug.
//! - **400 `{ error }`** — the number didn't normalize to E.164 server-side. We
//!   pre-validate client-side (see [`looks_like_e164`]) so this is rare, but we
//!   surface the daemon's message verbatim if it happens.
//! - **502 / other** — dial failed downstream; surface the error string.
//!
//! ## Privacy
//!
//! The typed number lives only in the `number` input signal and the request body.
//! It is never written to localStorage or any other persistence — unlike the
//! project/model selectors, a dialed number is transient by design.

use gloo_net::http::Request;
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::json;
use wasm_bindgen_futures::spawn_local;

use crate::daemon::Daemon;
use crate::icons::Phone;

/// The daemon's 503 body when telephony isn't provisioned (ocean-daemon
/// `call_place`). Only the fields the notice renders are modelled; `ok` and any
/// future fields are ignored by serde. `needed_env` is the authoritative list of
/// env vars the daemon needs to dial for real — we render it rather than
/// hard-coding our own copy, so the surface always tells the truth even if the
/// daemon's required-env set changes.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct TelephonyBlocked {
    /// Short reason, e.g. "telephony not configured".
    #[serde(default)]
    blocked_on: String,
    /// The subset actually missing right now (daemon-computed). May be empty if
    /// the daemon only reports the full `needed_env`.
    #[serde(default)]
    missing: Vec<String>,
    /// Every env var the outbound path needs (LIVEKIT_URL/_API_KEY/_API_SECRET +
    /// OCEAN_CALL_OUTBOUND_TRUNK + OCEAN_CALL_CALLER_NUMBER).
    #[serde(default)]
    needed_env: Vec<String>,
    /// Human note, e.g. "Requires a LiveKit Cloud account + a Twilio SIP trunk".
    #[serde(default)]
    note: String,
}

/// The daemon's plain error body (`{ ok: false, error }`) for 400/502/etc.
#[derive(Debug, Clone, Default, Deserialize)]
struct PlaceCallError {
    #[serde(default)]
    error: String,
}

/// What the control is currently doing. Drives the button's enabled/label state
/// and which feedback (if any) renders below the row.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// No call attempt in flight; the form is ready.
    Idle,
    /// A `POST /v1/calls/place` is in flight; the button is disabled and shows
    /// "dialing…". Cleared when the response lands (success steps back to Idle;
    /// failures move to `Error`/`Blocked`).
    Dialing,
    /// A non-config failure (400 bad number, 502 dial failed, network/decode).
    /// Holds the message to show. The form stays usable so the operator can fix
    /// the number and retry.
    Error(String),
    /// Telephony isn't configured (503). Holds the daemon's typed needed-env body
    /// so we can render exactly which creds are missing.
    Blocked(TelephonyBlocked),
}

/// Client-side E.164 gate, mirroring `ocean_call::normalize_e164` so we don't
/// POST a number the daemon will only 400 on. Same rules: strip to ASCII digits,
/// honor a leading `+`, assume US (+1) for a bare 10-digit number, treat a
/// leading-1 11-digit number as already-international, and require the final
/// digit count to be 8..=15.
///
/// Returns the normalized `+<digits>` string when valid, else `None`. We keep
/// this in lockstep with the daemon (the daemon re-validates server-side — this
/// is purely to give instant feedback and avoid a doomed round-trip).
fn looks_like_e164(input: &str) -> Option<String> {
    let had_plus = input.trim_start().starts_with('+');
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let normalized = if had_plus {
        format!("+{digits}")
    } else if digits.len() == 10 {
        // Bare US number → assume +1.
        format!("+1{digits}")
    } else if digits.len() == 11 && digits.starts_with('1') {
        format!("+{digits}")
    } else {
        format!("+{digits}")
    };
    let n_digits = normalized.len() - 1;
    if (8..=15).contains(&n_digits) {
        Some(normalized)
    } else {
        None
    }
}

/// The place-call control. Mount it next to [`crate::call::CallPanel`] — this is
/// the trigger, that panel is the live view the trigger spawns.
#[component]
pub fn PlaceCallControl(daemon: Daemon) -> impl IntoView {
    // The number being typed. Transient — never persisted (see module docs).
    let number = RwSignal::new(String::new());
    let phase = RwSignal::new(Phase::Idle);

    // True when the current input would pass the E.164 gate. Gates the button so
    // the operator can't fire a doomed request, and shows a quiet hint when the
    // field has content that isn't yet a valid number.
    let is_valid = move || looks_like_e164(&number.get()).is_some();
    let has_input = move || !number.get().trim().is_empty();
    let dialing = move || phase.get() == Phase::Dialing;

    // The submit path: validate, then POST { to } to the daemon and route the
    // response into `phase`. On success we clear the field — the CallPanel takes
    // over via the daemon's `call_started` frame on `/v1/events`.
    let place = {
        let daemon = daemon.clone();
        move || {
            let Some(normalized) = looks_like_e164(&number.get_untracked()) else {
                phase.set(Phase::Error("Enter a valid phone number (E.164).".into()));
                return;
            };
            // Don't double-fire while a dial is already in flight.
            if phase.get_untracked() == Phase::Dialing {
                return;
            }
            phase.set(Phase::Dialing);

            let url = daemon.url.get_untracked();
            spawn_local(async move {
                let post_url = format!("{}/v1/calls/place", url.trim_end_matches('/'));
                let body = json!({ "to": normalized });
                let req = match Request::post(&post_url)
                    .header("content-type", "application/json")
                    .json(&body)
                {
                    Ok(req) => req,
                    Err(err) => {
                        phase.set(Phase::Error(format!("Couldn't encode request: {err}")));
                        return;
                    }
                };
                match req.send().await {
                    Ok(resp) => {
                        let status = resp.status();
                        match status {
                            200 => {
                                // Dial accepted. The daemon has emitted
                                // CallStarted; CallPanel will flip live on its own
                                // SSE subscription. Clear the field and step back.
                                number.set(String::new());
                                phase.set(Phase::Idle);
                            }
                            503 => {
                                // Telephony not provisioned — render the daemon's
                                // typed needed-env body so the message is exact.
                                let blocked = resp
                                    .json::<TelephonyBlocked>()
                                    .await
                                    .unwrap_or_default();
                                phase.set(Phase::Blocked(blocked));
                            }
                            _ => {
                                // 400 (bad number), 502 (dial failed), or any
                                // other non-OK — surface the daemon's error text.
                                let msg = resp
                                    .json::<PlaceCallError>()
                                    .await
                                    .ok()
                                    .map(|e| e.error)
                                    .filter(|e| !e.is_empty())
                                    .unwrap_or_else(|| format!("call failed (HTTP {status})"));
                                phase.set(Phase::Error(msg));
                            }
                        }
                    }
                    Err(err) => {
                        phase.set(Phase::Error(format!("Couldn't reach the daemon: {err}")));
                    }
                }
            });
        }
    };

    // Wrap `place` so both the button click and the Enter key can call it. A
    // StoredValue lets the two closures share one non-Clone body.
    let place = StoredValue::new(place);

    // ----- derived render state for the feedback row -----------------------

    let error_msg = move || match phase.get() {
        Phase::Error(msg) => Some(msg),
        _ => None,
    };
    let blocked = move || match phase.get() {
        Phase::Blocked(b) => Some(b),
        _ => None,
    };
    // Show the "not yet valid" hint only once the operator has typed something
    // that isn't (yet) a number, and only when we're not already showing a
    // harder error/blocked notice.
    let show_invalid_hint = move || {
        has_input()
            && !is_valid()
            && error_msg().is_none()
            && blocked().is_none()
    };

    view! {
        <section class="ocean-place-call" aria-label="place a call">
            <div class="ocean-place-call__row">
                <span class="ocean-place-call__icon" aria-hidden="true">
                    <Phone />
                </span>
                <input
                    class="ocean-place-call__input"
                    type="tel"
                    inputmode="tel"
                    autocomplete="off"
                    placeholder="+1 (555) 123-4567"
                    aria-label="phone number to call"
                    prop:value=move || number.get()
                    on:input=move |ev| {
                        // Typing clears any stale error/blocked notice so the form
                        // feels live, not stuck on the last failure.
                        number.set(event_target_value(&ev));
                        if !matches!(phase.get_untracked(), Phase::Dialing) {
                            phase.set(Phase::Idle);
                        }
                    }
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            ev.prevent_default();
                            place.with_value(|p| p());
                        }
                    }
                    prop:disabled=dialing
                />
                <button
                    class="ocean-place-call__btn"
                    type="button"
                    title="Place an outbound call"
                    prop:disabled=move || dialing() || !is_valid()
                    on:click=move |_| place.with_value(|p| p())
                >
                    {move || if dialing() {
                        "dialing…".to_string()
                    } else {
                        "Call".to_string()
                    }}
                </button>
            </div>

            // Dialing status (OCEAN-284): a visible "dialing…" beat between the click
            // and the daemon's first `call_started` frame, so the outbound→live
            // handoff isn't a dead pause. The live CallPanel takes over the moment
            // that frame lands (this control steps back to Idle on the 200).
            <Show when=dialing fallback=|| ()>
                <p class="ocean-place-call__dialing" role="status">
                    <span class="ocean-place-call__dialing-dot"></span>
                    "Dialing — connecting the call…"
                </p>
            </Show>

            // Quiet inline hint while the typed value isn't a valid number yet.
            <Show when=show_invalid_hint fallback=|| ()>
                <p class="ocean-place-call__hint">
                    "Enter a phone number in E.164 form, e.g. +15551234567."
                </p>
            </Show>

            // Hard error (bad number rejected server-side, dial failed, network).
            <Show when=move || error_msg().is_some() fallback=|| ()>
                <p class="ocean-place-call__error" role="alert">
                    {move || error_msg().unwrap_or_default()}
                </p>
            </Show>

            // Telephony-not-configured notice (503). Calm, explanatory — names the
            // env the daemon still needs, straight from its typed response.
            <Show when=move || blocked().is_some() fallback=|| ()>
                {move || {
                    let b = blocked().unwrap_or_default();
                    let headline = if b.blocked_on.is_empty() {
                        "Telephony isn't set up yet.".to_string()
                    } else {
                        // e.g. "telephony not configured" → sentence-cased lead-in.
                        format!("Telephony isn't set up: {}.", b.blocked_on)
                    };
                    let note = if b.note.is_empty() {
                        "Outbound calling needs a LiveKit Cloud account plus a \
                         Twilio SIP trunk. Once those creds are set, this dials \
                         for real."
                            .to_string()
                    } else {
                        b.note.clone()
                    };
                    // Prefer the precise still-missing set if the daemon gave one;
                    // otherwise show the full required env list.
                    let env: Vec<String> = if b.missing.is_empty() {
                        b.needed_env.clone()
                    } else {
                        b.missing.clone()
                    };
                    view! {
                        <div class="ocean-place-call__blocked" role="status">
                            <p class="ocean-place-call__blocked-head">{headline}</p>
                            <p class="ocean-place-call__blocked-note">{note}</p>
                            <Show when={let env = env.clone(); move || !env.is_empty()} fallback=|| ()>
                                <ul class="ocean-place-call__env">
                                    <For
                                        each={let env = env.clone(); move || env.clone()}
                                        key=|v| v.clone()
                                        children=move |v| view! {
                                            <li class="ocean-place-call__env-var">{v}</li>
                                        }
                                    />
                                </ul>
                            </Show>
                        </div>
                    }
                }}
            </Show>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client E.164 gate must accept the same shapes the daemon's
    /// `normalize_e164` accepts (the tests there cover these exact inputs), so a
    /// number the operator can type is never blocked client-side only to be
    /// accepted server-side (or vice versa).
    #[test]
    fn accepts_the_formats_the_daemon_accepts() {
        assert_eq!(looks_like_e164("(703) 508-1859").as_deref(), Some("+17035081859"));
        assert_eq!(looks_like_e164("703-508-1859").as_deref(), Some("+17035081859"));
        assert_eq!(looks_like_e164("+1 703 508 1859").as_deref(), Some("+17035081859"));
        assert_eq!(looks_like_e164("17035081859").as_deref(), Some("+17035081859"));
        // A plausible international number (UK) with a leading +.
        assert_eq!(looks_like_e164("+44 20 7946 0958").as_deref(), Some("+442079460958"));
    }

    /// Garbage and out-of-range lengths are rejected before we ever POST.
    #[test]
    fn rejects_non_numbers_and_bad_lengths() {
        assert_eq!(looks_like_e164(""), None);
        assert_eq!(looks_like_e164("abc"), None);
        assert_eq!(looks_like_e164("   "), None);
        // 7 digits → below the E.164 floor of 8.
        assert_eq!(looks_like_e164("1234567"), None);
        // 16 digits → above the E.164 ceiling of 15.
        assert_eq!(looks_like_e164("+1234567890123456"), None);
    }

    /// The daemon's 503 body deserializes into the typed notice, including the
    /// full `needed_env` list the surface renders. Mirrors the exact JSON shape
    /// emitted by ocean-daemon `call_place` when `SipConfig::from_env` fails.
    #[test]
    fn telephony_blocked_body_deserializes() {
        let wire = r#"{
            "ok": false,
            "blocked_on": "telephony not configured",
            "missing": ["LIVEKIT_URL", "OCEAN_CALL_CALLER_NUMBER"],
            "needed_env": [
                "LIVEKIT_URL", "LIVEKIT_API_KEY", "LIVEKIT_API_SECRET",
                "OCEAN_CALL_OUTBOUND_TRUNK", "OCEAN_CALL_CALLER_NUMBER"
            ],
            "note": "Requires a LiveKit Cloud account + a Twilio SIP trunk (paid)."
        }"#;
        let b: TelephonyBlocked = serde_json::from_str(wire).expect("deserialize");
        assert_eq!(b.blocked_on, "telephony not configured");
        assert_eq!(b.missing.len(), 2);
        assert_eq!(b.needed_env.len(), 5);
        assert!(b.needed_env.contains(&"LIVEKIT_API_SECRET".to_string()));
        assert!(b.note.contains("Twilio"));
    }

    /// The plain error body (400/502) deserializes so we can surface the daemon's
    /// own message instead of a generic string.
    #[test]
    fn plain_error_body_deserializes() {
        let wire = r#"{ "ok": false, "error": "not a valid phone number: nope" }"#;
        let e: PlaceCallError = serde_json::from_str(wire).expect("deserialize");
        assert_eq!(e.error, "not a valid phone number: nope");
    }
}

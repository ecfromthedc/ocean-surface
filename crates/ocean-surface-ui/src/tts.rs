//! Text-to-speech playback of assistant replies.
//!
//! When an assistant turn finishes, POST `{text}` to `/api/tts`, receive mp3
//! bytes, wrap them in a Blob, `URL.createObjectURL` it, and play through an
//! `HtmlAudioElement`. A reactive `muted` signal gates the whole thing.

use gloo_net::http::Request;
use leptos::prelude::*;
use serde::Serialize;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Blob, BlobPropertyBag, HtmlAudioElement, Url};

#[derive(Serialize)]
struct TtsRequest<'a> {
    text: &'a str,
}

/// Speak `text` unless muted. No-ops on empty text.
pub fn speak(text: String, muted: RwSignal<bool>) {
    if muted.get_untracked() || text.trim().is_empty() {
        return;
    }
    spawn_local(async move {
        let body = TtsRequest { text: &text };
        let req = Request::post("/api/tts")
            .header("content-type", "application/json")
            .json(&body);
        let resp = match req {
            Ok(r) => r.send().await,
            Err(err) => {
                log::warn!("tts encode error: {err}");
                return;
            }
        };
        let resp = match resp {
            Ok(r) if r.ok() => r,
            Ok(r) => {
                log::warn!("tts http {}", r.status());
                return;
            }
            Err(err) => {
                log::warn!("tts request failed: {err}");
                return;
            }
        };
        let bytes = match resp.binary().await {
            Ok(b) => b,
            Err(err) => {
                log::warn!("tts body read failed: {err}");
                return;
            }
        };
        play_mp3(bytes);
    });
}

/// Build a Blob from mp3 bytes, object-URL it, and play. Revokes the URL when
/// playback ends so we don't leak object URLs across many replies.
fn play_mp3(bytes: Vec<u8>) {
    // Uint8Array → Blob([..], {type:"audio/mpeg"}).
    let array = js_sys::Uint8Array::from(bytes.as_slice());
    let parts = js_sys::Array::new();
    parts.push(&array.buffer());
    let bag = BlobPropertyBag::new();
    bag.set_type("audio/mpeg");
    let blob = match Blob::new_with_buffer_source_sequence_and_options(&parts, &bag) {
        Ok(b) => b,
        Err(_) => {
            log::warn!("tts: failed to build blob");
            return;
        }
    };
    let url = match Url::create_object_url_with_blob(&blob) {
        Ok(u) => u,
        Err(_) => {
            log::warn!("tts: failed to create object url");
            return;
        }
    };
    let audio = match HtmlAudioElement::new_with_src(&url) {
        Ok(a) => a,
        Err(_) => {
            let _ = Url::revoke_object_url(&url);
            return;
        }
    };

    // Revoke the object URL once playback finishes (one-shot).
    let cleanup_url = url.clone();
    let on_ended = Closure::once_into_js(move |_e: JsValue| {
        let _ = Url::revoke_object_url(&cleanup_url);
    });
    audio.set_onended(Some(on_ended.unchecked_ref()));

    // play() returns a promise that rejects if autoplay is blocked; await it
    // off-thread and swallow any error so it isn't an unhandled rejection.
    if let Ok(promise) = audio.play() {
        spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
}

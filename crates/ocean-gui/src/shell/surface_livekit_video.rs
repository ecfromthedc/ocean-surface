//! Native LiveKit video frame plumbing for the GPUI shell (OCEAN-97).
//!
//! OCEAN-94 wired the room connection, mic publish and presence roster but
//! deferred the heavy A/V work: subscribing to remote video tracks, decoding
//! their frames, and getting those pixels onto the GPUI render tree.
//!
//! This module owns the *remote video data path*. It converts a libwebrtc
//! `BoxVideoFrame` (which arrives as I420 / YUV) into a packed BGRA byte buffer
//! that can be wrapped in a `gpui::RenderImage` and drawn with `gpui::img(...)`.
//!
//! ## Why BGRA, not RGBA
//!
//! GPUI's image pipeline stores `RenderImage` frames in **BGRA** byte order:
//! its own PNG/JPEG decoder loads `RgbaImage` and then swaps R<->B before
//! constructing the `RenderImage` (see `gpui::elements::img`). When we build a
//! `RenderImage` ourselves we must therefore hand it BGRA bytes. libwebrtc's
//! `i420_to_bgra` does exactly that conversion in one pass, so we target it.
//!
//! ## Threading / cost
//!
//! Frame decode happens on the LiveKit client's tokio worker thread (off the
//! GPUI main thread). Only the latest decoded frame per participant is kept and
//! handed to the view via the existing event channel, so a slow main thread
//! drops stale frames instead of building an unbounded backlog. Constructing
//! the `RenderImage` (a main-thread-only `gpui` type) is left to the view.

/// A decoded remote video frame, ready to be wrapped in a `gpui::RenderImage`.
///
/// `bgra` is `width * height * 4` bytes in B, G, R, A order to match GPUI's
/// `RenderImage` byte layout. Produced on the LiveKit worker thread and moved
/// to the GPUI main thread for rendering.
#[derive(Clone, PartialEq, Eq)]
pub struct SurfaceVideoFrame {
    pub participant_identity: String,
    pub track_sid: String,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for SurfaceVideoFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SurfaceVideoFrame")
            .field("participant_identity", &self.participant_identity)
            .field("track_sid", &self.track_sid)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bgra_len", &self.bgra.len())
            .finish()
    }
}

impl SurfaceVideoFrame {
    #[must_use]
    pub fn expected_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// A frame is renderable when it has a non-zero size and its byte buffer
    /// matches the declared `width * height * 4` BGRA stride.
    #[must_use]
    pub fn is_renderable(&self) -> bool {
        self.width > 0 && self.height > 0 && self.bgra.len() == self.expected_len()
    }
}

// The decode path links native libwebrtc (`livekit::webrtc`), so it is only
// compiled with the `livekit` feature. Its sole caller — the per-track video
// decode task in `surface_livekit_session` — is gated the same way, so no
// stub is needed when the feature is off. The `SurfaceVideoFrame` value type
// above stays always-compiled for the shell's render path.
#[cfg(all(not(target_arch = "wasm32"), feature = "livekit"))]
mod native {
    use livekit::webrtc::native::yuv_helper;
    use livekit::webrtc::prelude::{VideoBuffer, VideoFrame};

    use super::SurfaceVideoFrame;

    /// Convert a libwebrtc video frame into a packed BGRA buffer.
    ///
    /// Only the buffer layouts whose pixel data is reachable through the
    /// *public* libwebrtc API are handled here: the internal `to_i420`/`to_argb`
    /// conversions live on a `pub(crate)` sealed trait we cannot call from this
    /// crate. In practice the standard native software decode path (VP8/VP9/H264
    /// → I420) and the hardware NV12 path cover the participants we care about.
    ///
    /// - **I420** (`as_i420`): converted via SIMD `i420_to_bgra` in one pass.
    /// - **NV12** (`as_nv12`): converted via `nv12_to_argb` (matches GPUI's
    ///   B,G,R,A byte order on little-endian targets).
    /// - Any other layout (e.g. native CVPixelBuffer/I422/I444/I010) returns
    ///   `None`; the tile simply keeps its previous frame. This is the
    ///   documented remainder — see module docs and OCEAN-97.
    pub fn decode_bgra<T>(
        participant_identity: &str,
        track_sid: &str,
        frame: &VideoFrame<T>,
    ) -> Option<SurfaceVideoFrame>
    where
        T: AsRef<dyn VideoBuffer>,
    {
        let buffer = frame.buffer.as_ref();
        let width = buffer.width();
        let height = buffer.height();
        if width == 0 || height == 0 {
            return None;
        }

        let dst_stride = width * 4;
        let mut bgra = vec![0u8; dst_stride as usize * height as usize];

        if let Some(i420) = buffer.as_i420() {
            let (stride_y, stride_u, stride_v) = i420.strides();
            let (data_y, data_u, data_v) = i420.data();
            yuv_helper::i420_to_bgra(
                data_y,
                stride_y,
                data_u,
                stride_u,
                data_v,
                stride_v,
                &mut bgra,
                dst_stride,
                width as i32,
                height as i32,
            );
        } else if let Some(nv12) = buffer.as_nv12() {
            let (stride_y, stride_uv) = nv12.strides();
            let (data_y, data_uv) = nv12.data();
            yuv_helper::nv12_to_argb(
                data_y,
                stride_y,
                data_uv,
                stride_uv,
                &mut bgra,
                dst_stride,
                width as i32,
                height as i32,
            );
        } else {
            return None;
        }

        Some(SurfaceVideoFrame {
            participant_identity: participant_identity.to_string(),
            track_sid: track_sid.to_string(),
            width,
            height,
            bgra,
        })
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "livekit"))]
pub use native::decode_bgra;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderable_requires_matching_bgra_stride() {
        let frame = SurfaceVideoFrame {
            participant_identity: "remote-a".to_string(),
            track_sid: "TR_video".to_string(),
            width: 2,
            height: 2,
            bgra: vec![0u8; 2 * 2 * 4],
        };
        assert_eq!(frame.expected_len(), 16);
        assert!(frame.is_renderable());
    }

    #[test]
    fn frame_with_wrong_buffer_len_is_not_renderable() {
        let frame = SurfaceVideoFrame {
            participant_identity: "remote-a".to_string(),
            track_sid: "TR_video".to_string(),
            width: 4,
            height: 4,
            bgra: vec![0u8; 8],
        };
        assert!(!frame.is_renderable());
    }

    #[test]
    fn zero_sized_frame_is_not_renderable() {
        let frame = SurfaceVideoFrame {
            participant_identity: "remote-a".to_string(),
            track_sid: "TR_video".to_string(),
            width: 0,
            height: 0,
            bgra: Vec::new(),
        };
        assert!(!frame.is_renderable());
    }
}

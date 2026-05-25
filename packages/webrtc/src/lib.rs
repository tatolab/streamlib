// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! WebRTC WHIP/WHEP transport processors for streamlib.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}
pub mod streaming;
pub mod webrtc_whep;
pub mod webrtc_whip;

pub use webrtc_whep::WebRtcWhepProcessor;
pub use webrtc_whip::WebRtcWhipProcessor;

pub use _generated_::{WebrtcWhepConfig, WebrtcWhipConfig};

streamlib_plugin_abi::export_plugin!(
    crate::WebRtcWhepProcessor::Processor,
    crate::WebRtcWhipProcessor::Processor,
);

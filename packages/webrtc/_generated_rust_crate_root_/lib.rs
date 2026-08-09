// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated crate root — the mechanical projection of this package's
//! `processors/` directory. Do not edit: it is rewritten before every
//! cargo invocation and is excluded from the packed `.slpkg`.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[path = "../processors/streaming/mod.rs"]
pub mod streaming;
#[path = "../processors/webrtc_whep.rs"]
pub mod webrtc_whep;
#[path = "../processors/webrtc_whip.rs"]
pub mod webrtc_whip;

streamlib_plugin_abi::export_plugin!(
    crate::webrtc_whep::WebRtcWhepProcessor::Processor,
    crate::webrtc_whip::WebRtcWhipProcessor::Processor,
);

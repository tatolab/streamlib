// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! WebRTC WHIP/WHEP transport processors for streamlib.

pub mod _generated_;
pub mod streaming;
pub mod webrtc_whep;
pub mod webrtc_whip;

pub use webrtc_whep::WebRtcWhepProcessor;
pub use webrtc_whip::WebRtcWhipProcessor;

pub use _generated_::{WebrtcWhepConfig, WebrtcWhipConfig};

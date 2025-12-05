// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC Implementation for macOS/iOS
//
// Provides WHIP (ingress) and WHEP (egress) signaling with WebRTC session management.

pub mod session;
pub mod whep;
pub mod whip;

pub use session::WebRtcSession;
pub use whep::{WhepClient, WhepConfig};
pub use whip::{WhipClient, WhipConfig};

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC Implementation for macOS/iOS
//
// Provides WHIP (ingress) and WHEP (egress) signaling with WebRTC session management.

pub mod session;
pub mod whep_client;
pub mod whip_client;

pub use session::WebRtcSession;
pub use whep_client::{WhepClient, WhepConfig};
pub use whip_client::{WhipClient, WhipConfig};

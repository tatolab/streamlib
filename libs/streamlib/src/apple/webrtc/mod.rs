// WebRTC Implementation for macOS/iOS
//
// Provides WHIP (ingress) and WHEP (egress) signaling with WebRTC session management.

pub mod whip;
pub mod whep;
pub mod session;

pub use whip::{WhipClient, WhipConfig};
pub use whep::{WhepClient, WhepConfig};
pub use session::{WebRtcSession, WebRtcSessionMode, SampleCallback};

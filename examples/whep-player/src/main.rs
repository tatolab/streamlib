// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! WHEP Player Example — receive H.264 + Opus from a WHEP endpoint and play it.
//!
//! # Registry-only migration status — DEFERRED, not in scope
//!
//! This example is intentionally a no-op at HEAD. Its real implementation
//! (preserved in git history before the registry-only migration) is
//! macOS-only: it decodes the received H.264 with VideoToolbox (macOS
//! hardware decode) and uses the deprecated compile-time typed-struct API.
//!
//! A Linux WHEP player is future work — it would wire
//! `WebRtcWhepProcessor → @tatolab/h264 (Vulkan) H264Decoder → @tatolab/display`
//! via runtime `add_module` / `ProcessorSpec` and load every package through
//! `Strategy::Registry` like the other examples. Restore the pipeline shape
//! from git history when building that.

fn main() {
    eprintln!(
        "whep-player is deferred and currently a no-op — WHEP playback decodes \
         with VideoToolbox (macOS-only) and has no Linux registry-only path \
         yet. See the module-level note in src/main.rs."
    );
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Streamlib XPC Broker Binary
//!
//! This is a minimal binary that serves as the XPC broker service.
//! It simply links streamlib, and the `#[ctor::ctor]` in subprocess_rhi
//! intercepts the `--subprocess-broker` flag and runs the broker.
//!
//! This binary should never actually reach main() when launched by launchd
//! because the ctor intercepts execution first.

// This import is required to ensure streamlib is linked and the ctor runs
use streamlib::BROKER_SERVICE_NAME;

fn main() {
    // This should never be reached when launched as broker.
    // If we get here, it means we weren't launched with --subprocess-broker.
    eprintln!("streamlib-broker: This binary should only be launched by launchd.");
    eprintln!("Usage: streamlib-broker --subprocess-broker");
    eprintln!("(Service name: {})", BROKER_SERVICE_NAME);
    std::process::exit(1);
}

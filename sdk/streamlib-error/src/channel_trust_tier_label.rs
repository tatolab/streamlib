// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// Trust tier of the iceoryx2 data channel a payload was refused on, named in
/// [`Error::PayloadExceedsChannelCeiling`](crate::Error::PayloadExceedsChannelCeiling).
/// Mirrors the engine's `iceoryx2::ChannelTrustTier` at the error boundary so
/// the ceiling error stays engine-free; the engine maps its own enum onto this
/// at the construction site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTrustTierLabel {
    /// In-process host-to-host channel.
    Trusted,
    /// Channel with a subprocess (Python / Deno) on either end.
    UntrustedSession,
}

impl std::fmt::Display for ChannelTrustTierLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trusted => f.write_str("trusted"),
            Self::UntrustedSession => f.write_str("untrusted-session"),
        }
    }
}

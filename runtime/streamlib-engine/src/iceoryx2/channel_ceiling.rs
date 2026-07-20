// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Node-level deployment override for the per-channel payload ceiling.
//!
//! The per-channel ceiling is structural by default — selected from the
//! channel's [`ChannelTrustTier`] via [`ChannelTrustTier::default_ceiling_bytes`].
//! An operator tunes it per deployment through a tier-scoped env var, read and
//! parsed here in the engine so [`streamlib_ipc_types`] stays logging-free.

use crate::iceoryx2::ChannelTrustTier;

/// Env var overriding the trusted-tier (in-process host-to-host) per-channel
/// payload ceiling, in bytes. An operator sets this per deployment; unset keeps
/// the built-in [`ChannelTrustTier::default_ceiling_bytes`].
pub const ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED: &str =
    "STREAMLIB_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED";

/// Env var overriding the untrusted-session-tier (subprocess-boundary)
/// per-channel payload ceiling, in bytes. An operator sets this per deployment;
/// raising it widens the subprocess trust boundary, so unset keeps the tighter
/// built-in [`ChannelTrustTier::default_ceiling_bytes`].
pub const ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION: &str =
    "STREAMLIB_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION";

/// Effective per-channel payload ceiling in bytes for `trust_tier`: the
/// operator's node-level deployment override (its tier's env var, a positive
/// byte count) when set and valid, else the tier's built-in default.
///
/// An unset var is byte-identical to the built-in default. A non-numeric, empty,
/// or zero value is a misconfiguration: it logs a `warn` and falls back to the
/// default — never panics. The default stays the safe built-in because raising
/// the untrusted-session cap widens the subprocess trust boundary.
pub fn effective_channel_ceiling_bytes(trust_tier: ChannelTrustTier) -> usize {
    let env_key = match trust_tier {
        ChannelTrustTier::Trusted => ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED,
        ChannelTrustTier::UntrustedSession => ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION,
    };
    let default_bytes = trust_tier.default_ceiling_bytes();
    let Ok(raw) = std::env::var(env_key) else {
        return default_bytes;
    };
    match raw.trim().parse::<usize>() {
        Ok(bytes) if bytes > 0 => bytes,
        _ => {
            tracing::warn!(
                env_var = env_key,
                value = %raw,
                tier = trust_tier.as_str(),
                default_ceiling_bytes = default_bytes,
                "ignoring invalid per-channel payload ceiling override; using the tier default"
            );
            default_bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine holds a single process-wide env; these overrides are read from
    /// it, so the cases that set/unset the same key run under one serialized test
    /// to keep the reads deterministic.
    #[test]
    fn env_override_replaces_tier_default_and_invalid_falls_back() {
        let trusted_default = ChannelTrustTier::Trusted.default_ceiling_bytes();
        let untrusted_default = ChannelTrustTier::UntrustedSession.default_ceiling_bytes();

        // Unset: byte-identical to the built-in tier default.
        unsafe {
            std::env::remove_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED);
            std::env::remove_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION);
        }
        assert_eq!(
            effective_channel_ceiling_bytes(ChannelTrustTier::Trusted),
            trusted_default
        );
        assert_eq!(
            effective_channel_ceiling_bytes(ChannelTrustTier::UntrustedSession),
            untrusted_default
        );

        // A valid positive override replaces the tier default.
        unsafe {
            std::env::set_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION, "1048576");
        }
        assert_eq!(
            effective_channel_ceiling_bytes(ChannelTrustTier::UntrustedSession),
            1_048_576,
            "a valid override must set the effective ceiling"
        );
        // The other tier is untouched by a sibling tier's override.
        assert_eq!(
            effective_channel_ceiling_bytes(ChannelTrustTier::Trusted),
            trusted_default
        );

        // Non-numeric, empty, and zero are misconfigurations that fall back.
        for bad in ["not-a-number", "", "0", "-5", "12mib"] {
            unsafe {
                std::env::set_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION, bad);
            }
            assert_eq!(
                effective_channel_ceiling_bytes(ChannelTrustTier::UntrustedSession),
                untrusted_default,
                "invalid override `{bad}` must fall back to the tier default"
            );
        }

        unsafe {
            std::env::remove_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION);
        }
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Node-level deployment override for the per-channel shared-memory chunk ceiling.
//!
//! The ceiling is structural by default — selected from the channel's
//! [`ChannelTrustTier`] via [`ChannelTrustTier::default_chunk_ceiling_bytes`]. An
//! operator tunes it per deployment through a tier-scoped env var, read and
//! parsed here in the engine: the override is a deployment concern, not part of
//! the wire contract [`streamlib_ipc_types`] carries between processes.
//!
//! Chunk, never payload: the ceiling bounds the shared-memory chunk a bag's
//! whole iceoryx2 sample occupies, and what one bag may carry is that less the
//! sample's own headers.

use crate::iceoryx2::ChannelTrustTier;

/// Env var overriding the trusted-tier (in-process host-to-host) per-channel
/// chunk ceiling, in bytes. An operator sets this per deployment; unset keeps
/// the built-in [`ChannelTrustTier::default_chunk_ceiling_bytes`].
pub const ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED: &str =
    "STREAMLIB_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED";

/// Env var overriding the untrusted-session-tier (subprocess-boundary)
/// per-channel chunk ceiling, in bytes. An operator sets this per deployment;
/// raising it widens the subprocess trust boundary, so unset keeps the tighter
/// built-in [`ChannelTrustTier::default_chunk_ceiling_bytes`].
pub const ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION: &str =
    "STREAMLIB_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION";

/// Effective per-channel shared-memory chunk ceiling in bytes for `trust_tier`:
/// the operator's node-level deployment override (its tier's env var, a positive
/// byte count) when set and valid, else the tier's built-in default.
///
/// An unset var is byte-identical to the built-in default. A non-numeric, empty,
/// or zero value is a misconfiguration: it logs a `warn` and falls back to the
/// default — never panics. The default stays the safe built-in because raising
/// the untrusted-session cap widens the subprocess trust boundary.
pub fn effective_channel_chunk_ceiling_bytes(trust_tier: ChannelTrustTier) -> usize {
    let env_key = match trust_tier {
        ChannelTrustTier::Trusted => ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED,
        ChannelTrustTier::UntrustedSession => ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION,
    };
    let default_bytes = trust_tier.default_chunk_ceiling_bytes();
    let Ok(raw) = std::env::var(env_key) else {
        return default_bytes;
    };
    match raw.trim().parse::<usize>() {
        Ok(bytes) if bytes > 0 => {
            let _ = warn_when_an_override_cannot_bound_the_shared_memory_chunk(
                env_key, trust_tier, bytes,
            );
            bytes
        }
        _ => {
            tracing::warn!(
                env_var = env_key,
                value = %raw,
                tier = trust_tier.as_str(),
                default_chunk_ceiling_bytes = default_bytes,
                "ignoring invalid per-channel chunk ceiling override; using the tier default"
            );
            default_bytes
        }
    }
}

/// Say so when an override cannot be the size of the chunk behind it, and
/// report whether it warned.
///
/// iceoryx2's pool allocator buckets a data segment at `next_power_of_two` of
/// the sample layout, so only a power-of-two ceiling is also the size of the
/// chunk a ceiling-sized bag takes. Both tier defaults are one. An override that
/// is not still bounds the payload — the ceiling's own job — but its chunk
/// rounds up past it, which is worth knowing on a host whose shared memory is
/// the reason the operator reached for the knob.
fn warn_when_an_override_cannot_bound_the_shared_memory_chunk(
    env_key: &str,
    trust_tier: ChannelTrustTier,
    override_bytes: usize,
) -> bool {
    if override_bytes.is_power_of_two() {
        return false;
    }
    tracing::warn!(
        env_var = env_key,
        tier = trust_tier.as_str(),
        chunk_ceiling_bytes = override_bytes,
        chunk_bytes_a_ceiling_sized_bag_takes =
            streamlib_ipc_types::chunk_bytes_a_ceiling_sized_bag_takes(override_bytes),
        "a per-channel ceiling that is not a power of two still bounds each bag, but iceoryx2 \
         rounds the shared-memory chunk behind it up past the ceiling; set a power of two to \
         make the ceiling the chunk size too"
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ceiling that is not a power of two cannot be the size of the chunk
    /// behind it, and an operator tuning shared memory is exactly who needs to
    /// know. The extremes go through the same door, because a diagnostic that
    /// panics the engine is worse than the misconfiguration it describes.
    #[test]
    fn an_override_that_cannot_be_its_own_chunk_size_says_so_and_a_power_of_two_does_not() {
        for power_of_two in [
            ChannelTrustTier::Trusted.default_chunk_ceiling_bytes(),
            ChannelTrustTier::UntrustedSession.default_chunk_ceiling_bytes(),
            1,
            4096,
        ] {
            assert!(
                !warn_when_an_override_cannot_bound_the_shared_memory_chunk(
                    "STREAMLIB_TEST",
                    ChannelTrustTier::Trusted,
                    power_of_two,
                ),
                "{power_of_two} is its own chunk size, so there is nothing to say"
            );
        }
        for not_a_power_of_two in [3usize, 100_000_000, usize::MAX] {
            assert!(
                warn_when_an_override_cannot_bound_the_shared_memory_chunk(
                    "STREAMLIB_TEST",
                    ChannelTrustTier::UntrustedSession,
                    not_a_power_of_two,
                ),
                "{not_a_power_of_two} takes a chunk larger than itself and must say so"
            );
        }
    }

    /// The engine holds a single process-wide env; these overrides are read from
    /// it, so the cases that set/unset the same key run under one serialized test
    /// to keep the reads deterministic.
    #[test]
    fn env_override_replaces_tier_default_and_invalid_falls_back() {
        let trusted_default = ChannelTrustTier::Trusted.default_chunk_ceiling_bytes();
        let untrusted_default = ChannelTrustTier::UntrustedSession.default_chunk_ceiling_bytes();

        // Unset: byte-identical to the built-in tier default.
        unsafe {
            std::env::remove_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_TRUSTED);
            std::env::remove_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION);
        }
        assert_eq!(
            effective_channel_chunk_ceiling_bytes(ChannelTrustTier::Trusted),
            trusted_default
        );
        assert_eq!(
            effective_channel_chunk_ceiling_bytes(ChannelTrustTier::UntrustedSession),
            untrusted_default
        );

        // A valid positive override replaces the tier default.
        unsafe {
            std::env::set_var(
                ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION,
                "1048576",
            );
        }
        assert_eq!(
            effective_channel_chunk_ceiling_bytes(ChannelTrustTier::UntrustedSession),
            1_048_576,
            "a valid override must set the effective ceiling"
        );
        // The other tier is untouched by a sibling tier's override.
        assert_eq!(
            effective_channel_chunk_ceiling_bytes(ChannelTrustTier::Trusted),
            trusted_default
        );

        // Non-numeric, empty, and zero are misconfigurations that fall back.
        for bad in ["not-a-number", "", "0", "-5", "12mib"] {
            unsafe {
                std::env::set_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION, bad);
            }
            assert_eq!(
                effective_channel_chunk_ceiling_bytes(ChannelTrustTier::UntrustedSession),
                untrusted_default,
                "invalid override `{bad}` must fall back to the tier default"
            );
        }

        unsafe {
            std::env::remove_var(ENV_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION);
        }
    }
}

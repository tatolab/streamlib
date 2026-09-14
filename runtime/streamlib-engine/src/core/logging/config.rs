// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Configuration for [`crate::core::logging::init`].

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use crate::core::logging::writer::JsonlSegmentRotationPolicy;
use crate::core::runtime::RuntimeUniqueId;

/// Environment variables read at [`init`](super::init) time.
pub mod env {
    /// Suppresses the pretty stdout mirror. JSONL is unaffected.
    pub const QUIET: &str = "STREAMLIB_QUIET";
    /// Batched JSONL flush size threshold in bytes.
    pub const BATCH_BYTES: &str = "STREAMLIB_LOG_BATCH_BYTES";
    /// Batched JSONL flush time threshold in milliseconds.
    pub const BATCH_MS: &str = "STREAMLIB_LOG_BATCH_MS";
    /// Bounded channel capacity (records). Drop-oldest when full.
    pub const CHANNEL_CAPACITY: &str = "STREAMLIB_LOG_CHANNEL_CAPACITY";
    /// Force `fdatasync` on every batch flush (default off).
    pub const FSYNC_ON_EVERY_BATCH: &str = "STREAMLIB_LOG_FSYNC_ON_EVERY_BATCH";
    /// Bytes after which the active JSONL segment rotates; `0` never rotates.
    pub const ROTATE_BYTES: &str = "STREAMLIB_LOG_ROTATE_BYTES";
    /// JSONL segments kept per runtime, the active one included; `0` keeps every one.
    pub const RETAIN_SEGMENTS: &str = "STREAMLIB_LOG_RETAIN_SEGMENTS";
}

const DEFAULT_BATCH_BYTES: usize = 64 * 1024;
const DEFAULT_BATCH_MS: u64 = 100;
const DEFAULT_CHANNEL_CAPACITY: usize = 65_536;
const DEFAULT_ROTATE_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_RETAIN_SEGMENTS: usize = 10;

/// Configuration passed to [`init`](super::init).
#[derive(Debug, Clone)]
pub struct StreamlibLoggingConfig {
    /// Service name used as the tracing `service.name` equivalent in the
    /// pretty layer's default formatter.
    pub service_name: String,

    /// Owning runtime's id. `None` disables JSONL writing (used by short-
    /// lived CLI invocations that only want env-filtered tracing).
    pub runtime_id: Option<Arc<RuntimeUniqueId>>,

    /// Enable the line-buffered pretty mirror. Overridden to
    /// `false` when `STREAMLIB_QUIET=1` is set.
    pub stdout: bool,

    /// Enable the batched JSONL file writer. Requires `runtime_id` to be
    /// set; silently disabled when `runtime_id == None`.
    pub jsonl: bool,

    /// Enable fd-level stdio interception. Default `false`; the main
    /// Rust runtime binary flips this to `true`. Wiring lands in #438.
    pub intercept_stdio: bool,

    /// Advanced tunables. Defaults below are used when fields are `None`;
    /// env vars override both.
    pub tunables: LoggingTunables,
}

/// Advanced tunables for the batched drain worker. Prefer environment
/// variables over construction-time values; env vars take precedence so
/// operators can tune without rebuilding.
#[derive(Debug, Clone, Default)]
pub struct LoggingTunables {
    pub batch_bytes: Option<usize>,
    pub batch_ms: Option<u64>,
    pub channel_capacity: Option<usize>,
    pub fsync_on_every_batch: Option<bool>,
    /// Bytes after which the active JSONL segment rotates; `Some(0)` never rotates.
    pub rotate_bytes: Option<u64>,
    /// JSONL segments kept per runtime, the active one included; `Some(0)` keeps every one.
    pub retain_segments: Option<usize>,
}

/// Effective tunables after env var resolution.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedTunables {
    pub batch_bytes: usize,
    pub batch_interval: Duration,
    pub channel_capacity: usize,
    pub fsync_on_every_batch: bool,
    pub segment_rotation: JsonlSegmentRotationPolicy,
}

impl ResolvedTunables {
    pub(crate) fn from_config(tunables: &LoggingTunables) -> Self {
        let batch_bytes = env_usize(env::BATCH_BYTES)
            .or(tunables.batch_bytes)
            .unwrap_or(DEFAULT_BATCH_BYTES);
        let batch_ms = env_u64(env::BATCH_MS)
            .or(tunables.batch_ms)
            .unwrap_or(DEFAULT_BATCH_MS);
        let channel_capacity = env_usize(env::CHANNEL_CAPACITY)
            .or(tunables.channel_capacity)
            .unwrap_or(DEFAULT_CHANNEL_CAPACITY);
        let fsync_on_every_batch = env_bool(env::FSYNC_ON_EVERY_BATCH)
            .or(tunables.fsync_on_every_batch)
            .unwrap_or(false);
        let rotate_bytes = env_u64(env::ROTATE_BYTES)
            .or(tunables.rotate_bytes)
            .unwrap_or(DEFAULT_ROTATE_BYTES);
        let retain_segments = env_usize(env::RETAIN_SEGMENTS)
            .or(tunables.retain_segments)
            .unwrap_or(DEFAULT_RETAIN_SEGMENTS);
        Self {
            batch_bytes,
            batch_interval: Duration::from_millis(batch_ms),
            channel_capacity,
            fsync_on_every_batch,
            segment_rotation: JsonlSegmentRotationPolicy {
                rotate_at_segment_bytes: (rotate_bytes > 0).then_some(rotate_bytes),
                retained_segment_count: NonZeroUsize::new(retain_segments),
            },
        }
    }
}

impl StreamlibLoggingConfig {
    /// Minimal config for short-lived CLI invocations: pretty stdout only,
    /// no JSONL, no interceptor.
    pub fn for_cli(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            runtime_id: None,
            stdout: true,
            jsonl: false,
            intercept_stdio: false,
            tunables: LoggingTunables::default(),
        }
    }

    /// Full config for a long-lived runtime: stdout + JSONL to disk,
    /// with fd-level stdio interception on by default so raw
    /// `println!` / `printf` output lands in the JSONL flagged as
    /// intercepted.
    pub fn for_runtime(service_name: impl Into<String>, runtime_id: Arc<RuntimeUniqueId>) -> Self {
        Self {
            service_name: service_name.into(),
            runtime_id: Some(runtime_id),
            stdout: true,
            jsonl: true,
            intercept_stdio: true,
            tunables: LoggingTunables::default(),
        }
    }

    /// `true` when the pretty stdout mirror should be installed, accounting
    /// for `STREAMLIB_QUIET`.
    pub(crate) fn effective_stdout(&self) -> bool {
        if !self.stdout {
            return false;
        }
        !env_bool(env::QUIET).unwrap_or(false)
    }
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.trim().parse::<usize>().ok()
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

fn env_bool(key: &str) -> Option<bool> {
    let raw = std::env::var(key).ok()?;
    match raw.trim() {
        "1" | "true" | "TRUE" | "True" | "yes" | "YES" => Some(true),
        "0" | "false" | "FALSE" | "False" | "no" | "NO" | "" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_tunable_env() {
        unsafe {
            std::env::remove_var(env::BATCH_BYTES);
            std::env::remove_var(env::BATCH_MS);
            std::env::remove_var(env::CHANNEL_CAPACITY);
            std::env::remove_var(env::FSYNC_ON_EVERY_BATCH);
            std::env::remove_var(env::ROTATE_BYTES);
            std::env::remove_var(env::RETAIN_SEGMENTS);
        }
    }

    #[test]
    #[serial]
    fn defaults_match_spec() {
        clear_tunable_env();
        let tunables = ResolvedTunables::from_config(&LoggingTunables::default());
        assert_eq!(tunables.batch_bytes, 64 * 1024);
        assert_eq!(tunables.batch_interval, Duration::from_millis(100));
        assert_eq!(tunables.channel_capacity, 65_536);
        assert!(!tunables.fsync_on_every_batch);
        assert_eq!(
            tunables.segment_rotation,
            JsonlSegmentRotationPolicy {
                rotate_at_segment_bytes: Some(100 * 1024 * 1024),
                retained_segment_count: NonZeroUsize::new(10),
            }
        );
    }

    #[test]
    #[serial]
    fn construction_tunables_apply_when_env_unset() {
        clear_tunable_env();
        let tunables = ResolvedTunables::from_config(&LoggingTunables {
            batch_bytes: Some(128),
            batch_ms: Some(5),
            channel_capacity: Some(16),
            fsync_on_every_batch: Some(true),
            rotate_bytes: Some(4096),
            retain_segments: Some(3),
        });
        assert_eq!(tunables.batch_bytes, 128);
        assert_eq!(tunables.batch_interval, Duration::from_millis(5));
        assert_eq!(tunables.channel_capacity, 16);
        assert!(tunables.fsync_on_every_batch);
        assert_eq!(
            tunables.segment_rotation,
            JsonlSegmentRotationPolicy {
                rotate_at_segment_bytes: Some(4096),
                retained_segment_count: NonZeroUsize::new(3),
            }
        );
    }

    #[test]
    #[serial]
    fn the_rotation_env_vars_outrank_the_construction_tunables() {
        clear_tunable_env();
        unsafe {
            std::env::set_var(env::ROTATE_BYTES, "2048");
            std::env::set_var(env::RETAIN_SEGMENTS, "7");
        }
        let tunables = ResolvedTunables::from_config(&LoggingTunables {
            rotate_bytes: Some(4096),
            retain_segments: Some(3),
            ..LoggingTunables::default()
        });
        clear_tunable_env();

        assert_eq!(
            tunables.segment_rotation,
            JsonlSegmentRotationPolicy {
                rotate_at_segment_bytes: Some(2048),
                retained_segment_count: NonZeroUsize::new(7),
            }
        );
    }

    #[test]
    #[serial]
    fn zero_turns_rotation_and_retention_off() {
        clear_tunable_env();
        unsafe {
            std::env::set_var(env::ROTATE_BYTES, "0");
            std::env::set_var(env::RETAIN_SEGMENTS, "0");
        }
        let tunables = ResolvedTunables::from_config(&LoggingTunables::default());
        clear_tunable_env();

        assert_eq!(
            tunables.segment_rotation,
            JsonlSegmentRotationPolicy::NEVER_ROTATE
        );
    }
}

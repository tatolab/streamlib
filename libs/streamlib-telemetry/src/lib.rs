// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Stand up the StreamLib runtime's tracing pipeline (stdout + optional file log).
//!
//! The runtime is an ephemeral log producer. Persistent on-disk logging in
//! JSONL form is filed separately under #430.

use std::path::PathBuf;

use anyhow::Result;
use tracing_subscriber::prelude::*;

/// Configuration for [`init_telemetry`].
pub struct TelemetryConfig {
    pub service_name: String,
    pub file_log_path: Option<PathBuf>,
    pub stdout_logging: bool,
}

/// Holds the file appender's worker guard so the background writer thread
/// stays alive for the duration of the process.
pub struct TelemetryGuard {
    _file_log_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Initialize the tracing subscriber. Safe to call multiple times — only the
/// first call installs the global subscriber; subsequent calls return a
/// no-op guard and the original subscriber stays live.
pub fn init_telemetry(config: TelemetryConfig) -> Result<TelemetryGuard> {
    static INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    if INITIALIZED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Ok(TelemetryGuard {
            _file_log_guard: None,
        });
    }

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".parse().unwrap());

    let stdout_layer = config.stdout_logging.then(tracing_subscriber::fmt::layer);

    let (file_layer, file_guard) = if let Some(ref log_path) = config.file_log_path {
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file_name = log_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}.log", config.service_name));
        let dir = log_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let file_appender = tracing_appender::rolling::never(dir, file_name);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false);
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    Ok(TelemetryGuard {
        _file_log_guard: file_guard,
    })
}

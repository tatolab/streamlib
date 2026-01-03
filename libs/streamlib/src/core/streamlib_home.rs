// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

/// Get the STREAMLIB_HOME directory path.
///
/// Resolution order:
/// 1. `STREAMLIB_HOME` environment variable (explicit override)
/// 2. `XDG_CONFIG_HOME/streamlib` (XDG compliance)
/// 3. `~/.streamlib` (default)
///
/// The directory structure under STREAMLIB_HOME:
/// ```text
/// ~/.streamlib/
/// ├── config.toml                    # Future: system-wide settings
/// ├── cache/
/// │   ├── uv/                        # Shared PyPI cache (UV_CACHE_DIR)
/// │   └── wheels/
/// │       └── streamlib_python.whl   # Pre-built wheel
/// └── runtimes/
///     └── {runtime_id}/
///         └── processors/
///             └── {processor_id}/
///                 ├── venv/          # Isolated Python venv
///                 └── data/          # Processor-specific storage
/// ```
pub fn get_streamlib_home() -> PathBuf {
    // 1. Explicit override
    if let Ok(home) = std::env::var("STREAMLIB_HOME") {
        return PathBuf::from(home);
    }

    // 2. XDG compliance
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("streamlib");
    }

    // 3. Default: ~/.streamlib
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".streamlib")
}

/// Ensure the STREAMLIB_HOME directory and standard subdirectories exist.
pub fn ensure_streamlib_home() -> std::io::Result<PathBuf> {
    let home = get_streamlib_home();

    // Create main directory
    std::fs::create_dir_all(&home)?;

    // Create standard subdirectories
    std::fs::create_dir_all(home.join("cache/wheels"))?;
    std::fs::create_dir_all(home.join("cache/uv"))?;
    std::fs::create_dir_all(home.join("runtimes"))?;

    Ok(home)
}

/// Get the path to the wheels cache directory.
pub fn get_wheels_cache_dir() -> PathBuf {
    get_streamlib_home().join("cache/wheels")
}

/// Get the path to the uv cache directory.
pub fn get_uv_cache_dir() -> PathBuf {
    get_streamlib_home().join("cache/uv")
}

/// Get the path to a runtime's directory.
pub fn get_runtime_dir(runtime_id: &str) -> PathBuf {
    get_streamlib_home().join("runtimes").join(runtime_id)
}

/// Get the path to a processor's directory within a runtime.
pub fn get_processor_dir(runtime_id: &str, processor_id: &str) -> PathBuf {
    get_runtime_dir(runtime_id)
        .join("processors")
        .join(processor_id)
}

/// Get the path to a processor's venv directory.
pub fn get_processor_venv_dir(runtime_id: &str, processor_id: &str) -> PathBuf {
    get_processor_dir(runtime_id, processor_id).join("venv")
}

/// Get the path to a processor's data directory.
pub fn get_processor_data_dir(runtime_id: &str, processor_id: &str) -> PathBuf {
    get_processor_dir(runtime_id, processor_id).join("data")
}

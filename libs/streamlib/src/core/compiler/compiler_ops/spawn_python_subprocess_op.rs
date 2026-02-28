// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;
use std::process::Command;

use crate::core::error::{Result, StreamError};

// ============================================================================
// Venv management
// ============================================================================

/// Ensure a hash-keyed cached venv exists, install deps on miss, and return the python path.
///
/// Venv location: `~/.streamlib/cache/venvs/{sha256_hex}/`
/// Cache key: SHA-256 of `(pyproject.toml contents + canonical project_path)`.
/// If no pyproject.toml exists, falls back to SHA-256 of `processor_id`.
/// On cache hit (python binary exists), returns immediately with zero `uv` calls.
/// On cache miss, creates venv and installs deps. On install failure, removes the
/// venv directory and returns an error.
pub fn ensure_processor_venv(processor_id: &str, project_path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};

    let uv_cache_dir = crate::core::streamlib_home::get_uv_cache_dir();

    let pyproject_path = if !project_path.as_os_str().is_empty() {
        let p = project_path.join("pyproject.toml");
        if p.exists() {
            Some(p)
        } else {
            None
        }
    } else {
        None
    };

    // Compute hash for cache key
    let hash_hex = if let Some(ref pyproject) = pyproject_path {
        let contents = std::fs::read_to_string(pyproject)
            .map_err(|e| StreamError::Runtime(format!("Failed to read pyproject.toml: {}", e)))?;
        let canonical = project_path.canonicalize().map_err(|e| {
            StreamError::Runtime(format!(
                "Failed to canonicalize project_path '{}': {}",
                project_path.display(),
                e
            ))
        })?;
        let mut hasher = Sha256::new();
        hasher.update(contents.as_bytes());
        hasher.update(canonical.to_string_lossy().as_bytes());
        format!("{:x}", hasher.finalize())
    } else {
        let mut hasher = Sha256::new();
        hasher.update(processor_id.as_bytes());
        format!("{:x}", hasher.finalize())
    };

    let venv_dir = crate::core::streamlib_home::get_cached_venv_dir(&hash_hex);

    // Platform-specific python binary path within venv
    #[cfg(unix)]
    let venv_python = venv_dir.join("bin").join("python");
    #[cfg(windows)]
    let venv_python = venv_dir.join("Scripts").join("python.exe");

    // Fast path (no lock) — venv already exists and has a python binary
    if venv_python.exists() {
        tracing::debug!(
            "[{}] Cache hit: reusing venv at {} (hash={})",
            processor_id,
            venv_dir.display(),
            &hash_hex[..12]
        );
        return Ok(venv_python.to_string_lossy().to_string());
    }

    // Serialize venv creation — multiple processors sharing the same pyproject.toml
    // produce the same hash and would otherwise race to create the same venv.
    static VENV_CREATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _lock = VENV_CREATION_LOCK
        .lock()
        .map_err(|e| StreamError::Runtime(format!("Venv creation lock poisoned: {}", e)))?;

    // Re-check after acquiring lock — another thread may have created it
    if venv_python.exists() {
        tracing::debug!(
            "[{}] Cache hit (after lock): reusing venv at {} (hash={})",
            processor_id,
            venv_dir.display(),
            &hash_hex[..12]
        );
        return Ok(venv_python.to_string_lossy().to_string());
    }

    // Cache miss — create venv
    tracing::info!(
        "[{}] Cache miss: creating venv at {} (hash={})",
        processor_id,
        venv_dir.display(),
        &hash_hex[..12]
    );

    std::fs::create_dir_all(venv_dir.parent().unwrap_or(&venv_dir)).map_err(|e| {
        StreamError::Runtime(format!("Failed to create venv parent directory: {}", e))
    })?;

    let output = run_uv(
        &["venv", venv_dir.to_str().unwrap_or(""), "--python", "3.12"],
        &uv_cache_dir,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&venv_dir);
        return Err(StreamError::Runtime(format!(
            "Failed to create venv for processor '{}': {}",
            processor_id, stderr
        )));
    }

    tracing::info!("[{}] Venv created", processor_id);

    // Install project dependencies (only when pyproject.toml exists)
    if pyproject_path.is_some() {
        tracing::info!(
            "[{}] Installing project deps from {}",
            processor_id,
            project_path.display()
        );

        let venv_python_str = venv_python.to_string_lossy().to_string();
        let project_path_str = project_path.to_string_lossy().to_string();

        let output = run_uv(
            &[
                "pip",
                "install",
                "-e",
                &project_path_str,
                "--python",
                &venv_python_str,
            ],
            &uv_cache_dir,
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let _ = std::fs::remove_dir_all(&venv_dir);
            return Err(StreamError::Runtime(format!(
                "Failed to install project deps for processor '{}': {}",
                processor_id, stderr
            )));
        }
    }

    Ok(venv_python.to_string_lossy().to_string())
}

/// Run a `uv` command with the given args and cache directory.
fn run_uv(args: &[&str], uv_cache_dir: &Path) -> Result<std::process::Output> {
    Command::new("uv")
        .args(args)
        .env("UV_CACHE_DIR", uv_cache_dir.to_str().unwrap_or(""))
        .output()
        .map_err(|e| {
            StreamError::Runtime(format!(
                "Failed to run uv (is uv installed?): {}. Install with: curl -LsSf https://astral.sh/uv/install.sh | sh",
                e
            ))
        })
}

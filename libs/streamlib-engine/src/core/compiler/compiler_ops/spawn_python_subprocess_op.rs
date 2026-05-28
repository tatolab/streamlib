// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;
use std::process::Command;

use crate::core::error::{Result, Error};

// ============================================================================
// Venv management
// ============================================================================

/// Ensure a hash-keyed cached venv exists, install deps on miss, and return the python path.
///
/// Venv location: `~/.streamlib/cache/venvs/{sha256_hex}/`
/// Cache key: SHA-256 of `(install-source contents + canonical project_path)`,
/// where the install source is the first wheel under `python/wheels/`
/// when present (`streamlib pack` populates this from `uv build --wheel`)
/// and the package's `pyproject.toml` otherwise.
/// On cache hit (python binary exists), returns immediately with zero `uv` calls.
/// On cache miss, creates venv and installs deps. On install failure, removes the
/// venv directory and returns an error.
///
/// Install path:
/// - Pre-built wheel under `python/wheels/<wheel>.whl`: `uv pip install <wheel>`
///   (binary install — no build backend like hatchling / maturin needed at install
///   time). This is the load-bearing path for container deploys: the runtime
///   container does not need a Python build toolchain.
/// - No wheel but `pyproject.toml` present: `uv pip install -e <project_path>`
///   (editable source-install, runs the package's declared build backend on the
///   install machine). Fallback for dev-tree iteration where the slpkg wasn't
///   produced via `streamlib pack`.
/// Compute the venv cache key for a processor's install source. Three
/// branches that match what we'll actually install: the wheel CONTENTS,
/// the `pyproject.toml` contents, or the bare `processor_id` as a last
/// resort. Wheel and source installs hash into different buckets
/// (distinct domain prefixes) — re-installing the same package via a
/// different shape gets its own venv rather than colliding.
///
/// The wheel branch hashes the wheel BYTES, not just its filename: a
/// rebuilt same-version package keeps the same wheel filename
/// (`pkg-0.1.0-py3-none-any.whl`), so a filename-keyed venv would hit a
/// stale install and silently run the OLD code after a source edit — the
/// exact stale-artifact trap the runtime-build subsystem exists to close,
/// one layer up. Hashing content guarantees a rebuilt wheel reinstalls.
fn compute_venv_cache_key(
    prebuilt_wheel: Option<&Path>,
    pyproject_path: Option<&Path>,
    project_path: &Path,
    processor_id: &str,
) -> Result<String> {
    use sha2::{Digest, Sha256};

    let canonical = |hasher: &mut Sha256| -> Result<()> {
        let c = project_path.canonicalize().map_err(|e| {
            Error::Runtime(format!(
                "Failed to canonicalize project_path '{}': {}",
                project_path.display(),
                e
            ))
        })?;
        hasher.update(c.to_string_lossy().as_bytes());
        Ok(())
    };

    if let Some(wheel) = prebuilt_wheel {
        let wheel_bytes = std::fs::read(wheel).map_err(|e| {
            Error::Runtime(format!("Failed to read wheel '{}': {}", wheel.display(), e))
        })?;
        let mut hasher = Sha256::new();
        hasher.update(b"wheel:");
        hasher.update(&wheel_bytes);
        canonical(&mut hasher)?;
        Ok(format!("{:x}", hasher.finalize()))
    } else if let Some(pyproject) = pyproject_path {
        let contents = std::fs::read_to_string(pyproject)
            .map_err(|e| Error::Runtime(format!("Failed to read pyproject.toml: {}", e)))?;
        let mut hasher = Sha256::new();
        hasher.update(b"source:");
        hasher.update(contents.as_bytes());
        canonical(&mut hasher)?;
        Ok(format!("{:x}", hasher.finalize()))
    } else {
        let mut hasher = Sha256::new();
        hasher.update(processor_id.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }
}

pub fn ensure_processor_venv(processor_id: &str, project_path: &Path) -> Result<String> {
    let uv_cache_dir = crate::core::streamlib_home::get_uv_cache_dir();

    let prebuilt_wheel = if project_path.as_os_str().is_empty() {
        None
    } else {
        find_first_wheel(&project_path.join("python").join("wheels"))?
    };

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

    let hash_hex = compute_venv_cache_key(
        prebuilt_wheel.as_deref(),
        pyproject_path.as_deref(),
        project_path,
        processor_id,
    )?;

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
        .map_err(|e| Error::Runtime(format!("Venv creation lock poisoned: {}", e)))?;

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
        Error::Runtime(format!("Failed to create venv parent directory: {}", e))
    })?;

    let output = run_uv(
        &["venv", venv_dir.to_str().unwrap_or(""), "--python", "3.12"],
        &uv_cache_dir,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&venv_dir);
        return Err(Error::Runtime(format!(
            "Failed to create venv for processor '{}': {}",
            processor_id, stderr
        )));
    }

    tracing::info!("[{}] Venv created", processor_id);

    // Install the package. Prefer the pre-built wheel (binary install, no
    // build backend at install time) when present; fall back to source-
    // install against pyproject.toml. Packages with neither (no wheel,
    // no pyproject.toml) get a bare venv — the processor's source `.py`
    // file ships standalone in the slpkg root and runs directly.
    if let Some(ref wheel) = prebuilt_wheel {
        tracing::info!(
            "[{}] Installing pre-built wheel {}",
            processor_id,
            wheel.display()
        );

        let venv_python_str = venv_python.to_string_lossy().to_string();
        let wheel_str = wheel.to_string_lossy().to_string();

        let output = run_uv(
            &[
                "pip",
                "install",
                &wheel_str,
                "--python",
                &venv_python_str,
            ],
            &uv_cache_dir,
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let _ = std::fs::remove_dir_all(&venv_dir);
            return Err(Error::Runtime(format!(
                "Failed to install wheel for processor '{}': {}",
                processor_id, stderr
            )));
        }
    } else if pyproject_path.is_some() {
        tracing::info!(
            "[{}] Installing project deps from source at {}",
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
            return Err(Error::Runtime(format!(
                "Failed to install project deps for processor '{}': {}",
                processor_id, stderr
            )));
        }
    }

    Ok(venv_python.to_string_lossy().to_string())
}

/// Enumerate `*.whl` files in `wheels_dir` and return the first one
/// (sorted), or `None` when the dir is missing or empty. Multiple
/// wheels in the same dir is a future multi-platform-matrix concern
/// (`streamlib pack` writes one wheel per build today); for now the
/// first match wins. The platform tags in the wheel filename plus
/// `uv pip install`'s own resolver are the safety net — `uv` refuses
/// to install a wheel whose tags don't match the venv's interpreter.
fn find_first_wheel(wheels_dir: &Path) -> Result<Option<std::path::PathBuf>> {
    if !wheels_dir.is_dir() {
        return Ok(None);
    }
    let mut wheels: Vec<std::path::PathBuf> = std::fs::read_dir(wheels_dir)
        .map_err(|e| {
            Error::Runtime(format!(
                "Failed to read wheels directory {}: {}",
                wheels_dir.display(),
                e
            ))
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "whl"))
        .collect();
    wheels.sort();
    Ok(wheels.into_iter().next())
}

/// Run a `uv` command with the given args and cache directory.
fn run_uv(args: &[&str], uv_cache_dir: &Path) -> Result<std::process::Output> {
    Command::new("uv")
        .args(args)
        .env("UV_CACHE_DIR", uv_cache_dir.to_str().unwrap_or(""))
        .output()
        .map_err(|e| {
            Error::Runtime(format!(
                "Failed to run uv (is uv installed?): {}. Install with: curl -LsSf https://astral.sh/uv/install.sh | sh",
                e
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn find_first_wheel_returns_none_when_dir_missing() {
        // The wheels dir is optional — every Python package shipped
        // pre-`streamlib pack`-with-wheels (or shipped via
        // `streamlib pack` with `--no-build` against a non-wheel
        // package) will simply not have one. The helper must report
        // that as `None`, not error — the install path falls back to
        // source-install in that case.
        let dir = tempdir().unwrap();
        let wheels = dir.path().join("python").join("wheels");
        let found = find_first_wheel(&wheels).unwrap();
        assert!(found.is_none(), "missing wheels dir must return None, got: {:?}", found);
    }

    #[test]
    fn venv_cache_key_changes_when_wheel_bytes_change_same_filename() {
        // Regression: a rebuilt same-version package keeps the SAME wheel
        // filename. The cache key must track wheel CONTENT, not the
        // filename — else a source edit silently runs stale code from a
        // cache-hit venv. Mentally revert to filename-hashing and key_a
        // == key_b here, re-opening the stale-artifact trap.
        let dir = tempdir().unwrap();
        let wheels = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        let wheel = wheels.join("pkg-0.1.0-py3-none-any.whl");

        std::fs::write(&wheel, b"PK\x03\x04 first build").unwrap();
        let key_a = compute_venv_cache_key(Some(&wheel), None, dir.path(), "P").unwrap();

        // Same filename, different bytes — a rebuild after a source edit.
        std::fs::write(&wheel, b"PK\x03\x04 second build, edited source").unwrap();
        let key_b = compute_venv_cache_key(Some(&wheel), None, dir.path(), "P").unwrap();
        assert_ne!(
            key_a, key_b,
            "same-filename wheel with new bytes must produce a new venv key"
        );

        // Identical bytes → identical key (cache hit is correct when unchanged).
        std::fs::write(&wheel, b"PK\x03\x04 first build").unwrap();
        let key_c = compute_venv_cache_key(Some(&wheel), None, dir.path(), "P").unwrap();
        assert_eq!(key_a, key_c, "identical wheel bytes must reuse the venv");
    }

    #[test]
    fn venv_cache_key_wheel_and_source_are_distinct_domains() {
        // A wheel install and a source (`-e pyproject`) install of the
        // same package must not collide — they install differently, so a
        // shared venv would be wrong. The domain prefix (`wheel:` vs
        // `source:`) keeps them in separate buckets even with identical
        // hashed bytes.
        let dir = tempdir().unwrap();
        let wheels = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        let wheel = wheels.join("pkg-0.1.0-py3-none-any.whl");
        std::fs::write(&wheel, b"same-bytes").unwrap();
        let pyproject = dir.path().join("pyproject.toml");
        std::fs::write(&pyproject, b"same-bytes").unwrap();

        let wheel_key =
            compute_venv_cache_key(Some(&wheel), Some(&pyproject), dir.path(), "P").unwrap();
        let source_key =
            compute_venv_cache_key(None, Some(&pyproject), dir.path(), "P").unwrap();
        assert_ne!(
            wheel_key, source_key,
            "wheel and source installs must hash into different buckets"
        );
    }

    #[test]
    fn find_first_wheel_filters_by_whl_extension() {
        // The packed slpkg's `python/wheels/` dir typically carries
        // only `*.whl` files, but a future `streamlib pack` extension
        // (multi-platform matrix, prebuilt-sdist passthrough, etc.)
        // could land sibling artifacts. The helper must pick the
        // wheel and skip the rest — `uv pip install` refuses to
        // install a tarball as if it were a wheel and the error
        // message wouldn't point at the layout bug.
        let dir = tempdir().unwrap();
        let wheels = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        std::fs::write(wheels.join("foo-0.1.0-py3-none-any.whl"), b"wheel-bytes").unwrap();
        std::fs::write(wheels.join("foo-0.1.0.tar.gz"), b"sdist-bytes").unwrap();
        std::fs::write(wheels.join("README.md"), b"docs").unwrap();

        let found = find_first_wheel(&wheels).unwrap().expect("expected a wheel");
        assert!(
            found.ends_with("foo-0.1.0-py3-none-any.whl"),
            "expected wheel match, got: {}",
            found.display()
        );
    }

    #[test]
    fn find_first_wheel_returns_sorted_first_for_deterministic_pick() {
        // When two wheels are present (e.g. a customer pre-populated
        // both a `py3-none-any` pure-Python wheel and a
        // platform-specific one) the helper picks the first by sorted
        // filename. Today's pack writes one wheel; multi-platform
        // matrix is a future extension. The deterministic-pick
        // invariant means the same slpkg always selects the same wheel
        // — if a future change lets `uv pip install` pick the right
        // wheel against the venv interpreter, this becomes the wrong
        // shape and the test catches the regression at refactor time.
        let dir = tempdir().unwrap();
        let wheels = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        std::fs::write(wheels.join("foo-0.1.0-cp312-cp312-linux_x86_64.whl"), b"a").unwrap();
        std::fs::write(wheels.join("foo-0.1.0-py3-none-any.whl"), b"b").unwrap();

        let found = find_first_wheel(&wheels).unwrap().expect("expected a wheel");
        // Sorted by filename, `cp312...` comes before `py3...`.
        assert!(
            found.ends_with("foo-0.1.0-cp312-cp312-linux_x86_64.whl"),
            "expected sorted-first wheel, got: {}",
            found.display()
        );
    }
}

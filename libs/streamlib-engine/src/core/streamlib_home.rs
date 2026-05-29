// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

/// The writable, regenerable working root — the counterpart to the
/// read-only `packages/` source dir. Holds the built-package cache,
/// Python venvs / uv cache, per-runtime data + logs, git resolver
/// checkouts, and the installed-packages manifest.
///
/// Resolution order:
/// 1. `STREAMLIB_HOME` environment variable (explicit override — the
///    install/Docker points it at the install location; tests point it
///    at a tempdir).
/// 2. `<app-root>/.streamlib`, where the app root is the install / clone
///    directory found by walking up from the running binary to the first
///    ancestor containing a `packages/` directory. This collocates all
///    working state inside the install so each box / container is one
///    self-contained folder — no global home state to track across a
///    fleet. (`.streamlib/` is gitignored.)
/// 3. `<binary-dir>/.streamlib` as an infallible last resort when the
///    binary isn't inside an app tree and no override is set. Never the
///    user home directory.
///
/// The directory structure under the resolved root:
/// ```text
/// <app-root>/.streamlib/
/// ├── cache/
/// │   ├── packages/                  # Built / extracted package artifacts
/// │   ├── uv/                        # Shared PyPI cache (UV_CACHE_DIR)
/// │   └── venvs/{sha256_hex}/        # Venvs keyed by pyproject.toml hash
/// ├── runtimes/{runtime_id}/         # Per-runtime data + JSONL logs
/// └── resolver-cache/                # Git checkouts for Strategy::Git
/// ```
pub fn get_streamlib_home() -> PathBuf {
    if let Ok(home) = std::env::var("STREAMLIB_HOME") {
        return PathBuf::from(home);
    }

    find_app_root()
        .map(|root| root.join(".streamlib"))
        .unwrap_or_else(fallback_home)
}

/// Walk up from the running binary to the first ancestor containing a
/// `packages/` directory — the streamlib install / workspace root. This
/// mirrors the runtime binary's package-source resolution; the difference
/// is that the cache root honors the `STREAMLIB_HOME` override (above)
/// while package-source resolution does not (source is fixed, the cache
/// is redirectable).
fn find_app_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    app_root_from(exe.parent()?)
}

/// Walk up from `start` to the first ancestor containing a `packages/`
/// directory. Pure helper so the walk-up is testable without depending on
/// the running binary's location.
fn app_root_from(start: &std::path::Path) -> Option<PathBuf> {
    let mut ancestor = Some(start);
    while let Some(dir) = ancestor {
        if dir.join("packages").is_dir() {
            return Some(dir.to_path_buf());
        }
        ancestor = dir.parent();
    }
    None
}

/// Infallible last resort: the running binary's own directory. Used only
/// when the binary isn't inside an app tree and `STREAMLIB_HOME` is unset
/// (e.g. an external host app that hasn't configured it). Never resolves
/// to the user home directory — collocated global state is deliberately gone.
fn fallback_home() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(".streamlib")))
        .unwrap_or_else(|| PathBuf::from(".streamlib"))
}

/// Ensure the STREAMLIB_HOME directory and standard subdirectories exist.
pub fn ensure_streamlib_home() -> std::io::Result<PathBuf> {
    let home = get_streamlib_home();

    // Create main directory
    std::fs::create_dir_all(&home)?;

    // Create standard subdirectories
    std::fs::create_dir_all(home.join("cache/wheels"))?;
    std::fs::create_dir_all(home.join("cache/uv"))?;
    std::fs::create_dir_all(home.join("cache/venvs"))?;
    std::fs::create_dir_all(home.join("cache/packages"))?;
    std::fs::create_dir_all(home.join("runtimes"))?;

    Ok(home)
}

/// Get the path to the uv cache directory.
pub fn get_uv_cache_dir() -> PathBuf {
    get_streamlib_home().join("cache/uv")
}

/// Get the path to a hash-keyed cached venv directory.
pub fn get_cached_venv_dir(hash: &str) -> PathBuf {
    get_streamlib_home().join("cache/venvs").join(hash)
}

/// Get the path to a cached extracted package directory.
pub fn get_cached_package_dir(cache_key: &str) -> PathBuf {
    get_streamlib_home().join("cache/packages").join(cache_key)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_root_is_first_ancestor_with_packages_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Mark `root` as an app root and nest the "binary" a few levels down.
        std::fs::create_dir_all(root.join("packages")).unwrap();
        let bin_dir = root.join("target").join("debug").join("deps");
        std::fs::create_dir_all(&bin_dir).unwrap();

        assert_eq!(app_root_from(&bin_dir).as_deref(), Some(root));
        // Resolves from the root itself too.
        assert_eq!(app_root_from(root).as_deref(), Some(root));
    }

    #[test]
    fn app_root_is_none_without_a_packages_dir() {
        // Revert the `packages/` marker check and this would wrongly return
        // a dir — the walk-up must find nothing when no ancestor qualifies.
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(app_root_from(&nested), None);
    }
}

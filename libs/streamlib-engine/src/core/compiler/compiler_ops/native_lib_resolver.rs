// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Resolves the on-disk path to a subprocess native FFI host cdylib
//! (`libstreamlib_python_native` / `libstreamlib_deno_native`). The Python and
//! Deno spawn ops share this so both runtimes resolve identically — the only
//! difference is the library stem.

use std::path::{Path, PathBuf};

use crate::core::{Error, Result};

/// Host target triple this engine was built for — the cache subdir the build
/// orchestrator writes the native host into. Same value as
/// `module_loader::processor_registration::host_target_triple`, read directly
/// here because that module is crate-private.
fn host_target_triple() -> &'static str {
    env!("STREAMLIB_HOST_TARGET")
}

/// Which subprocess native FFI host to resolve.
#[derive(Clone, Copy)]
pub(crate) enum SubprocessNativeRuntime {
    Python,
    Deno,
}

impl SubprocessNativeRuntime {
    /// Cdylib library stem (no `lib` prefix, no extension).
    fn lib_stem(self) -> &'static str {
        match self {
            SubprocessNativeRuntime::Python => "streamlib_python_native",
            SubprocessNativeRuntime::Deno => "streamlib_deno_native",
        }
    }

    /// Environment override pointing directly at a prebuilt host.
    fn env_var(self) -> &'static str {
        match self {
            SubprocessNativeRuntime::Python => "STREAMLIB_PYTHON_NATIVE_LIB",
            SubprocessNativeRuntime::Deno => "STREAMLIB_DENO_NATIVE_LIB",
        }
    }
}

/// Host-OS cdylib filename for `stem` (`lib<stem>.so` / `.dylib`, `<stem>.dll`).
fn native_lib_filename(stem: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("lib{stem}.dylib")
    } else if cfg!(target_os = "windows") {
        format!("{stem}.dll")
    } else {
        format!("lib{stem}.so")
    }
}

/// Pure tier resolution — returns the first existing candidate, or `None`.
/// Split out from [`resolve_subprocess_native_lib_path`] so the tier order is
/// unit-testable without mutating process env or the real streamlib home.
///
/// Resolution order:
/// 1. `env_override` (when set and the path exists) — a prebuilt host.
/// 2. `<home_data_dir>/cache/native/<triple>/<filename>` — the registry-built
///    host cache, populated on first use by the build orchestrator.
/// 3. `<workspace_root>/target/{debug,release}/<filename>` — monorepo dev only.
fn resolve_in(
    filename: &str,
    env_override: Option<&str>,
    home_data_dir: &Path,
    triple: &str,
    workspace_root: &Path,
) -> Option<String> {
    // Tier 1: explicit env override.
    if let Some(path) = env_override {
        if Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    // Tier 2: registry-built host in the streamlib home cache.
    let cached = home_data_dir
        .join("cache")
        .join("native")
        .join(triple)
        .join(filename);
    if cached.exists() {
        return Some(
            cached
                .canonicalize()
                .unwrap_or(cached)
                .to_string_lossy()
                .to_string(),
        );
    }

    // Tier 3: monorepo workspace target (in-tree dev fallback).
    for profile in ["debug", "release"] {
        let candidate = workspace_root.join("target").join(profile).join(filename);
        if candidate.exists() {
            return Some(
                candidate
                    .canonicalize()
                    .unwrap_or(candidate)
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }

    None
}

/// Resolve the path to a subprocess native FFI host cdylib.
///
/// Wires the real environment into [`resolve_in`]: the runtime's
/// `STREAMLIB_*_NATIVE_LIB` env override, the streamlib home cache (the durable
/// registry-consumer path — `CARGO_MANIFEST_DIR` is the cargo registry src tree
/// for a registry consumer, with no sibling `target/`, so tier 2 is the real
/// path there), and the monorepo `target/` dev fallback. Fails with an
/// actionable error listing every location tried.
pub(crate) fn resolve_subprocess_native_lib_path(
    runtime: SubprocessNativeRuntime,
) -> Result<String> {
    let filename = native_lib_filename(runtime.lib_stem());
    let env_override = std::env::var(runtime.env_var()).ok();
    let home_data_dir = crate::core::streamlib_home::get_streamlib_data_dir();
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

    resolve_in(
        &filename,
        env_override.as_deref(),
        &home_data_dir,
        host_target_triple(),
        &workspace_root,
    )
    .ok_or_else(|| {
        let cached = home_data_dir
            .join("cache")
            .join("native")
            .join(host_target_triple())
            .join(&filename);
        Error::Runtime(format!(
            "{filename} not found. Looked at ${} (unset or missing), the registry-built host \
             cache ({}), and the monorepo target/{{debug,release}}. For a registry consumer the \
             host is built on first Python/Deno use by the build orchestrator — run the pipeline \
             through `Runner::with_auto_build()`, or set ${} to a prebuilt host.",
            runtime.env_var(),
            cached.display(),
            runtime.env_var(),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const TRIPLE: &str = "x86_64-unknown-linux-gnu";
    const LIB: &str = "libstreamlib_python_native.so";

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"\x7fELF").unwrap();
    }

    #[test]
    fn env_override_wins_over_home_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let override_path = tmp.path().join("custom").join(LIB);
        touch(&override_path);
        // Home cache ALSO has one — tier 1 must still win.
        let home = tmp.path().join(".streamlib");
        touch(&home.join("cache").join("native").join(TRIPLE).join(LIB));

        let got = resolve_in(
            LIB,
            Some(override_path.to_str().unwrap()),
            &home,
            TRIPLE,
            tmp.path(),
        );
        assert_eq!(got.as_deref(), override_path.to_str());
    }

    #[test]
    fn home_cache_tier_resolves_when_no_env() {
        // Locks Fix B: revert the home-cache tier and this fails.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".streamlib");
        let cached = home.join("cache").join("native").join(TRIPLE).join(LIB);
        touch(&cached);

        let got = resolve_in(LIB, None, &home, TRIPLE, tmp.path());
        assert_eq!(
            got.map(|p| fs::canonicalize(p).unwrap()),
            Some(fs::canonicalize(&cached).unwrap())
        );
    }

    #[test]
    fn workspace_target_is_last_resort() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".streamlib"); // empty — no tier-2 hit
        let ws = tmp.path().join("ws");
        let target_lib = ws.join("target").join("debug").join(LIB);
        touch(&target_lib);

        let got = resolve_in(LIB, None, &home, TRIPLE, &ws);
        assert_eq!(
            got.map(|p| fs::canonicalize(p).unwrap()),
            Some(fs::canonicalize(&target_lib).unwrap())
        );
    }

    #[test]
    fn missing_env_override_falls_through_to_home_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".streamlib");
        let cached = home.join("cache").join("native").join(TRIPLE).join(LIB);
        touch(&cached);

        // Override points at a nonexistent path — must be ignored, not returned.
        let got = resolve_in(
            LIB,
            Some(tmp.path().join("does-not-exist.so").to_str().unwrap()),
            &home,
            TRIPLE,
            tmp.path(),
        );
        assert_eq!(
            got.map(|p| fs::canonicalize(p).unwrap()),
            Some(fs::canonicalize(&cached).unwrap())
        );
    }

    #[test]
    fn none_when_nothing_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let got = resolve_in(
            LIB,
            None,
            &tmp.path().join(".streamlib"),
            TRIPLE,
            &tmp.path().join("ws"),
        );
        assert_eq!(got, None);
    }
}

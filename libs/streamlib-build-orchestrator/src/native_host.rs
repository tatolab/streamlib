// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Builds the subprocess native FFI host cdylibs (`streamlib-python-native` /
//! `streamlib-deno-native`) from the Gitea cargo registry source into the
//! streamlib home cache.
//!
//! The host is the engine's own subprocess interpreter shim — engine-scoped,
//! not a user package — so it can't ride the per-package `materialize` path.
//! It's fetched and built once per host triple + version, then reused
//! (`IfStale`). The engine's `native_lib_resolver` finds it at
//! `<home>/.streamlib/cache/native/<triple>/lib<stem>.<ext>`.
//!
//! ABI note: the host depends only on `streamlib-consumer-rhi` +
//! `streamlib-adapter-*` (the plugin-ABI / consumer-RHI contract), never the
//! in-process engine — so building it with the consumer's toolchain is
//! ABI-sound.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_cargo_build::{
    host_dylib_extension, host_target_triple, run_cargo_build, CargoProfile,
};
use streamlib_engine::core::runtime::BuildError;

use crate::{build_failed, other};

/// Which subprocess native FFI host to ensure.
#[derive(Clone, Copy)]
pub(crate) enum NativeRuntime {
    Python,
    Deno,
}

impl NativeRuntime {
    /// Cargo crate name in the registry.
    fn crate_name(self) -> &'static str {
        match self {
            NativeRuntime::Python => "streamlib-python-native",
            NativeRuntime::Deno => "streamlib-deno-native",
        }
    }

    /// Cdylib library stem (no `lib` prefix, no extension).
    fn lib_stem(self) -> &'static str {
        match self {
            NativeRuntime::Python => "streamlib_python_native",
            NativeRuntime::Deno => "streamlib_deno_native",
        }
    }

    /// Environment override the engine's resolver honors as tier 1.
    fn env_var(self) -> &'static str {
        match self {
            NativeRuntime::Python => "STREAMLIB_PYTHON_NATIVE_LIB",
            NativeRuntime::Deno => "STREAMLIB_DENO_NATIVE_LIB",
        }
    }
}

/// Host-OS cdylib filename for `stem` (`lib<stem>.so` / `.dylib`, `<stem>.dll`).
fn lib_filename(stem: &str, ext: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{stem}.{ext}")
    } else {
        format!("lib{stem}.{ext}")
    }
}

/// Ensure the native host cdylib for `runtime` is built and cached at
/// `<home>/.streamlib/cache/native/<triple>/lib<stem>.<ext>`, fetching the
/// crate source from the Gitea cargo registry and building it from source.
///
/// No-ops when the runtime's `STREAMLIB_*_NATIVE_LIB` env override points at an
/// existing file (the resolver's tier 1 — nothing to build), and reuses the
/// cached host when a sibling `.version` stamp matches `version` (`IfStale`).
pub(crate) fn ensure_native_host(
    runtime: NativeRuntime,
    version: &str,
    profile: CargoProfile,
) -> Result<PathBuf, BuildError> {
    let triple = host_target_triple();
    let ext = host_dylib_extension();
    let filename = lib_filename(runtime.lib_stem(), ext);

    // Env override (resolver tier 1): the caller supplied a prebuilt host, so
    // the engine won't consult the cache — don't fetch or build.
    if let Ok(path) = std::env::var(runtime.env_var()) {
        if Path::new(&path).exists() {
            tracing::debug!(host = %path, env = runtime.env_var(), "native host via env override — skip build");
            return Ok(PathBuf::from(path));
        }
    }

    let cache_root = streamlib_engine::core::get_streamlib_data_dir()
        .join("cache")
        .join("native")
        .join(triple);
    let dest = cache_root.join(&filename);
    let stamp = cache_root.join(format!(".{}.version", runtime.lib_stem()));

    // IfStale: reuse a cached host whose stamp matches the requested version.
    if dest.is_file()
        && std::fs::read_to_string(&stamp)
            .map(|s| s.trim() == version)
            .unwrap_or(false)
    {
        tracing::debug!(host = %dest.display(), %version, "native host already cached — reuse");
        return Ok(dest);
    }

    // Consumer-side release-completeness pre-check: the native host crate is
    // itself a member of the engine release closure. If the registry holds a
    // partial release of `version`, fail fast naming the gap instead of a
    // cryptic cargo resolve error inside the standalone build below. No-op
    // for pre-atomic-release registries (no manifest) — see `release_check`.
    crate::release_check::assert_release_complete(
        runtime.crate_name(),
        &[(runtime.crate_name().to_string(), version.to_string())],
    )?;

    tracing::info!(
        crate_name = runtime.crate_name(),
        %version,
        profile = profile.label(),
        "building subprocess native host from registry source (first use / version change)"
    );

    // 1. Download the .crate source from the Gitea cargo registry (anonymous —
    //    the cargo registry's `auth-required` is false for downloads).
    let registry_url = std::env::var("STREAMLIB_REGISTRY_URL")
        .or_else(|_| std::env::var("GITEA_URL"))
        .map_err(|_| {
            other(
                runtime.crate_name(),
                "STREAMLIB_REGISTRY_URL (or GITEA_URL) must be set to fetch the native host source"
                    .to_string(),
            )
        })?;
    let url = format!(
        "{}/api/packages/tatolab/cargo/api/v1/crates/{}/{}/download",
        registry_url.trim_end_matches('/'),
        runtime.crate_name(),
        version,
    );
    let crate_bytes = http_get_bytes(&url).map_err(|e| {
        build_failed(
            runtime.crate_name(),
            format!("downloading native host source from {url}: {e}"),
        )
    })?;

    // 2. Extract the .crate (gzip-tar) into a scratch build dir under the home
    //    cache. The archive carries a `<crate>-<version>/` top-level dir.
    let build_root = streamlib_engine::core::get_streamlib_data_dir()
        .join("cache")
        .join("native-build");
    std::fs::create_dir_all(&build_root)
        .map_err(|e| other(runtime.crate_name(), format!("create native-build dir: {e}")))?;
    let crate_dir = build_root.join(format!("{}-{}", runtime.crate_name(), version));
    let _ = std::fs::remove_dir_all(&crate_dir);
    extract_crate_tarball(&crate_bytes, &build_root)
        .map_err(|e| build_failed(runtime.crate_name(), format!("extracting .crate: {e}")))?;

    // The crate is extracted under `<home>/.streamlib/cache/native-build/`,
    // which may itself sit inside a cargo workspace — e.g. when the streamlib
    // home is the repo root during an in-tree example run. Declare the
    // extracted crate its own workspace root so cargo doesn't treat it as a
    // member of that outer workspace (the published manifest has no
    // `[workspace]`, so cargo would otherwise walk up, find the repo's, and
    // fail: "current package believes it's in a workspace when it's not").
    {
        let manifest = crate_dir.join("Cargo.toml");
        let mut toml = std::fs::read_to_string(&manifest)
            .map_err(|e| other(runtime.crate_name(), format!("read extracted Cargo.toml: {e}")))?;
        if !toml.contains("[workspace]") {
            toml.push_str("\n[workspace]\n");
            std::fs::write(&manifest, &toml).map_err(|e| {
                other(runtime.crate_name(), format!("write standalone Cargo.toml: {e}"))
            })?;
        }
    }

    // 3. Build standalone. The published manifest carries inline
    //    `registry-index` on its Gitea deps, so dep resolution needs no
    //    `.cargo/config.toml`. `run_cargo_build` runs `cargo build -p <crate>`
    //    in `crate_dir` and returns the produced cdylib path.
    let cdylib = run_cargo_build(&crate_dir, runtime.crate_name(), ext, profile)
        .map_err(|e| build_failed(runtime.crate_name(), format!("cargo build: {e}")))?;

    // 4. Install into the cache + write the version stamp.
    std::fs::create_dir_all(&cache_root)
        .map_err(|e| other(runtime.crate_name(), format!("create native cache dir: {e}")))?;
    std::fs::copy(&cdylib, &dest).map_err(|e| {
        other(
            runtime.crate_name(),
            format!("copy {} -> {}: {e}", cdylib.display(), dest.display()),
        )
    })?;
    std::fs::write(&stamp, version)
        .map_err(|e| other(runtime.crate_name(), format!("write version stamp: {e}")))?;
    let _ = std::fs::remove_dir_all(&crate_dir);

    tracing::info!(host = %dest.display(), "native host built + cached");
    Ok(dest)
}

/// GET the bytes at `url` (no auth — cargo downloads are anonymous on the
/// streamlib Gitea instance).
fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Extract a `.crate` (gzip-compressed tar) into `dest_dir`. Shells out to
/// `tar` — the native-host build is a host-side, Linux/macOS-only step.
fn extract_crate_tarball(bytes: &[u8], dest_dir: &Path) -> Result<(), String> {
    let tmp = dest_dir.join(".download.crate");
    std::fs::write(&tmp, bytes).map_err(|e| format!("write temp .crate: {e}"))?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&tmp)
        .arg("-C")
        .arg(dest_dir)
        .status()
        .map_err(|e| format!("spawn tar: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        return Err(format!("tar exited with {status}"));
    }
    Ok(())
}

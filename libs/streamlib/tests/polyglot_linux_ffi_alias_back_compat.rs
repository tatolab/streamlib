// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFI alias back-compat lock for the broker → surface_share rename (#463).
//!
//! Before #463 the polyglot consumer cdylibs exported `slpn_broker_*` and
//! `sldn_broker_*` C symbols. The rename keeps those alive as
//! `#[deprecated]` aliases that delegate to the canonical `slpn_surface_*` /
//! `sldn_surface_*` implementations. This test loads each cdylib via
//! `libloading` and confirms both spellings resolve to callable code so a
//! Python or Deno app pinned to the legacy name keeps loading the new lib.
//!
//! Skip conditions:
//!   - `libstreamlib_{python,deno}_native.so` not under target/ → skip the
//!     corresponding case. CI builds both cdylibs before running tests, so a
//!     skip on a developer machine just means "build the cdylib first".

#![cfg(target_os = "linux")]

use std::path::PathBuf;

fn locate_native_lib(basename: &str) -> Option<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let workspace = PathBuf::from(&manifest_dir).join("..").join("..");
    for profile in &["debug", "release"] {
        let candidate = workspace.join("target").join(profile).join(basename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Symbol names every consumer cdylib must export — both the canonical
/// `<prefix>surface_*` and the legacy `<prefix>broker_*` alias.
fn symbols_for(prefix: &str) -> [(String, String); 5] {
    [
        (format!("{prefix}surface_connect"),
         format!("{prefix}broker_connect")),
        (format!("{prefix}surface_disconnect"),
         format!("{prefix}broker_disconnect")),
        (format!("{prefix}surface_resolve_surface"),
         format!("{prefix}broker_resolve_surface")),
        (format!("{prefix}surface_acquire_surface"),
         format!("{prefix}broker_acquire_surface")),
        (format!("{prefix}surface_unregister_surface"),
         format!("{prefix}broker_unregister_surface")),
    ]
}

fn assert_symbol_pair_resolves(lib_path: &std::path::Path, prefix: &str) {
    let lib = unsafe { libloading::Library::new(lib_path) }
        .unwrap_or_else(|e| panic!("dlopen {} failed: {}", lib_path.display(), e));

    for (canonical, legacy) in symbols_for(prefix) {
        // Both symbols must be present; the legacy one is a thin wrapper that
        // calls the canonical impl. The signature varies by op so we resolve
        // them as opaque `*mut c_void` function pointers — proof of presence
        // is enough; behavioral parity is exercised by the existing
        // polyglot_linux_check_out{,_deno} tests through the alias path.
        let canon_sym: libloading::Symbol<'_, *const std::ffi::c_void> =
            unsafe { lib.get(canonical.as_bytes()) }
                .unwrap_or_else(|e| panic!("missing canonical `{canonical}`: {e}"));
        assert!(
            !canon_sym.is_null(),
            "canonical symbol `{canonical}` resolved to null"
        );

        let legacy_sym: libloading::Symbol<'_, *const std::ffi::c_void> =
            unsafe { lib.get(legacy.as_bytes()) }
                .unwrap_or_else(|e| panic!("missing legacy alias `{legacy}`: {e}"));
        assert!(
            !legacy_sym.is_null(),
            "legacy alias `{legacy}` resolved to null"
        );
    }
}

#[test]
fn python_native_exports_both_surface_and_broker_ffi_names() {
    let Some(lib_path) = locate_native_lib("libstreamlib_python_native.so") else {
        eprintln!(
            "libstreamlib_python_native.so not under target/ — skipping; \
             build with `cargo build -p streamlib-python-native` first"
        );
        return;
    };
    assert_symbol_pair_resolves(&lib_path, "slpn_");
}

#[test]
fn deno_native_exports_both_surface_and_broker_ffi_names() {
    let Some(lib_path) = locate_native_lib("libstreamlib_deno_native.so") else {
        eprintln!(
            "libstreamlib_deno_native.so not under target/ — skipping; \
             build with `cargo build -p streamlib-deno-native` first"
        );
        return;
    };
    // Deno's macOS twin omits unregister_surface, but on Linux every op is
    // present (canonical + legacy).
    assert_symbol_pair_resolves(&lib_path, "sldn_");
}

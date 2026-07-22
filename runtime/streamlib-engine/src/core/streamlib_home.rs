// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

use streamlib_idents::{PackageRef, SemVer};

/// The streamlib app root — the install / clone directory, the top level
/// that holds both the read-only `packages/` source and the generated
/// `.streamlib/` working tree ([`get_streamlib_data_dir`]).
///
/// Resolution order:
/// 1. `STREAMLIB_HOME` environment variable (explicit override — the
///    install/Docker points it at the install location; tests point it
///    at a tempdir).
/// 2. The install / clone root found by walking up from the running binary
///    to the first ancestor containing a `packages/` directory. So
///    `get_streamlib_home().join("packages")` is the source dir, and each
///    box / container is one self-contained folder — no global home state
///    to track across a fleet.
/// 3. The running binary's own directory as an infallible last resort when
///    it isn't inside an app tree and no override is set. Never the user
///    home directory.
///
/// ```text
/// <streamlib-home>/                     ← app root (install / clone)
/// ├── packages/                         # read-only source (NOT under .streamlib)
/// └── .streamlib/                       # generated working tree — get_streamlib_data_dir()
///     ├── cache/
///     │   ├── packages/                 # built / extracted package artifacts
///     │   │                             #   (each Python package carries its own
///     │   │                             #   `.venv/`, provisioned by the orchestrator)
///     │   └── uv/                        # uv PyPI cache      (Python packages only)
///     ├── logs/<runtime_id>-<ts>.jsonl  # per-runtime JSONL logs
///     ├── resolver-cache/               # git / URL checkouts (Strategy::Git / Url)
///     └── packages.yaml                 # installed-packages manifest (streamlib add)
/// ```
///
/// Each subdir is created on demand by its consumer — an all-Rust,
/// `Strategy::Path` graph populates `cache/packages/` and `logs/`.
pub fn get_streamlib_home() -> PathBuf {
    if let Ok(home) = std::env::var("STREAMLIB_HOME") {
        return PathBuf::from(home);
    }

    find_app_root().unwrap_or_else(fallback_home)
}

/// The generated / regenerable working tree, `<streamlib-home>/.streamlib`.
/// Counterpart to the read-only `<streamlib-home>/packages` source: holds
/// the built-package cache, Python venvs / uv cache, per-runtime data +
/// logs, git resolver checkouts, and the installed-packages manifest. It is
/// gitignored, so collocating it in a dev workspace doesn't litter the tree.
pub fn get_streamlib_data_dir() -> PathBuf {
    get_streamlib_home().join(".streamlib")
}

/// Walk up from the running binary to the first ancestor containing a
/// `packages/` directory — the streamlib install / workspace root. This
/// mirrors the runtime binary's package-source resolution; the difference
/// is that the home root honors the `STREAMLIB_HOME` override (above) while
/// package-source resolution does not (source is fixed, the working tree is
/// redirectable).
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
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Ensure the generated working tree and its standard subdirectories exist.
pub fn ensure_streamlib_home() -> std::io::Result<PathBuf> {
    let data = get_streamlib_data_dir();

    std::fs::create_dir_all(data.join("cache/wheels"))?;
    std::fs::create_dir_all(data.join("cache/uv"))?;
    std::fs::create_dir_all(data.join("cache/packages"))?;
    std::fs::create_dir_all(data.join("runtimes"))?;

    Ok(get_streamlib_home())
}

/// Get the path to the uv cache directory.
pub fn get_uv_cache_dir() -> PathBuf {
    get_streamlib_data_dir().join("cache/uv")
}

/// Get the path to a cached extracted package directory.
pub fn get_cached_package_dir(cache_key: &str) -> PathBuf {
    get_streamlib_data_dir()
        .join("cache/packages")
        .join(cache_key)
}

/// The installed-package cache slot for a package at a version — the single
/// source of the `{name}-{version}` cache-key convention shared by `.slpkg`
/// extraction, registry resolution, orchestrator staging, and locked-run slot
/// derivation. A drift in any one of those sites would make locked runs look
/// in the wrong slot; route them all through here.
///
/// `app_modules_root` is the app root whose `streamlib_modules/` tree the
/// in-progress co-location relocation (#1506) will prefer over the shared
/// cache; it and the package org are threaded now so every deriver already
/// carries the app context, though this body returns the shared
/// `.streamlib/cache/packages/{name}-{version}` slot regardless of either.
pub fn installed_package_slot_dir(
    app_modules_root: Option<&Path>,
    pkg_ref: &PackageRef,
    version: SemVer,
) -> PathBuf {
    let _ = app_modules_root;
    get_cached_package_dir(&format!("{}-{}", pkg_ref.name.as_str(), version))
}

/// Temporary delegate over [`installed_package_slot_dir`] for the build
/// orchestrator's cross-crate call, which does not yet thread a
/// [`PackageRef`]/app root. Removed in the orchestrator-handoff step of the
/// #1506 relocation; no new caller.
pub fn get_cached_package_dir_for_name_version(
    package_name: &str,
    version: impl std::fmt::Display,
) -> PathBuf {
    get_cached_package_dir(&format!("{package_name}-{version}"))
}

/// Get the path to a runtime's directory.
pub fn get_runtime_dir(runtime_id: &str) -> PathBuf {
    get_streamlib_data_dir().join("runtimes").join(runtime_id)
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
    use streamlib_idents::{Org, Package};

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    /// Pins the seam's layout literal: the slot is always
    /// `<data-dir>/cache/packages/{name}-{version}`, regardless of org or the
    /// (currently unused) app-modules root. A relocation that changes this
    /// convention must update every deriver and this canary together.
    #[test]
    fn slot_dir_uses_name_dash_version_under_cache_packages() {
        let slot = installed_package_slot_dir(
            None,
            &pkg_ref("tatolab", "core"),
            SemVer::new(1, 2, 3),
        );
        let expected = get_streamlib_data_dir()
            .join("cache/packages")
            .join("core-1.2.3");
        assert_eq!(slot, expected);
    }

    /// write==read invariant: the seam is the single derivation, so a fixed
    /// `(pkg_ref, version)` yields one byte-identical slot no matter the app
    /// root — the property that lets a locked read find exactly the slot a
    /// `.slpkg` extract / registry reuse wrote. Also pins the temporary
    /// orchestrator delegate to the same output while it lives.
    #[test]
    fn slot_dir_is_app_root_invariant_and_matches_delegate() {
        let pkg = pkg_ref("tatolab", "core");
        let version = SemVer::new(4, 5, 6);

        let no_root = installed_package_slot_dir(None, &pkg, version);
        let with_root =
            installed_package_slot_dir(Some(Path::new("/some/app")), &pkg, version);
        assert_eq!(no_root, with_root, "app root must not move the slot");

        // The org is not part of the current key: a same-name package under a
        // different org derives the same slot (as the delegate, which has no
        // org, must too).
        let other_org = installed_package_slot_dir(None, &pkg_ref("acme", "core"), version);
        assert_eq!(no_root, other_org);
        assert_eq!(
            no_root,
            get_cached_package_dir_for_name_version("core", version)
        );
    }

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

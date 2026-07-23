// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

use streamlib_idents::app_modules::AppModulesDir;
use streamlib_idents::PackageRef;

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
/// ├── streamlib_modules/@org/name/      # installed / built package slots,
/// │                                     #   co-located under the APP root
/// │                                     #   (installed_package_slot_dir); each
/// │                                     #   Python slot carries its own `.venv/`
/// └── .streamlib/                       # generated working tree — get_streamlib_data_dir()
///     ├── cache/
///     │   └── uv/                        # uv PyPI cache      (Python packages only)
///     ├── logs/<runtime_id>-<ts>.jsonl  # per-runtime JSONL logs
///     └── resolver-cache/               # git / URL checkouts (Strategy::Git / Url)
/// ```
///
/// Each subdir is created on demand by its consumer — an all-Rust,
/// `Strategy::Path` graph populates `streamlib_modules/` and `logs/`. Installed
/// state is NOT a manifest here: a package's presence is its
/// `streamlib_modules/@org/name` slot, and the app's `streamlib.lock` records
/// how each was added.
pub fn get_streamlib_home() -> PathBuf {
    if let Ok(home) = std::env::var("STREAMLIB_HOME") {
        return PathBuf::from(home);
    }

    find_app_root().unwrap_or_else(fallback_home)
}

/// The generated / regenerable working tree, `<streamlib-home>/.streamlib`.
/// Counterpart to the read-only `<streamlib-home>/packages` source: holds the
/// Python uv cache, per-runtime data + logs, and git/URL resolver checkouts. It
/// is gitignored, so collocating it in a dev workspace doesn't litter the tree.
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

    std::fs::create_dir_all(data.join("cache/uv"))?;
    std::fs::create_dir_all(data.join("runtimes"))?;

    Ok(get_streamlib_home())
}

/// Get the path to the uv cache directory.
pub fn get_uv_cache_dir() -> PathBuf {
    get_streamlib_data_dir().join("cache/uv")
}

/// Environment override for the directory that contains the app's
/// `streamlib_modules/` folder — the GST_PLUGIN_PATH-style default a
/// daemon/host sets. A runtime override ([`set_app_modules_root_override`])
/// takes precedence.
pub(crate) const APP_MODULES_DIR_ENV: &str = "STREAMLIB_MODULES_DIR";

/// Process-wide override for the app-modules root, set via
/// [`Runner::set_app_modules_dir`]. `None` falls back to the env var, then the
/// process working directory.
///
/// [`Runner::set_app_modules_dir`]: crate::core::runtime::Runner::set_app_modules_dir
static APP_MODULES_ROOT_OVERRIDE: std::sync::RwLock<Option<PathBuf>> =
    std::sync::RwLock::new(None);

/// Tell the module loader which directory contains the app's
/// `streamlib_modules/` folder for lazy discovery, installed-slot derivation,
/// and locked-run resolution. `None` clears the override (back to env / cwd).
pub(crate) fn set_app_modules_root_override(root: Option<PathBuf>) {
    *APP_MODULES_ROOT_OVERRIDE
        .write()
        .expect("app-modules root override lock poisoned") = root;
}

/// The app-modules root: the runtime-set override, else the
/// `STREAMLIB_MODULES_DIR` env var, else the exact process working directory
/// (no walk-up). `None` only when the cwd is unresolvable and neither override
/// nor env is set — an `InstalledCache` resolution then has no slot to probe
/// and reports `ModuleNotFound`.
pub(crate) fn app_modules_root() -> Option<PathBuf> {
    if let Some(root) = APP_MODULES_ROOT_OVERRIDE
        .read()
        .expect("app-modules root override lock poisoned")
        .clone()
    {
        return Some(root);
    }
    if let Some(env) = std::env::var_os(APP_MODULES_DIR_ENV).filter(|env| !env.is_empty()) {
        return Some(PathBuf::from(env));
    }
    std::env::current_dir().ok()
}

/// The installed-package slot for a package — the single source of the
/// co-located `<app-root>/streamlib_modules/@org/name` convention shared by
/// `.slpkg` extraction, registry resolution, orchestrator staging, install,
/// and locked-run slot derivation. A drift in any one of those sites would
/// make locked runs look in the wrong slot; route them all through here.
///
/// `explicit_app_modules_root` pins the app root whose `streamlib_modules/`
/// tree owns the slot (the install/locked path threads the lockfile's parent
/// so write and read agree byte-for-byte). `None` resolves the app root via
/// [`app_modules_root`] (override > `STREAMLIB_MODULES_DIR` > cwd), the same
/// chain the module loader resolves against — so a `None` deriver lands in the
/// identical slot a resolved caller does. The slot is version-free: a package
/// occupies one `@org/name` dir; the pinned version is enforced against the
/// slot's manifest at the walker, not encoded in the path.
pub fn installed_package_slot_dir(
    explicit_app_modules_root: Option<&Path>,
    pkg_ref: &PackageRef,
) -> PathBuf {
    let app_root = explicit_app_modules_root
        .map(Path::to_path_buf)
        .or_else(app_modules_root)
        .unwrap_or_else(|| PathBuf::from("."));
    AppModulesDir::at(app_root).package_dir(pkg_ref)
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

    /// Pins the seam's layout: an explicit app root scopes the slot to
    /// `<app-root>/streamlib_modules/@org/name`, version-free. A relocation
    /// that changes this convention must update every deriver and this canary
    /// together.
    #[test]
    fn slot_dir_is_org_scoped_and_version_free_under_streamlib_modules() {
        let app_root = Path::new("/some/app");
        let slot = installed_package_slot_dir(Some(app_root), &pkg_ref("tatolab", "core"));
        let expected = app_root
            .join("streamlib_modules")
            .join("@tatolab")
            .join("core");
        assert_eq!(slot, expected);
    }

    /// write==read distinctness: the app root and the org each move the slot,
    /// so an install writing under one `(app-root, @org)` and a locked read
    /// under another never collide.
    #[test]
    fn slot_dir_moves_with_app_root_and_org() {
        let pkg = pkg_ref("tatolab", "core");

        let app_a = installed_package_slot_dir(Some(Path::new("/app/a")), &pkg);
        let app_b = installed_package_slot_dir(Some(Path::new("/app/b")), &pkg);
        assert_ne!(app_a, app_b, "the app root must move the slot");

        // A same-name package under a different org gets a distinct slot.
        let other_org =
            installed_package_slot_dir(Some(Path::new("/app/a")), &pkg_ref("acme", "core"));
        assert_ne!(app_a, other_org, "the org must move the slot");
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

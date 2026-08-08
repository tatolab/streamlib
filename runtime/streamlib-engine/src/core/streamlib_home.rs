// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use streamlib_idents::app_modules::AppModulesDir;
use streamlib_idents::PackageRef;

/// The streamlib home root — the parent of the generated `.streamlib/`
/// working tree ([`get_streamlib_data_dir`]).
///
/// Resolution order:
/// 1. `STREAMLIB_HOME` (explicit override — Docker/tests point it at a fixed
///    location or a tempdir).
/// 2. The process working directory, so a `dev`/`run` invocation keeps its
///    `./.streamlib/` beside the app being run.
/// 3. `.` as an infallible last resort. Never the running binary's directory
///    and never the user home directory — home state is project-local, not
///    collocated across a fleet.
pub fn get_streamlib_home() -> PathBuf {
    resolve_streamlib_home(
        std::env::var_os("STREAMLIB_HOME"),
        std::env::current_dir().ok(),
    )
}

/// Pure resolver behind [`get_streamlib_home`]: env override → cwd → `.`.
/// Split out so the resolution order is testable without mutating the process
/// environment or working directory. An empty `STREAMLIB_HOME` is ignored,
/// matching [`app_modules_root`]'s treatment of `STREAMLIB_MODULES_DIR`.
fn resolve_streamlib_home(env_override: Option<OsString>, cwd: Option<PathBuf>) -> PathBuf {
    if let Some(home) = env_override.filter(|home| !home.is_empty()) {
        return PathBuf::from(home);
    }
    cwd.unwrap_or_else(|| PathBuf::from("."))
}

/// The generated / regenerable working tree, `<streamlib-home>/.streamlib`.
/// Holds the Python uv cache, per-runtime data + logs, and git/URL resolver
/// checkouts. It is gitignored, so collocating it in a dev workspace doesn't
/// litter the tree.
pub fn get_streamlib_data_dir() -> PathBuf {
    get_streamlib_home().join(".streamlib")
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
    resolved_app_modules_dir(explicit_app_modules_root).package_dir(pkg_ref)
}

/// The app's `streamlib_modules/` handle the installed-slot convention resolves
/// against. Every derived path — the staging parent and the final
/// `@org/name` slot alike — comes off ONE instance, so the same-filesystem
/// invariant the staging promote's atomic rename depends on holds by
/// construction instead of by two lookups agreeing.
pub fn resolved_app_modules_dir(explicit_app_modules_root: Option<&Path>) -> AppModulesDir {
    AppModulesDir::at(
        explicit_app_modules_root
            .map(Path::to_path_buf)
            .or_else(app_modules_root)
            .unwrap_or_else(|| PathBuf::from(".")),
    )
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

    /// Pins the D2 resolution order: a non-empty `STREAMLIB_HOME` wins over
    /// the working directory. The override is the fixed-location path Docker
    /// and tests rely on.
    #[test]
    fn home_prefers_a_non_empty_env_override_over_cwd() {
        let home = resolve_streamlib_home(
            Some(OsString::from("/opt/streamlib")),
            Some(PathBuf::from("/some/cwd")),
        );
        assert_eq!(home, PathBuf::from("/opt/streamlib"));
    }

    /// An empty `STREAMLIB_HOME` is ignored — it names no location, so it
    /// falls through to the working directory rather than resolving to the
    /// empty path.
    #[test]
    fn home_ignores_an_empty_env_override_and_uses_cwd() {
        let home = resolve_streamlib_home(Some(OsString::new()), Some(PathBuf::from("/some/cwd")));
        assert_eq!(home, PathBuf::from("/some/cwd"));
    }

    /// No override: the working directory, so `./.streamlib/` lands beside the
    /// app being run. Never the running binary's directory or the user home.
    #[test]
    fn home_falls_back_to_cwd_then_dot() {
        assert_eq!(
            resolve_streamlib_home(None, Some(PathBuf::from("/some/cwd"))),
            PathBuf::from("/some/cwd")
        );
        // cwd unresolvable and no override → the infallible last resort.
        assert_eq!(resolve_streamlib_home(None, None), PathBuf::from("."));
    }
}

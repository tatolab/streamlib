// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

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

/// The host-toolchain discriminators that partition the installed-package
/// cache so two artifacts that are NOT interchangeable never share a slot.
///
/// Keying the slot on these — on top of the package identity — closes two
/// concrete collisions the bare `{name}-{version}` key had:
/// - a debug build and a release build of the same version overwriting one
///   another (`profile_label`), and
/// - a foreign-triple or foreign-ABI artifact loaded into a host it wasn't
///   built for (`host_triple` / `plugin_abi_version`).
///
/// The **write** side (the build orchestrator) fills this from the build
/// request's host triple, the linked plugin-ABI version, and its own cargo
/// profile; the **read** sides (`.slpkg` extraction, registry resolution,
/// locked-run slot derivation) reconstruct the identical context from the
/// running engine via [`host_package_cache_slot_context`] — so a slot a
/// build wrote and a later run reads can never disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCacheSlotContext {
    /// The rustc target triple the staged cdylib targets
    /// (e.g. `x86_64-unknown-linux-gnu`).
    pub host_triple: String,
    /// The plugin-ABI version the staged artifact was built against
    /// (`streamlib_plugin_abi::STREAMLIB_ABI_VERSION`).
    pub plugin_abi_version: u32,
    /// The cargo profile the artifact was built with (`dev` / `release`).
    pub profile_label: String,
}

/// Compose the flat slot-directory NAME for one package identity under one
/// host-toolchain [`PackageCacheSlotContext`]. The single source of the
/// cache-key convention shared by `.slpkg` extraction, registry resolution,
/// orchestrator staging, and locked-run slot derivation — a drift in any
/// one of those sites would make a locked run look in the wrong slot; route
/// them all through here.
///
/// `__` (double underscore) is a collision-free delimiter: an org and a
/// package name are `[a-z][a-z0-9-]*` (no underscore at all), a target
/// triple uses only single underscores (`x86_64`), a semver contains no
/// underscore, and the profile label is alphabetic — so `__` can never
/// appear inside a component and the join is unambiguous.
pub fn package_cache_slot_name(
    org: &str,
    package_name: &str,
    version: impl std::fmt::Display,
    context: &PackageCacheSlotContext,
) -> String {
    format!(
        "{org}__{package_name}__{version}__{triple}__abi{abi}__{profile}",
        triple = context.host_triple,
        abi = context.plugin_abi_version,
        profile = context.profile_label,
    )
}

/// The installed-cache slot path for a package identity under a host
/// [`PackageCacheSlotContext`]. Thin join of [`get_cached_package_dir`] over
/// [`package_cache_slot_name`].
pub fn get_cached_package_dir_for_slot(
    org: &str,
    package_name: &str,
    version: impl std::fmt::Display,
    context: &PackageCacheSlotContext,
) -> PathBuf {
    get_cached_package_dir(&package_cache_slot_name(org, package_name, version, context))
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

    fn ctx(triple: &str, abi: u32, profile: &str) -> PackageCacheSlotContext {
        PackageCacheSlotContext {
            host_triple: triple.to_string(),
            plugin_abi_version: abi,
            profile_label: profile.to_string(),
        }
    }

    #[test]
    fn slot_name_partitions_by_org_triple_abi_and_profile() {
        let linux = ctx("x86_64-unknown-linux-gnu", 7, "release");
        // Same name+version, different org → distinct slots (cross-org
        // collision the bare `{name}-{version}` key had).
        assert_ne!(
            package_cache_slot_name("acme", "core", "1.0.0", &linux),
            package_cache_slot_name("tatolab", "core", "1.0.0", &linux),
        );
        // Same identity, debug vs release → distinct slots (the thrash the
        // profile-less key had).
        assert_ne!(
            package_cache_slot_name("tatolab", "core", "1.0.0", &linux),
            package_cache_slot_name(
                "tatolab",
                "core",
                "1.0.0",
                &ctx("x86_64-unknown-linux-gnu", 7, "dev")
            ),
        );
        // Same identity, different triple / ABI → distinct slots.
        assert_ne!(
            package_cache_slot_name("tatolab", "core", "1.0.0", &linux),
            package_cache_slot_name("tatolab", "core", "1.0.0", &ctx("aarch64-apple-darwin", 7, "release")),
        );
        assert_ne!(
            package_cache_slot_name("tatolab", "core", "1.0.0", &linux),
            package_cache_slot_name("tatolab", "core", "1.0.0", &ctx("x86_64-unknown-linux-gnu", 8, "release")),
        );
    }

    #[test]
    fn slot_name_is_a_single_flat_path_component() {
        // install.rs records `staged_dir.file_name()` and the InstalledCache
        // loader joins it back as one component — the slot name must never
        // introduce a path separator.
        let name = package_cache_slot_name(
            "tatolab",
            "h264",
            "0.4.0",
            &ctx("x86_64-unknown-linux-gnu", 7, "release"),
        );
        assert!(!name.contains('/'), "slot name must be one component: {name}");
        assert_eq!(
            get_cached_package_dir_for_slot(
                "tatolab",
                "h264",
                "0.4.0",
                &ctx("x86_64-unknown-linux-gnu", 7, "release")
            ),
            get_cached_package_dir(&name),
        );
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

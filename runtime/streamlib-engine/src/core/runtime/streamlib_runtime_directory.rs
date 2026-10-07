// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one directory a runtime keeps what means nothing once its processes are
//! gone: the iceoryx2 domain, the surface-sharing socket, the local API socket, the node
//! registry and the processor interpreter lends.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::RuntimeUniqueId;
use crate::core::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};
use crate::core::error::{Error, Result};

/// The folder the resolver keeps inside `$XDG_RUNTIME_DIR`.
const STREAMLIB_FOLDER_INSIDE_XDG_RUNTIME_DIR: &str = "streamlib";

/// The shared temporary directory the per-user fallback folder is created in.
const SHARED_TEMPORARY_DIRECTORY_FOR_THE_FALLBACK: &str = "/tmp";

/// The folder inside the runtime directory each lend holding only a linked
/// `tatolab/runtime/` is kept in.
const PROCESSOR_INTERPRETER_LENDS_FOLDER: &str = "processor-interpreter-lends";

/// How many bytes of the package directory's SHA-256 name its lend.
const PROCESSOR_INTERPRETER_LEND_NAME_DIGEST_BYTES: usize = 8;

/// Numbers each lend this process builds aside, so two built at once never
/// share a name.
static LENDS_BUILT_BY_THIS_PROCESS: AtomicU64 = AtomicU64::new(0);

/// Every permission bit a group or other could hold.
const GROUP_AND_OTHER_PERMISSION_BITS: u32 = 0o077;

/// A runtime directory that has been resolved and created, the shared-`/tmp` fallback also trust-checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamlibRuntimeDirectory {
    path: PathBuf,
}

impl StreamlibRuntimeDirectory {
    /// Resolve this process's runtime directory, creating it and refusing one that fails the check.
    pub fn resolve() -> Result<Self> {
        #[cfg(target_os = "linux")]
        let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
        #[cfg(not(target_os = "linux"))]
        let xdg_runtime_dir: Option<OsString> = None;

        resolve_streamlib_runtime_directory(
            xdg_runtime_dir,
            Path::new(SHARED_TEMPORARY_DIRECTORY_FOR_THE_FALLBACK),
            current_process_uid(),
        )
    }

    /// The resolved directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The root every engine-owned iceoryx2 node in this runtime is configured with.
    pub fn iceoryx2_domain_root(&self) -> PathBuf {
        self.path.join("iox2")
    }

    /// The folder control-plane-hosting runtimes publish their discovery entries into.
    pub fn node_registry_directory(&self) -> PathBuf {
        self.path.join("nodes")
    }

    /// The Unix socket a runtime's surface-sharing service listens on.
    pub fn surface_share_socket_path(&self, runtime_id: &RuntimeUniqueId) -> PathBuf {
        self.path.join(format!("surface-share-{runtime_id}.sock"))
    }

    /// The Unix socket a runtime hosting its local API serves it on.
    pub fn local_api_socket_path(&self, runtime_id: &RuntimeUniqueId) -> PathBuf {
        self.path.join(format!("local-api-{runtime_id}.sock"))
    }

    /// A lend directory holding `tatolab/runtime` — a symlink to
    /// `runtime_package_directory` — and nothing else, made once per package
    /// directory and reused.
    ///
    /// The directory a package was imported from can hold a whole
    /// site-packages, which on a processor interpreter's `PYTHONPATH` would sit
    /// ahead of its standard library and its project.
    pub fn processor_interpreter_lend_directory_holding_only_the_runtime_package(
        &self,
        runtime_package_directory: &Path,
    ) -> Result<PathBuf> {
        let lend_failure = |what_failed: String| {
            Error::Runtime(format!(
                "no processor interpreter lend could be made for the runtime package `{}`: \
                 {what_failed}",
                runtime_package_directory.display()
            ))
        };
        let runtime_package_directory = runtime_package_directory
            .canonicalize()
            .map_err(|unresolvable| lend_failure(format!("it does not resolve: {unresolvable}")))?;
        let lends_folder = self.path.join(PROCESSOR_INTERPRETER_LENDS_FOLDER);
        create_directory_and_its_missing_parents_at_mode(&lends_folder, OWNER_ONLY_DIRECTORY_MODE)
            .map_err(|uncreatable| {
                lend_failure(format!(
                    "`{}` could not be created: {uncreatable}",
                    lends_folder.display()
                ))
            })?;
        let lend_name = processor_interpreter_lend_name_of(&runtime_package_directory);
        let lend_directory = lends_folder.join(&lend_name);
        if a_lend_links_its_runtime_package_to(&lend_directory, &runtime_package_directory) {
            return Ok(lend_directory);
        }

        // Built aside and renamed into place, so a concurrent runtime sees
        // either no lend or a whole one.
        let lend_being_built = lends_folder.join(format!(
            "{lend_name}.being-built-{}-{}",
            std::process::id(),
            LENDS_BUILT_BY_THIS_PROCESS.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&lend_being_built);
        let built = std::fs::create_dir_all(lend_being_built.join("tatolab")).and_then(|()| {
            std::os::unix::fs::symlink(
                &runtime_package_directory,
                runtime_package_link_in(&lend_being_built),
            )
        });
        if let Err(unbuildable) = built {
            let _ = std::fs::remove_dir_all(&lend_being_built);
            return Err(lend_failure(format!(
                "`{}` could not be built: {unbuildable}",
                lend_being_built.display()
            )));
        }
        let renamed = std::fs::rename(&lend_being_built, &lend_directory);
        let _ = std::fs::remove_dir_all(&lend_being_built);
        match renamed {
            Ok(()) => Ok(lend_directory),
            Err(_)
                if a_lend_links_its_runtime_package_to(
                    &lend_directory,
                    &runtime_package_directory,
                ) =>
            {
                Ok(lend_directory)
            }
            Err(unrenamable) => Err(lend_failure(format!(
                "`{}` could not be put in place, and what stands there links another \
                 package directory; remove it: {unrenamable}",
                lend_directory.display()
            ))),
        }
    }
}

/// The `tatolab/runtime` link inside `lend_directory`.
fn runtime_package_link_in(lend_directory: &Path) -> PathBuf {
    lend_directory.join("tatolab").join("runtime")
}

/// Whether `lend_directory` already links its `tatolab/runtime` to
/// `runtime_package_directory`.
fn a_lend_links_its_runtime_package_to(
    lend_directory: &Path,
    runtime_package_directory: &Path,
) -> bool {
    std::fs::read_link(runtime_package_link_in(lend_directory))
        .is_ok_and(|linked_package_directory| linked_package_directory == runtime_package_directory)
}

/// The lend's folder name: a digest of the canonical package directory, so one
/// install maps to one lend.
fn processor_interpreter_lend_name_of(runtime_package_directory: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    Sha256::digest(runtime_package_directory.as_os_str().as_bytes())
        [..PROCESSOR_INTERPRETER_LEND_NAME_DIGEST_BYTES]
        .iter()
        .map(|digest_byte| format!("{digest_byte:02x}"))
        .collect()
}

/// The real uid of this process.
pub(crate) fn current_process_uid() -> u32 {
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

/// The resolver with its three inputs named, so every arm is testable without
/// touching the process environment or the machine's shared `/tmp`.
fn resolve_streamlib_runtime_directory(
    xdg_runtime_dir: Option<OsString>,
    shared_temporary_directory: &Path,
    uid: u32,
) -> Result<StreamlibRuntimeDirectory> {
    if let Some(xdg_runtime_dir) = xdg_runtime_dir.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(xdg_runtime_dir).join(STREAMLIB_FOLDER_INSIDE_XDG_RUNTIME_DIR);
        create_directory_and_its_missing_parents_at_mode(&path, OWNER_ONLY_DIRECTORY_MODE)
            .map_err(|source| runtime_directory_creation_failure(&path, source))?;
        return Ok(StreamlibRuntimeDirectory { path });
    }

    let path = shared_temporary_directory.join(format!("streamlib-{uid}"));
    match create_directory_and_its_missing_parents_at_mode(&path, OWNER_ONLY_DIRECTORY_MODE) {
        Ok(()) => {}
        // Whatever already stands at the path, a symlink included, is the
        // trust check's to name and refuse.
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(runtime_directory_creation_failure(&path, source)),
    }
    refuse_a_fallback_directory_this_uid_cannot_trust(&path, uid)?;
    Ok(StreamlibRuntimeDirectory { path })
}

fn runtime_directory_creation_failure(path: &Path, source: std::io::Error) -> Error {
    Error::Runtime(format!(
        "the StreamLib runtime directory {} could not be created: {source}",
        path.display()
    ))
}

/// The fallback lives in a directory every user can write, so it is trusted only
/// as a real directory the uid owns with no group or other bits.
fn refuse_a_fallback_directory_this_uid_cannot_trust(path: &Path, uid: u32) -> Result<()> {
    let refusal = |what_is_wrong: String| {
        Error::Runtime(format!(
            "the StreamLib runtime directory {} cannot be trusted: {what_is_wrong}. \
             Remove it, or set XDG_RUNTIME_DIR to a directory only this user can reach",
            path.display()
        ))
    };
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| refusal(format!("it could not be inspected ({source})")))?;
    if metadata.file_type().is_symlink() {
        return Err(refusal("it is a symlink, not a directory".to_string()));
    }
    if !metadata.is_dir() {
        return Err(refusal("it is not a directory".to_string()));
    }
    if metadata.uid() != uid {
        return Err(refusal(format!(
            "it is owned by uid {}, not uid {uid}",
            metadata.uid()
        )));
    }
    let group_and_other_bits = metadata.mode() & GROUP_AND_OTHER_PERMISSION_BITS;
    if group_and_other_bits != 0 {
        return Err(refusal(format!(
            "its mode is {:o}, which grants group or other permissions",
            metadata.mode() & 0o7777
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fallback_path_for(shared_temporary_directory: &Path) -> PathBuf {
        shared_temporary_directory.join(format!("streamlib-{}", current_process_uid()))
    }

    fn refusal_text(outcome: Result<StreamlibRuntimeDirectory>) -> String {
        match outcome {
            Ok(directory) => panic!(
                "the resolver must refuse, but it resolved {}",
                directory.path().display()
            ),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn a_set_xdg_runtime_dir_resolves_to_its_streamlib_folder() {
        let xdg_runtime_dir =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let shared_temporary_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory(
            Some(xdg_runtime_dir.path().as_os_str().to_owned()),
            shared_temporary_directory.path(),
            current_process_uid(),
        )
        .unwrap();

        assert_eq!(directory.path(), xdg_runtime_dir.path().join("streamlib"));
        assert!(directory.path().is_dir());
        assert!(!fallback_path_for(shared_temporary_directory.path()).exists());
    }

    #[test]
    fn an_empty_xdg_runtime_dir_takes_the_per_user_fallback() {
        let shared_temporary_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory(
            Some(OsString::new()),
            shared_temporary_directory.path(),
            current_process_uid(),
        )
        .unwrap();

        assert_eq!(
            directory.path(),
            fallback_path_for(shared_temporary_directory.path())
        );
    }

    #[test]
    fn an_unset_xdg_runtime_dir_creates_an_owner_only_fallback() {
        let shared_temporary_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            current_process_uid(),
        )
        .unwrap();

        let metadata = std::fs::symlink_metadata(directory.path()).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.uid(), current_process_uid());
        assert_eq!(metadata.mode() & 0o777, 0o700);
    }

    #[test]
    fn a_fallback_that_already_exists_and_passes_the_check_is_taken_as_it_is() {
        let shared_temporary_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let fallback = fallback_path_for(shared_temporary_directory.path());
        create_directory_and_its_missing_parents_at_mode(&fallback, OWNER_ONLY_DIRECTORY_MODE)
            .unwrap();
        std::fs::write(fallback.join("left-by-an-earlier-run"), b"").unwrap();

        let directory = resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            current_process_uid(),
        )
        .unwrap();

        assert_eq!(directory.path(), fallback);
        assert!(fallback.join("left-by-an-earlier-run").exists());
    }

    #[test]
    fn a_fallback_that_is_a_symlink_is_refused_by_name() {
        let shared_temporary_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let somewhere_else =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let fallback = fallback_path_for(shared_temporary_directory.path());
        std::os::unix::fs::symlink(somewhere_else.path(), &fallback).unwrap();

        let refusal = refusal_text(resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            current_process_uid(),
        ));

        assert!(
            refusal.contains(&fallback.display().to_string()),
            "{refusal}"
        );
        assert!(refusal.contains("symlink"), "{refusal}");
    }

    #[test]
    fn a_fallback_owned_by_another_uid_is_refused_by_name() {
        let shared_temporary_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let this_uid = current_process_uid();
        let another_uid = this_uid.wrapping_add(1);
        let fallback = shared_temporary_directory
            .path()
            .join(format!("streamlib-{another_uid}"));
        create_directory_and_its_missing_parents_at_mode(&fallback, OWNER_ONLY_DIRECTORY_MODE)
            .unwrap();

        let refusal = refusal_text(resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            another_uid,
        ));

        assert!(
            refusal.contains(&fallback.display().to_string()),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("owned by uid {this_uid}, not uid {another_uid}")),
            "{refusal}"
        );
    }

    #[test]
    fn a_fallback_open_to_other_users_is_refused_by_name_at_0755() {
        a_fallback_at_this_mode_is_refused_naming_it(0o755);
    }

    #[test]
    fn a_fallback_open_to_its_group_is_refused_by_name_at_0770() {
        a_fallback_at_this_mode_is_refused_naming_it(0o770);
    }

    fn a_fallback_at_this_mode_is_refused_naming_it(mode: u32) {
        let shared_temporary_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let fallback = fallback_path_for(shared_temporary_directory.path());
        std::fs::create_dir(&fallback).unwrap();
        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(mode)).unwrap();

        let refusal = refusal_text(resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            current_process_uid(),
        ));

        assert!(
            refusal.contains(&fallback.display().to_string()),
            "{refusal}"
        );
        assert!(refusal.contains(&format!("mode is {mode:o}")), "{refusal}");
    }

    #[test]
    fn every_live_file_the_runtime_keeps_sits_inside_the_one_directory() {
        let directory = StreamlibRuntimeDirectory {
            path: PathBuf::from("/tmp/streamlib-1000"),
        };

        assert_eq!(
            directory.iceoryx2_domain_root(),
            PathBuf::from("/tmp/streamlib-1000/iox2")
        );
        assert_eq!(
            directory.node_registry_directory(),
            PathBuf::from("/tmp/streamlib-1000/nodes")
        );
        assert_eq!(
            directory.surface_share_socket_path(&RuntimeUniqueId::from("Rabc")),
            PathBuf::from("/tmp/streamlib-1000/surface-share-Rabc.sock")
        );
        assert_eq!(
            directory.local_api_socket_path(&RuntimeUniqueId::from("Rabc")),
            PathBuf::from("/tmp/streamlib-1000/local-api-Rabc.sock")
        );
    }

    /// A site-packages holding the runtime package beside what else a venv
    /// installs, and a runtime directory to lend it from.
    struct SitePackagesHoldingTheRuntimePackage {
        site_packages: tempfile::TempDir,
        runtime_directory_root: tempfile::TempDir,
    }

    impl SitePackagesHoldingTheRuntimePackage {
        fn installed() -> Self {
            let site_packages =
                crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
            let runtime_package_directory = site_packages.path().join("tatolab").join("runtime");
            std::fs::create_dir_all(&runtime_package_directory).unwrap();
            std::fs::write(runtime_package_directory.join("__init__.py"), "").unwrap();
            std::fs::create_dir_all(site_packages.path().join("enum")).unwrap();
            std::fs::write(site_packages.path().join("utils.py"), "").unwrap();
            std::fs::create_dir_all(site_packages.path().join("tatolab").join("stream")).unwrap();
            Self {
                site_packages,
                runtime_directory_root:
                    crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap(),
            }
        }

        fn runtime_package_directory(&self) -> PathBuf {
            self.site_packages.path().join("tatolab").join("runtime")
        }

        fn runtime_directory(&self) -> StreamlibRuntimeDirectory {
            StreamlibRuntimeDirectory {
                path: self.runtime_directory_root.path().to_path_buf(),
            }
        }
    }

    fn names_inside(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_lend_holds_only_a_tatolab_runtime_link_to_the_imported_package() {
        let installed = SitePackagesHoldingTheRuntimePackage::installed();

        let lend_directory = installed
            .runtime_directory()
            .processor_interpreter_lend_directory_holding_only_the_runtime_package(
                &installed.runtime_package_directory(),
            )
            .unwrap();

        assert!(lend_directory.starts_with(installed.runtime_directory_root.path()));
        assert_eq!(names_inside(&lend_directory), ["tatolab"]);
        assert_eq!(names_inside(&lend_directory.join("tatolab")), ["runtime"]);
        assert_eq!(
            std::fs::read_link(lend_directory.join("tatolab").join("runtime")).unwrap(),
            installed
                .runtime_package_directory()
                .canonicalize()
                .unwrap()
        );
        assert!(
            lend_directory
                .join("tatolab")
                .join("runtime")
                .join("__init__.py")
                .is_file()
        );
    }

    #[test]
    fn one_package_directory_is_lent_from_one_lend_and_another_from_its_own() {
        let installed = SitePackagesHoldingTheRuntimePackage::installed();
        let another_install = SitePackagesHoldingTheRuntimePackage::installed();
        let runtime_directory = installed.runtime_directory();

        let first_lend = runtime_directory
            .processor_interpreter_lend_directory_holding_only_the_runtime_package(
                &installed.runtime_package_directory(),
            )
            .unwrap();
        let second_lend = runtime_directory
            .processor_interpreter_lend_directory_holding_only_the_runtime_package(
                &installed.runtime_package_directory(),
            )
            .unwrap();
        let another_installs_lend = runtime_directory
            .processor_interpreter_lend_directory_holding_only_the_runtime_package(
                &another_install.runtime_package_directory(),
            )
            .unwrap();

        assert_eq!(first_lend, second_lend);
        assert_ne!(first_lend, another_installs_lend);
        assert_eq!(
            names_inside(
                &runtime_directory
                    .path()
                    .join(PROCESSOR_INTERPRETER_LENDS_FOLDER)
            )
            .len(),
            2,
            "nothing built aside is left behind"
        );
    }

    #[test]
    fn a_runtime_package_directory_that_does_not_exist_is_refused_naming_it() {
        let installed = SitePackagesHoldingTheRuntimePackage::installed();
        let missing_package_directory = installed.site_packages.path().join("no-such-package");

        let refusal = installed
            .runtime_directory()
            .processor_interpreter_lend_directory_holding_only_the_runtime_package(
                &missing_package_directory,
            )
            .unwrap_err()
            .to_string();

        assert!(
            refusal.contains(&missing_package_directory.display().to_string()),
            "{refusal}"
        );
    }
}

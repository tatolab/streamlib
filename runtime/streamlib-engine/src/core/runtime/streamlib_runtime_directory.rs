// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one directory a runtime keeps what means nothing once its processes are
//! gone: the iceoryx2 domain, the surface-sharing socket and the node registry.

use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::core::error::{Error, Result};

/// The folder the resolver keeps inside `$XDG_RUNTIME_DIR`.
const STREAMLIB_FOLDER_INSIDE_XDG_RUNTIME_DIR: &str = "streamlib";

/// The shared temporary directory the per-user fallback folder is created in.
const SHARED_TEMPORARY_DIRECTORY_FOR_THE_FALLBACK: &str = "/tmp";

/// Owner-only: read, write and search for the uid, nothing for group or other.
const OWNER_ONLY_DIRECTORY_MODE: u32 = 0o700;

/// Every permission bit a group or other could hold.
const GROUP_AND_OTHER_PERMISSION_BITS: u32 = 0o077;

/// A runtime directory that has been resolved, created and checked.
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
    pub fn surface_share_socket_path(&self, runtime_id: &str) -> PathBuf {
        self.path.join(format!("surface-share-{runtime_id}.sock"))
    }
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
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(OWNER_ONLY_DIRECTORY_MODE)
            .create(&path)
            .map_err(|source| {
                Error::Runtime(format!(
                    "the StreamLib runtime directory {} could not be created: {source}",
                    path.display()
                ))
            })?;
        return Ok(StreamlibRuntimeDirectory { path });
    }

    let path = shared_temporary_directory.join(format!("streamlib-{uid}"));
    match std::fs::DirBuilder::new()
        .mode(OWNER_ONLY_DIRECTORY_MODE)
        .create(&path)
    {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(Error::Runtime(format!(
                "the StreamLib runtime directory {} could not be created: {source}",
                path.display()
            )));
        }
    }
    refuse_a_fallback_directory_this_uid_cannot_trust(&path, uid)?;
    Ok(StreamlibRuntimeDirectory { path })
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
        let xdg_runtime_dir = tempfile::tempdir().unwrap();
        let shared_temporary_directory = tempfile::tempdir().unwrap();

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
        let shared_temporary_directory = tempfile::tempdir().unwrap();

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
        let shared_temporary_directory = tempfile::tempdir().unwrap();

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
        let shared_temporary_directory = tempfile::tempdir().unwrap();
        let fallback = fallback_path_for(shared_temporary_directory.path());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&fallback)
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
        let shared_temporary_directory = tempfile::tempdir().unwrap();
        let somewhere_else = tempfile::tempdir().unwrap();
        std::fs::set_permissions(somewhere_else.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let fallback = fallback_path_for(shared_temporary_directory.path());
        std::os::unix::fs::symlink(somewhere_else.path(), &fallback).unwrap();

        let refusal = refusal_text(resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            current_process_uid(),
        ));

        assert!(refusal.contains(&fallback.display().to_string()), "{refusal}");
        assert!(refusal.contains("symlink"), "{refusal}");
    }

    #[test]
    fn a_fallback_owned_by_another_uid_is_refused_by_name() {
        let shared_temporary_directory = tempfile::tempdir().unwrap();
        let this_uid = current_process_uid();
        let another_uid = this_uid.wrapping_add(1);
        let fallback = shared_temporary_directory
            .path()
            .join(format!("streamlib-{another_uid}"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&fallback)
            .unwrap();

        let refusal = refusal_text(resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            another_uid,
        ));

        assert!(refusal.contains(&fallback.display().to_string()), "{refusal}");
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
        let shared_temporary_directory = tempfile::tempdir().unwrap();
        let fallback = fallback_path_for(shared_temporary_directory.path());
        std::fs::create_dir(&fallback).unwrap();
        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(mode)).unwrap();

        let refusal = refusal_text(resolve_streamlib_runtime_directory(
            None,
            shared_temporary_directory.path(),
            current_process_uid(),
        ));

        assert!(refusal.contains(&fallback.display().to_string()), "{refusal}");
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
            directory.surface_share_socket_path("Rabc"),
            PathBuf::from("/tmp/streamlib-1000/surface-share-Rabc.sock")
        );
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one directory a runtime keeps what means nothing once its processes are
//! gone: the iceoryx2 domain, the surface-sharing socket, the local API socket and the node
//! registry.

// A test build resolves under its machine root; the real resolvers stay compiled for their tests.
#![cfg_attr(feature = "machine-directories-under-a-test-root", allow(dead_code))]

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};

/// The folder the resolver keeps inside `$XDG_RUNTIME_DIR`.
const STREAMLIB_FOLDER_INSIDE_XDG_RUNTIME_DIR: &str = "streamlib";

/// The shared temporary directory the per-user fallback folder is created in.
const SHARED_TEMPORARY_DIRECTORY_FOR_THE_FALLBACK: &str = "/tmp";

/// Every permission bit a group or other could hold.
const GROUP_AND_OTHER_PERMISSION_BITS: u32 = 0o077;

/// Why a runtime directory was refused, naming the directory.
#[derive(Debug, thiserror::Error)]
pub enum StreamlibRuntimeDirectoryRefusal {
    /// The directory, or a missing parent of it, could not be created.
    #[error("the StreamLib runtime directory {path} could not be created: {source}")]
    CouldNotBeCreated {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The shared-`/tmp` fallback is not a real directory this uid owns with no group or other bits.
    #[error(
        "the StreamLib runtime directory {path} cannot be trusted: {what_is_wrong}. \
         Remove it, or set XDG_RUNTIME_DIR to a directory only this user can reach"
    )]
    CannotBeTrusted {
        path: PathBuf,
        what_is_wrong: String,
    },
    /// This test build's machine root was refused.
    #[cfg(feature = "machine-directories-under-a-test-root")]
    #[error(transparent)]
    TestMachineRoot(#[from] crate::machine_directories_test_root::TestMachineRootRefusal),
}

/// The one directory a runtime keeps its live files in, resolved for the runtime or for a reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamlibRuntimeDirectory {
    path: PathBuf,
}

impl StreamlibRuntimeDirectory {
    /// Resolve this process's runtime directory, creating it and refusing one that fails the check.
    pub fn resolve() -> Result<Self, StreamlibRuntimeDirectoryRefusal> {
        #[cfg(feature = "machine-directories-under-a-test-root")]
        {
            let path =
                crate::machine_directories_test_root::TestMachineRoot::from_the_environment()?
                    .runtime_directory();
            create_directory_and_its_missing_parents_at_mode(&path, OWNER_ONLY_DIRECTORY_MODE)
                .map_err(
                    |source| StreamlibRuntimeDirectoryRefusal::CouldNotBeCreated {
                        path: path.clone(),
                        source,
                    },
                )?;
            Ok(StreamlibRuntimeDirectory { path })
        }
        #[cfg(not(feature = "machine-directories-under-a-test-root"))]
        resolve_streamlib_runtime_directory_for_the_runtime(
            std::env::var_os("XDG_RUNTIME_DIR"),
            cfg!(target_os = "linux"),
            Path::new(SHARED_TEMPORARY_DIRECTORY_FOR_THE_FALLBACK),
            current_process_uid(),
        )
    }

    /// Resolve the runtime directory this user's runtimes use, creating nothing, and refusing
    /// a shared-`/tmp` fallback that exists and fails the check.
    pub fn resolve_for_a_reader_without_creating() -> Result<Self, StreamlibRuntimeDirectoryRefusal>
    {
        #[cfg(feature = "machine-directories-under-a-test-root")]
        {
            Ok(StreamlibRuntimeDirectory {
                path: crate::machine_directories_test_root::TestMachineRoot::from_the_environment(
                )?
                .runtime_directory(),
            })
        }
        #[cfg(not(feature = "machine-directories-under-a-test-root"))]
        resolve_streamlib_runtime_directory_for_a_reader_without_creating(
            std::env::var_os("XDG_RUNTIME_DIR"),
            cfg!(target_os = "linux"),
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

    /// The Unix socket the runtime with `runtime_id` serves its surface-sharing service on.
    pub fn surface_share_socket_path(&self, runtime_id: &str) -> PathBuf {
        self.path.join(format!("surface-share-{runtime_id}.sock"))
    }

    /// The Unix socket the runtime with `runtime_id` serves its local API on.
    pub fn local_api_socket_path_for_runtime_id(&self, runtime_id: &str) -> PathBuf {
        self.path.join(format!("local-api-{runtime_id}.sock"))
    }

    /// The Unix socket the machine's runtime serves its local API on, at a fixed path.
    pub fn local_api_socket_path(&self) -> PathBuf {
        self.path.join("local-api.sock")
    }
}

/// The real uid of this process.
pub fn current_process_uid() -> u32 {
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

/// `$XDG_RUNTIME_DIR/streamlib`, when the platform honours that variable and it is set and non-empty.
fn the_streamlib_folder_inside_xdg_runtime_dir(
    xdg_runtime_dir: Option<OsString>,
    platform_is_linux: bool,
) -> Option<PathBuf> {
    xdg_runtime_dir
        .filter(|value| platform_is_linux && !value.is_empty())
        .map(|value| PathBuf::from(value).join(STREAMLIB_FOLDER_INSIDE_XDG_RUNTIME_DIR))
}

fn the_per_user_fallback_in(shared_temporary_directory: &Path, uid: u32) -> PathBuf {
    shared_temporary_directory.join(format!("streamlib-{uid}"))
}

/// The runtime's resolver with its inputs named, so every arm is testable without
/// touching the process environment or the machine's shared `/tmp`.
fn resolve_streamlib_runtime_directory_for_the_runtime(
    xdg_runtime_dir: Option<OsString>,
    platform_is_linux: bool,
    shared_temporary_directory: &Path,
    uid: u32,
) -> Result<StreamlibRuntimeDirectory, StreamlibRuntimeDirectoryRefusal> {
    if let Some(path) =
        the_streamlib_folder_inside_xdg_runtime_dir(xdg_runtime_dir, platform_is_linux)
    {
        create_directory_and_its_missing_parents_at_mode(&path, OWNER_ONLY_DIRECTORY_MODE)
            .map_err(
                |source| StreamlibRuntimeDirectoryRefusal::CouldNotBeCreated {
                    path: path.clone(),
                    source,
                },
            )?;
        return Ok(StreamlibRuntimeDirectory { path });
    }

    let path = the_per_user_fallback_in(shared_temporary_directory, uid);
    match create_directory_and_its_missing_parents_at_mode(&path, OWNER_ONLY_DIRECTORY_MODE) {
        Ok(()) => {}
        // Whatever already stands at the path, a symlink included, is the
        // trust check's to name and refuse.
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(StreamlibRuntimeDirectoryRefusal::CouldNotBeCreated { path, source });
        }
    }
    refuse_a_fallback_directory_this_uid_cannot_trust(&path, uid)?;
    Ok(StreamlibRuntimeDirectory { path })
}

/// The reader's resolver with its inputs named. A reader creates nothing: the
/// `$XDG_RUNTIME_DIR` folder sits in a per-user directory and is returned
/// unchecked, and a fallback that does not exist yet holds nothing to trust.
fn resolve_streamlib_runtime_directory_for_a_reader_without_creating(
    xdg_runtime_dir: Option<OsString>,
    platform_is_linux: bool,
    shared_temporary_directory: &Path,
    uid: u32,
) -> Result<StreamlibRuntimeDirectory, StreamlibRuntimeDirectoryRefusal> {
    if let Some(path) =
        the_streamlib_folder_inside_xdg_runtime_dir(xdg_runtime_dir, platform_is_linux)
    {
        return Ok(StreamlibRuntimeDirectory { path });
    }

    let path = the_per_user_fallback_in(shared_temporary_directory, uid);
    let nothing_stands_at_the_fallback = matches!(
        std::fs::symlink_metadata(&path),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound
    );
    if !nothing_stands_at_the_fallback {
        refuse_a_fallback_directory_this_uid_cannot_trust(&path, uid)?;
    }
    Ok(StreamlibRuntimeDirectory { path })
}

/// The fallback lives in a directory every user can write, so it is trusted only
/// as a real directory the uid owns with no group or other bits.
fn refuse_a_fallback_directory_this_uid_cannot_trust(
    path: &Path,
    uid: u32,
) -> Result<(), StreamlibRuntimeDirectoryRefusal> {
    let refusal = |what_is_wrong: String| StreamlibRuntimeDirectoryRefusal::CannotBeTrusted {
        path: path.to_path_buf(),
        what_is_wrong,
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
    use crate::test_support::a_temporary_directory_at_owner_only_mode;
    use std::os::unix::fs::PermissionsExt;

    /// Which resolver a refusal or placement test drives.
    #[derive(Debug, Clone, Copy)]
    enum ResolverArm {
        TheRuntimes,
        AReadersWithoutCreating,
    }

    const BOTH_RESOLVER_ARMS: [ResolverArm; 2] = [
        ResolverArm::TheRuntimes,
        ResolverArm::AReadersWithoutCreating,
    ];

    fn resolve_with(
        arm: ResolverArm,
        xdg_runtime_dir: Option<OsString>,
        platform_is_linux: bool,
        shared_temporary_directory: &Path,
        uid: u32,
    ) -> Result<StreamlibRuntimeDirectory, StreamlibRuntimeDirectoryRefusal> {
        match arm {
            ResolverArm::TheRuntimes => resolve_streamlib_runtime_directory_for_the_runtime(
                xdg_runtime_dir,
                platform_is_linux,
                shared_temporary_directory,
                uid,
            ),
            ResolverArm::AReadersWithoutCreating => {
                resolve_streamlib_runtime_directory_for_a_reader_without_creating(
                    xdg_runtime_dir,
                    platform_is_linux,
                    shared_temporary_directory,
                    uid,
                )
            }
        }
    }

    fn fallback_path_for(shared_temporary_directory: &Path) -> PathBuf {
        shared_temporary_directory.join(format!("streamlib-{}", current_process_uid()))
    }

    fn refusal_text(
        outcome: Result<StreamlibRuntimeDirectory, StreamlibRuntimeDirectoryRefusal>,
    ) -> String {
        match outcome {
            Ok(directory) => panic!(
                "the resolver must refuse, but it resolved {}",
                directory.path().display()
            ),
            Err(refusal) => refusal.to_string(),
        }
    }

    #[test]
    fn a_set_xdg_runtime_dir_resolves_to_its_streamlib_folder() {
        let xdg_runtime_dir = a_temporary_directory_at_owner_only_mode().unwrap();
        let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory_for_the_runtime(
            Some(xdg_runtime_dir.path().as_os_str().to_owned()),
            true,
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
        for arm in BOTH_RESOLVER_ARMS {
            let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();

            let directory = resolve_with(
                arm,
                Some(OsString::new()),
                true,
                shared_temporary_directory.path(),
                current_process_uid(),
            )
            .unwrap();

            assert_eq!(
                directory.path(),
                fallback_path_for(shared_temporary_directory.path()),
                "{arm:?}"
            );
        }
    }

    #[test]
    fn an_unset_xdg_runtime_dir_creates_an_owner_only_fallback() {
        let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory_for_the_runtime(
            None,
            true,
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
        for arm in BOTH_RESOLVER_ARMS {
            let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();
            let fallback = fallback_path_for(shared_temporary_directory.path());
            create_directory_and_its_missing_parents_at_mode(&fallback, OWNER_ONLY_DIRECTORY_MODE)
                .unwrap();
            std::fs::write(fallback.join("left-by-an-earlier-run"), b"").unwrap();

            let directory = resolve_with(
                arm,
                None,
                true,
                shared_temporary_directory.path(),
                current_process_uid(),
            )
            .unwrap();

            assert_eq!(directory.path(), fallback, "{arm:?}");
            assert!(fallback.join("left-by-an-earlier-run").exists(), "{arm:?}");
        }
    }

    #[test]
    fn a_fallback_that_is_a_symlink_is_refused_by_name() {
        for arm in BOTH_RESOLVER_ARMS {
            let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();
            let somewhere_else = a_temporary_directory_at_owner_only_mode().unwrap();
            let fallback = fallback_path_for(shared_temporary_directory.path());
            std::os::unix::fs::symlink(somewhere_else.path(), &fallback).unwrap();

            let refusal = refusal_text(resolve_with(
                arm,
                None,
                true,
                shared_temporary_directory.path(),
                current_process_uid(),
            ));

            assert!(
                refusal.contains(&fallback.display().to_string()),
                "{arm:?}: {refusal}"
            );
            assert!(refusal.contains("symlink"), "{arm:?}: {refusal}");
        }
    }

    #[test]
    fn a_fallback_owned_by_another_uid_is_refused_by_name() {
        for arm in BOTH_RESOLVER_ARMS {
            let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();
            let this_uid = current_process_uid();
            let another_uid = this_uid.wrapping_add(1);
            let fallback = shared_temporary_directory
                .path()
                .join(format!("streamlib-{another_uid}"));
            create_directory_and_its_missing_parents_at_mode(&fallback, OWNER_ONLY_DIRECTORY_MODE)
                .unwrap();

            let refusal = refusal_text(resolve_with(
                arm,
                None,
                true,
                shared_temporary_directory.path(),
                another_uid,
            ));

            assert!(
                refusal.contains(&fallback.display().to_string()),
                "{arm:?}: {refusal}"
            );
            assert!(
                refusal.contains(&format!("owned by uid {this_uid}, not uid {another_uid}")),
                "{arm:?}: {refusal}"
            );
        }
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
        for arm in BOTH_RESOLVER_ARMS {
            let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();
            let fallback = fallback_path_for(shared_temporary_directory.path());
            std::fs::create_dir(&fallback).unwrap();
            std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(mode)).unwrap();

            let refusal = refusal_text(resolve_with(
                arm,
                None,
                true,
                shared_temporary_directory.path(),
                current_process_uid(),
            ));

            assert!(
                refusal.contains(&fallback.display().to_string()),
                "{arm:?}: {refusal}"
            );
            assert!(
                refusal.contains(&format!("mode is {mode:o}")),
                "{arm:?}: {refusal}"
            );
        }
    }

    #[test]
    fn a_fallback_that_is_not_a_directory_is_refused_by_name() {
        for arm in BOTH_RESOLVER_ARMS {
            let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();
            let fallback = fallback_path_for(shared_temporary_directory.path());
            std::fs::write(&fallback, b"").unwrap();

            let refusal = refusal_text(resolve_with(
                arm,
                None,
                true,
                shared_temporary_directory.path(),
                current_process_uid(),
            ));

            assert!(
                refusal.contains(&fallback.display().to_string()),
                "{arm:?}: {refusal}"
            );
            assert!(refusal.contains("not a directory"), "{arm:?}: {refusal}");
        }
    }

    #[test]
    fn a_reader_takes_a_set_xdg_runtime_dirs_streamlib_folder_without_creating_or_checking_it() {
        let xdg_runtime_dir = a_temporary_directory_at_owner_only_mode().unwrap();
        let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();
        std::fs::set_permissions(
            xdg_runtime_dir.path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let directory = resolve_streamlib_runtime_directory_for_a_reader_without_creating(
            Some(xdg_runtime_dir.path().as_os_str().to_owned()),
            true,
            shared_temporary_directory.path(),
            current_process_uid(),
        )
        .unwrap();

        assert_eq!(directory.path(), xdg_runtime_dir.path().join("streamlib"));
        assert!(!directory.path().exists());
        assert_eq!(
            directory.node_registry_directory(),
            xdg_runtime_dir.path().join("streamlib").join("nodes")
        );
        assert!(!fallback_path_for(shared_temporary_directory.path()).exists());
    }

    #[test]
    fn a_reader_with_xdg_runtime_dir_unset_takes_the_per_user_fallback() {
        let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory_for_a_reader_without_creating(
            None,
            true,
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
    fn a_reader_off_linux_takes_the_per_user_fallback_whatever_xdg_runtime_dir_says() {
        let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory_for_a_reader_without_creating(
            Some(OsString::from("/run/user/1000")),
            false,
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
    fn a_fallback_that_does_not_exist_yet_is_resolved_for_a_reader_without_being_created() {
        let shared_temporary_directory = a_temporary_directory_at_owner_only_mode().unwrap();

        let directory = resolve_streamlib_runtime_directory_for_a_reader_without_creating(
            None,
            true,
            shared_temporary_directory.path(),
            current_process_uid(),
        )
        .unwrap();

        assert_eq!(
            directory.path(),
            fallback_path_for(shared_temporary_directory.path())
        );
        assert!(!directory.path().exists());
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
        assert_eq!(
            directory.local_api_socket_path_for_runtime_id("Rabc"),
            PathBuf::from("/tmp/streamlib-1000/local-api-Rabc.sock")
        );
        assert_eq!(
            directory.local_api_socket_path(),
            PathBuf::from("/tmp/streamlib-1000/local-api.sock")
        );
    }
}

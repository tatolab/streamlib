// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one directory the machine's runtime keeps what must outlive it: the
//! record of each kept stream and the runtime's own log.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};

/// The folder kept inside `$XDG_STATE_HOME` or `~/.local/state` on Linux.
const TATOLAB_FOLDER_INSIDE_THE_LINUX_STATE_HOME: &str = "tatolab";

/// The user's own Application Support folder on macOS, relative to `$HOME`.
const TATOLAB_FOLDER_INSIDE_THE_MACOS_HOME: &str = "Library/Application Support/Tatolab";

/// `~/.local/state`, the XDG default for `$XDG_STATE_HOME`, relative to `$HOME`.
const XDG_STATE_HOME_DEFAULT_INSIDE_HOME: &str = ".local/state";

/// Why the state directory was refused, naming what is wrong.
#[derive(Debug, thiserror::Error)]
pub enum TatolabStateDirectoryRefusal {
    /// Neither `$XDG_STATE_HOME` (Linux) nor `$HOME` names a place for it.
    #[error(
        "the Tatolab state directory has no place: HOME is not set{xdg_state_home_clause}",
        xdg_state_home_clause = the_xdg_state_home_clause(.platform_reads_xdg_state_home)
    )]
    HomeIsNotSet { platform_reads_xdg_state_home: bool },
    /// `$HOME` is set to a relative path.
    #[error(
        "the Tatolab state directory has no place: HOME is {value:?}, which is not an absolute path"
    )]
    HomeIsNotAnAbsolutePath { value: OsString },
    /// Something other than a directory stands at the path.
    #[error("the Tatolab state directory {path} exists and is not a directory")]
    IsNotADirectory { path: PathBuf },
    /// The directory, or a missing parent of it, could not be created.
    #[error("the Tatolab state directory {path} could not be created: {source}")]
    CouldNotBeCreated {
        path: PathBuf,
        source: std::io::Error,
    },
    /// This test build's machine root was refused.
    #[cfg(feature = "machine-directories-under-a-test-root")]
    #[error(transparent)]
    TestMachineRoot(#[from] crate::machine_directories_test_root::TestMachineRootRefusal),
}

fn the_xdg_state_home_clause(platform_reads_xdg_state_home: &bool) -> &'static str {
    if *platform_reads_xdg_state_home {
        " and XDG_STATE_HOME is not set to an absolute path"
    } else {
        ""
    }
}

/// Which platform's placement rule the resolver follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateDirectoryPlacementRule {
    LinuxXdgStateHome,
    MacOsUsersApplicationSupport,
}

impl StateDirectoryPlacementRule {
    #[cfg_attr(feature = "machine-directories-under-a-test-root", allow(dead_code))]
    fn for_this_platform() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOsUsersApplicationSupport
        } else {
            Self::LinuxXdgStateHome
        }
    }
}

/// The machine runtime's state directory, resolved: present on disk unless resolved for a reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TatolabStateDirectory {
    path: PathBuf,
}

impl TatolabStateDirectory {
    /// Resolve this user's state directory, creating it owner-only if absent and
    /// refusing a path that stands and is not a directory.
    pub fn resolve() -> Result<Self, TatolabStateDirectoryRefusal> {
        tatolab_state_directory_present_at(this_users_tatolab_state_directory_path()?)
    }

    /// Resolve where this user's state directory is, creating nothing: a reader naming the
    /// directory, which may not exist.
    pub fn resolve_for_a_reader_without_creating() -> Result<Self, TatolabStateDirectoryRefusal> {
        Ok(TatolabStateDirectory {
            path: this_users_tatolab_state_directory_path()?,
        })
    }

    /// The resolved directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `<state>/streams/`, one record per kept stream.
    pub fn kept_streams_directory(&self) -> PathBuf {
        self.path.join("streams")
    }

    /// `<state>/logs/`, the runtime's own log, which belongs to no stream.
    pub fn runtime_log_directory(&self) -> PathBuf {
        self.path.join("logs")
    }
}

/// This user's state directory path, from the process environment.
fn this_users_tatolab_state_directory_path() -> Result<PathBuf, TatolabStateDirectoryRefusal> {
    #[cfg(feature = "machine-directories-under-a-test-root")]
    {
        Ok(
            crate::machine_directories_test_root::TestMachineRoot::from_the_environment()?
                .state_directory(),
        )
    }
    #[cfg(not(feature = "machine-directories-under-a-test-root"))]
    tatolab_state_directory_path_from(
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
        StateDirectoryPlacementRule::for_this_platform(),
    )
}

/// The resolver with its inputs named, so every arm is testable without the
/// process environment.
#[cfg_attr(feature = "machine-directories-under-a-test-root", allow(dead_code))]
fn tatolab_state_directory_path_from(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
    placement_rule: StateDirectoryPlacementRule,
) -> Result<PathBuf, TatolabStateDirectoryRefusal> {
    if placement_rule == StateDirectoryPlacementRule::LinuxXdgStateHome
        && let Some(xdg_state_home) = xdg_state_home
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    {
        return Ok(xdg_state_home.join(TATOLAB_FOLDER_INSIDE_THE_LINUX_STATE_HOME));
    }
    let home = home.filter(|value| !value.is_empty()).ok_or(
        TatolabStateDirectoryRefusal::HomeIsNotSet {
            platform_reads_xdg_state_home: placement_rule
                == StateDirectoryPlacementRule::LinuxXdgStateHome,
        },
    )?;
    let home_path = PathBuf::from(&home);
    if !home_path.is_absolute() {
        return Err(TatolabStateDirectoryRefusal::HomeIsNotAnAbsolutePath { value: home });
    }
    Ok(match placement_rule {
        StateDirectoryPlacementRule::LinuxXdgStateHome => home_path
            .join(XDG_STATE_HOME_DEFAULT_INSIDE_HOME)
            .join(TATOLAB_FOLDER_INSIDE_THE_LINUX_STATE_HOME),
        StateDirectoryPlacementRule::MacOsUsersApplicationSupport => {
            home_path.join(TATOLAB_FOLDER_INSIDE_THE_MACOS_HOME)
        }
    })
}

fn tatolab_state_directory_present_at(
    path: PathBuf,
) -> Result<TatolabStateDirectory, TatolabStateDirectoryRefusal> {
    if path.exists() && !path.is_dir() {
        return Err(TatolabStateDirectoryRefusal::IsNotADirectory { path });
    }
    create_directory_and_its_missing_parents_at_mode(&path, OWNER_ONLY_DIRECTORY_MODE).map_err(
        |source| TatolabStateDirectoryRefusal::CouldNotBeCreated {
            path: path.clone(),
            source,
        },
    )?;
    Ok(TatolabStateDirectory { path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::a_temporary_directory_at_owner_only_mode;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn linux_takes_an_absolute_xdg_state_home() {
        let path = tatolab_state_directory_path_from(
            Some(OsString::from("/var/state-home")),
            Some(OsString::from("/home/someone")),
            StateDirectoryPlacementRule::LinuxXdgStateHome,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/var/state-home/tatolab"));
    }

    #[test]
    fn linux_falls_back_to_home_when_xdg_state_home_is_unset_empty_or_relative() {
        for xdg_state_home in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative")),
        ] {
            let path = tatolab_state_directory_path_from(
                xdg_state_home.clone(),
                Some(OsString::from("/home/someone")),
                StateDirectoryPlacementRule::LinuxXdgStateHome,
            )
            .unwrap();
            assert_eq!(
                path,
                PathBuf::from("/home/someone/.local/state/tatolab"),
                "{xdg_state_home:?}"
            );
        }
    }

    #[test]
    fn macos_takes_the_users_application_support_whatever_xdg_state_home_says() {
        let path = tatolab_state_directory_path_from(
            Some(OsString::from("/var/state-home")),
            Some(OsString::from("/Users/someone")),
            StateDirectoryPlacementRule::MacOsUsersApplicationSupport,
        )
        .unwrap();
        assert_eq!(
            path,
            PathBuf::from("/Users/someone/Library/Application Support/Tatolab")
        );
    }

    #[test]
    fn no_home_is_refused_by_name() {
        for (placement_rule, names_xdg_state_home) in [
            (StateDirectoryPlacementRule::LinuxXdgStateHome, true),
            (
                StateDirectoryPlacementRule::MacOsUsersApplicationSupport,
                false,
            ),
        ] {
            for home in [None, Some(OsString::new())] {
                let refusal =
                    tatolab_state_directory_path_from(None, home, placement_rule).unwrap_err();
                let refusal = refusal.to_string();
                assert!(refusal.contains("HOME is not set"), "{refusal}");
                assert_eq!(
                    refusal.contains("XDG_STATE_HOME"),
                    names_xdg_state_home,
                    "{refusal}"
                );
            }
        }
    }

    #[test]
    fn a_relative_home_is_refused_by_name() {
        let refusal = tatolab_state_directory_path_from(
            None,
            Some(OsString::from("someone")),
            StateDirectoryPlacementRule::LinuxXdgStateHome,
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("HOME is \"someone\""), "{refusal}");
    }

    #[test]
    fn an_absent_state_directory_is_created_owner_only_with_its_missing_parents() {
        let home = a_temporary_directory_at_owner_only_mode().unwrap();
        let path = tatolab_state_directory_path_from(
            None,
            Some(home.path().as_os_str().to_owned()),
            StateDirectoryPlacementRule::LinuxXdgStateHome,
        )
        .unwrap();

        let state_directory = tatolab_state_directory_present_at(path.clone()).unwrap();

        assert_eq!(state_directory.path(), path);
        for level in [
            home.path().join(".local"),
            home.path().join(".local/state"),
            path.clone(),
        ] {
            let mode = std::fs::metadata(&level).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", level.display());
        }
        assert_eq!(
            state_directory.kept_streams_directory(),
            path.join("streams")
        );
        assert_eq!(state_directory.runtime_log_directory(), path.join("logs"));
    }

    #[test]
    fn a_state_directory_that_already_exists_is_taken_as_it_is() {
        let state_home = a_temporary_directory_at_owner_only_mode().unwrap();
        let path = state_home.path().join("tatolab");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
        std::fs::write(path.join("left-by-an-earlier-run"), b"").unwrap();

        tatolab_state_directory_present_at(path.clone()).unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o750
        );
        assert!(path.join("left-by-an-earlier-run").exists());
    }

    /// Set only in the child process the reader test re-runs itself in, to the scratch home.
    const READER_RESOLVE_CHILD_SCRATCH_HOME_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_STATE_DIRECTORY_READER_SCRATCH_HOME";

    #[test]
    fn a_reader_resolves_the_state_directory_without_creating_it() {
        if let Some(scratch_home) =
            std::env::var_os(READER_RESOLVE_CHILD_SCRATCH_HOME_ENVIRONMENT_VARIABLE)
        {
            let read_path = TatolabStateDirectory::resolve_for_a_reader_without_creating()
                .unwrap()
                .path()
                .to_path_buf();

            assert!(
                read_path.starts_with(&scratch_home),
                "{}",
                read_path.display()
            );
            assert!(!read_path.exists(), "{} was created", read_path.display());
            assert_eq!(TatolabStateDirectory::resolve().unwrap().path(), read_path);
            assert!(read_path.is_dir());
            return;
        }

        let scratch_home = tempfile::Builder::new()
            .prefix("tl-")
            .tempdir_in("/tmp")
            .unwrap();
        let child = crate::test_support::rerun_this_test_in_a_child_process_with_its_environment(
            "tatolab_state_directory::tests::a_reader_resolves_the_state_directory_without_creating_it",
            |child_command| {
                child_command
                    .env(
                        READER_RESOLVE_CHILD_SCRATCH_HOME_ENVIRONMENT_VARIABLE,
                        scratch_home.path(),
                    )
                    .env("TATOLAB_TEST_MACHINE_ROOT", scratch_home.path())
                    .env("HOME", scratch_home.path())
                    .env_remove("XDG_STATE_HOME");
            },
        );
        assert!(
            child.status.success(),
            "the reader arm failed in the child: {}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr),
        );
    }

    #[test]
    fn a_state_directory_path_that_is_not_a_directory_is_refused_by_name() {
        let state_home = a_temporary_directory_at_owner_only_mode().unwrap();
        let path = state_home.path().join("tatolab");
        std::fs::write(&path, b"").unwrap();

        let refusal = tatolab_state_directory_present_at(path.clone())
            .unwrap_err()
            .to_string();

        assert!(refusal.contains(&path.display().to_string()), "{refusal}");
        assert!(refusal.contains("is not a directory"), "{refusal}");
    }
}

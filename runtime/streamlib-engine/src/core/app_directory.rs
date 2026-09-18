// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which directory the app a runtime belongs to was written in.
//!
//! Engine code that names something after the app — an unnamed runtime on the
//! mesh, an unnamed virtual camera — keys on this rather than on the shell's
//! working directory, so the name follows the app and not where it was
//! launched from.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Set by the CLI launcher to the app's anchor directory, as a full path.
pub const APP_DIRECTORY_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_APP_DIRECTORY";

/// The entry directory a language host captured from its own interpreter.
static APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST: OnceLock<PathBuf> = OnceLock::new();

/// Record where the app's entry file was run from, for a host that knows it and
/// the engine cannot see.
///
/// The wheel calls this from `Runtime()`'s constructor with the directory it
/// captured off `sys.path[0]`, which is the only thing that tells a hand-run
/// `python app.py` apart from a `streamlib run`. The first call wins and there
/// is no way back — the entry file does not move while the process lives, which
/// is also why no test records one: doing so would rename every runtime
/// constructed later in the same binary.
pub fn record_the_app_entry_directory_the_language_host_captured(entry_directory: PathBuf) {
    let _ = APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST.set(entry_directory);
}

/// The directory of the app this runtime belongs to.
pub fn resolve_the_app_directory_this_runtime_belongs_to() -> PathBuf {
    resolve_app_directory(
        std::env::var_os(APP_DIRECTORY_ENVIRONMENT_VARIABLE),
        APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST
            .get()
            .map(PathBuf::as_path),
        std::env::current_dir().ok(),
    )
}

/// The resolver with every input named, so each arm is testable without
/// reaching into the process's environment or its one-shot capture.
///
/// [`APP_DIRECTORY_ENVIRONMENT_VARIABLE`] first, so a CLI-launched app is
/// anchored where the launcher anchored it; then the entry directory a language
/// host recorded, which is what a hand-run `python app.py` has; then the working
/// directory, which is what a Rust app gets. An empty environment value reads as
/// unset, the way an empty `XDG_RUNTIME_DIR` does.
fn resolve_app_directory(
    app_directory_from_the_environment: Option<OsString>,
    entry_directory_the_language_host_captured: Option<&Path>,
    working_directory: Option<PathBuf>,
) -> PathBuf {
    app_directory_from_the_environment
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| entry_directory_the_language_host_captured.map(Path::to_path_buf))
        .or(working_directory)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CLI-launched app is anchored where its launcher anchored it, over
    /// everything else.
    #[test]
    fn the_environment_outranks_the_captured_entry_directory_and_the_working_directory() {
        assert_eq!(
            resolve_app_directory(
                Some(OsString::from("/apps/from-the-launcher")),
                Some(Path::new("/apps/from-the-interpreter")),
                Some(PathBuf::from("/apps/from-the-shell")),
            ),
            Path::new("/apps/from-the-launcher")
        );
    }

    /// A hand-run `python app.py` is anchored at the entry directory its
    /// interpreter reported, not at the shell it was launched from.
    #[test]
    fn the_captured_entry_directory_outranks_the_working_directory() {
        assert_eq!(
            resolve_app_directory(
                None,
                Some(Path::new("/apps/from-the-interpreter")),
                Some(PathBuf::from("/apps/from-the-shell")),
            ),
            Path::new("/apps/from-the-interpreter")
        );
    }

    /// A Rust app has neither, and takes the working directory.
    #[test]
    fn a_host_that_captured_nothing_takes_the_working_directory() {
        assert_eq!(
            resolve_app_directory(None, None, Some(PathBuf::from("/apps/from-the-shell"))),
            Path::new("/apps/from-the-shell")
        );
    }

    /// An empty variable is no variable, so a cleared one does not anchor an
    /// app at the filesystem root.
    #[test]
    fn an_empty_environment_value_reads_as_unset() {
        assert_eq!(
            resolve_app_directory(
                Some(OsString::new()),
                None,
                Some(PathBuf::from("/apps/from-the-shell")),
            ),
            Path::new("/apps/from-the-shell")
        );
    }

    /// A process with no readable working directory still resolves to
    /// something, rather than failing a runtime over a name.
    #[test]
    fn a_process_with_no_working_directory_still_resolves() {
        assert_eq!(resolve_app_directory(None, None, None), Path::new(""));
    }
}

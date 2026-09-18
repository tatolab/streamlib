// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which directory the app a runtime belongs to was written in.
//!
//! Engine code that names something after the app — an unnamed runtime on the
//! mesh, an unnamed virtual camera — keys on this rather than on the shell's
//! working directory, so the name follows the app and not where it was
//! launched from.

use std::path::PathBuf;
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
/// `python app.py` apart from a `streamlib run`. The first call wins: the
/// entry file does not move while the process lives.
pub fn record_the_app_entry_directory_the_language_host_captured(entry_directory: PathBuf) {
    let _ = APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST.set(entry_directory);
}

/// The directory of the app this runtime belongs to.
///
/// [`APP_DIRECTORY_ENVIRONMENT_VARIABLE`] first, so a CLI-launched app is
/// anchored where the launcher anchored it; then the entry directory a language
/// host recorded above, which is what a hand-run `python app.py` has; then the
/// working directory, which is what a Rust app gets.
pub fn resolve_the_app_directory_this_runtime_belongs_to() -> PathBuf {
    if let Some(from_environment) = std::env::var_os(APP_DIRECTORY_ENVIRONMENT_VARIABLE)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return from_environment;
    }
    if let Some(captured) = APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST.get() {
        return captured.clone();
    }
    std::env::current_dir().unwrap_or_default()
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::OsString;
use std::path::PathBuf;


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
/// environment or working directory. An empty `STREAMLIB_HOME` is ignored.
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

#[cfg(test)]
mod tests {
    use super::*;

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

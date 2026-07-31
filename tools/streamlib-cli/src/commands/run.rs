// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib run` / `streamlib dev` — boot the app's node with the control
//! plane hosted in-process.
//!
//! Both verbs resolve the app's entry file, then stand up a runtime carrying
//! the statically-linked api-server, so the node publishes a node-registry
//! entry and `nodes` / `graph` / `tap` drive it exactly as they drive a
//! `streamlib-runtime` process. The boot recipe itself lives in
//! [`streamlib_api_server::control_plane_host`] — the two hosts share it rather
//! than each carrying a copy.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::runtime::Runner;
use streamlib_api_server::control_plane_host::{
    ApiServerControlPlaneBindConfig, register_api_server_control_plane_processor_on_runtime,
};

/// The entry file both verbs resolve when `--file` is not given.
pub const DEFAULT_APP_ENTRY_FILE_NAME: &str = "app.py";

/// Which verb the user typed. Both boot identically today; the name is what
/// reaches the logs and the error text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppLaunchVerb {
    Run,
    Dev,
}

impl AppLaunchVerb {
    fn as_str(self) -> &'static str {
        match self {
            AppLaunchVerb::Run => "run",
            AppLaunchVerb::Dev => "dev",
        }
    }
}

/// Everything `run` / `dev` take from the command line.
pub struct AppLaunchArgs {
    pub verb: AppLaunchVerb,
    /// Explicit entry file (`-f`), overriding the `app.py` convention.
    pub entry_file: Option<PathBuf>,
    /// App root (`--dir`); the exact CWD when absent, never a parent.
    pub anchor_dir: Option<PathBuf>,
    pub bind_host: String,
    pub bind_port: u16,
    pub node_name: Option<String>,
}

/// Resolve the app root: `--dir` when given, else the exact CWD.
///
/// No walk-up, deliberately mirroring `streamlib add` — inside a monorepo a
/// walk-up makes "which app am I in" ambiguous.
fn resolve_anchor_dir(anchor_dir: Option<&Path>) -> Result<PathBuf> {
    match anchor_dir {
        Some(root) => Ok(root.to_path_buf()),
        None => std::env::current_dir().context("resolve current working directory"),
    }
}

/// Resolve the entry file whose `setup(rt)` the launched node wires its graph
/// from: `entry_file` outright (relative paths against `anchor_dir`), else
/// [`DEFAULT_APP_ENTRY_FILE_NAME`] directly at `anchor_dir`.
pub fn resolve_app_entry_file(
    verb: AppLaunchVerb,
    anchor_dir: &Path,
    entry_file: Option<&Path>,
) -> Result<PathBuf> {
    let Some(requested) = entry_file else {
        let conventional = anchor_dir.join(DEFAULT_APP_ENTRY_FILE_NAME);
        if !conventional.is_file() {
            anyhow::bail!(
                "no `{DEFAULT_APP_ENTRY_FILE_NAME}` in `{}`\n\
                 `streamlib {}` reads `{DEFAULT_APP_ENTRY_FILE_NAME}` from this directory only — \
                 it never searches parent directories.\n\
                 Run it from your app root, point at one with `--dir <app-root>`, or name the \
                 entry file with `-f <file>`.",
                anchor_dir.display(),
                verb.as_str(),
            );
        }
        return Ok(conventional);
    };

    let requested_path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        anchor_dir.join(requested)
    };
    if !requested_path.is_file() {
        anyhow::bail!(
            "no entry file at `{}` (from `-f {}`)",
            requested_path.display(),
            requested.display(),
        );
    }
    Ok(requested_path)
}

/// Boot the app's node and own its run loop until the user interrupts it.
pub fn launch_app_node(args: AppLaunchArgs) -> Result<()> {
    let anchor_dir = resolve_anchor_dir(args.anchor_dir.as_deref())?;
    let entry_file = resolve_app_entry_file(args.verb, &anchor_dir, args.entry_file.as_deref())?;

    tracing::info!(
        verb = args.verb.as_str(),
        entry = %entry_file.display(),
        "Starting StreamLib node"
    );

    let runtime = Runner::with_auto_build()?;
    register_api_server_control_plane_processor_on_runtime(
        &runtime,
        ApiServerControlPlaneBindConfig {
            bind_host: args.bind_host,
            bind_port: args.bind_port,
            node_name: args.node_name,
        },
    )?;

    runtime.start()?;
    tracing::info!("Node ready — `streamlib nodes` lists it. Press Ctrl+C to stop.");
    runtime.wait_for_signal()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir that removes itself when the test ends.
    struct TempDirRemovedOnDrop(PathBuf);

    impl TempDirRemovedOnDrop {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("streamlib-run-entry-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirRemovedOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_file(dir: &Path, name: &str, contents: &str) {
        if let Some(parent) = Path::new(name).parent() {
            std::fs::create_dir_all(dir.join(parent)).expect("create parent dir");
        }
        std::fs::write(dir.join(name), contents).expect("write file");
    }

    #[test]
    fn no_args_resolves_the_conventional_entry_at_the_anchor() {
        let temp = TempDirRemovedOnDrop::new("conventional");
        write_file(
            temp.path(),
            DEFAULT_APP_ENTRY_FILE_NAME,
            "def setup(rt):\n    pass\n",
        );

        let resolved = resolve_app_entry_file(AppLaunchVerb::Run, temp.path(), None)
            .expect("app.py at the anchor resolves");

        assert_eq!(resolved, temp.path().join(DEFAULT_APP_ENTRY_FILE_NAME));
    }

    #[test]
    fn explicit_entry_file_overrides_the_convention() {
        let temp = TempDirRemovedOnDrop::new("override");
        write_file(
            temp.path(),
            DEFAULT_APP_ENTRY_FILE_NAME,
            "def setup(rt):\n    pass\n",
        );
        write_file(temp.path(), "other.py", "def setup(rt):\n    pass\n");

        let resolved =
            resolve_app_entry_file(AppLaunchVerb::Run, temp.path(), Some(Path::new("other.py")))
                .expect("explicit entry resolves");

        assert_eq!(resolved, temp.path().join("other.py"));
    }

    #[test]
    fn explicit_entry_file_may_be_absolute() {
        let temp = TempDirRemovedOnDrop::new("absolute");
        write_file(temp.path(), "elsewhere.py", "def setup(rt):\n    pass\n");
        let absolute = temp.path().join("elsewhere.py");

        let resolved = resolve_app_entry_file(AppLaunchVerb::Dev, temp.path(), Some(&absolute))
            .expect("absolute entry resolves");

        assert_eq!(resolved, absolute);
    }

    #[test]
    fn a_missing_conventional_entry_names_the_convention_and_the_anchor() {
        let temp = TempDirRemovedOnDrop::new("missing");

        let error = resolve_app_entry_file(AppLaunchVerb::Dev, temp.path(), None)
            .expect_err("an empty anchor has no entry");

        let message = error.to_string();
        assert!(
            message.contains(DEFAULT_APP_ENTRY_FILE_NAME),
            "error must name the convention: {message}"
        );
        assert!(
            message.contains(&temp.path().display().to_string()),
            "error must name the anchor it searched: {message}"
        );
        assert!(
            message.contains("streamlib dev"),
            "error must name the verb the user typed: {message}"
        );
        assert!(
            message.contains("-f "),
            "error must offer the `-f` escape hatch: {message}"
        );
    }

    #[test]
    fn a_missing_explicit_entry_names_the_path_it_tried() {
        let temp = TempDirRemovedOnDrop::new("missing-explicit");

        let error =
            resolve_app_entry_file(AppLaunchVerb::Run, temp.path(), Some(Path::new("gone.py")))
                .expect_err("a nonexistent `-f` target is an error");

        assert!(
            error.to_string().contains("gone.py"),
            "error must name the path it tried: {error}"
        );
    }

    #[test]
    fn resolution_never_walks_up_to_a_parent() {
        let temp = TempDirRemovedOnDrop::new("no-walk-up");
        write_file(
            temp.path(),
            DEFAULT_APP_ENTRY_FILE_NAME,
            "def setup(rt):\n    pass\n",
        );
        let nested = temp.path().join("nested");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        let error = resolve_app_entry_file(AppLaunchVerb::Run, &nested, None)
            .expect_err("a parent's app.py must not be adopted");

        assert!(
            error.to_string().contains("never searches parent"),
            "error must state the no-walk-up rule: {error}"
        );
    }

    #[test]
    fn a_directory_named_like_the_entry_is_not_an_entry() {
        let temp = TempDirRemovedOnDrop::new("dir-not-file");
        std::fs::create_dir_all(temp.path().join(DEFAULT_APP_ENTRY_FILE_NAME))
            .expect("create dir shadowing the entry name");

        resolve_app_entry_file(AppLaunchVerb::Run, temp.path(), None)
            .expect_err("a directory named app.py is not an entry file");
    }

    #[test]
    fn the_anchor_is_the_cwd_when_dir_is_absent() {
        let cwd = std::env::current_dir().expect("cwd");

        assert_eq!(resolve_anchor_dir(None).expect("anchor resolves"), cwd);
    }

    #[test]
    fn the_anchor_is_the_dir_flag_when_given() {
        let temp = TempDirRemovedOnDrop::new("anchor-flag");

        assert_eq!(
            resolve_anchor_dir(Some(temp.path())).expect("anchor resolves"),
            temp.path(),
        );
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask stage-local-relay`: the machine's local relay, `moq-relay`,
//! built from its pinned crate with that crate's own lockfile.
//!
//! It is no workspace member and links nothing of ours: `cargo install` builds
//! it into an install root under `target/`, which it leaves alone on a second
//! run with the same version and features. `build-runtime` copies it into the
//! runtime unit's `bin/` beside `tatolabd`; the tests that start a local relay
//! read it from the install root.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The crate the local relay is built from.
pub const LOCAL_RELAY_CRATE_NAME: &str = "moq-relay";

/// The one version the runtime unit carries.
pub const PINNED_LOCAL_RELAY_CRATE_VERSION: &str = "0.17.7";

/// The features the local relay is built with: QUIC, and the WebSocket path
/// Safari plays over. No iroh and no LAN cluster discovery.
pub const LOCAL_RELAY_CRATE_FEATURES: &str = "noq,websocket";

/// The executable's name, in the install root's `bin/` and in the unit's.
pub const LOCAL_RELAY_EXECUTABLE_FILE_NAME: &str = "moq-relay";

/// Where `cargo install` puts the local relay, relative to the workspace.
const LOCAL_RELAY_INSTALL_ROOT_RELATIVE_TO_WORKSPACE: &str = "target/local-relay";

/// The install root the local relay is built into.
pub fn local_relay_install_root_in_the_workspace(workspace_root: &Path) -> PathBuf {
    workspace_root.join(LOCAL_RELAY_INSTALL_ROOT_RELATIVE_TO_WORKSPACE)
}

/// The staged executable.
pub fn staged_local_relay_executable_in_the_workspace(workspace_root: &Path) -> PathBuf {
    local_relay_install_root_in_the_workspace(workspace_root)
        .join("bin")
        .join(LOCAL_RELAY_EXECUTABLE_FILE_NAME)
}

/// The `cargo install` arguments that build the pinned relay into `install_root`.
pub fn local_relay_cargo_install_arguments(install_root: &Path) -> Vec<String> {
    vec![
        "install".to_string(),
        LOCAL_RELAY_CRATE_NAME.to_string(),
        "--version".to_string(),
        format!("={PINNED_LOCAL_RELAY_CRATE_VERSION}"),
        "--locked".to_string(),
        "--no-default-features".to_string(),
        "--features".to_string(),
        LOCAL_RELAY_CRATE_FEATURES.to_string(),
        "--root".to_string(),
        install_root.display().to_string(),
    ]
}

/// Build the pinned relay, or find it already built, and return the executable.
pub fn stage_the_local_relay(workspace_root: &Path) -> Result<PathBuf> {
    let install_root = local_relay_install_root_in_the_workspace(workspace_root);
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(&cargo)
        .args(local_relay_cargo_install_arguments(&install_root))
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("running `cargo install {LOCAL_RELAY_CRATE_NAME}`"))?;
    if !status.success() {
        bail!(
            "`cargo install {LOCAL_RELAY_CRATE_NAME} --version ={PINNED_LOCAL_RELAY_CRATE_VERSION}` \
             failed with {status}"
        );
    }
    let staged_executable = staged_local_relay_executable_in_the_workspace(workspace_root);
    if !staged_executable.is_file() {
        bail!(
            "`cargo install` reported success but left no {}",
            staged_executable.display()
        );
    }
    tracing::info!(
        "stage-local-relay: {LOCAL_RELAY_CRATE_NAME} {PINNED_LOCAL_RELAY_CRATE_VERSION} is at {}",
        staged_executable.display()
    );
    Ok(staged_executable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relay_is_built_from_the_exact_pinned_version_with_its_own_lockfile() {
        let arguments =
            local_relay_cargo_install_arguments(Path::new("/workspace/target/local-relay"));
        assert_eq!(
            arguments,
            [
                "install",
                "moq-relay",
                "--version",
                "=0.17.7",
                "--locked",
                "--no-default-features",
                "--features",
                "noq,websocket",
                "--root",
                "/workspace/target/local-relay",
            ]
        );
    }

    #[test]
    fn the_staged_relay_sits_in_the_install_roots_bin() {
        assert_eq!(
            staged_local_relay_executable_in_the_workspace(Path::new("/workspace")),
            Path::new("/workspace/target/local-relay/bin/moq-relay")
        );
    }

    #[test]
    fn the_tests_that_start_a_local_relay_read_it_from_this_install_root() {
        let staged_relative_to_the_workspace = format!(
            "{LOCAL_RELAY_INSTALL_ROOT_RELATIVE_TO_WORKSPACE}/bin/{LOCAL_RELAY_EXECUTABLE_FILE_NAME}"
        );
        for test_support in [
            include_str!("../../runtime/streamlib-relay-adapter-local/tests/common/mod.rs"),
            include_str!("../../runtime/tatolabd/tests/common/mod.rs"),
        ] {
            assert!(test_support.contains(&format!("../../{staged_relative_to_the_workspace}")));
        }
    }

    #[test]
    fn the_unit_and_tatolabd_agree_on_the_relays_file_name() {
        let tatolabd_side = include_str!("../../runtime/streamlib-relay-adapter-local/src/lib.rs");
        assert!(tatolabd_side.contains(&format!(
            "pub const LOCAL_RELAY_EXECUTABLE_FILE_NAME: &str = \"{LOCAL_RELAY_EXECUTABLE_FILE_NAME}\";"
        )));
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask stage-browser-host`: the machine's browser host, its CEF
//! subprocess and the CEF runtime they load, staged into the runtime unit's
//! `lib/tatolab/browser-host/` beside the lend.
//!
//! Both executables are built from `packages/tatolab-browser`'s own workspace
//! with its own lockfile, against the CEF binary distribution `CEF_PATH`
//! names, and the package's `stage_browser_host.py` copies them in with CEF's
//! libraries stripped of their debug sections. A unit built without
//! `CEF_PATH` carries no browser host, and `tatolabd` offers none.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    browser_host_executable_beside_the_lend, runtime_unit_lend_directory_in_the_workspace,
};

/// The browser package's workspace, relative to this one.
const BROWSER_PACKAGE_RELATIVE_TO_WORKSPACE: &str = "packages/tatolab-browser";

/// The script that stages the built executables and the CEF runtime.
const BROWSER_HOST_STAGING_SCRIPT_FILE_NAME: &str = "stage_browser_host.py";

/// Where the CEF binary distribution the build links is found.
const CEF_BINARY_DISTRIBUTION_ENVIRONMENT_VARIABLE: &str = "CEF_PATH";

/// The cargo packages building the host and the subprocess CEF starts.
const BROWSER_HOST_CARGO_PACKAGES: [&str; 2] =
    ["tatolab-browser-host", "tatolab-browser-page-bridge"];

/// The executables those packages build, which the staging script copies.
const BROWSER_HOST_EXECUTABLE_FILE_NAMES: [&str; 2] =
    ["tatolab-browser-host", "tatolab-browser-subprocess"];

/// The browser host's directory in the runtime unit in `workspace_root`.
pub fn runtime_unit_browser_host_directory_in_the_workspace(
    workspace_root: &Path,
) -> Result<PathBuf> {
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    let browser_host_executable = browser_host_executable_beside_the_lend(&lend_directory)
        .with_context(|| {
            format!(
                "the lend at {} has no directory beside it",
                lend_directory.display()
            )
        })?;
    browser_host_executable
        .parent()
        .map(Path::to_path_buf)
        .context("the browser host's executable sits in no directory")
}

/// The `cargo build` arguments that build both executables, optimized, from
/// the browser package's lockfile.
pub fn browser_host_cargo_build_arguments(browser_package_manifest: &Path) -> Vec<String> {
    let mut arguments = vec![
        "build".to_string(),
        "--locked".to_string(),
        "--release".to_string(),
        "--manifest-path".to_string(),
        browser_package_manifest.display().to_string(),
    ];
    for package in BROWSER_HOST_CARGO_PACKAGES {
        arguments.push("--package".to_string());
        arguments.push(package.to_string());
    }
    arguments.push("--bins".to_string());
    arguments.push("--message-format=json-render-diagnostics".to_string());
    arguments
}

/// The one directory cargo wrote both executables to, read from the
/// `compiler-artifact` messages of a `--message-format=json` build, so a
/// `CARGO_TARGET_DIR` or a `--target` triple is followed wherever it points.
pub fn directory_cargo_wrote_the_browser_host_executables_to(
    cargo_build_messages: &str,
) -> Result<PathBuf> {
    let mut executable_directories = Vec::new();
    for message_line in cargo_build_messages.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(message_line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        if let Some(executable) = message["executable"].as_str() {
            let executable = Path::new(executable);
            let is_a_browser_host_executable = executable
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .is_some_and(|file_name| BROWSER_HOST_EXECUTABLE_FILE_NAMES.contains(&file_name));
            if is_a_browser_host_executable && let Some(directory) = executable.parent() {
                executable_directories.push(directory.to_path_buf());
            }
        }
    }
    executable_directories.dedup();
    match executable_directories.as_slice() {
        [directory] => Ok(directory.clone()),
        [] => bail!("cargo reported building none of {BROWSER_HOST_EXECUTABLE_FILE_NAMES:?}"),
        directories => bail!(
            "cargo wrote {BROWSER_HOST_EXECUTABLE_FILE_NAMES:?} to more than one directory: \
             {directories:?}"
        ),
    }
}

/// Build and stage the browser host into the runtime unit, or say why it is
/// left out: `None` when `CEF_PATH` names no CEF distribution.
pub fn stage_the_browser_host(workspace_root: &Path) -> Result<Option<PathBuf>> {
    if std::env::var_os(CEF_BINARY_DISTRIBUTION_ENVIRONMENT_VARIABLE)
        .is_none_or(|cef_binary_distribution| cef_binary_distribution.is_empty())
    {
        let staged_directory =
            runtime_unit_browser_host_directory_in_the_workspace(workspace_root)?;
        if staged_directory.exists() {
            std::fs::remove_dir_all(&staged_directory).with_context(|| {
                format!(
                    "removing the browser host an earlier build staged at {}",
                    staged_directory.display()
                )
            })?;
        }
        tracing::warn!(
            "stage-browser-host: {CEF_BINARY_DISTRIBUTION_ENVIRONMENT_VARIABLE} names no CEF \
             binary distribution, so the runtime unit carries no browser host and no \
             BrowserSource can open a page"
        );
        return Ok(None);
    }
    let browser_package = workspace_root.join(BROWSER_PACKAGE_RELATIVE_TO_WORKSPACE);
    let browser_package_manifest = browser_package.join("Cargo.toml");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let built = std::process::Command::new(&cargo)
        .args(browser_host_cargo_build_arguments(
            &browser_package_manifest,
        ))
        .current_dir(&browser_package)
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("running `cargo build` for the browser host")?;
    if !built.status.success() {
        bail!("building the browser host failed with {}", built.status);
    }
    let executable_directory = directory_cargo_wrote_the_browser_host_executables_to(
        &String::from_utf8_lossy(&built.stdout),
    )?;
    let staged_directory = runtime_unit_browser_host_directory_in_the_workspace(workspace_root)?;
    let status = std::process::Command::new("python3")
        .arg(browser_package.join(BROWSER_HOST_STAGING_SCRIPT_FILE_NAME))
        .arg("--executable-directory")
        .arg(&executable_directory)
        .arg("--into")
        .arg(&staged_directory)
        .current_dir(&browser_package)
        .status()
        .with_context(|| format!("running {BROWSER_HOST_STAGING_SCRIPT_FILE_NAME}"))?;
    if !status.success() {
        bail!("{BROWSER_HOST_STAGING_SCRIPT_FILE_NAME} failed with {status}");
    }
    tracing::info!(
        "stage-browser-host: the browser host and the CEF runtime are in {}",
        staged_directory.display()
    );
    Ok(Some(staged_directory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_runtime::{
        BROWSER_HOST_EXECUTABLE_FILE_NAME, BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME,
    };

    #[test]
    fn both_executables_are_built_optimized_from_the_browser_packages_lockfile() {
        assert_eq!(
            browser_host_cargo_build_arguments(Path::new(
                "/workspace/packages/tatolab-browser/Cargo.toml"
            )),
            [
                "build",
                "--locked",
                "--release",
                "--manifest-path",
                "/workspace/packages/tatolab-browser/Cargo.toml",
                "--package",
                "tatolab-browser-host",
                "--package",
                "tatolab-browser-page-bridge",
                "--bins",
                "--message-format=json-render-diagnostics",
            ]
        );
    }

    #[test]
    fn the_executables_are_staged_from_where_cargo_wrote_them() {
        let cargo_build_messages = [
            r#"{"reason":"compiler-artifact","target":{"name":"tatolab_browser_tab_protocol"},"executable":null}"#,
            r#"{"reason":"compiler-artifact","executable":"/elsewhere/x86_64-unknown-linux-gnu/release/tatolab-browser-host"}"#,
            r#"{"reason":"compiler-artifact","executable":"/elsewhere/x86_64-unknown-linux-gnu/release/tatolab-browser-subprocess"}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ]
        .join("\n");
        assert_eq!(
            directory_cargo_wrote_the_browser_host_executables_to(&cargo_build_messages).unwrap(),
            Path::new("/elsewhere/x86_64-unknown-linux-gnu/release")
        );
    }

    #[test]
    fn a_build_that_reports_no_executable_is_refused() {
        assert!(
            directory_cargo_wrote_the_browser_host_executables_to(
                r#"{"reason":"build-finished","success":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn the_browser_host_is_staged_beside_the_lend() {
        assert_eq!(
            runtime_unit_browser_host_directory_in_the_workspace(Path::new("/workspace")).unwrap(),
            Path::new("/workspace/target/tatolab-runtime/lib/tatolab/browser-host")
        );
    }

    #[test]
    fn the_host_and_the_browser_package_agree_on_the_service_and_the_executables() {
        let tab_protocol = include_str!("../../packages/tatolab-browser/tab-protocol/src/lib.rs");
        assert!(tab_protocol.contains(&format!(
            "pub const BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME: &str = \"{}\";",
            BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME
        )));
        let host_manifest = include_str!("../../packages/tatolab-browser/host/Cargo.toml");
        assert!(
            host_manifest.contains(&format!("name = \"{}\"", BROWSER_HOST_EXECUTABLE_FILE_NAME))
        );
        let staging_script = include_str!("../../packages/tatolab-browser/stage_browser_host.py");
        assert!(staging_script.contains(&format!(
            "HOST_EXECUTABLE_NAME = \"{}\"",
            BROWSER_HOST_EXECUTABLE_FILE_NAME
        )));
    }
}

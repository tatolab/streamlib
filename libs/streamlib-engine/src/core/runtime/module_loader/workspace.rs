// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::errors::AddModuleError;

/// Resolve the workspace root used by
/// [`ModuleResolverStrategy::WorkspaceStaged`] (and the workspace
/// half of [`ModuleResolverStrategy::DefaultChain`]).
///
/// Env-var override wins when set — the env var IS the user's stated
/// intent, so a typo'd path surfaces as
/// [`AddModuleError::WorkspaceRootInvalid`] rather than silently
/// falling through to `cargo locate-project`. When the env var is
/// unset, the helper invokes `cargo locate-project --workspace` and
/// returns [`AddModuleError::WorkspaceRootNotFound`] if no workspace
/// is reachable.
///
/// [`ModuleResolverStrategy::WorkspaceStaged`]: super::ModuleResolverStrategy::WorkspaceStaged
/// [`ModuleResolverStrategy::DefaultChain`]: super::ModuleResolverStrategy::DefaultChain
pub(super) fn resolve_workspace_root(
) -> std::result::Result<std::path::PathBuf, AddModuleError> {
    if let Ok(env_root) = std::env::var("STREAMLIB_WORKSPACE_ROOT") {
        let path = std::path::PathBuf::from(&env_root);
        return if path.is_dir() {
            Ok(path)
        } else {
            Err(AddModuleError::WorkspaceRootInvalid { env_value: env_root })
        };
    }

    let output = std::process::Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .map_err(|_| AddModuleError::WorkspaceRootNotFound)?;
    if !output.status.success() {
        return Err(AddModuleError::WorkspaceRootNotFound);
    }
    let manifest_path = String::from_utf8(output.stdout)
        .map_err(|_| AddModuleError::WorkspaceRootNotFound)?;
    let trimmed = manifest_path.trim();
    if trimmed.is_empty() {
        return Err(AddModuleError::WorkspaceRootNotFound);
    }
    std::path::PathBuf::from(trimmed)
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or(AddModuleError::WorkspaceRootNotFound)
}

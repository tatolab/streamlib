// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `cargo xtask generate-control-contract`: writes the control contract file from the
//! operation registry, then every generated file that reads it. `--check` is the CI gate that
//! fails when the checked-in contract file differs from the registry; each generator that reads
//! the file keeps its own `--check`, so a red step names what drifted.

use std::path::Path;

use anyhow::{Context, Result, bail};
use streamlib_control_contract::{
    CONTROL_CONTRACT_FILE_WORKSPACE_PATH, CONTROL_OPERATIONS, control_contract_file_contents,
};

/// Regenerate the contract file and the files generated from it, or with `check_only` fail when
/// the checked-in contract file differs from a regeneration.
pub fn run(workspace_root: &Path, check_only: bool) -> Result<()> {
    let rendered_contract = control_contract_file_contents();
    let contract_file_path = workspace_root.join(CONTROL_CONTRACT_FILE_WORKSPACE_PATH);
    if check_only {
        let checked_in_contract = std::fs::read_to_string(&contract_file_path)
            .with_context(|| format!("reading {}", contract_file_path.display()))?;
        if checked_in_contract != rendered_contract {
            bail!(
                "{CONTROL_CONTRACT_FILE_WORKSPACE_PATH} differs from what the operation registry \
                 generates. An operation changed without the contract file being regenerated: run \
                 `cargo xtask generate-control-contract` and commit the result."
            );
        }
        tracing::info!("{CONTROL_CONTRACT_FILE_WORKSPACE_PATH} matches the operation registry");
        return Ok(());
    }
    std::fs::write(&contract_file_path, rendered_contract)
        .with_context(|| format!("writing {}", contract_file_path.display()))?;
    tracing::info!(
        "wrote {CONTROL_CONTRACT_FILE_WORKSPACE_PATH} ({} operations)",
        CONTROL_OPERATIONS.len()
    );
    crate::generate_runtime_client::run(workspace_root, false)
}

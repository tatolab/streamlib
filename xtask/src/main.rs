// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub mod check_boundaries;
pub mod check_device_wait_idle;
pub mod check_no_escalate_in_lifecycle;
pub mod check_no_in_process_placement;
pub mod check_no_inventory_submit;
pub mod check_no_reverse_dns;
pub mod check_no_streamlib_metadata;
pub mod check_schema_versions;
pub mod check_vendored_vulkanalia;
pub mod lint_logging;
pub mod normal_build_dep_graph;

/// Rust source roots a workspace crate may hold: the classic `src/` and the
/// folder-backed `processors/` a generated crate root declares its module arms
/// out of. Every source-walking gate shares this list, and it spells the
/// folder-backed root through [`streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME`]
/// so renaming that root cannot leave a gate scanning a directory that no
/// longer exists.
pub const RUST_CRATE_SOURCE_ROOT_DIR_NAMES: &[&str] =
    &["src", streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME];

/// Refuse a source-walking gate run that read no source at all.
///
/// A gate whose scan roots moved out from under it is indistinguishable from a
/// clean tree: both report zero violations. `unnoticed_consequence` names what
/// the gate would then let through, so the failure reads as the gate's own
/// contract rather than a generic count assertion. One sentence shape for every
/// gate, so a fifth one cannot invent a weaker phrasing.
pub fn ensure_source_walking_gate_read_source(
    gate_name: &str,
    scan_roots_description: &str,
    files_scanned: usize,
    unnoticed_consequence: &str,
) -> Result<()> {
    anyhow::ensure!(
        files_scanned > 0,
        "{gate_name} scanned 0 files under {scan_roots_description} — the scan roots \
         moved out from under the gate, which would let {unnoticed_consequence} unnoticed"
    );
    Ok(())
}

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "StreamLib development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ban ad-hoc logging in polyglot SDK library code (Python + TypeScript).
    /// Paired with the workspace clippy.toml `disallowed-macros` rule for Rust.
    LintLogging,

    /// Boundary-grep CI gate for the Vulkan RHI capability split. Fails on
    /// `ash`, raw `vulkanalia` outside RHI/adapter crates, cdylibs depending
    /// on the full `streamlib` crate, or privileged Vulkan calls outside
    /// the RHI. See `docs/architecture/subprocess-rhi-parity.md`.
    CheckBoundaries,

    /// CI gate for the package-as-publication-unit rule from milestone 10.
    /// Fails when any schema YAML declares a top-level `version` key
    /// (versioning lives in `streamlib.yaml`, not in individual schemas).
    /// See `docs/architecture/schema-identity-and-packaging.md`.
    CheckSchemaVersions,

    /// CI gate for #402's atomic cutover off language-native metadata.
    /// Fails on `[package.metadata.streamlib]`, `[tool.streamlib]`, or a
    /// top-level `streamlib` key in `deno.json` / `deno.jsonc`. The single
    /// source of truth is `streamlib.yaml`; see
    /// `docs/architecture/schema-identity-and-packaging.md` (anti-pattern 4).
    CheckNoStreamlibMetadata,

    /// CI gate for milestone-10's structured-identifier rule. Fails on
    /// legacy reverse-DNS schema literals (`com.tatolab.*`,
    /// `com.streamlib.*`) anywhere in live workspace code. Apple
    /// platform code (`*/apple/*`), test code (`#[cfg(test)]`,
    /// `tests/`, `*_test{s}.rs`), and Rust comments are allowed. See
    /// `docs/architecture/schema-identity-and-packaging.md`.
    CheckNoReverseDns,

    /// CI gate for the helper-process-placement-only ruling (owner
    /// 2026-08-04). Fails on the vocabulary of the banned model anywhere in
    /// the engine tree or `docs/`; the banned patterns and the two escape
    /// hatches are enumerated in
    /// [`check_no_in_process_placement`]. Markdown and Rust doc comments are
    /// scanned on purpose — the shipped violation announced itself in a `//!`
    /// line. See `docs/decisions/helper-process-placement-only.md`.
    CheckNoInProcessPlacement,

    /// CI gate for #793's all-dynamic registration rule. Fails on any
    /// `inventory::submit!(FactoryRegistration { ... })` in live code —
    /// the `#[processor]` macro no longer emits one, and reintroducing
    /// the pattern would bypass the dynamic-load model from milestone
    /// `All-Dynamic Package Loading` (#20). `RuntimeInitHookRegistration`
    /// inventory submissions are unaffected — only `FactoryRegistration`
    /// is flagged.
    CheckNoInventorySubmit,

    /// CI gate for the escalate-from-lifecycle ban. Fails when
    /// any fn taking `&RuntimeContextFullAccess<'_>` (typically
    /// `setup` / `teardown` / `setup_inner` / `teardown_inner`) calls
    /// `.escalate(...)` in its body. The lifecycle dispatch already
    /// holds the escalate gate; re-entry panics at runtime via
    /// `EscalateGate::enter`. The xtask is defense-in-depth — catches
    /// the violation at PR review before the runtime panic fires.
    CheckNoEscalateInLifecycle,

    /// CI gate for the `vkDeviceWaitIdle` threading discipline. Fails on any
    /// raw `device_wait_idle()` call in the engine outside the mutex-guarded
    /// `HostVulkanDevice::wait_idle` helper. `vkDeviceWaitIdle` is externally
    /// synchronized over the device + every queue it owns; a raw call that
    /// skips the per-queue mutexes races concurrent submits during
    /// multi-processor setup and crashes the driver (the validation layer
    /// reports `UNASSIGNED-Threading-Info`).
    CheckDeviceWaitIdle,



    /// Drift trip-wire for the vendored vulkanalia fork trees
    /// (`vendor/tatolab-vulkanalia{,-sys,-vma}`): hashes each vendored crate
    /// dir and fails on any byte change vs. the recorded hash — the guard
    /// against accidental in-place edits (a workspace `cargo fmt --all`
    /// sweep is the classic cause). Deliberate re-vendors update the
    /// recorded hashes in the same commit per
    /// `docs/architecture/vendored-vulkanalia.md`.
    CheckVendoredVulkanalia,

}


fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::LintLogging => lint_logging::run(&workspace_root()?)?,
        Commands::CheckBoundaries => check_boundaries::run(&workspace_root()?)?,
        Commands::CheckSchemaVersions => check_schema_versions::run(&workspace_root()?)?,
        Commands::CheckNoStreamlibMetadata => check_no_streamlib_metadata::run(&workspace_root()?)?,
        Commands::CheckNoReverseDns => check_no_reverse_dns::run(&workspace_root()?)?,
        Commands::CheckNoInProcessPlacement => {
            check_no_in_process_placement::run(&workspace_root()?)?
        }
        Commands::CheckNoInventorySubmit => check_no_inventory_submit::run(&workspace_root()?)?,
        Commands::CheckNoEscalateInLifecycle => {
            check_no_escalate_in_lifecycle::run(&workspace_root()?)?
        }
        Commands::CheckDeviceWaitIdle => check_device_wait_idle::run(&workspace_root()?)?,
        Commands::CheckVendoredVulkanalia => check_vendored_vulkanalia::run(&workspace_root()?)?,
    }

    Ok(())
}

/// Get the workspace root directory.
pub fn workspace_root() -> Result<PathBuf> {
    let output = std::process::Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .context("Failed to run cargo locate-project")?;

    let path = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in cargo output")?
        .trim()
        .to_string();

    PathBuf::from(path)
        .parent()
        .map(|p| p.to_path_buf())
        .context("Failed to get workspace root")
}

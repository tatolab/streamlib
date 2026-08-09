// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.
//!
//! For routine codegen, each Rust crate's `build.rs` invokes
//! `streamlib_jtd_codegen::build_rs::run_for_rust_crate` automatically.
//! This subcommand exists for ad-hoc generation and the Python / Deno
//! triggers (`setup.py` + `deno task setup`) that shell out to the CLI.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use streamlib_jtd_codegen::{GenerateOptions, RuntimeTarget, generate};

pub mod check_boundaries;
pub mod check_cdylib_reach;
pub mod check_consumer_rhi_repr;
pub mod check_device_wait_idle;
pub mod check_no_escalate_in_lifecycle;
pub mod check_no_in_process_placement;
pub mod check_no_inventory_submit;
pub mod check_no_reverse_dns;
pub mod check_no_streamlib_metadata;
pub mod check_processor_source_reachability;
pub mod check_processor_spec_new;
pub mod check_schema_versions;
pub mod check_vendored_vulkanalia;
pub mod generate_crate_roots;
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
    /// Generate code from JTD schemas using jtd-codegen.
    ///
    /// Thin wrapper around `streamlib-jtd-codegen`. The same pipeline is also
    /// reachable as `streamlib generate` for non-Rust developers (no rustup
    /// required).
    GenerateSchemas {
        /// Target language (default: rust)
        #[arg(long, default_value = "rust")]
        runtime: RuntimeTarget,

        /// Output directory (required)
        #[arg(long)]
        output: PathBuf,

        /// `streamlib.yaml`-driven mode: directory containing the manifest.
        /// The resolver walks declared dependencies and codegen ingests the
        /// resulting set.
        #[arg(long, group = "input")]
        project_dir: Option<PathBuf>,

        /// Process a single schema file
        #[arg(long, group = "input")]
        schema_file: Option<PathBuf>,

        /// Process all .yaml files in a directory
        #[arg(long, group = "input")]
        schema_dir: Option<PathBuf>,
    },

    /// Write every in-tree folder-backed package's generated Rust crate root
    /// (`_generated_rust_crate_root_/lib.rs`) from its `processors/` directory.
    ///
    /// Cargo resolves `[lib] path` at target resolution, before any build
    /// script runs, so this cannot live in a `build.rs`. Run it before any
    /// in-tree `cargo build` / `cargo test` that touches a package crate.
    GenerateCrateRoots,

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

    /// CI gate for the structured-everywhere `ProcessorSpec` rule from
    /// #707. Fails on `ProcessorSpec::new("PascalCase", ...)` — every
    /// call site must take a structured `SchemaIdent` (built via
    /// `SchemaIdent::new(...)` or via the macro-emitted
    /// `<Module>::schema_ident()`).
    CheckProcessorSpecNew,

    /// CI gate reporting `.rs` files under a folder-backed package's
    /// `processors/` directory that no `mod` chain in the generated module
    /// tree names. Cargo and clippy both ignore such a file — it is absent
    /// from the build, not excluded from it — so nothing else says a word.
    CheckProcessorSourceReachability,

    /// CI gate for the cdylib-reachability invariant on engine `Host*`
    /// constructors. Fails when any constructor-class method
    /// (`new*` / `create*` / `from_*`) inside an `impl HostVulkan*`
    /// block in the engine RHI references `host_inner()` or
    /// `host_callbacks()` — those break the cdylib direct-call path
    /// documented at `docs/architecture/cdylib-reachability.md`.
    CheckCdylibReach,

    /// CI gate for the escalate-from-lifecycle ban (anti-pattern #1
    /// in `docs/architecture/cdylib-reachability.md`). Fails when
    /// any fn taking `&RuntimeContextFullAccess<'_>` (typically
    /// `setup` / `teardown` / `setup_inner` / `teardown_inner`) calls
    /// `.escalate(...)` in its body. The lifecycle dispatch already
    /// holds the escalate gate; re-entry panics at runtime via
    /// `EscalateGate::enter`. The xtask is defense-in-depth — catches
    /// the violation at PR review before the runtime panic fires.
    CheckNoEscalateInLifecycle,

    /// CI gate for issue #1039's consumer-rhi `#[repr(...)]` discipline.
    /// Fails when any `pub enum` in `runtime/streamlib-consumer-rhi/src/`
    /// is missing an explicit `#[repr(...)]`, or when any
    /// `pub struct X(T)` scalar tuple newtype is missing
    /// `#[repr(transparent)]` / `#[repr(C)]`. Consumer-rhi POD types
    /// cross the plugin FFI boundary as bare scalars; their byte
    /// layout is part of the wire contract. See
    /// `docs/architecture/subprocess-rhi-parity.md`.
    CheckConsumerRhiRepr,

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
        Commands::GenerateSchemas {
            runtime,
            output,
            project_dir,
            schema_file,
            schema_dir,
        } => {
            // Human-run codegen (like the `streamlib generate` CLI): resolve the
            // active `streamlib link` checkout marker-first from the project dir.
            let link_checkout = project_dir
                .as_deref()
                .and_then(|d| streamlib_idents::ResolverOptions::from_env_or_marker(d).link_checkout);
            generate(GenerateOptions {
                runtime,
                output,
                project_dir,
                schema_file,
                schema_dir,
                workspace_root: workspace_root()?,
                write_lockfile: true,
                link_checkout,
            })?
        }
        Commands::GenerateCrateRoots => generate_crate_roots::run(&workspace_root()?)?,
        Commands::LintLogging => lint_logging::run(&workspace_root()?)?,
        Commands::CheckBoundaries => check_boundaries::run(&workspace_root()?)?,
        Commands::CheckSchemaVersions => check_schema_versions::run(&workspace_root()?)?,
        Commands::CheckNoStreamlibMetadata => check_no_streamlib_metadata::run(&workspace_root()?)?,
        Commands::CheckNoReverseDns => check_no_reverse_dns::run(&workspace_root()?)?,
        Commands::CheckNoInProcessPlacement => {
            check_no_in_process_placement::run(&workspace_root()?)?
        }
        Commands::CheckNoInventorySubmit => check_no_inventory_submit::run(&workspace_root()?)?,
        Commands::CheckProcessorSpecNew => check_processor_spec_new::run(&workspace_root()?)?,
        Commands::CheckProcessorSourceReachability => {
            check_processor_source_reachability::run(&workspace_root()?)?
        }
        Commands::CheckCdylibReach => check_cdylib_reach::run(&workspace_root()?)?,
        Commands::CheckNoEscalateInLifecycle => {
            check_no_escalate_in_lifecycle::run(&workspace_root()?)?
        }
        Commands::CheckConsumerRhiRepr => check_consumer_rhi_repr::run(&workspace_root()?)?,
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

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

pub mod check_boundaries;
pub mod check_clock_usage;
pub mod check_device_wait_idle;
pub mod check_no_escalate_in_lifecycle;
pub mod check_no_in_process_placement;
pub mod check_no_inventory_submit;
pub mod check_no_unbounded_cstr_from_ptr;
pub mod check_vendored_vulkanalia;
pub mod lint_logging;
pub mod normal_build_dep_graph;

/// Rust source roots a workspace crate may hold: the classic `src/` and the
/// folder-backed `processors/`. `lint_logging` walks these by name rather
/// than descending from the crate root, so a source tree outside both is
/// invisible to it.
pub const RUST_CRATE_SOURCE_ROOT_DIR_NAMES: &[&str] = &["src", "processors"];

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

/// Every source-walking gate, paired with the subcommand name that runs it alone.
///
/// Each gate reads the tree and reports; none builds the workspace. That is what
/// lets one process run all nine in well under a second, and why CI runs them as
/// a single job rather than one runner per gate.
const ALL_SOURCE_WALKING_GATES: &[(&str, fn(&Path) -> Result<()>)] = &[
    ("lint-logging", lint_logging::run),
    ("check-boundaries", check_boundaries::run),
    ("check-vendored-vulkanalia", check_vendored_vulkanalia::run),
    (
        "check-no-in-process-placement",
        check_no_in_process_placement::run,
    ),
    ("check-no-inventory-submit", check_no_inventory_submit::run),
    (
        "check-no-escalate-in-lifecycle",
        check_no_escalate_in_lifecycle::run,
    ),
    ("check-device-wait-idle", check_device_wait_idle::run),
    (
        "check-no-unbounded-cstr-from-ptr",
        check_no_unbounded_cstr_from_ptr::run,
    ),
    ("check-clock-usage", check_clock_usage::run),
];

/// Run every source-walking gate, reporting all failures rather than the first.
///
/// A gate that bails on first failure hides the rest behind a re-run, which is the
/// one thing a consolidated job must not reintroduce: eight separate jobs at least
/// told you about eight separate breakages at once.
fn run_all_source_walking_gates(workspace_root: &Path) -> Result<()> {
    let mut failed_gate_names: Vec<&str> = Vec::new();

    for (gate_name, run_gate) in ALL_SOURCE_WALKING_GATES {
        match run_gate(workspace_root) {
            Ok(()) => tracing::info!("PASS  {gate_name}"),
            Err(gate_failure) => {
                tracing::error!("FAIL  {gate_name}: {gate_failure:#}");
                failed_gate_names.push(gate_name);
            }
        }
    }

    anyhow::ensure!(
        failed_gate_names.is_empty(),
        "{} of {} source-walking gates failed: {}",
        failed_gate_names.len(),
        ALL_SOURCE_WALKING_GATES.len(),
        failed_gate_names.join(", ")
    );

    tracing::info!(
        "all {} source-walking gates passed",
        ALL_SOURCE_WALKING_GATES.len()
    );
    Ok(())
}

/// Run one command from the workspace root, failing on a non-zero exit status.
fn run_local_ci_gate_command(
    workspace_root: &Path,
    gate_name: &str,
    program: &str,
    arguments: &[&str],
) -> Result<()> {
    let exit_status = std::process::Command::new(program)
        .args(arguments)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("failed to spawn `{program}` for {gate_name}"))?;

    anyhow::ensure!(exit_status.success(), "{gate_name} failed ({exit_status})");
    Ok(())
}

/// Run the gates CI runs, in the order CI runs them, reporting every failure.
///
/// The point is that a green run here means a green run on the PR. Any gate added
/// to CI without being added here breaks that promise, so the two lists are meant
/// to be read side by side against `.github/workflows/`.
fn run_local_ci_gates(workspace_root: &Path) -> Result<()> {
    let mut failed_gate_names: Vec<&str> = Vec::new();

    if let Err(gate_failure) = run_all_source_walking_gates(workspace_root) {
        tracing::error!("{gate_failure:#}");
        failed_gate_names.push("source-walking gates");
    }

    let shelled_out_gates: &[(&str, &str, &[&str])] = &[
        (
            "license headers",
            "bash",
            &["scripts/check-license-headers.sh"],
        ),
        (
            "ship-change removed gate tests",
            "bash",
            &[".claude/scripts/tests/ship-change-removed-gate.test.sh"],
        ),
        (
            "xtask gate fixture tests",
            "cargo",
            &["test", "--locked", "-p", "xtask"],
        ),
        (
            "SDK + macros unit tests",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib",
                "-p",
                "streamlib-macros",
                "--lib",
            ],
        ),
        (
            "processor-macro emission locks",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib-engine",
                "--test",
                "attribute_macro_test",
            ],
        ),
    ];

    for (gate_name, program, arguments) in shelled_out_gates {
        tracing::info!("running {gate_name}");
        if let Err(gate_failure) =
            run_local_ci_gate_command(workspace_root, gate_name, program, arguments)
        {
            tracing::error!("{gate_failure:#}");
            failed_gate_names.push(gate_name);
        }
    }

    anyhow::ensure!(
        failed_gate_names.is_empty(),
        "{} local CI gate(s) failed: {}",
        failed_gate_names.len(),
        failed_gate_names.join(", ")
    );

    tracing::info!("all local CI gates passed");
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

    /// CI gate for the borrow-checked-C-string rule in the Vulkan RHI. Fails
    /// on any `CStr::from_ptr(<owner>.as_ptr())` under
    /// `runtime/streamlib-engine/src/vulkan/` or
    /// `runtime/streamlib-consumer-rhi/src/`. `CStr::from_ptr` returns an
    /// unbounded lifetime, so the borrow is never tied to the storage the
    /// pointer came from and survives it — the use-after-free two device
    /// bring-up paths shipped in #1846. `vk::StringArray::as_cstr` borrows
    /// from `&self` and is the drop-in. A bare pointer argument owned by an
    /// external API is not flagged.
    CheckNoUnboundedCstrFromPtr,

    /// CI gate for the wall-clock allowlist. Fails on a wall-clock read
    /// (`SystemTime::now`, `Utc::now`, `time.time_ns`, `datetime.now`, …)
    /// anywhere under `runtime/ sdk/ adapters/ xtask/` outside the four
    /// observability surfaces the plan permits it on: log record `host_ts`
    /// and `source_ts`, log file naming, and the control-plane pubsub event
    /// timestamp. Monotonic is the only legal clock on the data plane — a
    /// wall-clock value and a media timestamp share a unit and are different
    /// quantities, so subtracting across them is always a bug. There is no
    /// per-line pragma: widening the list is a plan change. See
    /// `docs/decisions/one-monotonic-clock.md`.
    CheckClockUsage,

    /// Drift trip-wire for the vendored vulkanalia fork trees
    /// (`vendor/tatolab-vulkanalia{,-sys,-vma}`): hashes each vendored crate
    /// dir and fails on any byte change vs. the recorded hash — the guard
    /// against accidental in-place edits (a workspace `cargo fmt --all`
    /// sweep is the classic cause). Deliberate re-vendors update the
    /// recorded hashes in the same commit per
    /// `docs/architecture/vendored-vulkanalia.md`.
    CheckVendoredVulkanalia,

    /// Run every source-walking gate in one process and report all failures.
    /// This is what CI's `source-gates` job runs; the per-gate subcommands stay
    /// for narrowing down a failure locally.
    CheckAllSourceGates,

    /// Run the gates CI runs, so a green run here predicts a green PR. Builds
    /// the workspace, so it is slower than `check-all-source-gates` alone.
    RunLocalCiGates,
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
        Commands::CheckNoInProcessPlacement => {
            check_no_in_process_placement::run(&workspace_root()?)?
        }
        Commands::CheckNoInventorySubmit => check_no_inventory_submit::run(&workspace_root()?)?,
        Commands::CheckNoEscalateInLifecycle => {
            check_no_escalate_in_lifecycle::run(&workspace_root()?)?
        }
        Commands::CheckDeviceWaitIdle => check_device_wait_idle::run(&workspace_root()?)?,
        Commands::CheckNoUnboundedCstrFromPtr => {
            check_no_unbounded_cstr_from_ptr::run(&workspace_root()?)?
        }
        Commands::CheckClockUsage => check_clock_usage::run(&workspace_root()?)?,
        Commands::CheckVendoredVulkanalia => check_vendored_vulkanalia::run(&workspace_root()?)?,
        Commands::CheckAllSourceGates => run_all_source_walking_gates(&workspace_root()?)?,
        Commands::RunLocalCiGates => run_local_ci_gates(&workspace_root()?)?,
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

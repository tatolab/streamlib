// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

pub mod check_boundaries;
pub mod check_bounded_apt_install;
pub mod check_clock_usage;
pub mod check_device_wait_idle;
pub mod check_no_escalate_in_lifecycle;
pub mod check_no_in_process_placement;
pub mod check_no_inventory_submit;
pub mod check_no_unbounded_cstr_from_ptr;
pub mod check_vendored_vulkanalia;
pub mod check_workspace_version_pins;
pub mod generate_third_party_notices;
pub mod lint_logging;
pub mod normal_build_dep_graph;

/// Rust source roots a workspace crate may hold: the classic `src/` and the
/// folder-backed `processors/`. `lint_logging` walks these by name rather
/// than descending from the crate root, so a source tree outside both is
/// invisible to it.
pub const RUST_CRATE_SOURCE_ROOT_DIR_NAMES: &[&str] = &["src", "processors"];

/// Tracked (and untracked-but-not-ignored) files under one repo-relative root.
///
/// `git ls-files` rather than a filesystem walk, for the reason every gate here
/// shares: CI walks a clean checkout, so "the files in the repo" is the
/// semantics meant, and the scan roots hold virtualenvs and build trees that
/// are not ours to gate. `-z` because a path containing a newline would
/// otherwise split into two entries and drop both from the scan.
pub fn list_repository_files_under(
    workspace_root: &Path,
    repo_relative_root: &str,
) -> Result<Vec<String>> {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .arg(repo_relative_root)
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("failed to run `git ls-files` for {repo_relative_root}"))?;

    anyhow::ensure!(
        output.status.success(),
        "`git ls-files {repo_relative_root}` failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    Ok(String::from_utf8(output.stdout)
        .with_context(|| format!("`git ls-files {repo_relative_root}` emitted non-UTF-8 paths"))?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Run `cargo metadata` for the workspace rooted at `manifest_dir` and return
/// the parsed resolve document.
///
/// `--locked` for the reason every other cargo invocation here carries it: a
/// gate that rewrites `Cargo.lock` as a side effect of reading the graph
/// reports on a graph the commit does not contain.
pub fn run_cargo_metadata_resolve_document(manifest_dir: &Path) -> Result<serde_json::Value> {
    let manifest_path = manifest_dir.join("Cargo.toml");
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(&manifest_path)
        .output()
        .with_context(|| format!("running cargo metadata at {}", manifest_path.display()))?;

    anyhow::ensure!(
        output.status.success(),
        "cargo metadata failed at {}: {}",
        manifest_path.display(),
        String::from_utf8_lossy(&output.stderr).trim(),
    );

    serde_json::from_slice(&output.stdout).context("parsing cargo metadata JSON")
}

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
/// lets one process run all eleven in well under a second, and why CI runs them as
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
    ("check-bounded-apt-install", check_bounded_apt_install::run),
    (
        "check-workspace-version-pins",
        check_workspace_version_pins::run,
    ),
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
        ("rustfmt", "cargo", &["fmt", "--all", "--check"]),
        // Default targets only. A test's `println!` is a test's business —
        // `lint-logging` exempts `tests` directories, and `--all-targets` here
        // would deny what that walk deliberately allows.
        // Same exclusion as CI so this really does mirror it: `skia-bindings`
        // cannot build on a runner, and a local gate that lints more than CI
        // does is a gate whose result nobody can act on.
        (
            "clippy",
            "cargo",
            &[
                "clippy",
                "--locked",
                "--workspace",
                "--exclude",
                "streamlib-adapter-skia",
                "--no-deps",
            ],
        ),
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
            "rig-brake tests",
            "bash",
            &[".claude/scripts/tests/rig-brake.test.sh"],
        ),
        (
            "xtask gate fixture tests",
            "cargo",
            &["test", "--locked", "-p", "xtask"],
        ),
        (
            "SDK + macros + processor-schema unit tests",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib",
                "-p",
                "streamlib-macros",
                "-p",
                "streamlib-processor-schema",
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
        (
            "media built-ins unit tests",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib-media-builtins",
                "--lib",
            ],
        ),
        (
            "control-plane unit tests (REST routes + MCP tool dispatch)",
            "cargo",
            &["test", "--locked", "-p", "streamlib-api-server", "--lib"],
        ),
        // Mirrors `test.yml`'s named slice exactly. `streamlib-engine`'s lib
        // tests are not run wholesale anywhere, so this list *is* the set of
        // engine-lib tests under CI — a test added to the workflow's slice
        // and not to this one makes the local runner report a coverage the
        // branch does not have.
        (
            "named engine-lib slice (the only engine-lib tests CI runs)",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib-engine",
                "--lib",
                "--",
                "core::processor_owned_window",
                "processor_owned_window_ops",
                "escalate_wire_encoding_tests",
                "core::compiler::compiler_ops::subprocess_escalate::tests::parse_texture_usages",
                "core::compiler::compiler_ops::subprocess_escalate::tests::the_implied_copy_bits",
                "core::context::audio_device_backend",
                "core::context::silent_null_audio_device_backend",
                "linux::alsa_audio_device_backend::tests::a_thread_that_stopped_because_it_was_told_to_reports_no_failure",
                "linux::alsa_audio_device_backend::tests::every_way_a_thread_dies_early_reaches_the_owner_naming_what_happened",
                "linux::alsa_audio_device_backend::tests::a_stalled_device_is_described_by_what_that_direction_stopped_doing",
                "linux::alsa_audio_device_backend::tests::a_stop_arriving_during_the_last_silent_wait_outranks_the_silence",
                "linux::pipewire_audio_device_backend::tests::a_failure_the_shim_reports_lands_in_the_report_the_owner_holds",
                "linux::pipewire_audio_device_backend::tests::a_failure_the_daemon_did_not_explain_is_still_reported_as_one",
                "iceoryx2::dropped_bag_counters::tests::asking_twice_for_one_links_counter_shares_the_count",
                "iceoryx2::dropped_bag_counters::tests::a_disconnected_links_count_leaves_with_it",
                "iceoryx2::mailbox::tests::an_eviction_is_counted_against_the_link_whose_bag_was_lost",
                "iceoryx2::mailbox::tests::a_mailbox_with_room_counts_nothing",
                "iceoryx2::mailbox::tests::every_bag_a_sustained_overrun_evicts_is_counted",
                "iceoryx2::mailbox::tests::passing_over_bags_to_reach_the_newest_is_not_a_drop_at_the_port",
                "iceoryx2::mailbox::tests::a_manually_injected_frame_evicts_with_no_link_to_charge",
                "iceoryx2::input::tests::each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port",
                "iceoryx2::input::tests::a_port_that_keeps_up_reports_a_zero_for_every_wired_link",
                "iceoryx2::input::tests::a_disconnected_links_count_goes_with_the_link",
                "core::graph::components::processor_metrics::tests::a_processors_metrics_render_every_inbound_links_losses_by_name",
                "core::graph::components::processor_metrics::tests::a_processor_that_has_lost_nothing_says_so_rather_than_staying_silent",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_dropping_destinations_node_renders_each_inbound_links_losses",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_helper_placed_destinations_node_carries_no_metrics_rather_than_a_zero",
                "core::runtime::tap::tests::stalled_downstream_never_blocks_the_drain_and_detach_returns_promptly",
                "iceoryx2::node::tests::overflow_enabled_publisher_does_not_block_on_full_buffer",
                "iceoryx2::channel_sizing_tests::every_channel_service_opens_under_safe_overflow",
                "iceoryx2::delivery_profile::tests::newest_resolves_to_skip_drop_shallow",
                "iceoryx2::delivery_profile::tests::ordered_resolves_to_fifo_drop_deep",
                "iceoryx2::delivery_profile::tests::profile_parses_known_and_rejects_unknown",
                "iceoryx2::delivery_profile::tests::manifest_str_roundtrips_through_the_declaration_constant",
                "iceoryx2::delivery_profile::tests::port_declaration_resolution::unregistered_processor_falls_back_to_newest",
                "iceoryx2::delivery_profile::tests::port_declaration_resolution::declared_profile_is_the_whole_answer",
                "iceoryx2::delivery_profile::tests::port_declaration_resolution::missing_declaration_is_a_wiring_error_naming_the_port",
                "iceoryx2::delivery_profile::tests::port_declaration_resolution::unknown_declared_value_is_rejected_with_the_legal_values",
                "core::json_schema::port_rendering_tests::port_info_output_renders_exactly_the_declared_keys",
                "core::json_schema::port_rendering_tests::port_info_output_carries_no_type_key_under_any_spelling",
                "core::json_schema::port_rendering_tests::port_descriptor_output_carries_no_type_key",
                "core::json_schema::port_rendering_tests::a_contract_bearing_port_renders_its_contract_beside_the_four",
                "core::json_schema::port_rendering_tests::a_port_declaring_the_sentinel_renders_it_as_a_whole_contract",
                "core::json_schema::port_rendering_tests::a_declared_contract_survives_the_descriptor_to_port_info_hop",
                "core::json_schema::port_rendering_tests::a_contract_bearing_descriptor_renders_its_contract_too",
            ],
        ),
        // The deviceless arm's integration binaries, which the workflow runs
        // beside the slice. `attribute_macro_test` aside, these are the only
        // engine integration tests CI runs at all.
        (
            "the deviceless audio arm's integration binaries",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib-engine",
                "--test",
                "silent_null_arm_plays_what_it_is_given",
                "--test",
                "silent_null_arm_captures_without_ever_dying",
            ],
        ),
        // The dependency closure's licences, against `deny.toml`'s allowlist.
        // Not a source-walking gate: those are in-process tree walkers by
        // contract, and this shells out to a binary that is not part of the
        // toolchain — `cargo install cargo-deny@0.20.2 --locked` if the run
        // reports no such command, matching the version `source-gates.yml`
        // pins. `--locked` because cargo-deny will otherwise rewrite
        // `Cargo.lock` to resolve the graph and then report on the rewrite.
        // `--workspace` so a crate reached only from a workspace member nobody
        // builds locally is still in scope, and `-D license-not-encountered` so
        // an allowance whose last user left the graph fails rather than warns.
        //
        // Last, like CI runs it: it is the only entry here that resolves the
        // whole dependency graph, so a failure in it cannot cost the others
        // their report.
        (
            "cargo deny check licenses",
            "cargo",
            &[
                "deny",
                "--locked",
                "--workspace",
                "check",
                "licenses",
                "-D",
                "license-not-encountered",
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
    /// anywhere under `runtime/ sdk/ adapters/ xtask/ packages/test-fixtures/`
    /// outside the four
    /// observability surfaces the plan permits it on: log record `host_ts`
    /// and `source_ts`, log file naming, and the control-plane pubsub event
    /// timestamp. Monotonic is the only legal clock on the data plane — a
    /// wall-clock value and a media timestamp share a unit and are different
    /// quantities, so subtracting across them is always a bug. There is no
    /// per-line pragma: widening the list is a plan change. See
    /// `docs/decisions/one-monotonic-clock.md`.
    CheckClockUsage,

    /// CI gate keeping every apt install in CI behind
    /// `.github/actions/install-linux-engine-build-dependencies`. Fails on any
    /// `apt-get` under `.github/workflows/`, and on any step calling that
    /// action without `timeout-minutes`. An inline `apt-get update && apt-get
    /// install` has no wall-clock bound, and the mode that costs is a mirror
    /// that is slow rather than stalled — one measured run fetched 35.6 MB at
    /// 48 kB/s over 12m17s while every request made progress, so neither
    /// `Acquire::Retries` (nothing failed) nor `Acquire::http::Timeout`
    /// (nothing went idle) engaged. Composite-action steps cannot declare
    /// `timeout-minutes`, so the caller's step is the only place the native
    /// backstop can live.
    CheckBoundedAptInstall,

    /// Drift trip-wire for the vendored vulkanalia fork trees
    /// (`vendor/tatolab-vulkanalia{,-sys,-vma}`): hashes each vendored crate
    /// dir and fails on any byte change vs. the recorded hash — the guard
    /// against accidental in-place edits (a workspace `cargo fmt --all`
    /// sweep is the classic cause). Deliberate re-vendors update the
    /// recorded hashes in the same commit per
    /// `docs/architecture/vendored-vulkanalia.md`.
    CheckVendoredVulkanalia,

    /// CI gate keeping every in-tree `{ path = "…", version = "…" }` requirement
    /// equal to `[workspace.package] version`. release-please's `simple` release
    /// type bumps the workspace version and ships no cargo dependency-requirement
    /// updater, so the pins sit still while the crates move. Inside one minor line
    /// that is invisible (`^0.17.0` matches `0.17.1`); the next breaking bump makes
    /// the workspace unresolvable, because `^0.17.0` excludes `0.18.0` — which is
    /// what held release 0.18.0 shut from 2026-08-11 and starved the PEP 503 index
    /// of every wheel since. `cargo metadata --no-deps` parses it clean, so only a
    /// real resolve catches it, and the first real resolve is on the release
    /// branch. `--fix` moves every drifted pin onto the workspace version; the
    /// release workflow calls it right after the bump.
    CheckWorkspaceVersionPins {
        /// Rewrite drifted pins instead of reporting them.
        #[arg(long)]
        fix: bool,
    },

    /// Run every source-walking gate in one process and report all failures.
    /// This is what CI's `source-gates` job runs; the per-gate subcommands stay
    /// for narrowing down a failure locally.
    CheckAllSourceGates,

    /// Run the gates CI runs, so a green run here predicts a green PR. Builds
    /// the workspace, so it is slower than `check-all-source-gates` alone.
    RunLocalCiGates,

    /// Regenerate `THIRD-PARTY-NOTICES.md` — the Rust closure's licence texts
    /// via `cargo about generate`, plus the vendored C++ projects that are not
    /// packages in the Cargo resolve graph and so reach the file only by being
    /// appended. Needs `cargo-about` installed and the network, which is why it
    /// is a command and not a gate; `cargo deny check licenses` is the half that
    /// runs on every PR. See [`generate_third_party_notices`] for the roster and
    /// why each project is on it.
    GenerateThirdPartyNotices,
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
        Commands::CheckBoundedAptInstall => check_bounded_apt_install::run(&workspace_root()?)?,
        Commands::CheckVendoredVulkanalia => check_vendored_vulkanalia::run(&workspace_root()?)?,
        Commands::CheckWorkspaceVersionPins { fix } => {
            let workspace_root = workspace_root()?;
            if fix {
                check_workspace_version_pins::rewrite_version_pins_to_workspace_version(
                    &workspace_root,
                )?;
            } else {
                check_workspace_version_pins::run(&workspace_root)?;
            }
        }
        Commands::CheckAllSourceGates => run_all_source_walking_gates(&workspace_root()?)?,
        Commands::RunLocalCiGates => run_local_ci_gates(&workspace_root()?)?,
        Commands::GenerateThirdPartyNotices => {
            generate_third_party_notices::run(&workspace_root()?)?
        }
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

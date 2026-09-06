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
pub mod codec_proof_image_measurement;
pub mod generate_third_party_notices;
pub mod lint_logging;
mod mp4_inspect;
pub mod normal_build_dep_graph;
pub mod psnr;

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
            "license header gate tests",
            "bash",
            &[".claude/scripts/tests/license-header-gate.test.sh"],
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
                "iceoryx2::mailbox::tests::a_hand_over_keeps_each_frames_inbound_link_and_its_order",
                "iceoryx2::mailbox::tests::a_hand_over_re_measures_every_frame_by_the_replacements_own_measure",
                "iceoryx2::mailbox::tests::a_mailbox_with_room_counts_nothing",
                "iceoryx2::mailbox::tests::every_bag_a_sustained_overrun_evicts_is_counted",
                "iceoryx2::mailbox::tests::passing_over_bags_to_reach_the_newest_is_not_a_drop_at_the_port",
                "iceoryx2::mailbox::tests::a_manually_injected_frame_evicts_with_no_link_to_charge",
                "iceoryx2::mailbox::tests::an_installed_notice_is_signalled_for_every_evicted_bag_and_never_with_room",
                "iceoryx2::input::tests::each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port",
                "iceoryx2::input::tests::a_port_that_keeps_up_reports_a_zero_for_every_wired_link",
                "iceoryx2::input::tests::a_disconnected_links_count_goes_with_the_link",
                "iceoryx2::input::tests::two_inbound_links_hand_a_reader_the_link_each_bag_arrived_on",
                "iceoryx2::input::tests::naming_the_inbound_link_a_bag_arrived_on_leaves_the_per_link_drop_counts_alone",
                "iceoryx2::input::tests::a_port_lists_the_inbound_links_wired_into_it_and_a_port_with_none_lists_none",
                "iceoryx2::input::tests::a_windowed_ports_read_names_the_one_link_that_feeds_it",
                "iceoryx2::input::tests::an_injected_bag_with_no_inbound_link_is_refused_by_name_rather_than_borrowing_one",
                "iceoryx2::input::tests::the_typed_read_deserializes_the_bag_and_a_drained_port_is_not_an_error",
                "core::graph::components::processor_metrics::tests::a_processors_metrics_render_every_inbound_links_losses_by_name",
                "core::graph::components::processor_metrics::tests::a_processor_that_has_lost_nothing_says_so_rather_than_staying_silent",
                "core::runtime::operations_runtime::connect_wires_without_inspecting_a_port_tests::connect_wires_a_producer_to_a_consumer_without_warning",
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
                "core::runtime::capability_extensions::tests::one_distribution_registering_two_capabilities_keeps_both_in_order",
                "core::runtime::capability_extensions::tests::two_distributions_registering_one_capability_name_are_refused_naming_both",
                "core::runtime::capability_extensions::tests::a_capability_registered_with_an_empty_name_is_refused_naming_its_distribution",
                "core::json_schema::capability_extension_rendering_tests::a_graph_with_no_extensions_still_carries_the_key_as_an_empty_list",
                "core::json_schema::capability_extension_rendering_tests::a_registered_extension_renders_its_name_version_and_distribution",
                "core::runtime::runtime::tests::to_json_renders_every_capability_this_process_registered",
                "core::json_schema::port_rendering_tests::port_info_output_renders_exactly_the_declared_keys",
                "core::json_schema::port_rendering_tests::port_info_output_carries_no_type_key_under_any_spelling",
                "core::json_schema::port_rendering_tests::port_descriptor_output_carries_no_type_key",
                "core::json_schema::port_rendering_tests::a_contract_bearing_port_renders_its_contract_beside_the_four",
                "core::json_schema::port_rendering_tests::a_port_declaring_the_sentinel_renders_it_as_a_whole_contract",
                "core::json_schema::port_rendering_tests::a_declared_contract_survives_the_descriptor_to_port_info_hop",
                "core::json_schema::port_rendering_tests::a_contract_bearing_descriptor_renders_its_contract_too",
                "iceoryx2::audio_window::audio_window_accumulator::stamp_arithmetic_tests::a_frame_index_past_a_u64_multiplys_reach_is_still_stamped_exactly",
                "iceoryx2::audio_window::audio_window_accumulator::stamp_arithmetic_tests::the_widening_changes_no_answer_a_window_sized_run_produces",
                "iceoryx2::audio_window::audio_window_stage_tests::a_gap_hidden_in_the_queue_costs_one_empty_read_and_no_more",
                "iceoryx2::audio_window::audio_window_stage_tests::a_single_evicted_block_displaces_the_stamps_enough_to_flush",
                "iceoryx2::audio_window::audio_window_stage_tests::a_full_mailbox_that_still_cannot_make_a_window_says_so_once",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::the_profiles_depth_is_a_floor_no_contract_undercuts",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::every_windows_depth_holds_a_windows_worth_of_the_assumed_quantum",
                "core::execution::thread_runner::tests::a_bag_that_does_not_complete_a_window_does_not_dispatch_the_reactive_runner",
                "core::execution::thread_runner::tests::a_processor_with_no_input_mailboxes_is_not_gated_at_all",
                "iceoryx2::input::tests::read_raw_bounded_stages_oversized_frame_and_redelivers",
                // The audio window contract's read-side stage — the stage's
                // exactness, its overlap, the flush that resets the
                // resampler's filter state, its refusals, and the readiness
                // floor that must never claim a window a read cannot then
                // produce, plus the same contract at the seam and at wire
                // time. Real iceoryx2 services need /dev/shm and nothing else.
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::a_block_written_by_the_stage_reads_back_as_the_block_it_wrote",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::a_decoded_block_borrows_its_payload_out_of_the_frame_body_rather_than_copying_it",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::an_absent_dtype_reads_as_f32_the_way_the_wire_contract_says",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::a_bag_carrying_extra_keys_is_read_rather_than_refused",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::an_unknown_dtype_is_refused_naming_the_value_and_the_legal_ones",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::a_payload_that_disagrees_with_the_count_is_refused_naming_both_lengths",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::a_bag_with_no_audio_block_keys_is_refused_rather_than_reshaped",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::an_i16_contract_saturates_at_both_endpoints_rather_than_wrapping",
                "iceoryx2::audio_window::audio_block_bag_wire_codec::tests::an_i16_scalar_survives_the_decode_and_encode_round_trip_unchanged",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::a_declared_contract_resolves_to_the_five_values_it_declared",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::a_declaration_the_stage_could_not_honour_is_refused_naming_the_port",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::a_one_second_window_is_sized_past_the_profiles_depth_by_its_own_quanta",
                "iceoryx2::audio_window::audio_window_stage_tests::a_48k_stereo_source_reaches_a_16k_mono_512_port_as_exact_windows_32ms_apart",
                "iceoryx2::audio_window::audio_window_stage_tests::the_first_window_carries_the_anchor_stamp_rather_than_one_a_group_delay_later",
                "iceoryx2::audio_window::audio_window_stage_tests::a_hop_of_160_against_a_window_of_512_overlaps_by_352_samples",
                "iceoryx2::audio_window::audio_window_stage_tests::one_1024_sample_quantum_against_a_512_512_contract_yields_exactly_two_windows",
                "iceoryx2::audio_window::audio_window_stage_tests::a_stream_that_stops_mid_window_hands_over_nothing_rather_than_a_short_block",
                "iceoryx2::audio_window::audio_window_stage_tests::the_first_window_after_a_gap_carries_no_energy_from_before_it",
                "iceoryx2::audio_window::audio_window_stage_tests::no_window_spans_a_gap_in_the_source_stream",
                "iceoryx2::audio_window::audio_window_stage_tests::a_stamp_jittering_inside_half_a_quantum_does_not_flush_the_run",
                "iceoryx2::audio_window::audio_window_stage_tests::a_stereo_source_reaching_a_mono_contract_is_averaged_across_its_channels",
                "iceoryx2::audio_window::audio_window_stage_tests::a_mono_source_reaching_a_stereo_contract_is_duplicated_across_its_channels",
                "iceoryx2::audio_window::audio_window_stage_tests::a_channel_pair_with_neither_side_at_one_is_refused_naming_both_counts",
                "iceoryx2::audio_window::audio_window_stage_tests::a_bag_the_stage_cannot_read_is_refused_by_name_rather_than_reshaped",
                "iceoryx2::audio_window::audio_window_stage_tests::an_i16_contract_emits_windows_whose_scalars_are_written_as_i16",
                "iceoryx2::audio_window::audio_window_stage_tests::the_readiness_floor_never_claims_a_window_the_read_cannot_then_produce",
                "iceoryx2::audio_window::audio_window_stage_tests::the_readiness_floor_says_yes_well_inside_the_depth_the_mailbox_is_sized_to",
                "iceoryx2::audio_window::audio_window_stage_tests::a_contract_declaring_no_channels_emits_the_sources_own_count",
                "iceoryx2::audio_window::audio_window_stage_tests::the_same_channel_free_contract_emits_mono_from_a_mono_source",
                "iceoryx2::audio_window::audio_window_stage_tests::a_channel_free_contract_still_resamples_to_the_rate_it_declared",
                "iceoryx2::audio_window::audio_window_stage_tests::a_source_that_changes_its_channel_count_flushes_rather_than_mixing_two_counts",
                "iceoryx2::audio_window::audio_window_stage_tests::readiness_on_a_channel_free_contract_never_claims_a_window_the_read_cannot_produce",
                "iceoryx2::audio_window::audio_window_stage_tests::a_source_a_declared_pair_would_refuse_rides_a_channel_free_contract_through",
                "core::json_schema::port_rendering_tests::a_port_that_declared_no_channel_count_renders_it_as_the_source",
                "iceoryx2::input::tests::a_windowed_port_reports_data_only_once_a_full_window_can_be_emitted",
                "iceoryx2::input::tests::a_resampling_windowed_port_reports_data_only_once_a_full_window_can_be_emitted",
                "iceoryx2::input::tests::one_1024_sample_quantum_reads_out_of_a_512_512_port_exactly_twice",
                "iceoryx2::input::tests::a_contract_less_port_still_reads_the_bag_the_producer_published_byte_for_byte",
                "iceoryx2::input::tests::a_windowed_ports_per_link_drop_counts_match_an_unwindowed_ports_under_the_same_overrun",
                "iceoryx2::input::tests::a_long_windows_mailbox_is_sized_from_its_contract_rather_than_the_profiles_depth",
                "iceoryx2::input::tests::a_bag_the_stage_cannot_read_is_refused_at_the_read_naming_the_port",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_windowed_destinations_contract_resolves_at_wire_time",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_port_declaring_no_contract_resolves_to_none",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_match_device_contract_wires_awaiting_its_device_rather_than_refusing",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_second_inbound_link_into_a_windowed_port_is_refused_naming_the_port_and_both_links",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::the_one_inbound_link_a_windowed_port_takes_is_not_refused",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_disconnected_link_does_not_count_against_a_windowed_ports_one_inbound_link",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::the_sentinel_reads_as_a_port_awaiting_its_device_rather_than_as_five_values",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::an_unsettled_sentinel_is_refused_naming_the_resolution_mechanism",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::a_device_stream_format_resolves_to_the_contract_that_plays_on_it",
                "iceoryx2::audio_window::resolved_audio_window_contract::tests::a_device_stream_format_the_stage_could_not_honour_is_refused_too",
                "iceoryx2::audio_window::device_matched_audio_window_contracts::tests::a_port_nothing_resolved_reads_back_as_unsettled",
                "iceoryx2::audio_window::device_matched_audio_window_contracts::tests::a_settled_port_renders_the_five_values_the_device_gave_it",
                "iceoryx2::audio_window::device_matched_audio_window_contracts::tests::resolving_a_second_time_replaces_the_contract_rather_than_keeping_the_first",
                "iceoryx2::input::tests::a_port_awaiting_its_device_hands_a_reader_nothing_however_much_is_queued",
                "iceoryx2::input::tests::settling_a_port_converts_the_bags_that_arrived_before_the_device_was_known",
                "iceoryx2::input::tests::only_a_port_still_holding_the_sentinel_is_listed_as_awaiting_its_device",
                "iceoryx2::input::tests::a_contract_settled_before_its_port_existed_is_still_there_for_the_wiring",
                "iceoryx2::input::tests::a_device_format_the_stage_cannot_honour_is_refused_at_the_settle_naming_the_port",
                "iceoryx2::input::tests::a_bag_evicted_at_a_port_with_an_unsettled_contract_says_so_once_naming_the_port",
                "iceoryx2::input::tests::a_settled_ports_evictions_are_not_reported_as_an_unsettled_contract",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_match_device_port_on_a_helper_placed_destination_is_refused_at_wire_time",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_settled_contract_reaches_a_helper_placed_destination_as_five_values",
                "core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_settled_contract_reaches_graph_on_the_port_that_settled_it",
                "core::compiler::compiler_ops::spawn_processor_op::tests::a_setup_that_settled_no_match_device_contract_is_refused_naming_the_port",
                "core::compiler::compiler_ops::spawn_processor_op::tests::a_setup_that_settled_its_contract_is_not_refused",
                "core::compiler::compiler_ops::spawn_processor_op::tests::a_processor_holding_no_input_mailboxes_is_not_refused",
                "core::json_schema::port_rendering_tests::a_settled_match_device_port_renders_the_five_values_its_device_gave",
                "core::json_schema::port_rendering_tests::an_unsettled_match_device_port_still_renders_the_sentinel",
                "core::json_schema::port_rendering_tests::the_settled_contracts_render_on_the_port_and_not_as_a_component_of_their_own",
                // The chroma-siting lock: the reconstruction offset against
                // the chroma_sample_loc_type the encoder leaves unsignalled.
                "vulkan::video::nv12_to_rgb::tests::chroma_is_reconstructed_at_the_siting_the_encoders_bitstream_implies",
                "vulkan::video::decode::tests::annex_b_framed_parameter_sets_open_a_decodable_stream",
                "vulkan::video::decode::tests::either_start_code_length_frames_parameter_sets_the_reader_accepts",
                "vulkan::video::decode::tests::parameter_sets_carrying_no_start_code_are_refused_rather_than_silently_dropped",
                "vulkan::video::decode::tests::empty_parameter_sets_are_refused_naming_what_a_decoder_needed",
                "vulkan::video::decode::tests::parameter_sets_missing_one_required_set_are_refused_naming_only_that_one",
                "vulkan::video::decode::tests::h265_parameter_sets_carrying_no_vps_still_open_a_decodable_stream",
                "vulkan::video::decode::tests::a_truncated_h265_nal_header_is_not_counted_as_a_parameter_set",
                "vulkan::video::decode::tests::a_sync_point_access_unit_reads_back_as_its_parameter_sets_then_its_idr",
                "vulkan::video::decode::tests::trailing_zero_bytes_between_nal_units_stay_out_of_the_payload",
                // The conformance-window crop, which is the whole of the
                // H.265 CTU-pad contract in pure math: a 1088-tall coded
                // picture publishes 1080, and the chroma format decides how
                // many luma rows an offset takes.
                "vulkan::video::decode::decoded_picture_display_window::tests::the_ctu_pad_this_engines_h265_encoder_emits_crops_back_to_1080",
                "vulkan::video::decode::decoded_picture_display_window::tests::the_chroma_format_decides_how_many_luma_rows_an_offset_takes",
                "vulkan::video::decode::decoded_picture_display_window::tests::an_aligned_extent_carries_no_window_and_crops_nothing",
                "vulkan::video::decode::decoded_picture_display_window::tests::a_left_top_offset_moves_the_origin_rather_than_shrinking_the_far_edge",
                "vulkan::video::decode::decoded_picture_display_window::tests::a_window_cropping_past_the_coded_picture_is_refused_rather_than_wrapped",
                "vulkan::video::decode::decoded_picture_display_window::tests::offsets_that_overflow_their_own_sum_are_refused_rather_than_wrapped",
                "vulkan::video::decode::decoded_picture_display_window::tests::h264_frame_cropping_takes_two_luma_rows_per_offset_when_frame_coded",
                "vulkan::video::decode::decoded_picture_display_window::tests::h264_field_coding_doubles_the_rows_an_offset_takes",
                "vulkan::video::decode::decoded_picture_display_window::tests::h264_separate_colour_planes_crop_in_monochrome_units",
                "vulkan::video::decode::decoded_picture_display_window::tests::an_h264_window_cropping_past_the_coded_picture_is_refused_too",
            ],
        ),
        // The rig-tier integration binary that drives the two `match_device`
        // call sites, compiled only: it stands up a real graph and needs a GPU,
        // so building it is what keeps it from rotting between rig runs.
        (
            "the match_device integration binary compiles",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib-media-builtins",
                "--test",
                "speaker_sink_matches_its_device",
                "--no-run",
            ],
        ),
        // The rig-tier camera→H264Encoder round trip, compiled only for the
        // same reason: it needs a Vulkan Video encode queue and a /dev/video*
        // device, and it is the only thing driving the encoder's production
        // seam between rig runs.
        (
            "the H264Encoder bag-convention integration binary compiles",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib-media-builtins",
                "--test",
                "h264_encoder_publishes_the_bag_convention",
                "--no-run",
            ],
        ),
        // #1077 read forwards: test pattern -> encode -> decode in one
        // graph, once per codec off one shared harness. Compiled only — they
        // need Vulkan Video encode *and* decode queues, which no CI runner
        // has. The H.265 arm is where the CTU crop is asserted end to end.
        (
            "the codec round-trip integration binaries compile",
            "cargo",
            &[
                "test",
                "--locked",
                "-p",
                "streamlib-media-builtins",
                "--test",
                "h264_decoder_completes_the_round_trip",
                "--test",
                "h265_decoder_completes_the_round_trip",
                "--no-run",
            ],
        ),
        // The engine-owned codec round-trip rig. Examples are not a default
        // cargo target, so no other entry here builds it and it would rot
        // between rig runs unnoticed.
        (
            "the codec round-trip rig example compiles",
            "cargo",
            &[
                "build",
                "--locked",
                "-p",
                "streamlib-engine",
                "--example",
                "codec_roundtrip_rig",
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

    /// The codec proof's scorer: PSNR of a decoded frame set against the
    /// references that produced it, and the vivid rig's channel-mean drift
    /// lock, with the three bug-injection modes that keep either gate
    /// non-vacuous. Pure image math over PNGs `streamlib exchange` wrote —
    /// GPU-free, so it is tested in CI while the rig runs that feed it are
    /// not. See `docs/plan/ARCHITECTURE.md` §Media I/O.
    #[command(subcommand)]
    Psnr(psnr::PsnrCommand),

    /// Report what a recording actually contains — tracks and the inbound
    /// link each one was named after, sample entries, fragments and durations
    /// — as JSON. Pure Rust over `mp4-atom`, the same pin `Mp4Sink` writes
    /// with, so nothing downstream needs ffprobe.
    Mp4Inspect(mp4_inspect::Mp4InspectCommand),

    /// Regenerate `THIRD-PARTY-NOTICES.md` — the Rust closure's licence texts
    /// via `cargo about generate`, plus the vendored C++ projects that are not
    /// packages in the Cargo resolve graph and so reach the file only by being
    /// appended. Needs `cargo-about` installed and the network, which is why it
    /// is a command and not a gate; `cargo deny check licenses` is the half that
    /// runs on every PR. See [`generate_third_party_notices`] for the roster and
    /// why each project is on it.
    GenerateThirdPartyNotices {
        /// Generate for one standalone extension wheel under `packages/`
        /// instead of the engine workspace: its own closure, its own file, and
        /// no vendored C++ appendix.
        #[arg(long, value_name = "PACKAGE_DIRECTORY")]
        extension_package_directory: Option<PathBuf>,
    },
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
        Commands::Psnr(psnr_command) => psnr::run(psnr_command)?,
        Commands::Mp4Inspect(inspect_command) => mp4_inspect::run(inspect_command)?,
        Commands::GenerateThirdPartyNotices {
            extension_package_directory,
        } => generate_third_party_notices::run(
            &workspace_root()?,
            &generate_third_party_notices::NoticesGenerationTarget::
                from_optional_extension_package_directory(extension_package_directory),
        )?,
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

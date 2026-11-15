// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Building an engine, what it describes of itself, where its records land, and its setup hooks.

use super::two_streams_sharing_one_engine_tests::every_record_of_the_stream_log_at;
use super::*;

/// A task on the engine's tokio runtime still holding a stream as the
/// `Runner` drops never holds the engine past it: the runtime the
/// `Runner` owns goes first, taking the task with it, so the engine's last
/// drop is the `Runner`'s own and never lands on one of that runtime's
/// workers.
#[test]
#[serial]
fn a_task_holding_a_stream_as_the_runner_drops_never_holds_the_engine_past_it() {
    crate::core::test_support::ensure_test_mocks_registered();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("the engine builds");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "held");
    let the_engine = Arc::downgrade(stream.engine_resources_shared_by_every_stream());
    let (the_task_holds_its_stream, the_task_holds_its_stream_now) = std::sync::mpsc::channel();

    let held_stream = Arc::clone(&stream);
    stream
        .engine_resources_shared_by_every_stream()
        .tokio_runtime_variant
        .handle()
        .spawn(async move {
            let _ = the_task_holds_its_stream.send(());
            std::future::pending::<()>().await;
            drop(held_stream);
        });
    the_task_holds_its_stream_now
        .recv_timeout(Duration::from_secs(5))
        .expect("the task holds its stream");
    drop(stream);
    drop(runner);

    assert!(
        the_engine.upgrade().is_none(),
        "the engine outlived the Runner, held by a task on the runtime's workers"
    );
}

// All Runner::new() tests are `#[serial]` because the runtime
// reads/writes process-global env vars (XDG_RUNTIME_DIR,
// STREAMLIB_RUNTIME_ID) and every runtime's listeners share the
// process-wide event bus. The test module's `#[serial]` default group
// serializes every test that constructs a Runner so nobody reads env
// mid-mutation.

#[test]
#[serial]
fn test_runtime_creation() {
    let _runtime = Runner::new();
    // Runtime creates successfully
}

/// `Runner::new()` must not walk any compile-time-linked registration
/// source: the engine substrate ships empty.
///
/// `PROCESSOR_REGISTRY` is a process-global `LazyLock` and earlier
/// tests in the same binary may have populated it, so the
/// assertion is over the *delta* `Runner::new()` introduces, not
/// the absolute size.
#[test]
#[serial]
fn runner_new_registers_zero_processors() {
    use crate::core::processors::PROCESSOR_REGISTRY;
    let before = PROCESSOR_REGISTRY.list_registered().len();
    let _runtime = Runner::new().expect("Runner::new");
    let after = PROCESSOR_REGISTRY.list_registered().len();
    assert_eq!(
        after, before,
        "Runner::new() must not register any processors — the engine \
         substrate ships empty (issue #793). Delta: {before} → {after}."
    );
}

/// Nothing is claimed for a machine's name: two runners on one machine
/// both construct, both stay up together, and each names the machine the
/// same way wherever the name is read.
#[test]
#[serial]
fn two_runners_on_one_machine_both_construct_and_name_it_alike() {
    let project_directory = a_project_directory_this_test_owns();
    let first = Runner::new().expect("the first runner constructs");
    let second = Runner::new().expect("a second runner on the same machine constructs");

    assert_ne!(first.runtime_id(), second.runtime_id());
    assert_eq!(first.machine_name(), second.machine_name());
    for runner in [&first, &second] {
        let stream = an_empty_stream_loaded_into(runner, project_directory.path(), "main");
        assert_eq!(
            stream.to_json().expect("the graph serializes")["machine"],
            runner.machine_name().name()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
#[serial]
fn runner_new_caps_timer_slack_to_one_nanosecond() {
    // Linux default is 50_000 ns. `Runner::new` calls
    // `prctl(PR_SET_TIMERSLACK, 1)` as its very first step;
    // `prctl(PR_GET_TIMERSLACK)` on the same thread should report 1.
    let _runtime = Runner::new().expect("Runner::new");
    let slack = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK) };
    assert_eq!(
        slack, 1,
        "expected timer slack 1 ns after Runner::new (got {})",
        slack
    );
}

#[test]
#[serial]
fn test_new_outside_tokio_creates_owned_runtime() {
    // Outside tokio context - creates owned runtime
    let runtime = Runner::new().unwrap();
    assert!(matches!(
        runtime
            .engine_resources_shared_by_every_stream
            .tokio_runtime_variant,
        TokioRuntimeVariant::TokioRuntimeTheRunnerOwns(_)
    ));
    assert!(runtime._tokio_runtime_the_runner_owns.is_some());
}

/// Fail-without-fix: a plain drop of the runtime waits for the blocking task
/// below for its whole minute.
#[test]
fn an_owned_tokio_runtime_whose_blocking_task_never_returns_still_drops_within_its_budget() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime builds");
    let (blocking_task_started, blocking_task_has_started) = std::sync::mpsc::channel();
    runtime.spawn_blocking(move || {
        let _ = blocking_task_started.send(());
        std::thread::sleep(Duration::from_secs(60));
    });
    blocking_task_has_started
        .recv_timeout(Duration::from_secs(5))
        .expect("the blocking task starts");

    let owned = TokioRuntimeShutDownWithinItsBudget::owning(runtime);
    let started = std::time::Instant::now();
    drop(owned);

    assert!(
        started.elapsed() < OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET + Duration::from_secs(1),
        "dropping the runtime took {:?}",
        started.elapsed()
    );
}

#[test]
#[serial]
fn test_new_inside_tokio_uses_external_handle() {
    // Inside tokio context - auto-detects and uses external handle
    let temp_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = temp_rt.block_on(async { Runner::new() });
    assert!(result.is_ok());
    let runtime = result.unwrap();
    assert!(matches!(
        runtime
            .engine_resources_shared_by_every_stream
            .tokio_runtime_variant,
        TokioRuntimeVariant::ExternalTokioHandle(_)
    ));
}

/// Fail-without-fix: the request wrote the configuration onto the node
/// before the processor was asked, so one it refused at commit stayed in
/// `graph` as though it had been taken.
#[test]
#[serial]
fn a_requested_configuration_waits_for_the_commit_rather_than_landing_on_the_graph_node() {
    use crate::core::compiler::PendingOperation;
    use crate::core::processors::ProcessorSpec;
    use crate::core::test_support::{MockSourceTakingOneSetting, ensure_test_mocks_registered};

    ensure_test_mocks_registered();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
    let processor_id = stream
        .add_processor(ProcessorSpec::new(
            MockSourceTakingOneSetting::processor_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the mock is added");

    stream
        .update_processor_config(&processor_id, serde_json::json!({"frame_width": 3}))
        .expect("the update is queued");

    let config_on_the_node = stream.compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(&processor_id)
            .first()
            .expect("the node is in the graph")
            .config
            .clone()
    });
    assert_eq!(config_on_the_node, Some(serde_json::json!({})));
    assert!(
        stream.compiler.logged_pending_operations().iter().any(|op| matches!(
            op,
            PendingOperation::UpdateProcessorConfig { processor_id: queued_for, config_to_apply }
                if *queued_for == processor_id && *config_to_apply == serde_json::json!({"frame_width": 3})
        )),
        "the update carries its configuration to the commit"
    );
}

#[test]
#[serial]
fn a_configuration_its_node_types_config_schema_refuses_is_refused_and_never_queued() {
    use crate::core::compiler::PendingOperation;
    use crate::core::processors::ProcessorSpec;
    use crate::core::test_support::{MockSourceTakingOneSetting, ensure_test_mocks_registered};

    ensure_test_mocks_registered();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
    let processor_id = stream
        .add_processor(ProcessorSpec::new(
            MockSourceTakingOneSetting::processor_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the mock is added");

    let refusal = stream
        .update_processor_config(&processor_id, serde_json::json!({"frame_width": "wide"}))
        .expect_err("a string where the schema states an integer is refused")
        .to_string();

    assert!(refusal.contains("`frame_width`"), "{refusal}");
    assert!(refusal.contains("\"wide\""), "{refusal}");
    assert!(
        !stream
            .compiler
            .logged_pending_operations()
            .iter()
            .any(|op| matches!(op, PendingOperation::UpdateProcessorConfig { .. })),
        "a refused update reaches no commit"
    );
}

#[test]
#[serial]
fn test_sync_methods_work_inside_tokio() {
    // Verify sync methods work when called from tokio context
    let temp_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    temp_rt.block_on(async {
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().unwrap();
        let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
        // Sync methods should work (use spawn + channel internally)
        let json = stream.to_json().unwrap();
        assert!(json["nodes"].is_array());
    });
}

#[test]
#[serial]
fn a_runtimes_uptime_never_decreases_and_grows_once_a_millisecond_has_passed() {
    let runner = Runner::new().expect("the engine builds");
    let first_up_for_ms = runner.describe_this_runtime().up_for_ms;
    let first_read_measured_at = Instant::now();
    assert!(runner.describe_this_runtime().up_for_ms >= first_up_for_ms);
    while first_read_measured_at.elapsed() < Duration::from_millis(2) {
        std::thread::yield_now();
    }
    assert!(
        runner.describe_this_runtime().up_for_ms > first_up_for_ms,
        "2 ms past the first read, the uptime is larger than its {first_up_for_ms} ms"
    );
}

#[test]
#[serial]
fn a_runtime_describes_itself_by_its_crate_version_and_the_engine_build_id() {
    let runtime_description = Runner::new()
        .expect("the engine builds")
        .describe_this_runtime();
    assert_eq!(runtime_description.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        runtime_description.build_id,
        crate::core::helper_process_transport::ENGINE_BUILD_ID
    );
}

#[test]
#[serial]
fn a_runtime_without_an_own_log_directory_names_no_log_folder() {
    assert_eq!(
        Runner::new()
            .expect("the engine builds")
            .describe_this_runtime()
            .runtime_log_directory,
        None
    );
}

#[test]
#[serial]
fn a_runtime_given_an_own_log_directory_names_it_as_its_log_folder() {
    let state_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let runtime_own_log_directory = state_directory.path().join("logs");
    let runner = Runner::new_with_construction_options(RunnerConstructionOptions {
        runtime_own_log_directory: Some(runtime_own_log_directory.clone()),
        ..RunnerConstructionOptions::default()
    })
    .expect("the runner constructs");
    assert_eq!(
        runner.describe_this_runtime().runtime_log_directory,
        Some(runtime_own_log_directory)
    );
}

/// With a runtime own-log directory, the records no stream emitted are
/// written to `tatolabd-<started_at_millis>.jsonl` there, stamped with the
/// runtime's id and no stream, and a stream's records are not.
#[test]
#[serial]
fn the_records_no_stream_emitted_land_in_the_runtimes_own_log_and_a_streams_do_not() {
    let project_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let state_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let runtime_own_log_directory = state_directory.path().join("logs");
    let runner = Runner::new_with_construction_options(RunnerConstructionOptions {
        runtime_own_log_directory: Some(runtime_own_log_directory.clone()),
        ..RunnerConstructionOptions::default()
    })
    .expect("the runner constructs");
    let runtime_own_log = runner
        .runtime_own_log_path()
        .expect("the runtime keeps its own log")
        .to_path_buf();
    assert_eq!(
        runtime_own_log.parent(),
        Some(runtime_own_log_directory.as_path())
    );
    let file_name = runtime_own_log
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        file_name.starts_with("tatolabd-") && file_name.ends_with(".jsonl"),
        "{file_name}"
    );

    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    stream
        .log_route()
        .run_entered(|| tracing::info!("token-of-the-stream"));
    tracing::info!("token-no-stream-emitted");
    runner.unload_stream("main").expect("the stream unloads");
    let runtime_id = runner.runtime_id().to_string();
    drop(stream);
    drop(runner);

    let records = every_record_of_the_stream_log_at(&runtime_own_log);
    assert!(
        records
            .iter()
            .any(|record| record.message == "token-no-stream-emitted"
                && record.runtime_id == runtime_id
                && record.stream.is_none()),
        "the runtime's own log lacks the record no stream emitted: {records:#?}"
    );
    for record in &records {
        assert_eq!(record.stream, None, "{record:?}");
        assert_ne!(record.message, "token-of-the-stream", "{record:?}");
    }
}

/// The setup hooks run once, with the engine's GPU context and its own
/// surface store, when the first stream's start creates it — every hook
/// even after one fails, that start reporting the failure — and a hook
/// installed after them is refused.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn the_setup_hooks_run_once_each_even_after_one_fails_and_a_later_one_is_refused() {
    use std::sync::atomic::AtomicUsize;

    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let hook_runs = Arc::new(AtomicUsize::new(0));
    let hooks_saw_the_engines_surface_store = Arc::new(AtomicBool::new(true));
    for hook_fails in [true, false] {
        let hook_runs = Arc::clone(&hook_runs);
        let hooks_saw_the_engines_surface_store = Arc::clone(&hooks_saw_the_engines_surface_store);
        runner
            .install_setup_hook(move |engine_gpu_context| {
                hook_runs.fetch_add(1, Ordering::SeqCst);
                if cfg!(any(target_os = "linux", target_os = "macos"))
                    && engine_gpu_context.surface_store().is_none()
                {
                    hooks_saw_the_engines_surface_store.store(false, Ordering::SeqCst);
                }
                if hook_fails {
                    return Err(Error::Configuration("the first setup hook fails".into()));
                }
                Ok(())
            })
            .expect("a hook installed before any start is queued");
    }

    let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");
    let first_start_failure = first
        .start()
        .expect_err("the failing hook aborts the start that ran it");
    assert!(
        first_start_failure
            .to_string()
            .contains("the first setup hook fails"),
        "{first_start_failure}"
    );
    assert_eq!(
        hook_runs.load(Ordering::SeqCst),
        2,
        "a failing hook stopped the rest"
    );
    assert!(hooks_saw_the_engines_surface_store.load(Ordering::SeqCst));

    let second = an_empty_stream_loaded_into(&runner, project_directory.path(), "second");
    second.start().expect("the second stream starts");
    assert_eq!(hook_runs.load(Ordering::SeqCst), 2, "the hooks ran again");
    assert!(
        runner.install_setup_hook(|_| Ok(())).is_err(),
        "a hook installed after the hooks ran was queued where nothing runs it"
    );

    runner.shut_down().expect("the engine shuts down");
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Stopping, unloading and waiting on the streams loaded in one engine, and the machine's shutdown reaching them.

use super::two_streams_sharing_one_engine_tests::TickCountingInTheFirstStream;
use super::*;

/// A stream's shutdown thread stops a stream an embedding host already
/// stopped, so the second `stop()` must be a no-op rather than a second
/// full teardown that republishes the transition to every subscriber.
///
/// Mental-revert: removing the already-stopped early return in `stop()`
/// makes the second call publish `RuntimeStopping`/`RuntimeStopped` again
/// and fails the counts below.
#[test]
#[serial_test::serial]
fn stopping_an_already_stopped_stream_is_a_no_op() {
    use crate::core::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};

    #[derive(Default)]
    struct StopTransitionCounter {
        stopping: usize,
        stopped: usize,
    }

    struct CountingListener(Arc<Mutex<StopTransitionCounter>>);

    impl EventListener for CountingListener {
        fn on_event(&mut self, event: &Event) -> Result<()> {
            let mut counts = self.0.lock();
            match event {
                Event::OfALoadedStream {
                    event: RuntimeEvent::RuntimeStopping,
                    ..
                } => counts.stopping += 1,
                Event::OfALoadedStream {
                    event: RuntimeEvent::RuntimeStopped,
                    ..
                } => counts.stopped += 1,
                _ => {}
            }
            Ok(())
        }

        fn on_events_lost(&mut self, _lost_event_count: u64) -> Result<()> {
            Ok(())
        }
    }

    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("a runner boots without a graph");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
    let counts = Arc::new(Mutex::new(StopTransitionCounter::default()));
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(CountingListener(Arc::clone(&counts))));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(stream.loaded_stream_identity()),
            Arc::clone(&listener),
        )
        .expect("subscribe establishes the subscriber");

    stream.stop().expect("the first stop succeeds");
    stream.stop().expect("the second stop succeeds");

    // Delivery is not synchronous with the publish, so wait for the first
    // teardown's pair to land before counting — otherwise a duplicate that
    // simply arrived late would read as "published once".
    let delivery_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < delivery_deadline {
        if counts.lock().stopped >= 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(250));

    let observed = counts.lock();
    assert_eq!(
        observed.stopping, 1,
        "RuntimeStopping must be published exactly once across two stops",
    );
    assert_eq!(
        observed.stopped, 1,
        "RuntimeStopped must be published exactly once across two stops",
    );
}

/// One stream's shutdown ends that stream and takes it out of the table,
/// and leaves the other loaded, unrequested and as it was.
#[test]
#[serial]
fn one_streams_shutdown_ends_it_and_leaves_the_other_loaded() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let shut_down = an_empty_stream_loaded_into(&runner, project_directory.path(), "shut-down");
    let left_running =
        an_empty_stream_loaded_into(&runner, project_directory.path(), "left-running");
    let (recorded_by_the_other, _listener) = record_every_event_on_the_topic_of(&left_running);

    shut_down.ask_for_this_streams_shutdown("the test shuts one stream down");
    assert!(
        shut_down.wait_for_this_streams_end_within(A_STREAM_ENDS_WITHIN),
        "the stream whose shutdown was requested never ended"
    );

    assert_eq!(
        shut_down.how_this_stream_ended(),
        Some(HowALoadedStreamEnded::Stopped)
    );
    assert_eq!(shut_down.status(), RuntimeStatus::Stopped);
    assert_eq!(runner.ids_of_the_loaded_streams(), ["left-running"]);
    assert!(!left_running.has_ended());
    assert_eq!(left_running.status(), RuntimeStatus::Initial);
    assert_eq!(
        left_running.this_streams_shutdown_escalation().escalation(),
        RuntimeShutdownEscalation::NotRequested
    );
    assert!(
        recorded_before_a_sentinel_on_the_topic_of(&left_running, &recorded_by_the_other)
            .is_empty(),
        "the other stream heard the shut-down stream's events"
    );

    shut_down.ask_for_this_streams_shutdown("a second request");
    assert_eq!(
        shut_down.this_streams_shutdown_escalation().escalation(),
        RuntimeShutdownEscalation::Graceful,
        "a second request escalates nothing further"
    );
}

/// A graph change in one stream is logged on its own compiler and heard
/// by its own listener; the other stream's transaction, graph and topic
/// see nothing.
#[test]
#[serial]
fn one_streams_graph_change_reaches_only_its_own_compiler_and_topic() {
    use crate::core::compiler::PendingOperation;
    use crate::core::nodes::NodeSpec;
    use crate::core::pubsub::{Event, RuntimeEvent};
    use crate::core::test_support::{MockOutputOnlyNode, ensure_test_mocks_registered};

    ensure_test_mocks_registered();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let changed = an_empty_stream_loaded_into(&runner, project_directory.path(), "changed");
    let untouched = an_empty_stream_loaded_into(&runner, project_directory.path(), "untouched");
    let (recorded_by_the_changed, _changed_listener) = record_every_event_on_the_topic_of(&changed);
    let (recorded_by_the_untouched, _untouched_listener) =
        record_every_event_on_the_topic_of(&untouched);

    let node_run_id = changed
        .add_node(NodeSpec::new(
            MockOutputOnlyNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the mock is added");

    assert!(
        changed
            .compiler
            .logged_pending_operations()
            .iter()
            .any(|op| matches!(op, PendingOperation::AddNode(id) if *id == node_run_id)),
        "the change is logged on its own stream's compiler"
    );
    assert!(
        untouched.compiler.logged_pending_operations().is_empty(),
        "another stream's change reached this stream's compiler"
    );
    assert_eq!(untouched.to_json().unwrap()["nodes"], serde_json::json!([]));
    assert!(
        recorded_before_a_sentinel_on_the_topic_of(&changed, &recorded_by_the_changed)
            .iter()
            .any(|event| matches!(
                event,
                Event::OfALoadedStream {
                    event: RuntimeEvent::GraphDidChange,
                    stream,
                } if stream.stream_id == "changed"
            )),
        "the changed stream's listener never heard its graph change"
    );
    assert!(
        recorded_before_a_sentinel_on_the_topic_of(&untouched, &recorded_by_the_untouched)
            .is_empty(),
        "the untouched stream's listener heard another stream's graph change"
    );
}

/// A stream whose teardown outlives its watchdog is unloaded, its helper
/// process groups killed and no other stream's, and every other stream is
/// left alive. The budget is injected so the test never waits fifteen
/// seconds.
#[test]
#[serial]
fn the_watchdog_on_one_streams_teardown_unloads_it_and_leaves_the_other_alive() {
    use crate::core::runtime::{
        deregister_a_helper_process_group, register_a_helper_process_group,
    };
    use crate::core::test_support::a_process_parked_in_a_process_group_of_its_own;

    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let hung = runner
        .load_an_empty_stream_whose_teardown_watchdog_fires_after(
            options_loading_a_stream_of_the_test_project_at(project_directory.path(), "hung"),
            Duration::from_millis(300),
        )
        .expect("the stream loads");
    let alive = an_empty_stream_loaded_into(&runner, project_directory.path(), "alive");
    let mut hung_streams_helper = a_process_parked_in_a_process_group_of_its_own();
    let mut alive_streams_helper = a_process_parked_in_a_process_group_of_its_own();
    assert!(register_a_helper_process_group(
        hung_streams_helper.id() as i32,
        hung.stream_tag()
    ));
    assert!(register_a_helper_process_group(
        alive_streams_helper.id() as i32,
        alive.stream_tag()
    ));

    // The stop takes this lock after it marks the stream stopping, so
    // holding it hangs the teardown where no budget of its own reaches.
    let the_hung_teardown_waits_on = hung.runtime_context.lock();
    hung.ask_for_this_streams_shutdown("the test hangs this stream's teardown");
    let ended = hung.wait_for_this_streams_end_within(A_STREAM_ENDS_WITHIN);
    let how_it_ended = hung.how_this_stream_ended();
    let names_once_it_ended = runner.ids_of_the_loaded_streams();
    let hung_helper_exited = (0..500).any(|_| {
        if hung_streams_helper.try_wait().ok().flatten().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
        false
    });
    let alive_helper_still_running = alive_streams_helper.try_wait().ok().flatten().is_none();
    drop(the_hung_teardown_waits_on);
    deregister_a_helper_process_group(hung_streams_helper.id() as i32);
    deregister_a_helper_process_group(alive_streams_helper.id() as i32);
    let _ = alive_streams_helper.kill();
    let _ = alive_streams_helper.wait();
    let _ = hung_streams_helper.wait();

    assert!(ended, "the watchdog never ended the hung stream");
    assert!(
        matches!(
            how_it_ended,
            Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog { .. })
        ),
        "the hung stream ended as {how_it_ended:?}"
    );
    assert_eq!(names_once_it_ended, ["alive"]);
    assert!(
        hung_helper_exited,
        "the watchdog left the hung stream's helper process group running"
    );
    assert!(
        alive_helper_still_running,
        "the watchdog killed another stream's helper process group"
    );
    assert!(!alive.has_ended());
    assert_eq!(alive.status(), RuntimeStatus::Initial);
    assert!(!alive.this_streams_shutdown_escalation().is_requested());

    let released_by = std::time::Instant::now() + A_STREAM_ENDS_WITHIN;
    while hung.status() != RuntimeStatus::Stopped {
        assert!(
            std::time::Instant::now() < released_by,
            "the released teardown never finished"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        matches!(
            hung.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog { .. })
        ),
        "a teardown that finished after its watchdog fired rewrote how the stream ended"
    );
}

/// A call naming a stream that is not loaded is refused naming the loaded
/// streams; a call's name is cast before it is looked up.
#[test]
#[serial]
fn a_call_naming_a_stream_not_loaded_is_refused_naming_the_loaded_streams() {
    use crate::core::runtime::OperationsOnTheStreamsLoadedInThisRuntime;

    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let camera = an_empty_stream_loaded_into(&runner, project_directory.path(), "camera");
    an_empty_stream_loaded_into(&runner, project_directory.path(), "microphone");

    for stream_id_not_loaded in ["display", "", "..", "Camera"] {
        let refusal = runner
            .runtime_operations_of_the_stream_a_call_names(stream_id_not_loaded)
            .err()
            .expect("an id no loaded stream holds is refused")
            .to_string();
        assert!(
            refusal.contains(&format!("no stream with the id `{stream_id_not_loaded}`"))
                && refusal.contains("camera")
                && refusal.contains("microphone"),
            "the refusal must name the id and the loaded streams: {refusal}"
        );
    }
    assert!(
        runner
            .node_catalog_of_the_stream_a_call_names("display")
            .is_err()
    );
    assert!(
        runner
            .node_types_described_in_the_interpreter_of_the_stream_a_call_names("display")
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &runner
            .loaded_stream_of_the_id("camera")
            .expect("a call names a stream by its exact id"),
        &camera
    ));
}

/// The machine's shutdown request, seen by the engine that owns the
/// machine's signals, walks every loaded stream to its end.
#[test]
#[serial]
fn the_machines_shutdown_walks_every_loaded_stream_to_its_end() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");
    let second = an_empty_stream_loaded_into(&runner, project_directory.path(), "second");

    let (every_stream_ended, every_stream_has_ended) = std::sync::mpsc::channel();
    let waiting_runner = Arc::clone(&runner);
    std::thread::spawn(move || {
        let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
            waiting_runner
                .request_the_shutdown_of_every_loaded_stream("the test shuts the machine down")?;
            waiting_runner
                .wait_until_every_stream_has_ended()
                .how_the_first_failed_stream_ended
        });
        let _ = every_stream_ended.send(outcome);
    });
    every_stream_has_ended
        .recv_timeout(A_STREAM_ENDS_WITHIN)
        .expect("the machine's shutdown never ended every stream")
        .expect("every stream ended cleanly");

    for stream in [&first, &second] {
        assert_eq!(
            stream.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::Stopped)
        );
    }
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

/// A stream loaded while the wait for every stream runs is waited for and
/// returned with the others, so a teardown scanning what the wait returns
/// cannot miss it.
///
/// Only the wait's own walk to the machine's shutdown ends that stream, so
/// the wait has polled while it was loaded.
#[test]
#[serial]
fn a_stream_loaded_while_every_stream_is_awaited_is_among_the_streams_the_wait_returns() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");

    let (the_wait_began, the_wait_has_begun) = std::sync::mpsc::channel();
    let (every_stream_ended, every_stream_has_ended) = std::sync::mpsc::channel();
    let waiting_runner = Arc::clone(&runner);
    std::thread::spawn(move || {
        let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
            let _ = the_wait_began.send(());
            Ok(waiting_runner.wait_until_every_stream_has_ended())
        });
        let _ = every_stream_ended.send(outcome);
    });
    the_wait_has_begun
        .recv_timeout(A_STREAM_ENDS_WITHIN)
        .expect("the waiting thread never began its wait");
    let loaded_during_the_wait =
        an_empty_stream_loaded_into(&runner, project_directory.path(), "loaded-during-the-wait");
    runner.unload_stream("first").expect("first unloads");
    runner
        .request_the_shutdown_of_every_loaded_stream("the test shuts the machine down")
        .expect("the machine's shutdown is requested");

    let every_stream_ended_during_the_wait = every_stream_has_ended
        .recv_timeout(A_STREAM_ENDS_WITHIN)
        .expect("the wait never returned once every stream had ended")
        .expect("the waiting thread owned the machine's shutdown signals");
    every_stream_ended_during_the_wait
        .how_the_first_failed_stream_ended
        .expect("every stream ended cleanly");
    for stream in [&first, &loaded_during_the_wait] {
        assert!(
            every_stream_ended_during_the_wait
                .streams_seen_during_the_wait
                .iter()
                .any(|seen| Arc::ptr_eq(seen, stream)),
            "the wait returned no `{}`",
            stream.stream_id()
        );
    }
    assert_eq!(
        loaded_during_the_wait.how_this_stream_ended(),
        Some(HowALoadedStreamEnded::Stopped)
    );
}

/// A stream out of the table whose end is not yet recorded — where its
/// shutdown thread leaves it between the two — is waited for and returned
/// with the others.
///
/// The wait holds every stream it has seen, so the stream's reference
/// count rising says the wait has looked; the stream is only asked to end
/// after that, so a wait that returns first has returned past it.
#[test]
#[serial]
fn the_wait_for_every_stream_returns_a_stream_out_of_the_table_once_its_end_is_recorded() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let leaving = an_empty_stream_loaded_into(&runner, project_directory.path(), "leaving");
    runner
        .engine_resources_shared_by_every_stream
        .remove_from_the_stream_table(&leaving);
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert_eq!(leaving.how_this_stream_ended(), None);

    let (the_wait_returned, the_wait_has_returned) = std::sync::mpsc::channel();
    let waiting_runner = Arc::clone(&runner);
    let leaving_as_the_waiter_reads_it = Arc::clone(&leaving);
    let references_before_the_wait_looked = Arc::strong_count(&leaving);
    std::thread::spawn(move || {
        let every_stream_ended_during_the_wait = waiting_runner.wait_until_every_stream_has_ended();
        let _ = the_wait_returned.send((
            leaving_as_the_waiter_reads_it.how_this_stream_ended(),
            every_stream_ended_during_the_wait
                .streams_seen_during_the_wait
                .iter()
                .any(|seen| Arc::ptr_eq(seen, &leaving_as_the_waiter_reads_it)),
        ));
    });
    let deadline = Instant::now() + A_STREAM_ENDS_WITHIN;
    while Arc::strong_count(&leaving) == references_before_the_wait_looked {
        if let Ok((how_it_ended_as_the_wait_returned, _)) = the_wait_has_returned.try_recv() {
            panic!(
                "the wait returned while a stream out of the table had no end recorded \
                 ({how_it_ended_as_the_wait_returned:?})"
            );
        }
        assert!(
            Instant::now() < deadline,
            "the wait never looked at the stream out of the table"
        );
        std::thread::yield_now();
    }
    leaving.ask_for_this_streams_shutdown("the test ends the stream out of the table");

    let (how_it_ended_as_the_wait_returned, the_wait_saw_it) = the_wait_has_returned
        .recv_timeout(A_STREAM_ENDS_WITHIN)
        .expect("the wait never returned once the stream had ended");
    assert_eq!(
        how_it_ended_as_the_wait_returned,
        Some(HowALoadedStreamEnded::Stopped)
    );
    assert!(the_wait_saw_it, "the wait returned no `leaving`");
}

/// The engine's shutdown ends a stream out of the table whose end is not
/// yet recorded, and returns only once it is.
#[test]
#[serial]
fn the_engines_shutdown_returns_only_once_a_stream_out_of_the_table_has_its_end_recorded() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let leaving = an_empty_stream_loaded_into(&runner, project_directory.path(), "leaving");
    let loaded = an_empty_stream_loaded_into(&runner, project_directory.path(), "loaded");
    runner
        .engine_resources_shared_by_every_stream
        .remove_from_the_stream_table(&leaving);
    assert_eq!(leaving.how_this_stream_ended(), None);

    runner.shut_down().expect("the engine shuts down cleanly");

    for stream in [&leaving, &loaded] {
        assert_eq!(
            stream.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::Stopped),
            "`{}`",
            stream.stream_id()
        );
    }
}

/// The wait `tatolabd` blocks on returns only once the machine's shutdown
/// is requested, and walks every loaded stream to it on the way.
#[test]
#[serial]
fn the_wait_for_a_machine_shutdown_returns_once_one_is_requested_and_ends_every_stream() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");

    let (wait_returned, the_wait_has_returned) = std::sync::mpsc::channel();
    let waiting_runner = Arc::clone(&runner);
    std::thread::spawn(move || {
        let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
            waiting_runner.wait_until_a_machine_shutdown_is_requested();
            Ok(())
        });
        let _ = wait_returned.send(outcome);
    });
    assert!(
        the_wait_has_returned
            .recv_timeout(Duration::from_millis(300))
            .is_err(),
        "the wait returned before any machine shutdown was requested"
    );
    assert!(!stream.has_ended());

    crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
        "the test shuts the machine down",
    )
    .unwrap();

    the_wait_has_returned
        .recv_timeout(A_STREAM_ENDS_WITHIN)
        .expect("the wait never saw the machine's shutdown request")
        .expect("the wait owned the machine's shutdown signals");
    assert!(stream.wait_for_this_streams_end_within(A_STREAM_ENDS_WITHIN));
}

/// A machine shutdown requested after a wait's walk of the streams but before
/// its check is still walked to every stream: the wait sees the request and
/// returns, and the owner clears the escalation once its run ends, so the walk
/// must cover every level the check saw.
///
/// The predicate raises the request itself on its first call, landing it in
/// exactly that window every time.
#[test]
#[serial]
fn a_machine_shutdown_requested_between_the_walk_and_the_check_still_ends_every_stream() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");

    runner
        .run_owning_the_machine_shutdown_signals(|| {
            runner.block_until(&|| {
                crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
                    "the test requests the shutdown inside the wait's check",
                )
                .unwrap();
                crate::core::runtime::is_the_machines_shutdown_requested()
            });
            Ok(())
        })
        .expect("the wait owned the machine's shutdown signals");
    assert!(stream.wait_for_this_streams_end_within(A_STREAM_ENDS_WITHIN));
}

/// The wait for the last other reference returns once another holder
/// hands the engine back.
#[test]
#[serial]
fn the_wait_for_the_last_other_reference_returns_once_it_is_dropped() {
    let runner = Runner::new().expect("Runner::new");
    let held_by_another_holder = Arc::clone(&runner);
    let other_holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(held_by_another_holder);
    });

    assert!(runner.wait_until_this_reference_alone_holds_the_engine(A_STREAM_ENDS_WITHIN));
    other_holder
        .join()
        .expect("the other holder dropped its reference");
}

/// The wait for the last other reference gives up once its budget passes.
#[test]
#[serial]
fn the_wait_for_the_last_other_reference_stops_at_its_budget() {
    let runner = Runner::new().expect("Runner::new");
    let _held_past_the_budget = Arc::clone(&runner);

    let wait_started = Instant::now();
    assert!(!runner.wait_until_this_reference_alone_holds_the_engine(Duration::from_millis(30)));
    assert!(wait_started.elapsed() < A_STREAM_ENDS_WITHIN);
}

/// The wait for the last other reference, inside a run that owns the
/// machine's shutdown signals, walks every loaded stream to the machine's
/// shutdown level while it waits.
#[test]
#[serial]
fn the_wait_for_the_last_other_reference_walks_every_loaded_stream_to_the_machines_shutdown() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");

    let waiting_runner = Arc::clone(&runner);
    let waiting_stream = Arc::clone(&stream);
    let (wait_returned, the_wait_has_returned) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
            waiting_runner
                .request_the_shutdown_of_every_loaded_stream("the test shuts the machine down")?;
            let alone = waiting_runner
                .wait_until_this_reference_alone_holds_the_engine(Duration::from_secs(2));
            Ok((alone, waiting_stream.has_ended()))
        });
        let _ = wait_returned.send(outcome);
    });

    let (alone, stream_ended_during_the_wait) = the_wait_has_returned
        .recv_timeout(A_STREAM_ENDS_WITHIN)
        .expect("the wait never returned")
        .expect("the wait owned the machine's shutdown signals");
    assert!(!alone, "the test's own reference still holds the engine");
    assert!(
        stream_ended_during_the_wait,
        "the wait never walked the loaded stream to the machine's shutdown"
    );
}

/// Poll `escalation_of` until it reports `wanted` or `within` passes,
/// returning the last level it reported.
fn the_escalation_once_it_reaches(
    wanted: RuntimeShutdownEscalation,
    within: Duration,
    escalation_of: impl Fn() -> RuntimeShutdownEscalation,
) -> RuntimeShutdownEscalation {
    let deadline = std::time::Instant::now() + within;
    loop {
        let reached = escalation_of();
        if reached >= wanted || std::time::Instant::now() >= deadline {
            return reached;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A machine shutdown forced while an unload waits on a stream forces
/// that stream, though no thread owns the machine's shutdown signals to
/// walk it there.
///
/// Fail-without-fix: the unload's wait only ever asks for the graceful
/// step, so the stream stays `Graceful` and a native callback it holds
/// keeps its whole graceful join budget.
#[test]
#[serial]
fn a_forced_machine_shutdown_forces_a_stream_an_unload_is_waiting_on() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let unloading = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloading");

    // The stop takes this lock after it marks the stream stopping, so
    // holding it keeps the unload waiting until the test lets go.
    let the_held_teardown_waits_on = unloading.runtime_context.lock();
    let (unload_returned, the_unload_has_returned) = std::sync::mpsc::channel();
    let unloading_runner = Arc::clone(&runner);
    std::thread::spawn(move || {
        let _ = unload_returned.send(unloading_runner.unload_stream("unloading"));
    });
    let asked_for = the_escalation_once_it_reaches(
        RuntimeShutdownEscalation::Graceful,
        A_STREAM_ENDS_WITHIN,
        || unloading.this_streams_shutdown_escalation().escalation(),
    );

    crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
    crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
    let forced_to = the_escalation_once_it_reaches(
        RuntimeShutdownEscalation::Forced,
        Duration::from_secs(2),
        || unloading.this_streams_shutdown_escalation().escalation(),
    );
    let returned_while_held = the_unload_has_returned.try_recv().is_ok();
    drop(the_held_teardown_waits_on);
    let unload_outcome = the_unload_has_returned.recv_timeout(A_STREAM_ENDS_WITHIN);

    assert_eq!(asked_for, RuntimeShutdownEscalation::Graceful);
    assert_eq!(
        forced_to,
        RuntimeShutdownEscalation::Forced,
        "the unload's wait left the stream at its graceful step after the machine's \
         shutdown was forced"
    );
    assert!(
        !returned_while_held,
        "the unload returned while the stream's teardown was still held"
    );
    unload_outcome
        .expect("the unload never returned once its teardown was let go")
        .expect("the stream unloads cleanly");
    assert_eq!(
        unloading.how_this_stream_ended(),
        Some(HowALoadedStreamEnded::Stopped)
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

/// Set once [`CallbackHeldUntilTheTestLetsGo`] is inside its callback.
static THE_HELD_CALLBACK_HAS_BEGUN: AtomicBool = AtomicBool::new(false);

/// Set by the test to let [`CallbackHeldUntilTheTestLetsGo`] return.
static THE_HELD_CALLBACK_MAY_RETURN: AtomicBool = AtomicBool::new(false);

/// A source whose first callback does not return until the test lets it,
/// or for a minute.
#[crate::node(execution = continuous(interval_ms = 5))]
pub(crate) struct CallbackHeldUntilTheTestLetsGo;

impl crate::core::ContinuousNode for CallbackHeldUntilTheTestLetsGo::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        THE_HELD_CALLBACK_HAS_BEGUN.store(true, Ordering::SeqCst);
        let held_until = std::time::Instant::now() + Duration::from_secs(60);
        while !THE_HELD_CALLBACK_MAY_RETURN.load(Ordering::SeqCst)
            && std::time::Instant::now() < held_until
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_forced_machine_shutdown_abandons_a_held_native_callback_an_unload_is_waiting_on() {
    use crate::core::nodes::{NODE_REGISTRY, NodeSpec};

    const NATIVE_NODE_THREAD_GRACEFUL_JOIN_BUDGET: Duration = Duration::from_secs(5);

    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    THE_HELD_CALLBACK_HAS_BEGUN.store(false, Ordering::SeqCst);
    THE_HELD_CALLBACK_MAY_RETURN.store(false, Ordering::SeqCst);
    NODE_REGISTRY.register::<CallbackHeldUntilTheTestLetsGo::Node>();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let holding = an_empty_stream_loaded_into(&runner, project_directory.path(), "holding");
    holding
        .add_node(NodeSpec::new(
            CallbackHeldUntilTheTestLetsGo::node_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the holding source is added");
    holding.start().expect("the stream starts");
    let callback_begun_by = std::time::Instant::now() + Duration::from_secs(30);
    while !THE_HELD_CALLBACK_HAS_BEGUN.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < callback_begun_by,
            "the holding source never entered its callback"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let (unload_returned, the_unload_has_returned) = std::sync::mpsc::channel();
    let unloading_runner = Arc::clone(&runner);
    let unload_began = std::time::Instant::now();
    std::thread::spawn(move || {
        let outcome = unloading_runner.unload_stream("holding");
        let _ = unload_returned.send((outcome, unload_began.elapsed()));
    });
    the_escalation_once_it_reaches(
        RuntimeShutdownEscalation::Graceful,
        A_STREAM_ENDS_WITHIN,
        || holding.this_streams_shutdown_escalation().escalation(),
    );
    crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
    crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
    let unload_outcome = the_unload_has_returned.recv_timeout(A_STREAM_ENDS_WITHIN);
    let abandoned_threads = holding.node_threads_abandoned_and_still_running().len();
    THE_HELD_CALLBACK_MAY_RETURN.store(true, Ordering::SeqCst);

    let (_ended_reporting, unloaded_in) =
        unload_outcome.expect("the unload never returned after the machine's shutdown was forced");
    assert_eq!(
        holding.this_streams_shutdown_escalation().escalation(),
        RuntimeShutdownEscalation::Forced
    );
    assert!(
        unloaded_in < NATIVE_NODE_THREAD_GRACEFUL_JOIN_BUDGET / 2,
        "the unload took {unloaded_in:?}; a forced shutdown abandons a held native callback \
         well inside the {NATIVE_NODE_THREAD_GRACEFUL_JOIN_BUDGET:?} graceful budget"
    );
    assert_eq!(
        abandoned_threads, 1,
        "the held callback's thread is abandoned, not joined"
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

/// A stream loaded while the machine is shutting every stream down is
/// refused by name.
#[test]
#[serial]
fn a_load_while_the_machine_shuts_down_is_refused_by_name() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
        "the test shuts the machine down",
    )
    .expect("the machine's shutdown is requested");

    let Err(refusal) = runner.load_an_empty_stream(
        options_loading_a_stream_of_the_test_project_at(project_directory.path(), "too-late"),
    ) else {
        panic!("a stream loaded while the machine shuts down");
    };

    let refusal = refusal.to_string();
    assert!(refusal.contains("too-late"), "{refusal}");
    assert!(refusal.contains("shutting"), "{refusal}");
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

/// A watched load the machine's shutdown reaches before or while the
/// stream is built is abandoned, never refused: its host exits as an
/// interrupt rather than as a refusal.
#[test]
#[serial]
fn a_watched_load_the_machines_shutdown_reaches_while_it_builds_is_abandoned() {
    let _machine_level_cleared =
        crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
        "the test shuts the machine down",
    )
    .expect("the machine's shutdown is requested");
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{
            "name": "source",
            "type": TickCountingInTheFirstStream::node_class_import_path().as_str()
        }]
    }))
    .expect("the test graph reads");

    let load_outcome = runner
        .load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested(
            &graph,
            options_loading_a_stream_of_the_test_project_at(project_directory.path(), "too-late"),
        )
        .expect("a load the machine's shutdown reached is abandoned, not refused");

    assert!(matches!(
        load_outcome,
        StreamLoadObservingInterrupts::AbandonedForAnInterrupt
    ));
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a helper inherits, how it is stopped, and how its death and its
//! overrunning function node calls are noticed.

use super::*;

/// A helper that forks a worker and leaves at once. The worker outlives it
/// holding whatever it inherited, which is the survivor the descriptor
/// sweep exists to leave empty-handed.
const A_HELPER_THAT_FORKS_A_WORKER_AND_LEAVES: &str = r#"
import os, time
if os.fork() == 0:
    time.sleep(30)
    os._exit(0)
"#;

/// A pipe whose write end is inheritable on purpose, standing in for any
/// descriptor this process holds that a helper must not keep.
fn an_inheritable_pipe() -> (OwnedFd, OwnedFd) {
    let (read_end, write_end) = std::io::pipe().expect("pipe");
    let (read_end, write_end) = (OwnedFd::from(read_end), OwnedFd::from(write_end));
    // SAFETY: `write_end` is a live descriptor this process owns; clearing
    // its close-on-exec flag is what makes it the inheritable descriptor the
    // sweep must catch.
    assert_eq!(
        unsafe { libc::fcntl(write_end.as_raw_fd(), libc::F_SETFD, 0) },
        0,
        "F_SETFD"
    );
    (read_end, write_end)
}

/// Whether the descriptor reports end of file inside `budget`, which it can
/// only do once every holder of the other end has let go.
fn a_descriptor_reports_end_of_file_within(raw_fd: libc::c_int, budget: Duration) -> bool {
    let mut watched = libc::pollfd {
        fd: raw_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` for one descriptor this test owns.
    let ready = unsafe { libc::poll(&mut watched, 1, budget.as_millis() as libc::c_int) };
    ready > 0 && watched.revents & (libc::POLLHUP | libc::POLLIN) != 0
}

/// Wait for `process_id` to become a zombie, so a test asserting what the
/// notice says is not racing the child's own exit.
fn a_helper_process_becomes_collectable_within(process_id: u32, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if a_child_process_has_exited_without_being_reaped(process_id) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn nothing_a_helper_starts_can_hold_the_apps_output_open_past_its_own_exit() {
    // Asserted the way it bites: the app's standard
    // output is a pipe somebody reads to its end, and a grandchild holding
    // a copy of the write end keeps that read waiting after the app is gone.
    let (read_end, write_end) = an_inheritable_pipe();

    let mut command = Command::new("python3");
    command
        .arg("-c")
        .arg(A_HELPER_THAT_FORKS_A_WORKER_AND_LEAVES)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);

    let mut child = command.spawn().expect("the stub helper to start");
    let helper_process_id = child.id() as libc::pid_t;
    // Only the fork's survivor can be holding it now.
    drop(write_end);
    let _ = child.wait();

    let the_apps_output_closed =
        a_descriptor_reports_end_of_file_within(read_end.as_raw_fd(), Duration::from_secs(2));

    // SAFETY: the group is this test's own child's; the survivor is in it.
    unsafe { libc::killpg(helper_process_id, libc::SIGKILL) };

    assert!(
        the_apps_output_closed,
        "a worker the helper forked inherited the app's output and held it open"
    );
}

/// Set only in the child process the sweep-placement test re-runs itself in.
const DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_HOST_SWEEP_CHILD_ICEORYX2_DOMAIN_ROOT";

#[test]
fn a_helper_exit_reclaims_the_iceoryx2_nodes_it_left_whatever_ended_it() {
    // A node is dead only once the process holding it is gone, so it is
    // opened in a child test process that is then killed where it stands.
    if let Some(domain_root) = std::env::var_os(DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE) {
        let _node = crate::iceoryx2::Iceoryx2Node::new(
            Path::new(&domain_root),
            "streamlib-test/host-sweep-placement",
        )
        .expect("a node opens in the engine-owned domain");
        // SAFETY: this process signalling itself, which is what leaves the
        // node registered with no process behind it.
        unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGKILL) };
        // Darwin can return from `kill` before a self-directed SIGKILL has
        // landed; parking holds the node until it does.
        loop {
            std::thread::park();
        }
    }

    // Named from this test process's own pid rather than through a
    // temp-directory crate, so the one test needing a private domain adds
    // no dependency to the engine.
    // `/tmp` rather than `std::env::temp_dir()`: on macOS that is a
    // `/var/folders/...` path long enough to overrun the budget iceoryx2's
    // socket paths leave a domain root.
    let domain = Path::new("/tmp").join(format!("streamlib-host-sweep-{}", std::process::id()));
    let domain_root = domain.join("iox2");
    streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
        &domain_root,
        streamlib_runtime_client_contract::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
    )
    .expect("a private domain root");
    let dead_node_owner = Command::new(std::env::current_exe().unwrap())
        .args([
            "core::compiler::compiler_ops::processor_interpreter_spawn_host::tests::helper_process_lifetime_tests::\
             a_helper_exit_reclaims_the_iceoryx2_nodes_it_left_whatever_ended_it",
            "--exact",
            "--test-threads=1",
        ])
        .env(
            DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
            &domain_root,
        )
        .output()
        .expect("the test binary re-runs this test in a child process");
    // The signal and not merely a non-zero exit: a child that panicked
    // before it opened its node would also exit non-zero, and would leave
    // the domain empty — against which the zero below asserts nothing.
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&dead_node_owner.status),
        Some(libc::SIGKILL),
        "the child must die where it stood, holding its node: {}",
        String::from_utf8_lossy(&dead_node_owner.stderr),
    );

    // A host whose own helper has already left, closed the ordinary way.
    let mut host = spawn_host_for_test();
    host.iceoryx2_domain_root = Some(domain_root.clone());
    host.child = Some(
        Command::new("true")
            .spawn()
            .expect("a stand-in helper that is already done"),
    );

    host.take_the_helper_process_group_down();

    let left_for_somebody_else =
        crate::iceoryx2::reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(&domain_root)
            .expect("the domain can be swept");
    std::fs::remove_dir_all(&domain).ok();

    assert_eq!(
        left_for_somebody_else, 0,
        "the helper's exit left a dead node for somebody else to reclaim — the sweep runs \
         at every exit, not only at a crash the engine detected"
    );
}

#[test]
fn every_path_onto_the_ladder_asks_the_helper_to_stop_and_tear_down_exactly_once() {
    // Any callback interrupted at shutdown is owed a `teardown()`,
    // `setup()` included — and a helper still importing then has
    // never reached the engine's `stop()` hook, which is where the commands
    // normally go out. So the ladder asks for itself, idempotently.
    //
    // Fail-without-fix: leave the send in `stop()` alone and the
    // registration path waits out both budgets for replies to commands
    // that were never sent, then warns that a teardown it never asked for
    // did not finish.
    let mut host = spawn_host_for_test();
    assert!(!host.shutdown_was_already_asked_of_this_helper);

    // With no bridge the ask short-circuits before its sends, so the flag
    // is the whole of what is observable here: that every route onto the
    // ladder goes through the ask, and that a second route does not repeat
    // it. That the commands then reach the wire is the rig scenario's, in
    // `test_helper_crashes_restarts_and_teardown.py`.
    host.ask_the_helper_to_stop_and_tear_down();
    assert!(host.shutdown_was_already_asked_of_this_helper);

    host.shutdown_was_already_asked_of_this_helper = false;
    host.stop_the_helper_process_on_the_shutdown_ladder();
    assert!(
        host.shutdown_was_already_asked_of_this_helper,
        "the ladder walked without ever asking the helper to stop or tear down"
    );
}

#[test]
fn the_two_shutdown_commands_are_the_ones_the_helper_answers() {
    // One pairing, in one place: the command the parent writes and the
    // reply tag the rung waits on come from the same value, so a rename of
    // either cannot leave the ladder waiting for a tag nobody sends.
    let [stop, teardown] = HelperProcessShutdownCommand::BOTH_IN_THE_ORDER_THE_LADDER_SENDS_THEM;

    assert_eq!((stop.command_tag(), stop.reply_tag()), ("stop", "stopped"));
    assert_eq!(
        (teardown.command_tag(), teardown.reply_tag()),
        ("teardown", "done")
    );
}

#[test]
fn a_dead_helper_is_noticed_by_its_process_while_a_survivor_still_holds_its_socket() {
    // A crash is detected by the process itself rather than by its socket,
    // asserted as the contrast that matters: a worker the helper forked
    // inherits the escalate socket through the fork, so the bridge's EOF
    // never arrives and a dead helper would read as a live one — still
    // wired, still able to escalate.
    let mut command = Command::new("python3");
    command
        .arg("-c")
        .arg(A_HELPER_THAT_FORKS_A_WORKER_AND_LEAVES)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    let mut escalate_transport =
        EscalateTransport::attach(&mut command).expect("an escalate socketpair");

    let child = command.spawn().expect("the stub helper to start");
    let helper_process_id = child.id();
    escalate_transport.release_child_end();
    let parent_end_of_the_escalate_socket = escalate_transport.into_parent_ends().bridge;

    assert!(
        a_helper_process_becomes_collectable_within(helper_process_id, Duration::from_secs(5)),
        "the stub helper never exited"
    );
    assert!(
        !a_descriptor_reports_end_of_file_within(
            parent_end_of_the_escalate_socket.as_raw_fd(),
            Duration::from_millis(500),
        ),
        "the socket reached EOF, so this asserts nothing the old signal did not already catch"
    );

    // SAFETY: the group is this test's own child's; the survivor is in it.
    unsafe { libc::killpg(helper_process_id as libc::pid_t, libc::SIGKILL) };
    let mut child = child;
    let _ = child.wait();
}

#[test]
fn the_escalate_socket_survives_the_sweep_that_takes_every_other_descriptor() {
    // The sweep marks everything past stdio close-on-exec and
    // `EscalateTransport::attach` clears the flag again on the one
    // descriptor a helper is owed, so the order of the two `pre_exec`
    // registrations is the whole of whether a helper has a channel at all.
    let mut command = Command::new("python3");
    command
        .arg("-c")
        .arg(
            r#"
import os, sys
os.fstat(int(os.environ["STREAMLIB_ESCALATE_FD"]))
os.fstat(int(os.environ["STREAMLIB_NATIVE_ESCALATE_FD"]))
sys.exit(0)
"#,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    let mut escalate_transport =
        EscalateTransport::attach(&mut command).expect("an escalate socketpair");

    let mut child = command.spawn().expect("the stub helper to start");
    escalate_transport.release_child_end();
    let exit_status = child.wait().expect("the stub helper to exit");

    assert!(
        exit_status.success(),
        "the helper could not stat the escalate socket it was handed: {exit_status}"
    );
}

#[test]
fn a_helper_that_died_while_setting_up_is_refused_naming_what_it_wrote_to_standard_error() {
    let mut child = Command::new("sh")
        .args([
            "-c",
            "printf 'some earlier line\\n[streamlib] this helper imported engine build A, \
             its parent is build B\\n' >&2; exit 1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sh starts");
    let mut host = spawn_host_for_test();
    host.child_standard_error_tail = Some(spawn_standard_error_reader_keeping_its_tail(
        child.stderr.take().expect("stderr is piped"),
        "Pblur",
    ));
    child.wait().expect("sh exits");

    let refusal = host
        .refuse_the_helper_process_that_died_while_setting_up()
        .to_string();

    assert!(host.child_is_gone);
    assert!(
        refusal.contains("[BlurProcessor] its helper process died before it finished setting up"),
        "{refusal}"
    );
    assert!(
        refusal.ends_with(
            "some earlier line\n[streamlib] this helper imported engine build A, its parent \
             is build B"
        ),
        "{refusal}"
    );
}

/// A helper that dies on its own mid-run is noticed by its process, its
/// group is taken down, and why it died — its exit and the end of its
/// standard error — is kept for the restart that follows.
///
/// Fail-without-fix: keep the early return on a host already given up on
/// and a helper that died during a lifecycle exchange is never reaped or
/// restarted; drop the reason and the node restarts saying nothing.
#[test]
fn a_helper_that_died_on_its_own_is_noticed_with_its_exit_and_its_standard_error() {
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "printf 'Traceback (most recent call last):\\nRuntimeError: boom\\n' >&2; exit 3",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    let mut child = command.spawn().expect("sh starts");
    let mut host = spawn_host_for_test();
    host.child_standard_error_tail = Some(spawn_standard_error_reader_keeping_its_tail(
        child.stderr.take().expect("stderr is piped"),
        "Pblur",
    ));
    let helper_process_id = child.id();
    host.child = Some(child);
    host.give_up_on_the_helper_process();
    assert!(
        a_helper_process_becomes_collectable_within(helper_process_id, Duration::from_secs(5)),
        "the stand-in helper never exited"
    );

    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();

    assert!(host.child.is_none(), "the dead helper was not reaped");
    assert!(host.has_failed_unrecoverably());
    let why = host
        .why_the_out_of_process_helper_died_on_its_own()
        .expect("the death is kept for the restart");
    assert!(why.contains("exit status: 3"), "{why}");
    assert!(why.contains(&format!("pid={helper_process_id}")), "{why}");
    assert!(why.ends_with("RuntimeError: boom"), "{why}");
}

/// The failure a node reports over its bridge reaches the lifecycle loop
/// through its spawn host, and a later bridge for the same node keeps it.
#[test]
fn a_failure_the_node_reports_of_itself_is_read_through_its_spawn_host() {
    let host = spawn_host_for_test();
    assert_eq!(host.failure_the_node_reported_of_itself(), None);

    let what_a_later_bridge_reaches = host.what_the_node_reaches.clone();
    what_a_later_bridge_reaches
        .failure_the_node_reported_of_itself
        .record("the page stopped rendering".to_string());

    assert_eq!(
        host.failure_the_node_reported_of_itself().as_deref(),
        Some("the page stopped rendering")
    );
}

/// A helper the engine stopped itself, or one that is still alive, never
/// reads as having died on its own, so it is never restarted.
#[test]
fn a_helper_still_running_has_not_died_on_its_own() {
    let mut host = spawn_host_for_test();
    let child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sleep starts");
    host.child = Some(child);

    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();

    assert!(host.child.is_some());
    assert_eq!(host.why_the_out_of_process_helper_died_on_its_own(), None);
    if let Some(mut child) = host.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// A host whose helper writes on a board its envelope holds, with one
/// island of the function node `lfo-1` recorded for its setup command.
fn a_spawn_host_whose_helper_calls_function_node_lfo_1() -> (
    ProcessorInterpreterSpawnHostProcessor,
    Arc<crate::iceoryx2::HelperProcessLossCountBoardWriter>,
    crate::iceoryx2::Iceoryx2Node,
) {
    a_spawn_host_driven_in_whose_helper_calls_function_node_lfo_1(ProcessExecution::Reactive)
}

fn a_spawn_host_driven_in_whose_helper_calls_function_node_lfo_1(
    child_process_execution: ProcessExecution,
) -> (
    ProcessorInterpreterSpawnHostProcessor,
    Arc<crate::iceoryx2::HelperProcessLossCountBoardWriter>,
    crate::iceoryx2::Iceoryx2Node,
) {
    let host = spawn_host_for_test_driven_in(child_process_execution);
    let (board, board_writer, helper_node) =
        crate::iceoryx2::a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    host.link_wiring
        .helper_placed_processor_loss_counts()
        .hold_the_board_of_this_spawn(board);
    host.link_wiring
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(serde_json::json!({
            "input_port": "brightness",
            "link_id": "L-island",
            "output": {"node": "Plfo1", "port": "result"},
            "nodes": [{"node": "Plfo1", "name": "lfo-1", "import_path": "ops:lfo", "links_into": []}],
        }))
        .expect("an island rides the setup command before setup begins");
    (host, board_writer, helper_node)
}

fn a_stand_in_helper_in_its_own_process_group(shell_command: &str) -> Child {
    let mut command = Command::new("sh");
    command
        .args(["-c", shell_command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    command.spawn().expect("sh starts")
}

/// A helper whose function-node call runs past the watchdog's budget is
/// ended as though it crashed, and the reason names the node that stalled.
///
/// Fail-without-fix: read only whether the process exited and a helper
/// spinning inside a compiled body holds its reader for ever.
#[test]
fn a_function_node_call_past_the_watchdog_budget_ends_its_helper_naming_the_node() {
    let (mut host, board_writer, _helper_node) =
        a_spawn_host_whose_helper_calls_function_node_lfo_1();
    host.child = Some(a_stand_in_helper_in_its_own_process_group("sleep 30"));
    let started_long_enough_ago = (MediaClock::now().as_nanos() as u64)
        .saturating_sub(Duration::from_secs(5).as_nanos() as u64);
    board_writer.note_a_function_node_call_started(0, started_long_enough_ago);

    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();

    assert!(host.child.is_none(), "the stalled helper was not ended");
    let why = host
        .why_the_out_of_process_helper_died_on_its_own()
        .expect("the stall is kept for the restart, as a crash would be");
    assert!(why.starts_with("function node `lfo-1` stalled"), "{why}");
    assert!(why.contains("past the 250 ms"), "{why}");
}

/// A helper that names a node for overrunning its frame interval three
/// times running is ended, and the node is kept for promotion, as a crash
/// would keep it.
#[test]
fn a_function_node_named_for_overrunning_its_frame_interval_ends_its_helper_for_promotion() {
    let (mut host, board_writer, _helper_node) =
        a_spawn_host_whose_helper_calls_function_node_lfo_1();
    host.child = Some(a_stand_in_helper_in_its_own_process_group("sleep 30"));
    board_writer.time_each_function_node_call_against_the_frame_interval(Duration::from_millis(10));
    for _ in 0..FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED {
        let call = board_writer.note_a_function_node_call_started(0, 1_000_000_000);
        board_writer.note_the_function_node_call_returned_at(call, 1_020_000_000);
    }

    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();

    assert!(
        host.child.is_none(),
        "the helper of the node that overran was not ended"
    );
    let why = host
        .why_the_out_of_process_helper_died_on_its_own()
        .expect("the overrun is kept for the restart, as a crash would be");
    assert!(
        why.starts_with("function node `lfo-1` overran its island's"),
        "{why}"
    );
    assert!(why.contains("the longest call 20 ms"), "{why}");
    assert_eq!(
        host.function_node_call_that_lost_the_helper
            .as_ref()
            .map(|call| call.function_node_name.as_str()),
        Some("lfo-1"),
        "the node that overran is the one promoted"
    );
}

/// A continuous reader's budget is three of its frame intervals once they
/// pass the floor: a call is left running past the floor and inside them,
/// and ended past them.
#[test]
fn a_continuous_readers_watchdog_budget_is_three_of_its_frame_intervals() {
    let (mut host, board_writer, _helper_node) =
        a_spawn_host_driven_in_whose_helper_calls_function_node_lfo_1(
            ProcessExecution::Continuous { interval_ms: 200 },
        );
    host.child = Some(a_stand_in_helper_in_its_own_process_group("sleep 30"));
    let started_ago =
        |ago: Duration| (MediaClock::now().as_nanos() as u64).saturating_sub(ago.as_nanos() as u64);

    let inside_the_budget =
        board_writer.note_a_function_node_call_started(0, started_ago(Duration::from_millis(400)));
    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();
    assert!(
        host.child.is_some(),
        "a call 400 ms in is inside a 600 ms budget"
    );
    assert_eq!(host.why_the_out_of_process_helper_died_on_its_own(), None);
    board_writer.note_the_function_node_call_returned(inside_the_budget);

    board_writer.note_a_function_node_call_started(0, started_ago(Duration::from_millis(900)));
    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();
    assert!(
        host.child.is_none(),
        "a call 900 ms in is past a 600 ms budget"
    );
    let why = host
        .why_the_out_of_process_helper_died_on_its_own()
        .expect("the stall is kept for the restart");
    assert!(why.contains("past the 600 ms"), "{why}");
}

/// A call still inside its budget leaves the helper running.
#[test]
fn a_function_node_call_inside_the_watchdog_budget_leaves_its_helper_running() {
    let (mut host, board_writer, _helper_node) =
        a_spawn_host_whose_helper_calls_function_node_lfo_1();
    host.child = Some(a_stand_in_helper_in_its_own_process_group("sleep 30"));
    board_writer.note_a_function_node_call_started(0, MediaClock::now().as_nanos() as u64);

    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();

    assert!(host.child.is_some());
    assert_eq!(host.why_the_out_of_process_helper_died_on_its_own(), None);
    host.take_the_helper_process_group_down();
}

/// A helper that died inside a function-node call is reported as that
/// node's crash.
#[test]
fn a_helper_that_died_inside_a_function_node_call_names_the_node() {
    let (mut host, board_writer, _helper_node) =
        a_spawn_host_whose_helper_calls_function_node_lfo_1();
    let child = a_stand_in_helper_in_its_own_process_group("exit 134");
    let helper_process_id = child.id();
    host.child = Some(child);
    board_writer.note_a_function_node_call_started(0, MediaClock::now().as_nanos() as u64);
    assert!(
        a_helper_process_becomes_collectable_within(helper_process_id, Duration::from_secs(5)),
        "the stand-in helper never exited"
    );

    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();

    let why = host
        .why_the_out_of_process_helper_died_on_its_own()
        .expect("the death is kept for the restart");
    assert!(
        why.starts_with("function node `lfo-1` crashed its helper process during its call"),
        "{why}"
    );
    assert!(why.contains("exit status: 134"), "{why}");
}

/// A helper that died with two calls under way blames neither: the board
/// names the older, which need not be the one that crashed.
#[test]
fn a_helper_that_died_with_two_function_node_calls_under_way_blames_neither() {
    let (mut host, board_writer, _helper_node) =
        a_spawn_host_whose_helper_calls_function_node_lfo_1();
    let child = a_stand_in_helper_in_its_own_process_group("exit 134");
    let helper_process_id = child.id();
    host.child = Some(child);
    let now = MediaClock::now().as_nanos() as u64;
    board_writer.note_a_function_node_call_started(0, now);
    board_writer.note_a_function_node_call_started(0, now + 1);
    assert!(
        a_helper_process_becomes_collectable_within(helper_process_id, Duration::from_secs(5)),
        "the stand-in helper never exited"
    );

    host.detect_and_clean_up_after_an_out_of_process_helper_that_died();

    let why = host
        .why_the_out_of_process_helper_died_on_its_own()
        .expect("the death is kept for the restart");
    assert!(
        why.starts_with(
            "its helper process died with 2 function-node calls under way, the oldest to \
             `lfo-1`, so which crashed it is not known"
        ),
        "{why}"
    );
}

/// A descendant still holding the pipe cannot stall the refusal: the wait
/// gives up at its deadline with what had arrived.
#[test]
fn a_standard_error_left_open_bounds_the_wait_and_keeps_what_arrived() {
    let (mut still_open_writer, reader) =
        std::os::unix::net::UnixStream::pair().expect("socketpair");
    let tail = spawn_standard_error_reader_keeping_its_tail(reader, "Pblur");
    std::io::Write::write_all(&mut still_open_writer, b"said before exiting\n").unwrap();
    let recorded_by = Instant::now() + Duration::from_secs(10);
    while tail.text_once_closed_or_after(Duration::ZERO).is_empty() {
        assert!(Instant::now() < recorded_by, "the line was never recorded");
        std::thread::yield_now();
    }

    let (text_sender, text_receiver) = std::sync::mpsc::channel();
    let waiting_tail = tail.clone();
    std::thread::spawn(move || {
        let _ =
            text_sender.send(waiting_tail.text_once_closed_or_after(Duration::from_millis(200)));
    });
    let text = text_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the wait on a pipe nobody closed returned at its deadline");

    assert_eq!(text, "said before exiting");
    drop(still_open_writer);
}

/// Only the end of a long standard error is kept, so a helper that wrote
/// without bound cannot grow the parent's memory with it.
#[test]
fn only_the_last_bytes_of_a_long_standard_error_are_kept() {
    let tail = ChildStandardStreamRecording::keeping(ChildStandardStreamKeptBytes::TheLast(
        STANDARD_ERROR_TAIL_BYTES,
    ));
    tail.record(b"the earliest bytes, overwritten");
    tail.record(&vec![b'x'; STANDARD_ERROR_TAIL_BYTES]);
    tail.record(b"the reason");
    tail.mark_the_pipe_closed();

    let text = tail.text_once_closed_or_after(Duration::ZERO);

    assert_eq!(text.len(), STANDARD_ERROR_TAIL_BYTES);
    assert!(text.ends_with("xthe reason"));
}

/// Only the start of a long standard output is kept, and a pipe that never
/// closes still hands over what arrived once the deadline passes.
#[test]
fn only_the_first_bytes_of_a_long_standard_output_are_kept_and_read_without_its_close() {
    let (mut still_open_writer, reader) =
        std::os::unix::net::UnixStream::pair().expect("socketpair");
    let head = spawn_standard_output_reader_keeping_its_head(reader, 8);
    std::io::Write::write_all(&mut still_open_writer, b"the head").unwrap();
    std::io::Write::write_all(&mut still_open_writer, b", then what is dropped").unwrap();
    let recorded_by = Instant::now() + Duration::from_secs(10);
    while head.bytes_once_closed_or_after(Duration::ZERO).len() < 8 {
        assert!(Instant::now() < recorded_by, "the head was never recorded");
        std::thread::yield_now();
    }

    let bytes = head.bytes_once_closed_or_after(Duration::from_millis(200));

    assert_eq!(bytes, b"the head");
    drop(still_open_writer);
}

/// A helper that died writing nothing is still refused by name, and says
/// there was nothing to carry rather than ending on an empty line.
#[test]
fn a_helper_that_died_writing_nothing_is_refused_saying_so() {
    let refusal =
        refusal_of_a_helper_process_that_died_while_setting_up("BlurProcessor", "").to_string();
    assert!(
        refusal.ends_with(
            "[BlurProcessor] its helper process died before it finished setting up. It wrote \
             nothing to its standard error."
        ),
        "{refusal}"
    );
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::execution::node_scheduling_loop::{
    ContinuousLoopWaitOutcome, PAUSE_CHECK_INTERVAL, ReactiveLoopFdWaiter, ReactiveLoopWakeOutcome,
    next_continuous_deadline_ns, wait_for_a_deadline_or_a_control,
};
use crate::core::graph::{NodeState, ShutdownChannelComponent};
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use iceoryx2::prelude::*;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::time::Instant;

fn unique_suffix(tag: &str) -> String {
    format!(
        "test/runner/{tag}/{}",
        mint_machine_global_unique_name_suffix()
    )
}

/// A production shutdown channel and a duplicate of its wake fd, as the
/// spawn op hands one to a reactive runner.
fn shutdown_channel_and_its_wake_fd() -> (ShutdownChannelComponent, OwnedFd) {
    let shutdown_channel = ShutdownChannelComponent::new();
    let shutdown_wake_fd = shutdown_channel
        .try_clone_shutdown_wake_fd()
        .expect("the shutdown wake fd duplicates");
    (shutdown_channel, shutdown_wake_fd)
}

fn ports_would_return_something(node: &Arc<Mutex<NodeInstance>>) -> Option<bool> {
    node.lock()
        .iceoryx2_input_mailboxes_inner()
        .map(|mailboxes| mailboxes.any_port_has_data())
}

fn drain_input_listener(node: &Arc<Mutex<NodeInstance>>) {
    if let Some(mailboxes) = node.lock().iceoryx2_input_mailboxes_inner() {
        mailboxes.drain_listener();
    }
}

fn run_reactive_scheduling_loop_recording_callbacks(
    node: &Arc<Mutex<NodeInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    shutdown_wake_fd: Option<OwnedFd>,
    pause_gate: &AtomicBool,
    mut make_callback: impl FnMut(NodeSchedulingCallback),
) {
    let stream_clock = StreamClock::starting_now(crate::core::context::StreamClockSpec::Real);
    let step_progress = StepProgressOfOneNode::default();
    let mut host = NodeSchedulingLoopHostOnAnEngineThread {
        node,
        shutdown_rx,
        shutdown_wake_fd,
        pause_gate,
        stream_clock: &stream_clock,
        step_progress: &step_progress,
        call_node: |callback, _counted_on_the_step_ledger| make_callback(callback),
    };
    run_reactive_scheduling_loop("Preactive-scheduling", &mut host);
}

/// Wait on `stream_clock` for `deadline_ns` the way a continuous loop does,
/// on an engine thread's host over `shutdown_rx`.
fn wait_for_stream_time_or_shutdown(
    stream_clock: &StreamClock,
    deadline_ns: i64,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    stops_waiting_early: impl Fn() -> bool,
) -> ContinuousLoopWaitOutcome {
    let (node, _mailboxes) = input_only_node_with_plain_port(None);
    let step_progress = StepProgressOfOneNode::default();
    let pause_gate = AtomicBool::new(false);
    let mut host = NodeSchedulingLoopHostOnAnEngineThread {
        node: &node,
        shutdown_rx,
        shutdown_wake_fd: None,
        pause_gate: &pause_gate,
        stream_clock,
        step_progress: &step_progress,
        call_node: |_, _| {},
    };
    wait_for_a_deadline_or_a_control(&mut host, stream_clock, deadline_ns, stops_waiting_early)
}

fn listener_fd_of(listener: &iceoryx2::port::listener::Listener<ipc_threadsafe::Service>) -> i32 {
    // SAFETY: every caller uses the fd only while `listener` is alive.
    unsafe { listener.file_descriptor().native_handle() }
}

#[test]
fn a_bag_that_does_not_complete_a_window_does_not_dispatch_the_reactive_runner() {
    use crate::core::test_support::MockWindowedAudioConsumerNode;

    let mut instance = NodeInstance::new(Box::new(
        <MockWindowedAudioConsumerNode::Node as crate::core::GeneratedNode>::from_config(
            Default::default(),
        )
        .expect("the mock constructs from its default config"),
    ));
    instance
        .install_iceoryx2_resources()
        .expect("the mock accepts its iceoryx2 resources");
    let mailboxes = instance
        .iceoryx2_input_mailboxes_inner()
        .expect("a windowed mock holds input mailboxes");
    mailboxes.add_windowed_port(
        "audio",
        crate::iceoryx2::ReadMode::ReadNextInOrder,
        windowed_mock_contract(),
    );
    let node = Arc::new(Mutex::new(instance));

    assert_eq!(
        ports_would_return_something(&node),
        Some(false),
        "an empty windowed port must not dispatch"
    );

    // A third of the declared 512-sample window.
    let link_into_audio = a_test_link_into(&mailboxes, "audio");
    link_into_audio.send(&one_mono_audio_frame_for(160, 0));
    assert_eq!(
        ports_would_return_something(&node),
        Some(false),
        "160 of 512 samples is not a window, and dispatching here would hand \
         `process()` an empty read"
    );

    for block in 1..4i64 {
        link_into_audio.send(&one_mono_audio_frame_for(
            160,
            block * 160 * 1_000_000_000 / 16_000,
        ));
    }
    assert_eq!(
        ports_would_return_something(&node),
        Some(true),
        "640 samples completes a window, and a ready window must not sit latent"
    );
}

#[test]
fn a_node_with_no_input_mailboxes_is_not_gated_at_all() {
    use crate::core::test_support::MockOutputOnlyNode;

    let instance = NodeInstance::new(Box::new(
        <MockOutputOnlyNode::Node as crate::core::GeneratedNode>::from_config(Default::default())
            .expect("the mock constructs from its default config"),
    ));
    let node = Arc::new(Mutex::new(instance));

    assert_eq!(
        ports_would_return_something(&node),
        None,
        "there is nothing to gate on, and each caller says what that means"
    );
}

fn windowed_mock_contract() -> crate::iceoryx2::ResolvedAudioWindowContract {
    crate::iceoryx2::ResolvedAudioWindowContract::from_declared_values(
        &crate::core::descriptors::AudioWindowContractDeclaredValues {
            sample_rate: 16_000,
            channels: Some(1),
            dtype: "f32".to_string(),
            window_size: 512,
            hop: 512,
        },
    )
    .expect("the mock's own declaration resolves")
}

/// One sample carrying `frames` mono 16 kHz samples as an audio block bag.
fn one_mono_audio_frame_for(
    frames: usize,
    first_sample_timestamp_ns: i64,
) -> crate::iceoryx2::channel_sample_test_support::TestChannelSample {
    crate::iceoryx2::channel_sample_test_support::TestChannelSample::of_a_mono_channels_block(
        frames,
        16_000,
        first_sample_timestamp_ns,
    )
}

struct ReactiveSchedulingLoopOnItsOwnThread {
    node_callbacks: Arc<Mutex<Vec<NodeSchedulingCallback>>>,
    shutdown_channel: ShutdownChannelComponent,
    pause_gate: Arc<AtomicBool>,
    loop_thread: std::thread::JoinHandle<()>,
}

/// Whether the pause gate is already closed when the loop's thread starts.
#[derive(Clone, Copy)]
enum PauseGateAtLoopStart {
    Open,
    Closed,
}

impl ReactiveSchedulingLoopOnItsOwnThread {
    fn start(
        node: Arc<Mutex<NodeInstance>>,
        mailboxes: Arc<crate::iceoryx2::InputMailboxesInner>,
        each_dispatch_takes: std::time::Duration,
        pause_gate_at_loop_start: PauseGateAtLoopStart,
    ) -> Self {
        let node_callbacks: Arc<Mutex<Vec<NodeSchedulingCallback>>> = Arc::default();
        let (mut shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
        let shutdown_receiver = shutdown_channel
            .take_receiver()
            .expect("a fresh shutdown channel holds its receiver");
        let pause_gate = Arc::new(AtomicBool::new(matches!(
            pause_gate_at_loop_start,
            PauseGateAtLoopStart::Closed
        )));
        let loop_thread = std::thread::spawn({
            let node_callbacks = Arc::clone(&node_callbacks);
            let pause_gate = Arc::clone(&pause_gate);
            move || {
                run_reactive_scheduling_loop_recording_callbacks(
                    &node,
                    &shutdown_receiver,
                    Some(shutdown_wake_fd),
                    &pause_gate,
                    |callback| {
                        if callback == NodeSchedulingCallback::Process {
                            let _ = mailboxes.read_raw("in1");
                            std::thread::sleep(each_dispatch_takes);
                        }
                        node_callbacks.lock().push(callback);
                    },
                )
            }
        });
        Self {
            node_callbacks,
            shutdown_channel,
            pause_gate,
            loop_thread,
        }
    }

    fn count_of(&self, callback: NodeSchedulingCallback) -> usize {
        self.node_callbacks
            .lock()
            .iter()
            .filter(|made| **made == callback)
            .count()
    }

    /// Whether `callback` has been made `times` times within `deadline`.
    fn made_within(
        &self,
        callback: NodeSchedulingCallback,
        times: usize,
        deadline: std::time::Duration,
    ) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < deadline {
            if self.count_of(callback) >= times {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.count_of(callback) >= times
    }

    fn stop(self) {
        self.shutdown_channel.signal_shutdown();
        self.loop_thread
            .join()
            .expect("the scheduling loop must not panic");
    }
}

fn input_only_node_with_plain_port(
    listener: Option<iceoryx2::port::listener::Listener<ipc_threadsafe::Service>>,
) -> (
    Arc<Mutex<NodeInstance>>,
    Arc<crate::iceoryx2::InputMailboxesInner>,
) {
    use crate::core::test_support::MockInputOnlyNode;

    let mut instance = NodeInstance::new(Box::new(
        <MockInputOnlyNode::Node as crate::core::GeneratedNode>::from_config(Default::default())
            .expect("the mock constructs from its default config"),
    ));
    instance
        .install_iceoryx2_resources()
        .expect("the mock accepts its iceoryx2 resources");
    let mailboxes = instance
        .iceoryx2_input_mailboxes_inner()
        .expect("an input-only mock holds input mailboxes");
    mailboxes.add_port("in1", 16, crate::iceoryx2::ReadMode::ReadNextInOrder);
    if let Some(listener) = listener {
        mailboxes.set_listener(listener);
    }
    (Arc::new(Mutex::new(instance)), mailboxes)
}

/// One wire frame stamped for `in1`, carrying four opaque bytes.
fn one_frame_for_in1() -> crate::iceoryx2::channel_sample_test_support::TestChannelSample {
    crate::iceoryx2::channel_sample_test_support::TestChannelSample::carrying(0, &[0; 4])
}

/// A test link bound into `port` of `mailboxes`.
fn a_test_link_into(
    mailboxes: &crate::iceoryx2::InputMailboxesInner,
    port: &str,
) -> crate::iceoryx2::channel_sample_test_support::TestLinkIntoAnInputPort {
    crate::iceoryx2::channel_sample_test_support::TestLinkIntoAnInputPort::bound_into(
        mailboxes, port, 16,
    )
}

fn open_one_listener_event_service(
    node: &iceoryx2::node::Node<ipc_threadsafe::Service>,
    tag: &str,
) -> iceoryx2::service::port_factory::event::PortFactory<ipc_threadsafe::Service> {
    node.service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
        .event()
        .max_notifiers(1)
        .max_listeners(1)
        .open_or_create()
        .unwrap()
}

#[test]
fn a_node_slower_than_its_upstream_keeps_every_notify_deliverable() {
    const FRAMES_AT_ONE_KILOHERTZ: usize = 1000;

    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let service = open_one_listener_event_service(&node, "slow-consumer");
    let notifier = service.notifier_builder().create().unwrap();
    let (node, mailboxes) = input_only_node_with_plain_port(Some(
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
            .unwrap(),
    ));
    let link_into_in1 = a_test_link_into(&mailboxes, "in1");
    let running = ReactiveSchedulingLoopOnItsOwnThread::start(
        node,
        Arc::clone(&mailboxes),
        std::time::Duration::from_millis(2),
        PauseGateAtLoopStart::Open,
    );

    let mut undelivered_notifies = 0;
    for _ in 0..FRAMES_AT_ONE_KILOHERTZ {
        link_into_in1.send(&one_frame_for_in1());
        if notifier.notify().unwrap() == 0 {
            undelivered_notifies += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let dispatches = running.count_of(NodeSchedulingCallback::Process);
    running.stop();

    assert!(
        dispatches >= 100,
        "the consumer must have been kept busy for the burst; it dispatched {dispatches} times"
    );
    assert_eq!(
        undelivered_notifies, 0,
        "{undelivered_notifies} of {FRAMES_AT_ONE_KILOHERTZ} notifies reached nobody"
    );
}

#[test]
fn bags_that_arrived_during_a_pause_are_dispatched_after_the_resume_with_nothing_more_published() {
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let service = open_one_listener_event_service(&node, "paused-consumer");
    let notifier = service.notifier_builder().create().unwrap();
    let (node, mailboxes) = input_only_node_with_plain_port(Some(
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
            .unwrap(),
    ));
    let link_into_in1 = a_test_link_into(&mailboxes, "in1");
    let running = ReactiveSchedulingLoopOnItsOwnThread::start(
        node,
        Arc::clone(&mailboxes),
        std::time::Duration::ZERO,
        PauseGateAtLoopStart::Closed,
    );
    assert!(
        running.made_within(
            NodeSchedulingCallback::OnPause,
            1,
            std::time::Duration::from_secs(5)
        ),
        "the loop must see the pause"
    );

    for _ in 0..3 {
        link_into_in1.send(&one_frame_for_in1());
        notifier.notify().unwrap();
    }
    // Several paused ticks, each of which drains the listener.
    std::thread::sleep(PAUSE_CHECK_INTERVAL * 5);
    running.pause_gate.store(false, Ordering::Release);

    let dispatched = running.made_within(
        NodeSchedulingCallback::Process,
        3,
        std::time::Duration::from_secs(2),
    );
    let dispatches = running.count_of(NodeSchedulingCallback::Process);
    running.stop();
    assert!(
        dispatched,
        "three bags were waiting at the resume and {dispatches} were dispatched"
    );
}

/// A first live link creates its subscriber before its listener, so a bag
/// published in between notifies nobody and waits in the subscriber. The
/// runner reaches its next pass with that bag queued and a listener holding
/// nothing, which is the state built here before the loop starts.
///
/// Fail-without-fix: drop the check before the wait and the runner sleeps
/// on the empty listener until the upstream publishes again.
#[test]
fn a_bag_queued_before_the_listener_existed_is_dispatched_with_no_notify() {
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let service = open_one_listener_event_service(&node, "first-link-gap");
    let (node, mailboxes) = input_only_node_with_plain_port(None);
    let link_into_in1 = a_test_link_into(&mailboxes, "in1");
    link_into_in1.send(&one_frame_for_in1());
    mailboxes.set_listener(
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
            .unwrap(),
    );

    let running = ReactiveSchedulingLoopOnItsOwnThread::start(
        node,
        mailboxes,
        std::time::Duration::ZERO,
        PauseGateAtLoopStart::Open,
    );
    let dispatched = running.made_within(
        NodeSchedulingCallback::Process,
        1,
        std::time::Duration::from_secs(2),
    );
    running.stop();
    assert!(
        dispatched,
        "the queued bag must be dispatched without waiting for a notify"
    );
}

/// The drain every reactive tick runs leaves the listener fd not readable
/// however many notifications piled up behind it, and the next send makes
/// it readable again — so a wait after a drain wakes only for something new.
///
/// Fail-without-fix: make `drain_input_listener` a no-op and the fd still
/// reads readable after the drain.
#[test]
fn a_drained_listener_waits_for_the_next_send_however_many_came_before() {
    use crate::core::test_support::MockInputOnlyNode;

    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let service = open_one_listener_event_service(&node, "drain-then-wait");
    let notifier = service.notifier_builder().create().unwrap();

    let mut instance = NodeInstance::new(Box::new(
        <MockInputOnlyNode::Node as crate::core::GeneratedNode>::from_config(Default::default())
            .expect("the mock constructs from its default config"),
    ));
    instance
        .install_iceoryx2_resources()
        .expect("the mock accepts its iceoryx2 resources");
    let input_mailboxes = instance
        .iceoryx2_input_mailboxes_inner()
        .expect("an input-only mock holds input mailboxes");
    input_mailboxes.set_listener(
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
            .unwrap(),
    );
    let waiter = ReactiveLoopFdWaiter::new(
        input_mailboxes
            .listener_fd()
            .expect("the listener was just installed"),
        1,
        None,
    )
    .expect("the readiness queue registers the listener fd");
    let node = Arc::new(Mutex::new(instance));

    for _ in 0..8192 {
        notifier.notify().unwrap();
    }
    drain_input_listener(&node);
    let outcome_after_the_drain = waiter.wait(std::time::Duration::ZERO);
    assert!(
        matches!(outcome_after_the_drain, ReactiveLoopWakeOutcome::TimedOut),
        "a drained listener must not wake the next wait; got {outcome_after_the_drain:?}"
    );

    notifier.notify().unwrap();
    let outcome_after_the_next_send = waiter.wait(std::time::Duration::ZERO);
    assert!(
        matches!(
            outcome_after_the_next_send,
            ReactiveLoopWakeOutcome::Notified
        ),
        "the next send must wake the wait; got {outcome_after_the_next_send:?}"
    );
}

/// A waiter on a fresh listener, and the notifier that feeds it.
struct ListenerWaiterFixture {
    _node: iceoryx2::node::Node<ipc_threadsafe::Service>,
    notifier: iceoryx2::port::notifier::Notifier<ipc_threadsafe::Service>,
    listener: iceoryx2::port::listener::Listener<ipc_threadsafe::Service>,
    shutdown_channel: ShutdownChannelComponent,
    waiter: ReactiveLoopFdWaiter,
    /// Registered by the waiter without being owned by it.
    _shutdown_wake_fd: OwnedFd,
}

impl ListenerWaiterFixture {
    fn new(tag: &str) -> Self {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, tag);
        let notifier = service.notifier_builder().create().unwrap();
        let listener =
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
                .unwrap();
        let (shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
        let waiter = ReactiveLoopFdWaiter::new(
            listener_fd_of(&listener),
            1,
            Some(shutdown_wake_fd.as_raw_fd()),
        )
        .expect("the readiness queue registers both fds");
        Self {
            _node: node,
            notifier,
            listener,
            shutdown_channel,
            waiter,
            _shutdown_wake_fd: shutdown_wake_fd,
        }
    }
}

/// The waiter reports the listener's readiness. Level-triggered, so the
/// notify lands first and the wait is a zero-bound poll.
#[test]
fn a_delivered_notify_makes_the_wait_report_notified() {
    let fixture = ListenerWaiterFixture::new("wait-notified");
    fixture.notifier.notify().unwrap();

    let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

    assert!(
        matches!(outcome, ReactiveLoopWakeOutcome::Notified),
        "expected Notified, got {outcome:?}"
    );
    fixture.listener.try_wait(|_| {}).unwrap();
}

/// The production shutdown signal makes the waiter report shutdown, with
/// no listener activity at all.
#[test]
fn a_signalled_shutdown_makes_the_wait_report_shutdown() {
    let fixture = ListenerWaiterFixture::new("wait-shutdown");
    fixture.shutdown_channel.signal_shutdown();

    let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

    assert!(
        matches!(outcome, ReactiveLoopWakeOutcome::ControlArrived),
        "expected Shutdown, got {outcome:?}"
    );
}

/// A runner woken by both exits rather than draining one more frame.
#[test]
fn shutdown_outranks_a_notify_reported_by_the_same_wait() {
    let fixture = ListenerWaiterFixture::new("wait-both");
    fixture.notifier.notify().unwrap();
    fixture.shutdown_channel.signal_shutdown();

    let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

    assert!(
        matches!(outcome, ReactiveLoopWakeOutcome::ControlArrived),
        "expected Shutdown, got {outcome:?}"
    );
    fixture.listener.try_wait(|_| {}).unwrap();
}

/// A wait with nothing to report returns on its own, which is what lets a
/// runner whose listener was replaced while it slept come round to rebuild
/// rather than sleeping on a dead fd until shutdown.
#[test]
fn a_wait_with_nothing_to_report_times_out() {
    let fixture = ListenerWaiterFixture::new("wait-idle");

    let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

    assert!(
        matches!(outcome, ReactiveLoopWakeOutcome::TimedOut),
        "an idle wait must time out rather than block; got {outcome:?}"
    );
}

/// The one cross-thread check: a shutdown made visible only on the wake
/// fd ends a reactive loop blocked in its wait. The crossbeam half of the
/// shutdown is withheld, so the channel-poll fallback never exits here.
/// The deadline is generous and nothing is asserted about latency.
#[test]
fn a_shutdown_seen_only_on_the_wake_fd_ends_a_loop_blocked_in_its_wait() {
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let service = open_one_listener_event_service(&node, "loop-wake-fd-shutdown");
    let (node, _mailboxes) = input_only_node_with_plain_port(Some(
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
            .unwrap(),
    ));
    let (shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
    let (withheld_shutdown_sender, withheld_shutdown_receiver) = crossbeam_channel::bounded(1);
    let (loop_exited_sender, loop_exited_receiver) = crossbeam_channel::bounded(1);
    let loop_thread = std::thread::spawn(move || {
        run_reactive_scheduling_loop_recording_callbacks(
            &node,
            &withheld_shutdown_receiver,
            Some(shutdown_wake_fd),
            &AtomicBool::new(false),
            |_| {},
        );
        let _ = loop_exited_sender.send(());
    });

    shutdown_channel.signal_shutdown();
    let exited_on_the_wake_fd = loop_exited_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .is_ok();
    let _ = withheld_shutdown_sender.send(());
    loop_thread
        .join()
        .expect("the scheduling loop must not panic");

    assert!(
        exited_on_the_wake_fd,
        "the loop never saw the wake fd's shutdown, so it is not waiting on it"
    );
}

#[test]
fn a_helper_dying_soon_after_each_start_is_restarted_four_times_then_fails() {
    let id = NodeRunId::from("Pblur");
    let state = ObservableNodeState::new(NodeState::Running);
    let mut restart_backoff = ChildProcessRestartBackoff::new();

    for death in 1..=4 {
        let next_start = after_a_helper_process_death(
            &id,
            &state,
            &mut restart_backoff,
            format!("its helper process ended with exit status: {death}"),
            Duration::from_millis(200),
        );
        assert!(next_start.is_some(), "death {death} gave up");
        assert_eq!(state.current(), NodeState::Restarting);
    }
    let fifth = after_a_helper_process_death(
        &id,
        &state,
        &mut restart_backoff,
        "its helper process ended with exit status: 5".to_string(),
        Duration::ZERO,
    );

    assert!(fifth.is_none());
    assert_eq!(state.current(), NodeState::Error);
    let failure_reason = state.failure_reason().expect("the failure is explained");
    assert!(
        failure_reason.contains("died 5 times in a row")
            && failure_reason.ends_with("exit status: 5"),
        "{failure_reason}"
    );
    assert_eq!(
        state
            .helper_process_restart_record()
            .last_exit_reason
            .as_deref(),
        Some("its helper process ended with exit status: 5")
    );
}

#[test]
fn a_helper_that_ran_healthily_before_dying_starts_again_after_the_first_wait() {
    let id = NodeRunId::from("Pblur");
    let state = ObservableNodeState::new(NodeState::Running);
    let mut restart_backoff = ChildProcessRestartBackoff::new();
    let before = Instant::now();

    let next_start = after_a_helper_process_death(
        &id,
        &state,
        &mut restart_backoff,
        "its helper process ended with signal: 11 (SIGSEGV)".to_string(),
        Duration::from_secs(3600),
    )
    .expect("a healthy helper is never given up on");

    assert_eq!(state.current(), NodeState::Restarting);
    assert!(next_start >= before + Duration::from_secs(1));
    assert!(next_start < before + Duration::from_secs(2));
}

#[test]
fn a_continuous_deadline_steps_one_interval_on_its_grid() {
    assert_eq!(next_continuous_deadline_ns(1_000, 100, 1_050), 1_100);
    assert_eq!(next_continuous_deadline_ns(1_000, 100, 900), 1_100);
}

#[test]
fn an_overrun_skips_to_the_next_deadline_after_now_on_the_same_grid() {
    assert_eq!(next_continuous_deadline_ns(1_000, 100, 1_350), 1_400);
    assert_eq!(next_continuous_deadline_ns(1_000, 100, 1_400), 1_500);
}

fn a_scaled_stream_clock(scale: f64) -> StreamClock {
    StreamClock::starting_now(crate::core::context::StreamClockSpec::Scaled {
        scale,
        step_ns: crate::core::context::DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
    })
}

#[test]
fn a_wait_on_a_half_speed_stream_takes_twice_the_stream_time_on_the_machine_clock() {
    let stream_clock = a_scaled_stream_clock(0.5);
    let (_shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded::<()>();
    let machine_started = Instant::now();
    let deadline_stream_ns = stream_clock.now_ns() + 20_000_000;

    let outcome =
        wait_for_stream_time_or_shutdown(&stream_clock, deadline_stream_ns, &shutdown_rx, || false);

    assert_eq!(outcome, ContinuousLoopWaitOutcome::DeadlineReached);
    assert!(machine_started.elapsed() >= Duration::from_millis(40));
    assert!(stream_clock.now_ns() >= deadline_stream_ns);
}

#[test]
fn a_rescale_reaches_a_wait_already_under_way() {
    let stream_clock = Arc::new(a_scaled_stream_clock(0.001));
    let (_shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded::<()>();
    let deadline_stream_ns = stream_clock.now_ns() + 50_000_000;
    let rescaling_clock = Arc::clone(&stream_clock);
    let rescaler = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        rescaling_clock
            .rescale("warped", 1.0)
            .expect("a scaled clock rescales");
    });
    let machine_started = Instant::now();

    let outcome =
        wait_for_stream_time_or_shutdown(&stream_clock, deadline_stream_ns, &shutdown_rx, || false);

    rescaler.join().expect("the rescaling thread finishes");
    assert_eq!(outcome, ContinuousLoopWaitOutcome::DeadlineReached);
    assert!(machine_started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_shutdown_ends_a_continuous_wait_before_its_deadline() {
    let stream_clock = a_scaled_stream_clock(0.001);
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded::<()>();
    shutdown_tx.send(()).expect("the shutdown channel is open");

    let outcome = wait_for_stream_time_or_shutdown(
        &stream_clock,
        stream_clock.now_ns() + 1_000_000_000,
        &shutdown_rx,
        || false,
    );

    assert_eq!(outcome, ContinuousLoopWaitOutcome::Stopped);
}

/// A reactive loop sends its node's kept last message to a reader that connects while the node
/// writes nothing and nothing arrives for it.
///
/// Fail-without-fix: drop the send from the reactive loop's pass, and the reader never reads.
#[test]
fn a_reactive_loop_sends_a_kept_message_to_a_reader_that_connects_while_it_idles() {
    use crate::core::test_support::MockOutputOnlyNode;
    use crate::iceoryx2::{
        ChannelEgressConfig, ChannelTrustTier, Iceoryx2Node, MessageGroupBoundary,
        TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    };

    let mut instance = NodeInstance::new(Box::new(
        <MockOutputOnlyNode::Node as crate::core::GeneratedNode>::from_config(Default::default())
            .expect("the mock constructs from its default config"),
    ));
    instance
        .install_iceoryx2_resources()
        .expect("the mock accepts its iceoryx2 resources");
    let output_writer = instance
        .iceoryx2_output_writer_inner()
        .expect("an output-only mock holds an output writer");
    output_writer.keep_the_last_message_of_output_ports(["out1".to_string()]);
    output_writer
        .write_bytes_with_timestamp("out1", b"the value", 0, MessageGroupBoundary::StartsGroup)
        .expect("a write to a declared port no link reached sends nothing");
    let node = Arc::new(Mutex::new(instance));

    let (mut shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
    let shutdown_receiver = shutdown_channel
        .take_receiver()
        .expect("a fresh shutdown channel holds its receiver");
    let loop_thread = std::thread::spawn({
        let node = Arc::clone(&node);
        move || {
            run_reactive_scheduling_loop_recording_callbacks(
                &node,
                &shutdown_receiver,
                Some(shutdown_wake_fd),
                &AtomicBool::new(false),
                |_| {},
            )
        }
    });

    let channel = Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix("kept-message/pubsub"), 2, 4)
        .expect("open the channel data service");
    output_writer.set_channel_publisher(
        "out1",
        &channel,
        channel.create_publisher(64).expect("the port's publisher"),
        ChannelEgressConfig {
            service_name: "test/runner/out1".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 64,
            chunk_ceiling_bytes: TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );
    let reader = channel.create_subscriber(4).expect("the port's reader");

    let started = Instant::now();
    let mut received = None;
    while received.is_none() && started.elapsed() < Duration::from_secs(5) {
        received = reader.receive().expect("the reader receives");
        std::thread::sleep(Duration::from_millis(5));
    }
    shutdown_channel.signal_shutdown();
    loop_thread
        .join()
        .expect("the scheduling loop must not panic");

    let received = received.expect("the loop sent the kept message to the late reader");
    assert_eq!(
        crate::iceoryx2::channel_sample_test_support::stamped_payload_of(&received),
        b"the value"
    );
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The reader and the escalate workers over a real socketpair, with a
//! stand-in dispatch in place of the GPU one, so what the reader never waits
//! on is provable without a device.

use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME: Duration = Duration::from_secs(5);

/// One bridge's three threads, with the helper's end of the socket in the
/// test's hands.
struct BridgeThreadsDrivenFromTheHelperEnd {
    helper_end_writer: BufWriter<UnixStream>,
    helper_end_reader: BufReader<UnixStream>,
    lifecycle_rx: Receiver<serde_json::Value>,
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    threads: SubprocessBridgeThreads,
}

impl BridgeThreadsDrivenFromTheHelperEnd {
    fn dispatching_through(escalate_request_dispatch: EscalateRequestDispatch) -> Self {
        let (parent_end, helper_end) = UnixStream::pair().expect("socketpair");
        let parent_read_half = parent_end.try_clone().expect("clone the parent end");
        let (lifecycle_tx, lifecycle_rx) = mpsc::channel();
        let parent_side = Arc::new(
            ParentSideOfOneSubprocessBridgeSharedByItsThreads::over(
                parent_end,
                "p-escalate-worker-test".to_string(),
            )
            .expect("clone the parent end"),
        );
        let threads = spawn_the_reader_and_its_two_escalate_workers(
            BufReader::new(parent_read_half),
            Arc::clone(&parent_side),
            escalate_request_dispatch,
            lifecycle_tx,
        )
        .expect("spawn the bridge's threads");
        Self {
            helper_end_writer: BufWriter::new(
                helper_end.try_clone().expect("clone the helper end"),
            ),
            helper_end_reader: BufReader::new(helper_end),
            lifecycle_rx,
            parent_side,
            threads,
        }
    }

    fn send_from_the_helper(&mut self, frame: &serde_json::Value) {
        write_frame(&mut self.helper_end_writer, frame).expect("write from the helper end");
    }

    fn request_id_of_the_next_answer_the_helper_reads(&mut self) -> String {
        read_frame(&mut self.helper_end_reader)
            .expect("read an answer at the helper end")
            .get("request_id")
            .and_then(|request_id| request_id.as_str())
            .expect("the answer names its request")
            .to_string()
    }
}

/// A stand-in dispatch that holds any request for its held op until the
/// test lets it go, reports every log record, every request it starts and
/// every handle released, and answers each request with its own id and a
/// handle named after it.
struct EscalateDispatchHoldingOneOp {
    escalate_request_dispatch: EscalateRequestDispatch,
    let_the_held_request_go: mpsc::Sender<()>,
    request_ids_started: Receiver<String>,
    log_records_dispatched: Receiver<String>,
    handles_released: Receiver<String>,
}

/// An op the escalate worker answers, held to stand for slow GPU work there.
const HELD_ON_THE_ESCALATE_WORKER: &str = "wait_device_idle";

impl EscalateDispatchHoldingOneOp {
    fn holding(held_op: &'static str) -> Self {
        let (let_the_held_request_go, held_request_gate) = mpsc::channel::<()>();
        let held_request_gate = Mutex::new(held_request_gate);
        let (request_started_tx, request_ids_started) = mpsc::channel();
        let request_started_tx = Mutex::new(request_started_tx);
        let (log_record_tx, log_records_dispatched) = mpsc::channel();
        let log_record_tx = Mutex::new(log_record_tx);
        let (handle_released_tx, handles_released) = mpsc::channel();
        let handle_released_tx = Mutex::new(handle_released_tx);
        let escalate_request_dispatch: EscalateRequestDispatch = Arc::new(move |frame| {
            let op = frame.get("op").and_then(|op| op.as_str()).unwrap_or("");
            if op == ESCALATE_OP_ANSWERED_BY_NOTHING {
                let message = frame["message"].as_str().unwrap_or("").to_string();
                let _ = log_record_tx.lock().unwrap().send(message);
                return None;
            }
            let request_id = frame["request_id"].as_str().unwrap_or("").to_string();
            if op == "release_handle" {
                let handle_id = frame["handle_id"].as_str().unwrap_or("").to_string();
                let _ = handle_released_tx.lock().unwrap().send(handle_id);
            } else {
                let _ = request_started_tx.lock().unwrap().send(request_id.clone());
            }
            if op == held_op {
                let _ = held_request_gate.lock().unwrap().recv();
            }
            Some(serde_json::json!({
                "rpc": "escalate_response",
                "request_id": request_id,
                "result": "ok",
                "handle_id": format!("handle-of-{request_id}"),
            }))
        });
        Self {
            escalate_request_dispatch,
            let_the_held_request_go,
            request_ids_started,
            log_records_dispatched,
            handles_released,
        }
    }
}

/// Fail-without-fix: dispatch escalates inline on the reader and neither
/// the log record nor `ready` is routed until the held op returns, so
/// both waits below run out.
#[test]
fn a_slow_escalate_never_delays_a_log_record_or_a_lifecycle_reply_sent_behind_it() {
    let dispatch = EscalateDispatchHoldingOneOp::holding(HELD_ON_THE_ESCALATE_WORKER);
    let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
        &dispatch.escalate_request_dispatch,
    ));

    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        HELD_ON_THE_ESCALATE_WORKER,
        "r-slow",
    ));
    bridge.send_from_the_helper(&log_frame());
    bridge.send_from_the_helper(&serde_json::json!({"rpc": "ready"}));

    assert_eq!(
        dispatch
            .request_ids_started
            .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
            .expect("the slow request reaches the worker"),
        "r-slow"
    );
    assert_eq!(
        dispatch
            .log_records_dispatched
            .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
            .expect("a log record sent behind a held escalate is dispatched while it is held"),
        "hello from subprocess"
    );
    let lifecycle_reply = bridge
        .lifecycle_rx
        .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
        .expect("a lifecycle reply sent behind a held escalate is routed while it is held");
    assert_eq!(lifecycle_reply["rpc"], "ready");

    dispatch
        .let_the_held_request_go
        .send(())
        .expect("the held request is still waiting");
    assert_eq!(
        bridge.request_id_of_the_next_answer_the_helper_reads(),
        "r-slow",
        "the held request is still answered once it returns"
    );
}

/// Fail-without-fix: dispatch each request on its own thread and the
/// requests overlap and answer out of the order they arrived in.
#[test]
fn escalates_from_one_helper_are_answered_one_at_a_time_in_the_order_they_arrived() {
    let requests_in_flight = Arc::new(AtomicUsize::new(0));
    let most_requests_ever_in_flight = Arc::new(AtomicUsize::new(0));
    let escalate_request_dispatch: EscalateRequestDispatch = {
        let requests_in_flight = Arc::clone(&requests_in_flight);
        let most_requests_ever_in_flight = Arc::clone(&most_requests_ever_in_flight);
        Arc::new(move |frame| {
            let in_flight = requests_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            most_requests_ever_in_flight.fetch_max(in_flight, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(2));
            requests_in_flight.fetch_sub(1, Ordering::SeqCst);
            Some(serde_json::json!({
                "rpc": "escalate_response",
                "request_id": frame["request_id"],
                "result": "ok",
            }))
        })
    };
    let mut bridge =
        BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(escalate_request_dispatch);

    let request_ids: Vec<String> = (0..8).map(|index| format!("r-{index}")).collect();
    for request_id in &request_ids {
        bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
            "run_compute_kernel",
            request_id,
        ));
    }

    let answered_in_order: Vec<String> = request_ids
        .iter()
        .map(|_| bridge.request_id_of_the_next_answer_the_helper_reads())
        .collect();
    assert_eq!(answered_in_order, request_ids);
    assert_eq!(
        most_requests_ever_in_flight.load(Ordering::SeqCst),
        1,
        "one helper's escalates never run concurrently"
    );
}

/// A request past the helper's queue bound is refused at once, so the
/// reader keeps routing the helper's lifecycle replies behind it.
///
/// Fail-without-fix: block the reader until the queue has room and
/// neither the refusal nor `ready` arrives while the held op runs.
#[test]
fn a_request_past_the_helpers_queue_bound_is_refused_rather_than_stalling_the_reader() {
    let dispatch = EscalateDispatchHoldingOneOp::holding(HELD_ON_THE_ESCALATE_WORKER);
    let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
        &dispatch.escalate_request_dispatch,
    ));
    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        HELD_ON_THE_ESCALATE_WORKER,
        "r-held",
    ));
    dispatch
        .request_ids_started
        .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
        .expect("the held request reaches the worker");

    for queued_index in 0..ESCALATE_REQUESTS_QUEUED_PER_HELPER {
        bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
            "run_compute_kernel",
            &format!("r-queued-{queued_index}"),
        ));
    }
    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        "run_compute_kernel",
        "r-past-the-bound",
    ));
    bridge.send_from_the_helper(&serde_json::json!({"rpc": "ready"}));

    bridge
        .helper_end_reader
        .get_ref()
        .set_read_timeout(Some(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME))
        .expect("a read timeout on the helper end");
    let refusal = read_frame(&mut bridge.helper_end_reader)
        .expect("the request past the bound is answered while the held op runs");
    assert_eq!(refusal["request_id"], "r-past-the-bound");
    assert_eq!(refusal["result"], "err");
    let lifecycle_reply = bridge
        .lifecycle_rx
        .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
        .expect("a lifecycle reply behind a full queue is still routed");
    assert_eq!(lifecycle_reply["rpc"], "ready");

    dispatch
        .let_the_held_request_go
        .send(())
        .expect("the held request is still waiting");
}

/// Fail-without-fix: drop the given-up check in the worker and the queued
/// acquire is dispatched after the bridge gave up, past the teardown
/// drain that would have released it.
#[test]
fn an_escalate_still_queued_when_its_bridge_gives_up_is_never_dispatched() {
    let dispatch = EscalateDispatchHoldingOneOp::holding(HELD_ON_THE_ESCALATE_WORKER);
    let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
        &dispatch.escalate_request_dispatch,
    ));

    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        HELD_ON_THE_ESCALATE_WORKER,
        "r-held",
    ));
    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        "acquire_pixel_buffer",
        "r-queued-behind-it",
    ));
    assert_eq!(
        dispatch
            .request_ids_started
            .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
            .expect("the held request reaches the worker"),
        "r-held"
    );

    bridge.parent_side.give_up_on_the_subprocess("is gone");
    dispatch
        .let_the_held_request_go
        .send(())
        .expect("the held request is still waiting");
    let BridgeThreadsDrivenFromTheHelperEnd {
        helper_end_writer,
        helper_end_reader,
        threads,
        ..
    } = bridge;
    drop(helper_end_writer);
    drop(helper_end_reader);
    threads
        .frame_demultiplexing_reader_thread
        .join()
        .expect("the reader leaves at EOF");
    threads
        .escalate_worker_thread
        .join()
        .expect("the worker leaves once the reader has");

    let started_after_the_bridge_gave_up: Vec<String> =
        dispatch.request_ids_started.try_iter().collect();
    assert!(
        started_after_the_bridge_gave_up.is_empty(),
        "a request queued behind the give-up was dispatched: \
         {started_after_the_bridge_gave_up:?}"
    );
}

/// A frame's dispatch sent behind a kernel build is answered while the
/// build still runs.
///
/// Fail-without-fix: queue the build on the escalate worker and the
/// dispatch waits out the whole build, so the read below runs out.
#[test]
fn a_kernel_build_never_delays_a_dispatch_sent_behind_it() {
    let dispatch = EscalateDispatchHoldingOneOp::holding(ESCALATE_OP_BUILDING_A_KERNEL);
    let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
        &dispatch.escalate_request_dispatch,
    ));

    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        ESCALATE_OP_BUILDING_A_KERNEL,
        "r-build",
    ));
    assert_eq!(
        dispatch
            .request_ids_started
            .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
            .expect("the build reaches its worker"),
        "r-build"
    );
    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        "run_compute_kernel",
        "r-frame",
    ));

    bridge
        .helper_end_reader
        .get_ref()
        .set_read_timeout(Some(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME))
        .expect("a read timeout on the helper end");
    assert_eq!(
        bridge.request_id_of_the_next_answer_the_helper_reads(),
        "r-frame",
        "the dispatch is answered while the build it was sent behind still runs"
    );

    dispatch
        .let_the_held_request_go
        .send(())
        .expect("the build is still running");
    assert_eq!(
        bridge.request_id_of_the_next_answer_the_helper_reads(),
        "r-build",
        "the build is still answered once it finishes"
    );
}

/// Fail-without-fix: dispatch each build on its own thread and the builds
/// overlap and answer out of the order they arrived in.
#[test]
fn kernel_builds_from_one_helper_are_answered_one_at_a_time_in_the_order_they_arrived() {
    let builds_in_flight = Arc::new(AtomicUsize::new(0));
    let most_builds_ever_in_flight = Arc::new(AtomicUsize::new(0));
    let escalate_request_dispatch: EscalateRequestDispatch = {
        let builds_in_flight = Arc::clone(&builds_in_flight);
        let most_builds_ever_in_flight = Arc::clone(&most_builds_ever_in_flight);
        Arc::new(move |frame| {
            let in_flight = builds_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            most_builds_ever_in_flight.fetch_max(in_flight, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(2));
            builds_in_flight.fetch_sub(1, Ordering::SeqCst);
            Some(serde_json::json!({
                "rpc": "escalate_response",
                "request_id": frame["request_id"],
                "result": "ok",
            }))
        })
    };
    let mut bridge =
        BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(escalate_request_dispatch);

    let request_ids: Vec<String> = (0..8).map(|index| format!("r-build-{index}")).collect();
    for request_id in &request_ids {
        bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
            ESCALATE_OP_BUILDING_A_KERNEL,
            request_id,
        ));
    }

    let answered_in_order: Vec<String> = request_ids
        .iter()
        .map(|_| bridge.request_id_of_the_next_answer_the_helper_reads())
        .collect();
    assert_eq!(answered_in_order, request_ids);
    assert_eq!(
        most_builds_ever_in_flight.load(Ordering::SeqCst),
        1,
        "one helper's kernel builds never run concurrently"
    );
}

/// A build that finishes once its bridge gave up releases the kernel it
/// registered, so a registration made after teardown's drain never
/// outlives the helper.
///
/// Fail-without-fix: answer the build as the escalate worker answers any
/// request and no release is ever dispatched.
#[test]
fn a_kernel_build_finishing_after_its_bridge_gave_up_releases_what_it_registered() {
    let dispatch = EscalateDispatchHoldingOneOp::holding(ESCALATE_OP_BUILDING_A_KERNEL);
    let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
        &dispatch.escalate_request_dispatch,
    ));

    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        ESCALATE_OP_BUILDING_A_KERNEL,
        "r-build",
    ));
    assert_eq!(
        dispatch
            .request_ids_started
            .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
            .expect("the build reaches its worker"),
        "r-build"
    );

    bridge.parent_side.give_up_on_the_subprocess("is gone");
    dispatch
        .let_the_held_request_go
        .send(())
        .expect("the build is still running");

    assert_eq!(
        dispatch
            .handles_released
            .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
            .expect("the kernel the unanswered build registered is released"),
        "handle-of-r-build"
    );
}

/// Fail-without-fix: drop the given-up check in the build worker and the
/// queued build is dispatched after the bridge gave up.
#[test]
fn a_kernel_build_still_queued_when_its_bridge_gives_up_is_never_dispatched() {
    let dispatch = EscalateDispatchHoldingOneOp::holding(ESCALATE_OP_BUILDING_A_KERNEL);
    let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
        &dispatch.escalate_request_dispatch,
    ));

    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        ESCALATE_OP_BUILDING_A_KERNEL,
        "r-build-held",
    ));
    bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
        ESCALATE_OP_BUILDING_A_KERNEL,
        "r-build-queued-behind-it",
    ));
    assert_eq!(
        dispatch
            .request_ids_started
            .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
            .expect("the held build reaches its worker"),
        "r-build-held"
    );

    bridge.parent_side.give_up_on_the_subprocess("is gone");
    // Twice: a build dispatched past the give-up is held too, and must return
    // for the worker to be joined and the assertion below to say so.
    for _ in 0..2 {
        dispatch
            .let_the_held_request_go
            .send(())
            .expect("the held build is still running");
    }
    let BridgeThreadsDrivenFromTheHelperEnd {
        helper_end_writer,
        helper_end_reader,
        threads,
        ..
    } = bridge;
    drop(helper_end_writer);
    drop(helper_end_reader);
    threads
        .frame_demultiplexing_reader_thread
        .join()
        .expect("the reader leaves at EOF");
    threads
        .kernel_build_worker_thread
        .join()
        .expect("the build worker leaves once the reader has");

    let started_after_the_bridge_gave_up: Vec<String> =
        dispatch.request_ids_started.try_iter().collect();
    assert!(
        started_after_the_bridge_gave_up.is_empty(),
        "a build queued behind the give-up was dispatched: \
         {started_after_the_bridge_gave_up:?}"
    );
}

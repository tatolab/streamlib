// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's per-surface timeline pair across real processes on macOS.
//!
//! A spawned helper imports `produce_done` / `consume_done` as Metal shared
//! events over the surface-share Mach channel and the two sides order frames
//! on them; a pair whose export fails orders the same frames host-side; and
//! a helper that stalls on the engine's timeline and is killed never costs
//! the engine its device. Needs MoltenVK on a real GPU.

#![cfg(target_os = "macos")]

#[path = "support/surface_share_mach_test_pixels.rs"]
mod surface_share_mach_test_pixels;

use std::io::{BufRead as _, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;
use streamlib_engine::apple_surface_share::{
    CROSS_PROCESS_TIMELINE_WAIT_BOUND, ConsumerReleaseOutcome, CrossProcessTimelinePair,
    IOSurfaceShareState, MachSurfaceShareService, create_iosurface_mach_send_right,
    create_private_iosurface_with_packed_rows,
};
use streamlib_engine::core::rhi::PixelFormat;
use streamlib_engine::host_rhi::{
    HostVulkanDevice, HostVulkanTimelineSemaphore, RhiCommandRecorder,
};
use streamlib_surface_client::{
    SURFACE_SHARE_HAS_CONSUME_DONE_PORT, SURFACE_SHARE_HAS_PRODUCE_DONE_PORT,
    SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE, SurfaceShareMachServiceConnection,
};
use surface_share_mach_test_pixels::with_the_surface_bytes;

const HELPER_BINARY: &str = env!("CARGO_BIN_EXE_metal_shared_event_timeline_helper");

/// Generous: a helper's MoltenVK device is ~0.5 s cold, and its first
/// IOSurface call reads its executable's directory as the main bundle.
const HELPER_EVENT_BUDGET: Duration = Duration::from_secs(30);

const PING_PONG_ROUNDS: u64 = 500;
const HOST_SIDE_ROUNDS: u64 = 120;

/// An engine: its own Vulkan device, a running surface-share service, and one
/// surface registered with a timeline pair.
struct EngineWithOneSurfaceAndItsTimelinePair {
    device: Arc<HostVulkanDevice>,
    service: MachSurfaceShareService,
    iosurface: CFRetained<IOSurfaceRef>,
    pair: Arc<CrossProcessTimelinePair>,
    _registering_connection: SurfaceShareMachServiceConnection,
}

impl EngineWithOneSurfaceAndItsTimelinePair {
    /// `None` when this machine has no Vulkan device to run on.
    fn start(label: &str, surface_id: &str, timelines_export: bool) -> Option<Self> {
        let device = match HostVulkanDevice::new() {
            Ok(device) => device,
            Err(unavailable) => {
                tracing::warn!("skipping — no Vulkan device: {unavailable}");
                return None;
            }
        };
        let timeline = || {
            Arc::new(if timelines_export {
                HostVulkanTimelineSemaphore::new_exportable(device.device(), 0)
                    .expect("an exportable timeline")
            } else {
                HostVulkanTimelineSemaphore::new(device.device(), 0).expect("a local timeline")
            })
        };
        let pair = Arc::new(CrossProcessTimelinePair::new(timeline(), timeline()));

        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(
            state.clone(),
            format!(
                "com.tatolab.streamlib.timeline-test.{label}.{}",
                std::process::id()
            ),
        );
        service.start().expect("the service starts");
        state
            .cross_process_timeline_pairs()
            .insert(surface_id, Arc::clone(&pair));

        let iosurface = create_private_iosurface_with_packed_rows(16, 16, 4, PixelFormat::Bgra32)
            .expect("a private IOSurface");
        let mut ports =
            vec![create_iosurface_mach_send_right(&iosurface).expect("a port to the surface")];
        let carries_timeline_pair = pair.append_exported_send_rights_to(&mut ports);
        let registering_connection = SurfaceShareMachServiceConnection::connect(
            service.service_name(),
            Duration::from_secs(10),
        )
        .expect("this process connects to its own service");
        let (registered, _) = registering_connection
            .send_request_with_ports(
                &serde_json::json!({
                    "op": "register",
                    "surface_id": surface_id,
                    "runtime_id": "R-engine",
                    "width": 16,
                    "height": 16,
                    "format": "bgra32",
                    SURFACE_SHARE_HAS_PRODUCE_DONE_PORT: carries_timeline_pair,
                    SURFACE_SHARE_HAS_CONSUME_DONE_PORT: carries_timeline_pair,
                }),
                ports,
            )
            .expect("register round-trip");
        assert_eq!(registered, serde_json::json!({"success": true}));

        Some(Self {
            device,
            service,
            iosurface,
            pair,
            _registering_connection: registering_connection,
        })
    }

    /// Signal `produce_done` at `value` from the engine's GPU queue.
    fn produce_on_the_gpu(&self, recorder: &mut RhiCommandRecorder, value: u64) {
        recorder.begin().expect("begin");
        recorder
            .submit_signaling_timeline(self.pair.produce_done(), value)
            .expect("the engine's GPU signals produce_done");
    }
}

struct SpawnedHelperProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_lines: mpsc::Receiver<Option<String>>,
}

impl SpawnedHelperProcess {
    fn spawn(engine: &EngineWithOneSurfaceAndItsTimelinePair, arguments: &[&str]) -> Self {
        let mut child = Command::new(HELPER_BINARY)
            .args(arguments)
            .env(
                SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE,
                engine.service.service_name(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn the helper");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("the helper's stdout");
        let (line_sender, stdout_lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                let _ = line_sender.send(Some(line));
            }
            let _ = line_sender.send(None);
        });
        Self {
            child,
            stdin,
            stdout_lines,
        }
    }

    fn next_line(&self) -> String {
        self.stdout_lines
            .recv_timeout(HELPER_EVENT_BUDGET)
            .expect("the helper reported within the budget")
            .expect("the helper reported before exiting")
    }

    fn hand_off(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("the helper's stdin");
        writeln!(stdin, "{line}").expect("the helper takes the hand-off");
        stdin.flush().expect("flush the hand-off");
    }

    fn wait_for_exit(mut self) -> std::process::ExitStatus {
        let end_of_output = self
            .stdout_lines
            .recv_timeout(HELPER_EVENT_BUDGET)
            .expect("the helper closed its output within the budget");
        assert_eq!(
            end_of_output, None,
            "the helper reported more than expected"
        );
        self.child.wait().expect("reap the helper")
    }
}

/// Five hundred rounds, device-side on the helper's queue: the helper's GPU
/// waits for each `produce_done` value and signals the matching
/// `consume_done`; the engine's GPU signals `produce_done` and the engine
/// waits for the release host-side, bounded. Every round releases on time
/// and at exactly its own value, and both processes read the same counters.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn engine_and_helper_order_hundreds_of_frames_on_the_shared_events() {
    let Some(engine) = EngineWithOneSurfaceAndItsTimelinePair::start("ping-pong", "slot-pp", true)
    else {
        return;
    };
    let rounds = PING_PONG_ROUNDS.to_string();
    let helper = SpawnedHelperProcess::spawn(&engine, &["ping-pong", "slot-pp", &rounds]);
    let _admission = engine
        .service
        .rendezvous()
        .admit_helper_process(helper.child.id());
    assert_eq!(helper.next_line(), "IMPORTED produce_done=0 consume_done=0");

    let mut recorder = RhiCommandRecorder::new(&engine.device, "ping-pong").expect("a recorder");
    let mut mismatched_rounds = Vec::new();
    for round in 1..=PING_PONG_ROUNDS {
        engine.produce_on_the_gpu(&mut recorder, round);
        let outcome = engine
            .pair
            .wait_for_consumer_release(round)
            .expect("bounded wait");
        let released_at = engine.pair.consume_done().current_value().expect("counter");
        if outcome != ConsumerReleaseOutcome::Released || released_at != round {
            mismatched_rounds.push((round, outcome, released_at));
        }
    }

    assert_eq!(mismatched_rounds, Vec::new());
    assert_eq!(
        helper.next_line(),
        format!("DONE produce_done={PING_PONG_ROUNDS} consume_done={PING_PONG_ROUNDS}")
    );
    assert!(helper.wait_for_exit().success());
    assert!(!engine.pair.orders_host_side());
}

/// Timelines that will not export register without ports, and the same
/// frames order host-side: the engine completes production before each
/// hand-off, and the helper's release arrives as a message.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn frames_order_host_side_when_the_timeline_export_fails() {
    let Some(engine) = EngineWithOneSurfaceAndItsTimelinePair::start("host-side", "slot-hs", false)
    else {
        return;
    };
    assert!(engine.pair.orders_host_side());
    let rounds = HOST_SIDE_ROUNDS.to_string();
    let mut helper = SpawnedHelperProcess::spawn(&engine, &["host-side", "slot-hs", &rounds]);
    let _admission = engine
        .service
        .rendezvous()
        .admit_helper_process(helper.child.id());
    assert_eq!(helper.next_line(), "READY");

    let mut recorder = RhiCommandRecorder::new(&engine.device, "host-side").expect("a recorder");
    for round in 1..=HOST_SIDE_ROUNDS {
        with_the_surface_bytes(&engine.iosurface, |bytes| bytes[0] = (round % 256) as u8);
        engine.produce_on_the_gpu(&mut recorder, round);
        engine
            .pair
            .complete_production_before_hand_off(round)
            .expect("production completes before the hand-off");
        helper.hand_off(&format!("FRAME {round}"));
        assert_eq!(
            engine
                .pair
                .wait_for_consumer_release(round)
                .expect("bounded wait"),
            ConsumerReleaseOutcome::Released,
            "round {round}"
        );
    }

    assert_eq!(helper.next_line(), "DONE mismatches=0");
    assert!(helper.wait_for_exit().success());
}

/// A helper whose GPU waits on a `produce_done` value that never comes holds
/// the frame past the bound and is then killed. The engine is forced past it
/// within the bound, never waits on the helper device-side, and is still
/// producing on a live device after the ~5 s at which a device-side wait
/// would have lost it.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
fn a_stalled_then_killed_helper_leaves_the_engine_producing_on_a_live_device() {
    let Some(engine) = EngineWithOneSurfaceAndItsTimelinePair::start("stall", "slot-stall", true)
    else {
        return;
    };
    let mut helper = SpawnedHelperProcess::spawn(&engine, &["stall", "slot-stall"]);
    let _admission = engine
        .service
        .rendezvous()
        .admit_helper_process(helper.child.id());
    assert_eq!(helper.next_line(), "HOLDING");
    let stall_began = Instant::now();

    let mut recorder = RhiCommandRecorder::new(&engine.device, "stall").expect("a recorder");
    engine.produce_on_the_gpu(&mut recorder, 1);
    let started = Instant::now();
    assert_eq!(
        engine
            .pair
            .wait_for_consumer_release(1)
            .expect("bounded wait"),
        ConsumerReleaseOutcome::ForcedPastAStalledConsumer
    );
    assert!(started.elapsed() < CROSS_PROCESS_TIMELINE_WAIT_BOUND + Duration::from_secs(1));

    helper.child.kill().expect("kill the helper");
    helper.child.wait().expect("reap the helper");

    let mut round = 1;
    while stall_began.elapsed() < Duration::from_secs(7) {
        round += 1;
        engine.produce_on_the_gpu(&mut recorder, round);
        recorder
            .wait_for_completion()
            .expect("the engine's device is alive");
        std::thread::sleep(Duration::from_millis(16));
    }
    engine
        .pair
        .produce_done()
        .wait(round, 1_000_000_000)
        .expect("the engine is still producing");
    assert!(round > 2, "the engine kept producing after the kill");
}

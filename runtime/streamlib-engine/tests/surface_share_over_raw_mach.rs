// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-share service's raw-Mach arm across real processes.
//!
//! A spawned helper process receives an IOSurface from the engine over raw
//! Mach — no launchd plist, no bundle — and reads and edits its pixels; a
//! process nobody admitted is refused; a helper's death releases what it
//! held, and the engine's death makes a helper tear down. No GPU: IOSurface
//! and Mach need none, so this runs on the macOS CI runner.

#![cfg(target_os = "macos")]

#[path = "support/surface_share_mach_test_pixels.rs"]
mod surface_share_mach_test_pixels;

use std::io::BufRead as _;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;
use streamlib_engine::apple_surface_share::{
    IOSurfaceShareState, MachSurfaceShareService, create_iosurface_mach_send_right,
    create_private_iosurface_with_packed_rows,
};
use streamlib_engine::core::rhi::PixelFormat;
use streamlib_surface_client::{
    SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE, SurfaceShareMachServiceConnection,
};
use surface_share_mach_test_pixels::{engine_pattern_byte, with_the_surface_bytes};

const HELPER_BINARY: &str = env!("CARGO_BIN_EXE_surface_share_mach_helper");

/// Generous: a helper's first IOSurface call reads its executable's directory
/// as the main bundle, which is slow in a build tree.
const HELPER_EVENT_BUDGET: Duration = Duration::from_secs(20);

fn a_unique_service_name(label: &str) -> String {
    format!(
        "com.tatolab.streamlib.surface-share-test.{label}.{}",
        std::process::id()
    )
}

/// A running service with one surface, written with the engine's pattern
/// and registered under `surface_id` by this process.
struct EngineWithOneSharedSurface {
    state: IOSurfaceShareState,
    service: MachSurfaceShareService,
    iosurface: CFRetained<IOSurfaceRef>,
    _registering_connection: SurfaceShareMachServiceConnection,
}

impl EngineWithOneSharedSurface {
    fn start(label: &str, surface_id: &str, unadmitted_connection_wait_budget: Duration) -> Self {
        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(state.clone(), a_unique_service_name(label))
            .with_unadmitted_connection_wait_budget(unadmitted_connection_wait_budget);
        service.start().expect("the service starts");

        let iosurface = create_private_iosurface_with_packed_rows(64, 32, 4, PixelFormat::Bgra32)
            .expect("a private IOSurface");
        with_the_surface_bytes(&iosurface, |bytes| {
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = engine_pattern_byte(index);
            }
        });
        let registering_connection = SurfaceShareMachServiceConnection::connect(
            service.service_name(),
            Duration::from_secs(10),
        )
        .expect("this process connects to its own service");
        let iosurface_port =
            create_iosurface_mach_send_right(&iosurface).expect("a port to the surface");
        let (registered, _) = registering_connection
            .send_request_with_ports(
                &serde_json::json!({
                    "op": "register",
                    "surface_id": surface_id,
                    "runtime_id": "R-engine",
                    "width": 64,
                    "height": 32,
                    "format": "bgra32",
                }),
                vec![iosurface_port],
            )
            .expect("register round-trip");
        assert_eq!(registered, serde_json::json!({"success": true}));
        Self {
            state,
            service,
            iosurface,
            _registering_connection: registering_connection,
        }
    }

    fn outstanding_check_outs_of(&self, surface_id: &str) -> u32 {
        self.state
            .check_out_leases()
            .outstanding_check_out_count(surface_id)
            .expect("the lease table reads")
    }
}

/// A helper process whose stdout lines arrive on a channel, `None` marking
/// the end of its output — which is when every process holding the pipe
/// has exited.
struct SpawnedHelperProcess {
    child: Child,
    stdout_lines: mpsc::Receiver<Option<String>>,
}

impl SpawnedHelperProcess {
    /// Run the helper binary with `arguments`, handing it `service_name`
    /// when there is one to connect to.
    fn spawn(service_name: Option<&str>, arguments: &[&str]) -> Self {
        let mut command = Command::new(HELPER_BINARY);
        command.args(arguments).stdout(Stdio::piped());
        if let Some(service_name) = service_name {
            command.env(
                SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE,
                service_name,
            );
        }
        let mut child = command.spawn().expect("spawn the helper");
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
            stdout_lines,
        }
    }

    fn next_line(&self) -> Option<String> {
        self.stdout_lines
            .recv_timeout(HELPER_EVENT_BUDGET)
            .expect("the helper reported within the budget")
    }

    fn wait_for_exit(mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + HELPER_EVENT_BUDGET;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the helper did not exit within the budget"
            );
            std::thread::yield_now();
        }
    }
}

fn settles_within(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::yield_now();
    }
    condition()
}

#[test]
fn an_admitted_helper_reads_the_pixels_the_engine_wrote_and_its_edit_lands() {
    let engine =
        EngineWithOneSharedSurface::start("round-trip", "slot-round-trip", Duration::from_secs(5));
    let helper = SpawnedHelperProcess::spawn(
        Some(engine.service.service_name()),
        &["read-and-edit", "slot-round-trip"],
    );
    let _admission = engine
        .service
        .rendezvous()
        .admit_helper_process(helper.child.id());

    let report = helper.next_line();
    let status = helper.wait_for_exit();
    assert!(status.success(), "the helper failed: {report:?}");
    assert_eq!(report.as_deref(), Some("EDITED 8192 bytes"));

    let first_byte_the_helper_did_not_invert = with_the_surface_bytes(&engine.iosurface, |bytes| {
        bytes
            .iter()
            .enumerate()
            .position(|(index, byte)| *byte != engine_pattern_byte(index) ^ 0xFF)
    });
    assert_eq!(
        first_byte_the_helper_did_not_invert, None,
        "the helper's edit is visible in the engine's surface"
    );
    assert!(!engine.iosurface.is_in_use());
    assert_eq!(engine.outstanding_check_outs_of("slot-round-trip"), 0);
}

#[test]
fn a_helper_nobody_admitted_is_refused_and_given_no_port() {
    let engine =
        EngineWithOneSharedSurface::start("impostor", "slot-impostor", Duration::from_millis(300));
    let impostor = SpawnedHelperProcess::spawn(
        Some(engine.service.service_name()),
        &["read-and-edit", "slot-impostor"],
    );

    let report = impostor.next_line().unwrap_or_default();
    let status = impostor.wait_for_exit();
    assert_eq!(status.code(), Some(3), "the impostor was refused: {report}");
    assert!(
        report.contains("neither this process nor a helper process it admitted"),
        "{report}"
    );
    assert!(
        !engine.iosurface.is_in_use(),
        "no port to the surface ever left"
    );
    assert_eq!(engine.outstanding_check_outs_of("slot-impostor"), 0);
    with_the_surface_bytes(&engine.iosurface, |bytes| {
        assert!(
            bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == engine_pattern_byte(index)),
            "the surface is untouched"
        );
    });
}

#[test]
fn killing_a_helper_that_holds_a_surface_releases_it_and_its_registrations() {
    let engine =
        EngineWithOneSharedSurface::start("helper-death", "slot-held", Duration::from_secs(5));
    let mut helper =
        SpawnedHelperProcess::spawn(Some(engine.service.service_name()), &["hold", "slot-held"]);
    let _admission = engine
        .service
        .rendezvous()
        .admit_helper_process(helper.child.id());

    assert_eq!(helper.next_line().as_deref(), Some("HOLDING"));
    assert!(
        engine.iosurface.is_in_use(),
        "the helper's use count holds the surface"
    );
    assert_eq!(engine.outstanding_check_outs_of("slot-held"), 1);
    assert!(
        engine
            .state
            .surface_ids()
            .contains(&"helper-own-surface".to_string())
    );

    helper.child.kill().expect("SIGKILL the helper");
    helper.wait_for_exit();

    // The kernel tears the dead task's IOSurface client down asynchronously:
    // on a loaded machine the use count clears a few hundred microseconds
    // after the reap rather than by it.
    assert!(
        settles_within(Duration::from_secs(1), || !engine.iosurface.is_in_use()),
        "the kernel released the dead helper's use count promptly after it was reaped"
    );
    assert!(
        settles_within(HELPER_EVENT_BUDGET, || {
            engine.outstanding_check_outs_of("slot-held") == 0
        }),
        "the dead helper's checkout lease was released"
    );
    assert!(
        settles_within(HELPER_EVENT_BUDGET, || {
            !engine
                .state
                .surface_ids()
                .contains(&"helper-own-surface".to_string())
        }),
        "the dead helper's own registration was released"
    );
    assert!(
        engine
            .state
            .surface_ids()
            .contains(&"slot-held".to_string())
    );
}

#[test]
fn killing_the_engine_makes_its_helper_tear_down() {
    let mut engine = SpawnedHelperProcess::spawn(None, &["engine"]);

    let child_report = engine.next_line().unwrap_or_default();
    assert!(child_report.starts_with("CHILD "), "{child_report}");
    assert_eq!(engine.next_line().as_deref(), Some("HOLDING"));

    engine.child.kill().expect("SIGKILL the engine");
    engine.child.wait().expect("reap the engine");

    assert_eq!(
        engine.next_line().as_deref(),
        Some("TORE DOWN"),
        "the helper saw the engine die and released its surface"
    );
    assert_eq!(
        engine.next_line(),
        None,
        "the helper exited, closing the last end of the pipe"
    );
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Helper-process fixture for the cross-process timeline integration test.
//!
//! One role per run, named by the first argument, the surface id second:
//!
//! - `ping-pong <surface_id> <rounds>` imports the surface's timeline pair
//!   and queues, device-side, "wait for `produce_done` to reach r, then
//!   signal `consume_done` to r" for every round up front.
//! - `host-side <surface_id> <rounds>` checks out a surface registered with
//!   no timeline ports; for each `FRAME r` line on stdin it reads the frame's
//!   last byte and reports its release with `signal_consume_done`.
//! - `stall <surface_id>` imports the pair, queues a device-side wait for a
//!   `produce_done` value the engine never signals, and waits to be killed.
//!
//! Reports to the parent on stdout, one line per event.

#![allow(clippy::disallowed_macros)]

/// The Mach channel and the Metal shared event are macOS-only, so elsewhere
/// the binary exists to keep the target compiling and nothing drives it.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let surface_id = arguments.get(2).map(String::as_str).unwrap_or_default();
    let rounds = arguments
        .get(3)
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(0);
    match arguments.get(1).map(String::as_str) {
        Some("ping-pong") => timeline_helper::ping_pong(surface_id, rounds),
        Some("host-side") => timeline_helper::host_side(surface_id, rounds),
        Some("stall") => timeline_helper::stall(surface_id),
        other => {
            eprintln!("metal_shared_event_timeline_helper: unknown role {other:?}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "macos")]
mod timeline_helper {
    use std::io::{BufRead as _, Write as _};
    use std::sync::Arc;
    use std::time::Duration;

    use objc2_io_surface::IOSurfaceRef;
    use streamlib_consumer_rhi::{ConsumerVulkanDevice, ConsumerVulkanTimelineSemaphore};
    use streamlib_surface_client::{
        OwnedMachSendRight, SURFACE_SHARE_HAS_PRODUCE_DONE_PORT,
        SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE, SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE,
        SurfaceShareMachServiceConnection,
    };

    const IMPORTED_WAIT_BUDGET_NS: u64 = 20_000_000_000;

    fn report(line: &str) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }

    fn fail(line: &str) -> ! {
        report(line);
        std::process::exit(3);
    }

    fn connect() -> SurfaceShareMachServiceConnection {
        let service_name = std::env::var(SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE)
            .expect("the service name in the environment");
        SurfaceShareMachServiceConnection::connect(&service_name, Duration::from_secs(10))
            .unwrap_or_else(|failure| fail(&format!("CONNECT_FAILED {failure}")))
    }

    fn check_out(
        connection: &SurfaceShareMachServiceConnection,
        surface_id: &str,
    ) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
        let (answer, ports) = connection
            .send_request_with_ports(
                &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
                Vec::new(),
            )
            .expect("check_out round-trip");
        if let Some(refusal) = answer.get("error") {
            fail(&format!("CHECK_OUT_REFUSED {refusal}"));
        }
        (answer, ports)
    }

    /// The surface's `produce_done` and `consume_done`, imported into this
    /// process's own Vulkan device.
    fn import_timeline_pair(
        surface_id: &str,
    ) -> (
        SurfaceShareMachServiceConnection,
        ConsumerVulkanTimelineSemaphore,
        ConsumerVulkanTimelineSemaphore,
    ) {
        let connection = connect();
        let (answer, ports) = check_out(&connection, surface_id);
        if answer[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] != true || ports.len() != 3 {
            fail(&format!("NO_TIMELINE_PORTS {answer}"));
        }
        let device = Arc::new(
            ConsumerVulkanDevice::new()
                .unwrap_or_else(|failure| fail(&format!("NO_DEVICE {failure}"))),
        );
        let import = |port: &OwnedMachSendRight| {
            ConsumerVulkanTimelineSemaphore::from_imported_metal_shared_event_mach_send_right(
                &device, port,
            )
            .unwrap_or_else(|failure| fail(&format!("IMPORT_FAILED {failure}")))
        };
        let produce_done = import(&ports[1]);
        let consume_done = import(&ports[2]);
        (connection, produce_done, consume_done)
    }

    pub fn ping_pong(surface_id: &str, rounds: u64) {
        let (_connection, produce_done, consume_done) = import_timeline_pair(surface_id);
        report(&format!(
            "IMPORTED produce_done={} consume_done={}",
            produce_done.current_value().expect("counter"),
            consume_done.current_value().expect("counter")
        ));
        for round in 1..=rounds {
            produce_done
                .submit_device_wait_then_signal(round, &consume_done, round)
                .unwrap_or_else(|failure| fail(&format!("SUBMIT_FAILED {failure}")));
        }
        consume_done
            .wait(rounds, IMPORTED_WAIT_BUDGET_NS)
            .unwrap_or_else(|failure| fail(&format!("WAIT_FAILED {failure}")));
        report(&format!(
            "DONE produce_done={} consume_done={}",
            produce_done.current_value().expect("counter"),
            consume_done.current_value().expect("counter")
        ));
    }

    pub fn host_side(surface_id: &str, rounds: u64) {
        let connection = connect();
        let (answer, ports) = check_out(&connection, surface_id);
        if answer[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] != false || ports.len() != 1 {
            fail(&format!("UNEXPECTED_TIMELINE_PORTS {answer}"));
        }
        let iosurface = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
            .expect("the port names an IOSurface");
        report("READY");

        let mut mismatches = 0u64;
        let mut stdin_lines = std::io::stdin().lock().lines();
        for round in 1..=rounds {
            let line = stdin_lines
                .next()
                .and_then(Result::ok)
                .unwrap_or_else(|| fail("STDIN_CLOSED"));
            let handed_off: u64 = line
                .strip_prefix("FRAME ")
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| fail(&format!("BAD_LINE {line}")));
            let last_byte_offset = iosurface.bytes_per_row() * iosurface.height() - 1;
            // SAFETY: an offset inside the surface's packed rows; the engine
            // writes the next frame only after this release.
            let last_byte = unsafe {
                *iosurface
                    .base_address()
                    .as_ptr()
                    .cast::<u8>()
                    .add(last_byte_offset)
            };
            if handed_off != round || u64::from(last_byte) != round % 256 {
                mismatches += 1;
            }
            let (released, _) = connection
                .send_request_with_ports(
                    &serde_json::json!({
                        "op": SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE,
                        "surface_id": surface_id,
                        "value": round,
                    }),
                    Vec::new(),
                )
                .expect("signal_consume_done round-trip");
            if released.get("error").is_some() {
                fail(&format!("RELEASE_REFUSED {released}"));
            }
        }
        report(&format!("DONE mismatches={mismatches}"));
    }

    pub fn stall(surface_id: &str) {
        let (_connection, produce_done, consume_done) = import_timeline_pair(surface_id);
        produce_done
            .submit_device_wait_then_signal(u64::MAX / 2, &consume_done, 1)
            .unwrap_or_else(|failure| fail(&format!("SUBMIT_FAILED {failure}")));
        report("HOLDING");
        loop {
            std::thread::park();
        }
    }
}

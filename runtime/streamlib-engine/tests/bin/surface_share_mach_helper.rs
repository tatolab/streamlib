// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Helper-process fixture for the raw-Mach surface-share integration test.
//!
//! One role per run, named by the first argument:
//!
//! - `read-and-edit <surface_id>` checks the surface out, verifies the
//!   engine's pattern byte for byte, inverts every byte, and exits.
//! - `hold <surface_id>` checks the surface out, raises its use count,
//!   registers a surface of its own, and waits to be killed.
//! - `hold-until-the-engine-dies <surface_id>` checks the surface out, raises
//!   its use count, and tears down once the engine's service goes away.
//! - `engine` runs a surface-share service with one registered surface and a
//!   `hold-until-the-engine-dies` child, and waits to be killed.
//!
//! Reports to the parent on stdout, one line per event.

#![allow(clippy::disallowed_macros)]

/// The service this fixture talks to is macOS-only, so elsewhere the binary
/// exists to keep the target compiling and nothing drives it.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let surface_id = arguments.get(2).map(String::as_str).unwrap_or_default();
    match arguments.get(1).map(String::as_str) {
        Some("read-and-edit") => mach_helper::read_and_edit(surface_id),
        Some("hold") => mach_helper::hold(surface_id),
        Some("hold-until-the-engine-dies") => mach_helper::hold_until_the_engine_dies(surface_id),
        Some("engine") => mach_helper::engine(),
        other => {
            eprintln!("surface_share_mach_helper: unknown role {other:?}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "macos")]
#[path = "../support/surface_share_mach_test_pixels.rs"]
mod surface_share_mach_test_pixels;

#[cfg(target_os = "macos")]
mod mach_helper {
    use std::io::Write as _;
    use std::time::Duration;

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

    use super::surface_share_mach_test_pixels::{engine_pattern_byte, with_the_surface_bytes};

    fn report(line: &str) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }

    fn connect_or_exit() -> SurfaceShareMachServiceConnection {
        let service_name = std::env::var(SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE)
            .expect("the service name in the environment");
        match SurfaceShareMachServiceConnection::connect(&service_name, Duration::from_secs(10)) {
            Ok(connection) => connection,
            Err(refusal) if refusal.kind() == std::io::ErrorKind::PermissionDenied => {
                report(&format!("REFUSED {refusal}"));
                std::process::exit(3);
            }
            Err(failure) => {
                report(&format!("CONNECT_FAILED {failure}"));
                std::process::exit(4);
            }
        }
    }

    fn check_out_or_exit(
        connection: &SurfaceShareMachServiceConnection,
        surface_id: &str,
    ) -> CFRetained<IOSurfaceRef> {
        let (answer, ports) = connection
            .send_request_with_ports(
                &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
                Vec::new(),
            )
            .expect("check_out round-trip");
        if let Some(refusal) = answer.get("error") {
            report(&format!("CHECK_OUT_REFUSED {refusal}"));
            std::process::exit(5);
        }
        let iosurface_port = ports.into_iter().next().expect("one IOSurface port");
        IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
            .expect("the port names an IOSurface")
    }

    pub fn read_and_edit(surface_id: &str) {
        let connection = connect_or_exit();
        let iosurface = check_out_or_exit(&connection, surface_id);
        let first_mismatch = with_the_surface_bytes(&iosurface, |bytes| {
            let first_mismatch = bytes
                .iter()
                .enumerate()
                .position(|(index, byte)| *byte != engine_pattern_byte(index));
            if first_mismatch.is_none() {
                for byte in bytes.iter_mut() {
                    *byte ^= 0xFF;
                }
            }
            first_mismatch
        });
        if let Some(index) = first_mismatch {
            report(&format!("MISMATCH at byte {index}"));
            std::process::exit(6);
        }
        connection
            .send_request_with_ports(
                &serde_json::json!({"op": "release_check_out", "surface_id": surface_id}),
                Vec::new(),
            )
            .expect("release_check_out round-trip");
        report(&format!(
            "EDITED {} bytes",
            iosurface.bytes_per_row() * iosurface.height()
        ));
    }

    pub fn hold(surface_id: &str) {
        let connection = connect_or_exit();
        let held = check_out_or_exit(&connection, surface_id);
        held.increment_use_count();

        let own_surface = create_private_iosurface_with_packed_rows(8, 8, 4, PixelFormat::Bgra32)
            .expect("the helper's own surface");
        let own_surface_port =
            create_iosurface_mach_send_right(&own_surface).expect("a port to the surface");
        let (registered, _) = connection
            .send_request_with_ports(
                &serde_json::json!({
                    "op": "register",
                    "surface_id": "helper-own-surface",
                    "runtime_id": "R-helper",
                    "width": 8,
                    "height": 8,
                    "format": "bgra32",
                }),
                vec![own_surface_port],
            )
            .expect("register round-trip");
        assert_eq!(registered, serde_json::json!({"success": true}));

        report("HOLDING");
        loop {
            std::thread::park();
        }
    }

    pub fn hold_until_the_engine_dies(surface_id: &str) {
        let connection = connect_or_exit();
        let held = check_out_or_exit(&connection, surface_id);
        held.increment_use_count();
        report("HOLDING");

        let the_engine_went_away = connection
            .wait_for_the_service_to_go_away(None)
            .expect("wait on the service");
        assert!(the_engine_went_away);
        held.decrement_use_count();
        drop(held);
        drop(connection);
        report("TORE DOWN");
    }

    pub fn engine() {
        let service_name = format!(
            "com.tatolab.streamlib.surface-share-test.engine.{}",
            std::process::id()
        );
        let mut service = MachSurfaceShareService::new(IOSurfaceShareState::new(), service_name);
        service.start().expect("the engine's service starts");
        let rendezvous = service.rendezvous();

        let iosurface = create_private_iosurface_with_packed_rows(16, 16, 4, PixelFormat::Bgra32)
            .expect("the engine's surface");
        let connection = SurfaceShareMachServiceConnection::connect(
            rendezvous.service_name(),
            Duration::from_secs(10),
        )
        .expect("the engine connects to its own service");
        let iosurface_port =
            create_iosurface_mach_send_right(&iosurface).expect("a port to the surface");
        connection
            .send_request_with_ports(
                &serde_json::json!({
                    "op": "register",
                    "surface_id": "slot-engine",
                    "runtime_id": "R-engine",
                }),
                vec![iosurface_port],
            )
            .expect("register round-trip");

        let mut child = std::process::Command::new(std::env::current_exe().expect("this binary"))
            .args(["hold-until-the-engine-dies", "slot-engine"])
            .env(
                SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE,
                rendezvous.service_name(),
            )
            .spawn()
            .expect("spawn the helper");
        let _admission = rendezvous.admit_helper_process(child.id());
        report(&format!("CHILD {}", child.id()));
        let _ = child.wait();
        loop {
            std::thread::park();
        }
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One runtime on a mesh, in its own OS process, for
//! `runtime_mesh_two_processes`.
//!
//! The mesh is between processes by construction, so its proof needs a second
//! one. This binary constructs a `Runner` — no GPU, no `start()` — and reports
//! `graph`'s `mesh` object as one JSON line at a time, so the test reads what a
//! peer actually sees rather than inferring it.
//!
//! Closing its stdin is how the test asks for a clean leave: the runtime stops,
//! which undeclares its token before closing the session. A test that wants an
//! abrupt exit kills it instead.
//!
//! The report goes down a **duplicate of fd 1 taken before the runtime exists**,
//! because a constructed runtime installs the fd-level stdio interceptor and
//! every later `println!` would land in the log pipeline rather than reaching
//! the parent.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use streamlib_engine::core::runtime::{Runner, RuntimeMeshConfiguration};

/// How often the peer reports what it currently sees.
const HOW_OFTEN_THE_PEER_REPORTS: std::time::Duration = std::time::Duration::from_millis(100);

/// What the peer writes once its runtime is constructed, whatever its mesh
/// session did — so a test waiting on a local-only runtime is not left waiting.
const READY_LINE: &str = "READY";

/// What the peer writes instead when the runtime refused to be constructed.
const REFUSED_LINE_PREFIX: &str = "REFUSED ";

fn main() {
    let report = ReportChannelTakenBeforeTheRuntimeExists::take();
    let configuration = read_the_mesh_configuration_from_the_command_line();

    let runtime = match Runner::new_with_runtime_mesh_configuration(configuration) {
        Ok(runtime) => runtime,
        Err(construction_failure) => {
            report.write_line(&format!("{REFUSED_LINE_PREFIX}{construction_failure}"));
            std::process::exit(2);
        }
    };

    let asked_to_leave = Arc::new(AtomicBool::new(false));
    read_stdin_until_it_closes(Arc::clone(&asked_to_leave));

    report.write_line(READY_LINE);
    while !asked_to_leave.load(Ordering::Relaxed) {
        let graph = runtime.to_json().expect("the graph renders");
        report.write_line(&graph["mesh"].to_string());
        std::thread::sleep(HOW_OFTEN_THE_PEER_REPORTS);
    }

    runtime.stop().expect("the runtime stops");
}

/// A duplicate of fd 1, taken before anything replaces fd 1 itself.
struct ReportChannelTakenBeforeTheRuntimeExists(std::os::fd::OwnedFd);

impl ReportChannelTakenBeforeTheRuntimeExists {
    fn take() -> Self {
        // Close-on-exec at birth rather than afterwards with a second `fcntl`:
        // a spawn on another thread can land between the two calls, and this
        // descriptor is a duplicate of the app's own stdout.
        //
        // SAFETY: fd 1 is open at process start, and the duplicate is checked
        // before anything adopts it.
        let duplicated = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(duplicated >= 0, "this process has no stdout to report on");
        // SAFETY: `duplicated` is a fresh descriptor nothing else owns.
        Self(unsafe { std::os::fd::FromRawFd::from_raw_fd(duplicated) })
    }

    fn write_line(&self, line: &str) {
        use std::io::Write as _;
        let mut channel = std::fs::File::from(self.0.try_clone().expect("the report channel"));
        let _ = writeln!(channel, "{line}");
        let _ = channel.flush();
    }
}

/// The five values, spelled as flags so the test drives exactly the doors an
/// author has.
fn read_the_mesh_configuration_from_the_command_line() -> RuntimeMeshConfiguration {
    let mut configuration = RuntimeMeshConfiguration::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let mut value = || arguments.next().expect("every flag takes a value");
        match flag.as_str() {
            "--runtime-name" => configuration.runtime_name = Some(value()),
            "--mesh-name" => configuration.mesh_name = Some(value()),
            "--mesh-peer" => configuration
                .mesh_peer_endpoints
                .get_or_insert_with(Vec::new)
                .push(value()),
            "--mesh-listen" => configuration
                .mesh_listen_endpoints
                .get_or_insert_with(Vec::new)
                .push(value()),
            "--multicast-discovery" => {
                configuration.mesh_multicast_discovery = Some(value() == "on")
            }
            unknown => panic!("unknown flag {unknown:?}"),
        }
    }
    configuration
}

/// Watch stdin on its own thread: the parent closing it is the ask to leave.
fn read_stdin_until_it_closes(asked_to_leave: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().lock().read_line(&mut line).unwrap_or(0) > 0 {
            line.clear();
        }
        asked_to_leave.store(true, Ordering::Relaxed);
    });
}

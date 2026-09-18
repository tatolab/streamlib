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
//! With `--observe-only` it constructs no runtime at all and instead reads the
//! mesh the way `streamlib nodes` does, reporting what it saw and exiting. That
//! lives here rather than in the test process because the multicast interface
//! is pinned through the environment, and a test binary cannot set its own
//! environment without racing every other thread reading it.
//!
//! The report goes down a **duplicate of fd 1 taken before the runtime exists**,
//! because a constructed runtime installs the fd-level stdio interceptor and
//! every later `println!` would land in the log pipeline rather than reaching
//! the parent.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use streamlib_engine::core::runtime::{
    Runner, RuntimeMeshConfiguration, RuntimeMeshObservationRequest, observe_a_runtime_mesh,
};

/// How often the peer reports what it currently sees.
const HOW_OFTEN_THE_PEER_REPORTS: std::time::Duration = std::time::Duration::from_millis(100);

/// What the peer writes once its runtime is constructed, whatever its mesh
/// session did — so a test waiting on a local-only runtime is not left waiting.
const READY_LINE: &str = "READY";

/// What the peer writes instead when the runtime refused to be constructed.
const REFUSED_LINE_PREFIX: &str = "REFUSED ";

/// Read the mesh rather than joining it, and report what one look saw.
const OBSERVE_ONLY_FLAG: &str = "--observe-only";

fn main() {
    let report = ReportChannelTakenBeforeTheRuntimeExists::take();
    let configuration = read_the_mesh_configuration_from_the_command_line();

    if std::env::args().any(|argument| argument == OBSERVE_ONLY_FLAG) {
        report_what_one_look_at_the_mesh_saw(&report, configuration);
        return;
    }

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

/// Look at the mesh without joining it, and report the runtime names one look
/// saw as a JSON array — the same shape the peer reports its own peers in, so
/// the test reads both the same way.
fn report_what_one_look_at_the_mesh_saw(
    report: &ReportChannelTakenBeforeTheRuntimeExists,
    configuration: RuntimeMeshConfiguration,
) {
    report.write_line(READY_LINE);
    let looked = observe_a_runtime_mesh(RuntimeMeshObservationRequest {
        mesh_name: configuration.mesh_name,
        mesh_peer_endpoints: configuration.mesh_peer_endpoints,
        mesh_multicast_discovery: configuration.mesh_multicast_discovery,
    });
    match looked {
        Ok(looked) => report.write_line(&serde_json::json!({ "peers": looked.peers }).to_string()),
        Err(look_failure) => {
            report.write_line(&format!("{REFUSED_LINE_PREFIX}{look_failure}"));
            std::process::exit(2);
        }
    }
}

/// A duplicate of fd 1, taken before anything replaces fd 1 itself.
struct ReportChannelTakenBeforeTheRuntimeExists(std::fs::File);

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
        // Through `&File`, which is itself a writer, so a line costs no `dup`.
        let mut channel = &self.0;
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
            OBSERVE_ONLY_FLAG => {}
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

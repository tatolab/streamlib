// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One runtime that asks for, or answers, a link request — for
//! `cross_runtime_link_requests_two_processes`.
//!
//! A link request is between runtimes by construction, and a third-party
//! wiring needs three, so its proof needs its own processes. Unlike the
//! pull fixture's peer this one builds a real `Runner` — a request is applied
//! through `connect` itself, so there has to be a graph to apply it into. It
//! never calls `start()`: that needs a GPU and CI has none, and a request is
//! applied against a graph rather than against running processors.
//!
//! It is driven over stdin, one JSON command per line, and reports one JSON
//! line per command plus `graph` on a tick — so an arm asserts on what a
//! runtime actually renders rather than on what it inferred.
//!
//! Closing its stdin is how the test asks for a clean leave.
//!
//! The report goes down a **duplicate of fd 1 taken before the runtime
//! exists**, because a constructed runtime installs the fd-level stdio
//! interceptor and every later write to fd 1 would land in the log pipeline
//! rather than reaching the parent.

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use streamlib_engine::core::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib_engine::core::graph::{
    LinkRequestUniqueId, LinkUniqueId, MeshPortAddress, OutputLinkPortRef, ProcessorUniqueId,
};
use streamlib_engine::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib_engine::core::runtime::{Runner, RuntimeMeshConfiguration, RuntimeOperations};

/// What the peer writes once its runtime is constructed.
const READY_LINE: &str = "READY";

/// What the peer writes instead when the runtime refused to be constructed.
const REFUSED_LINE_PREFIX: &str = "REFUSED ";

/// How often the peer reports its own `graph`.
const HOW_OFTEN_THE_PEER_REPORTS: std::time::Duration = std::time::Duration::from_millis(100);

/// The one output port every processor this peer adds publishes.
const THE_OUTPUT_PORT: &str = "video";

/// The one input port every processor this peer adds consumes.
const THE_INPUT_PORT: &str = "frames_from_upstream";

fn main() {
    let report = ReportChannelTakenBeforeTheRuntimeExists::take();
    let how = HowToRunThisPeer::read_from_the_command_line();
    register_the_one_processor_type_this_peer_adds();

    let runtime = match Runner::new_with_runtime_mesh_configuration(how.mesh.clone()) {
        Ok(runtime) => runtime,
        Err(construction_failure) => {
            report.write_line(&format!("{REFUSED_LINE_PREFIX}{construction_failure}"));
            std::process::exit(2);
        }
    };

    let asked_to_leave = Arc::new(AtomicBool::new(false));
    read_every_command_until_stdin_closes(
        Arc::clone(&runtime),
        report.clone(),
        Arc::clone(&asked_to_leave),
    );

    report.write_line(READY_LINE);
    while !asked_to_leave.load(Ordering::Relaxed) {
        match runtime.to_json() {
            Ok(graph) => report.write_line(&graph.to_string()),
            Err(unreadable) => report.write_line(
                &serde_json::json!({ "graph_unreadable": unreadable.to_string() }).to_string(),
            ),
        }
        std::thread::sleep(HOW_OFTEN_THE_PEER_REPORTS);
    }

    runtime.stop().expect("the runtime stops");
}

/// Register the one descriptor-only type this peer adds nodes of.
///
/// Descriptor-only because `connect` checks a port exists and nothing more:
/// no instance is ever constructed, which is what keeps this GPU-free.
fn register_the_one_processor_type_this_peer_adds() {
    let import_path = ProcessorClassImportPath::new(the_class_path()).expect("a legal class path");
    let mut descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new("LinkRequestPeerProcessor").expect("a legal short name"),
        import_path,
        "a node for the link-request fixture to wire",
    );
    descriptor
        .outputs
        .push(PortDescriptor::new(THE_OUTPUT_PORT, "output", true));
    descriptor
        .inputs
        .push(PortDescriptor::new(THE_INPUT_PORT, "input", true).with_delivery_profile("newest"));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
}

fn the_class_path() -> String {
    "cross_runtime_link_request_peer:LinkRequestPeerProcessor".to_string()
}

/// One command the fixture sends, as it rides stdin.
#[derive(serde::Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum WhatTheFixtureAsked {
    /// Add a processor under this display name.
    Add { display_name: String },
    /// Ask another runtime to carry a port into one of its inputs. A source
    /// with no runtime name is one of this runtime's own, named by display
    /// name — which is the push case.
    RequestLink {
        #[serde(default)]
        from_runtime_name: Option<String>,
        from_display_name: String,
        to_runtime_name: String,
        to_display_name: String,
    },
    /// Ask another runtime to remove a link.
    RequestDisconnect {
        input_runtime_name: String,
        link_id: String,
    },
    /// Cancel a request no runtime has applied.
    CancelRequest { link_request_id: String },
}

/// Read commands off stdin on their own thread, answering each.
fn read_every_command_until_stdin_closes(
    runtime: Arc<Runner>,
    report: ReportChannelTakenBeforeTheRuntimeExists,
    asked_to_leave: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            let answered = match serde_json::from_str::<WhatTheFixtureAsked>(&line) {
                Ok(asked) => answer_one_command(&runtime, asked),
                Err(unreadable) => {
                    serde_json::json!({ "command_unreadable": unreadable.to_string() })
                }
            };
            report.write_line(&answered.to_string());
        }
        asked_to_leave.store(true, Ordering::Relaxed);
    });
}

/// Do what one command asks and say what came back.
fn answer_one_command(runtime: &Arc<Runner>, asked: WhatTheFixtureAsked) -> serde_json::Value {
    match asked {
        WhatTheFixtureAsked::Add { display_name } => {
            let mut spec = ProcessorSpec::new(
                ProcessorClassImportPath::new(the_class_path()).expect("a legal class path"),
                serde_json::Value::Null,
            );
            spec.display_name = Some(display_name.clone());
            match runtime.add_processor(spec) {
                Ok(processor_id) => serde_json::json!({
                    "added": display_name,
                    "processor_id": processor_id.as_str(),
                }),
                Err(refusal) => serde_json::json!({ "refused": refusal.to_string() }),
            }
        }
        WhatTheFixtureAsked::RequestLink {
            from_runtime_name,
            from_display_name,
            to_runtime_name,
            to_display_name,
        } => {
            let from = match from_runtime_name {
                // A source on another runtime: this peer is wiring two others.
                Some(runtime_name) => {
                    match MeshPortAddress::new(runtime_name, from_display_name, THE_OUTPUT_PORT) {
                        Ok(address) => OutputLinkPortRef::on_another_runtime(address),
                        Err(not_an_address) => {
                            return serde_json::json!({ "refused": not_an_address.to_string() });
                        }
                    }
                }
                // One of this runtime's own, named the way an author names it.
                None => match the_processor_this_runtime_displays_as(runtime, &from_display_name) {
                    Some(processor_id) => OutputLinkPortRef::new(processor_id, THE_OUTPUT_PORT),
                    None => {
                        return serde_json::json!({
                            "refused": format!("this runtime displays no {from_display_name}")
                        });
                    }
                },
            };
            let to = match MeshPortAddress::new(to_runtime_name, to_display_name, THE_INPUT_PORT) {
                Ok(address) => address,
                Err(not_an_address) => {
                    return serde_json::json!({ "refused": not_an_address.to_string() });
                }
            };
            match runtime.request_link_on_remote_input_runtime(from, to) {
                Ok(link_request_id) => {
                    serde_json::json!({ "link_request_id": link_request_id.as_str() })
                }
                Err(refusal) => serde_json::json!({ "refused": refusal.to_string() }),
            }
        }
        WhatTheFixtureAsked::RequestDisconnect {
            input_runtime_name,
            link_id,
        } => match runtime.request_disconnect_on_remote_input_runtime(
            input_runtime_name,
            LinkUniqueId::from(link_id),
        ) {
            Ok(link_request_id) => {
                serde_json::json!({ "link_request_id": link_request_id.as_str() })
            }
            Err(refusal) => serde_json::json!({ "refused": refusal.to_string() }),
        },
        WhatTheFixtureAsked::CancelRequest { link_request_id } => {
            match runtime.cancel_link_request(&LinkRequestUniqueId::from(link_request_id.as_str()))
            {
                Ok(()) => serde_json::json!({ "cancelled": link_request_id }),
                Err(refusal) => serde_json::json!({ "refused": refusal.to_string() }),
            }
        }
    }
}

/// The id of the node this runtime displays under `display_name`.
fn the_processor_this_runtime_displays_as(
    runtime: &Arc<Runner>,
    display_name: &str,
) -> Option<ProcessorUniqueId> {
    let graph = runtime.to_json().ok()?;
    graph["nodes"]
        .as_array()?
        .iter()
        .find(|node| node["display_name"].as_str() == Some(display_name))
        .and_then(|node| node["id"].as_str())
        .map(ProcessorUniqueId::from)
}

/// Every flag the fixture drives.
struct HowToRunThisPeer {
    mesh: RuntimeMeshConfiguration,
}

impl HowToRunThisPeer {
    fn read_from_the_command_line() -> Self {
        let mut mesh = RuntimeMeshConfiguration::default();
        let mut arguments = std::env::args().skip(1);
        while let Some(flag) = arguments.next() {
            let mut value = || arguments.next().expect("every flag takes a value");
            match flag.as_str() {
                "--runtime-name" => mesh.runtime_name = Some(value()),
                "--mesh-name" => mesh.mesh_name = Some(value()),
                "--mesh-peer" => mesh
                    .mesh_peer_endpoints
                    .get_or_insert_with(Vec::new)
                    .push(value()),
                "--mesh-listen" => mesh
                    .mesh_listen_endpoints
                    .get_or_insert_with(Vec::new)
                    .push(value()),
                "--multicast-discovery" => mesh.mesh_multicast_discovery = Some(value() == "on"),
                unknown => panic!("unknown flag {unknown:?}"),
            }
        }
        Self { mesh }
    }
}

/// A duplicate of fd 1, taken before anything replaces fd 1 itself.
#[derive(Clone)]
struct ReportChannelTakenBeforeTheRuntimeExists(Arc<std::fs::File>);

impl ReportChannelTakenBeforeTheRuntimeExists {
    fn take() -> Self {
        // SAFETY: fd 1 is open at process start, and the duplicate is checked
        // before anything adopts it.
        let duplicated = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(duplicated >= 0, "this process has no stdout to report on");
        // SAFETY: `duplicated` is a fresh descriptor nothing else owns.
        Self(Arc::new(unsafe {
            std::os::fd::FromRawFd::from_raw_fd(duplicated)
        }))
    }

    fn write_line(&self, line: &str) {
        use std::io::Write as _;
        let mut channel = &*self.0;
        let _ = writeln!(channel, "{line}");
        let _ = channel.flush();
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link requests between runtimes in separate OS processes — the proof that a
//! runtime can push its output into another runtime's input, that a third
//! runtime can wire two others, and that a request survives the runtime it
//! names being absent, silent, or unwilling.
//!
//! Most arms are two runtimes; the third-party and disconnect arms are three,
//! because "a runtime that is neither end wired them" cannot be checked with
//! two.
//!
//! GPU-free: every peer builds a `Runner` and never starts it. `start()` needs
//! a GPU and CI has none, and a link request is applied against a graph rather
//! than against running processors — which is exactly why `connect` applying it
//! is provable here at all.
//!
//! Serial, and each arm takes its own mesh name, its own runtime names, its own
//! loopback port and its own iceoryx2 domain. The arms at once would put a
//! dozen runtimes with real network endpoints on one loopback interface, which
//! tests the harness rather than the engine.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serial_test::serial;

/// How long an arm waits for something it is expecting. Generous: a scouting
/// delay, a description round trip and a request round trip on a loaded CI
/// runner.
const HOW_LONG_AN_ARM_WAITS: Duration = Duration::from_secs(60);

/// How long a peer asked to leave has to be gone.
const HOW_LONG_A_PEER_HAS_TO_LEAVE: Duration = Duration::from_secs(30);

/// The variable that pins multicast scouting, so a test never scouts on the
/// machine's real network.
const MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_MESH_MULTICAST_INTERFACE";

/// Where scouting is pinned to.
const LOOPBACK_INTERFACE: &str = "127.0.0.1";

/// The display name the sending runtime gives the processor it pushes from.
const THE_SOURCES_DISPLAY_NAME: &str = "CameraSource";

/// The display name the receiving runtime gives the processor pushed into.
const THE_DESTINATIONS_DISPLAY_NAME: &str = "DisplayWindow";

/// The input port every peer's processor declares.
const THE_INPUT_PORT: &str = "frames_from_upstream";

/// A mesh name no other arm and no other machine uses.
fn a_mesh_name_of_its_own(arm: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "xr-{arm}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A loopback port no other arm will be handed.
///
/// The TCP listener that found it is kept for the whole run rather than
/// released: a released ephemeral port is handed straight back out, so a later
/// arm can be given the port an earlier arm's peers are still on.
fn a_free_loopback_port() -> u16 {
    static PORTS_ALREADY_HANDED_OUT: Mutex<Vec<std::net::TcpListener>> = Mutex::new(Vec::new());
    let held =
        std::net::TcpListener::bind((LOOPBACK_INTERFACE, 0)).expect("the loopback has a free port");
    let port = held
        .local_addr()
        .expect("a bound listener has an address")
        .port();
    PORTS_ALREADY_HANDED_OUT.lock().push(held);
    port
}

/// A QUIC-over-UDP listen endpoint on a port of this arm's own.
fn a_listen_endpoint_of_its_own() -> String {
    format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port())
}

/// A runtime directory of this peer's own, so two arms never share an iceoryx2
/// domain, a node registry or a surface socket.
///
/// `peer` is kept to a few characters on purpose: the directory holds the
/// iceoryx2 domain, whose Unix socket paths leave the root a 63-byte budget
/// that the engine refuses past by name.
fn a_runtime_directory_of_its_own(peer: &str) -> tempfile::TempDir {
    assert!(
        peer.len() <= 12,
        "{peer:?} would push the iceoryx2 domain root past its 63-byte budget"
    );
    tempfile::Builder::new()
        .prefix(&format!("sl-{peer}-"))
        .tempdir_in("/tmp")
        .expect("a temporary runtime directory")
}

/// How a peer is launched.
struct HowToLaunchAPeer {
    runtime_name: String,
    mesh_name: String,
    listen_endpoints: Vec<String>,
    peer_endpoints: Vec<String>,
    runtime_directory: std::path::PathBuf,
}

/// One peer process, with everything it has reported.
struct LinkRequestPeerProcess {
    child: Child,
    reported: Arc<Mutex<Vec<serde_json::Value>>>,
    came_up: Arc<Mutex<bool>>,
    why_it_refused: Arc<Mutex<Option<String>>>,
}

impl LinkRequestPeerProcess {
    fn launch(how: HowToLaunchAPeer) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cross_runtime_link_request_peer"));
        command
            .arg("--runtime-name")
            .arg(&how.runtime_name)
            .arg("--mesh-name")
            .arg(&how.mesh_name)
            .arg("--multicast-discovery")
            .arg("off")
            .env(
                MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE,
                LOOPBACK_INTERFACE,
            )
            // The one knob the runtime directory takes, which is where the
            // registry, the surface socket and the iceoryx2 domain all land —
            // so two arms never share any of the three.
            .env("XDG_RUNTIME_DIR", &how.runtime_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for listen in &how.listen_endpoints {
            command.arg("--mesh-listen").arg(listen);
        }
        for peer in &how.peer_endpoints {
            command.arg("--mesh-peer").arg(peer);
        }

        let mut child = command.spawn().expect("the peer binary launches");
        let stdout = child.stdout.take().expect("the peer's stdout is piped");
        let reported = Arc::new(Mutex::new(Vec::new()));
        let came_up = Arc::new(Mutex::new(false));
        let why_it_refused = Arc::new(Mutex::new(None));

        let reported_by_the_reader = Arc::clone(&reported);
        let came_up_for_the_reader = Arc::clone(&came_up);
        let why_it_refused_for_the_reader = Arc::clone(&why_it_refused);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line == "READY" {
                    *came_up_for_the_reader.lock() = true;
                } else if let Some(why) = line.strip_prefix("REFUSED ") {
                    *why_it_refused_for_the_reader.lock() = Some(why.to_string());
                } else if let Ok(reported) = serde_json::from_str::<serde_json::Value>(&line) {
                    reported_by_the_reader.lock().push(reported);
                }
            }
        });

        Self {
            child,
            reported,
            came_up,
            why_it_refused,
        }
    }

    /// Wait until the peer says it is up, failing with whatever it refused for.
    fn wait_until_it_is_up(&self) {
        self.wait_until("the peer comes up", || {
            if let Some(why) = self.why_it_refused.lock().as_ref() {
                panic!("the peer refused to come up: {why}");
            }
            *self.came_up.lock()
        });
    }

    /// Wait until `what_to_wait_for` holds, or fail naming what was reported.
    fn wait_until(&self, described: &str, mut what_to_wait_for: impl FnMut() -> bool) {
        let gave_up_at = Instant::now() + HOW_LONG_AN_ARM_WAITS;
        while Instant::now() < gave_up_at {
            if what_to_wait_for() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for {described}; the peer last reported {:?}",
            self.the_graph_it_last_reported()
        );
    }

    /// Send one command down the peer's stdin.
    fn ask_it_to(&mut self, command: serde_json::Value) {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .expect("the peer's stdin is piped");
        writeln!(stdin, "{command}").expect("the command reaches the peer");
        stdin.flush().expect("the command is flushed");
    }

    /// Add a processor under `display_name` and wait for the peer to say so.
    fn add_a_processor_displayed_as(&mut self, display_name: &str) {
        self.ask_it_to(serde_json::json!({
            "command": "add",
            "display_name": display_name,
        }));
        self.wait_until(&format!("the peer to add {display_name}"), || {
            self.everything_it_has_reported().iter().any(|reported| {
                reported.get("added").and_then(|it| it.as_str()) == Some(display_name)
            })
        });
    }

    fn everything_it_has_reported(&self) -> Vec<serde_json::Value> {
        self.reported.lock().clone()
    }

    /// The last whole `graph` this peer reported.
    fn the_graph_it_last_reported(&self) -> serde_json::Value {
        self.everything_it_has_reported()
            .iter()
            .rev()
            .find(|reported| reported.get("links").is_some())
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }

    /// Every link this peer's own `graph` currently holds.
    fn the_links_it_last_reported(&self) -> Vec<serde_json::Value> {
        self.the_graph_it_last_reported()["links"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Every link request this peer is still waiting on.
    fn the_requests_it_is_waiting_on(&self) -> Vec<serde_json::Value> {
        self.the_graph_it_last_reported()["mesh"]["link_requests_awaiting_runtime"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The id of the last link request this peer reported making.
    fn the_last_link_request_it_made(&self) -> String {
        self.everything_it_has_reported()
            .iter()
            .rev()
            .find_map(|reported| Some(reported.get("link_request_id")?.as_str()?.to_string()))
            .expect("the peer reported a link request")
    }

    /// The id of the one link this peer's graph holds.
    fn the_one_link_it_holds(&self) -> String {
        let [link] = self
            .the_links_it_last_reported()
            .try_into()
            .unwrap_or_else(|links| panic!("expected exactly one link, got {links:?}"));
        link["id"].as_str().expect("a link has an id").to_string()
    }

    /// Ask the peer to leave cleanly, and wait for it to go.
    fn ask_it_to_leave(&mut self) {
        drop(self.child.stdin.take());
        let gave_up_at = Instant::now() + HOW_LONG_A_PEER_HAS_TO_LEAVE;
        while Instant::now() < gave_up_at {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(cannot_wait) => panic!("the peer could not be waited on: {cannot_wait}"),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        panic!("the peer did not finish leaving within {HOW_LONG_A_PEER_HAS_TO_LEAVE:?}");
    }
}

impl Drop for LinkRequestPeerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One runtime pushes its own output into another runtime's input, and the
/// link lands on the runtime that owns the input, naming the one that asked.
///
/// What it catches: a push applied on the sending runtime — which would leave a
/// link whose destination names no node there and nothing at all on the
/// receiver — and a `created_by_runtime_name` rendered from the renderer rather
/// than from the request.
#[test]
#[serial]
fn a_push_lands_on_the_runtime_that_owns_the_input_naming_the_runtime_that_asked() {
    let mesh_name = a_mesh_name_of_its_own("push");
    let receiving_endpoint = a_listen_endpoint_of_its_own();
    let receiving_runtime_directory = a_runtime_directory_of_its_own("push-recv");
    let sending_runtime_directory = a_runtime_directory_of_its_own("push-send");

    let receiving = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-push-receiver".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![receiving_endpoint.clone()],
        peer_endpoints: Vec::new(),
        runtime_directory: receiving_runtime_directory.path().to_path_buf(),
    });
    let mut receiving = receiving;
    receiving.wait_until_it_is_up();
    receiving.add_a_processor_displayed_as(THE_DESTINATIONS_DISPLAY_NAME);

    let mut sending = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-push-sender".to_string(),
        mesh_name,
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![receiving_endpoint],
        runtime_directory: sending_runtime_directory.path().to_path_buf(),
    });
    sending.wait_until_it_is_up();
    sending.add_a_processor_displayed_as(THE_SOURCES_DISPLAY_NAME);

    sending.ask_it_to(serde_json::json!({
        "command": "request_link",
        "from_display_name": THE_SOURCES_DISPLAY_NAME,
        "to_runtime_name": "xr-push-receiver",
        "to_display_name": THE_DESTINATIONS_DISPLAY_NAME,
    }));

    receiving.wait_until("the pushed link to land on the receiving runtime", || {
        !receiving.the_links_it_last_reported().is_empty()
    });

    let [link] = receiving
        .the_links_it_last_reported()
        .try_into()
        .unwrap_or_else(|links| panic!("expected exactly one link, got {links:?}"));
    assert_eq!(
        link["created_by_runtime_name"], "xr-push-sender",
        "the link must name the runtime that asked for it, not the one rendering it: {link}"
    );
    assert_eq!(
        link["source"],
        serde_json::json!({
            "runtime_name": "xr-push-sender",
            "processor_display_name": THE_SOURCES_DISPLAY_NAME,
            "port_name": "video",
        }),
        "the source is the address the request named: {link}"
    );
    assert_eq!(link["target"]["port_name"], THE_INPUT_PORT, "{link}");
    assert!(
        link["target"]["processor_id"].is_string(),
        "the destination resolved to one of this runtime's own nodes: {link}"
    );

    // The request left the sender the moment it was applied: its outcome is the
    // receiving runtime's to render now, on the link itself.
    sending.wait_until("the sender to stop holding the request", || {
        sending.the_requests_it_is_waiting_on().is_empty()
    });

    sending.ask_it_to_leave();
    receiving.ask_it_to_leave();
}

/// A third runtime wires two others, and the link names *it* — neither end.
///
/// What it catches: a `created_by_runtime_name` taken from the link's source
/// rather than from the request, which reads correctly for a push and names
/// the wrong runtime for every third-party wiring.
#[test]
#[serial]
fn a_third_runtime_wires_two_others_and_the_link_names_the_runtime_that_asked() {
    let mesh_name = a_mesh_name_of_its_own("thirdparty");
    let receiving_endpoint = a_listen_endpoint_of_its_own();
    let receiving_runtime_directory = a_runtime_directory_of_its_own("tp-recv");
    let sending_runtime_directory = a_runtime_directory_of_its_own("tp-send");
    let wiring_runtime_directory = a_runtime_directory_of_its_own("tp-agent");

    let mut receiving = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-tp-receiver".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![receiving_endpoint.clone()],
        peer_endpoints: Vec::new(),
        runtime_directory: receiving_runtime_directory.path().to_path_buf(),
    });
    receiving.wait_until_it_is_up();
    receiving.add_a_processor_displayed_as(THE_DESTINATIONS_DISPLAY_NAME);

    let mut sending = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-tp-sender".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![receiving_endpoint.clone()],
        runtime_directory: sending_runtime_directory.path().to_path_buf(),
    });
    sending.wait_until_it_is_up();
    sending.add_a_processor_displayed_as(THE_SOURCES_DISPLAY_NAME);

    // The agent adds no processor at all: it is neither end of the link.
    let mut wiring = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-tp-agent".to_string(),
        mesh_name,
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![receiving_endpoint],
        runtime_directory: wiring_runtime_directory.path().to_path_buf(),
    });
    wiring.wait_until_it_is_up();

    wiring.ask_it_to(serde_json::json!({
        "command": "request_link",
        "from_runtime_name": "xr-tp-sender",
        "from_display_name": THE_SOURCES_DISPLAY_NAME,
        "to_runtime_name": "xr-tp-receiver",
        "to_display_name": THE_DESTINATIONS_DISPLAY_NAME,
    }));

    receiving.wait_until("the third party's link to land", || {
        !receiving.the_links_it_last_reported().is_empty()
    });

    let [link] = receiving
        .the_links_it_last_reported()
        .try_into()
        .unwrap_or_else(|links| panic!("expected exactly one link, got {links:?}"));
    assert_eq!(
        link["created_by_runtime_name"], "xr-tp-agent",
        "a link neither end asked for names the runtime that did: {link}"
    );
    assert_eq!(link["source"]["runtime_name"], "xr-tp-sender", "{link}");
    assert!(
        wiring.the_links_it_last_reported().is_empty(),
        "the runtime that asked holds no link of its own: {:?}",
        wiring.the_links_it_last_reported()
    );

    wiring.ask_it_to_leave();
    sending.ask_it_to_leave();
    receiving.ask_it_to_leave();
}

/// A request naming a processor the answering runtime does not hold comes back
/// refused, in that runtime's own words, listing what it does display.
///
/// What it catches: a refusal read as silence and resent forever, and a
/// refusal that reaches the log but never the runtime that asked.
#[test]
#[serial]
fn a_request_naming_a_processor_the_answering_runtime_lacks_is_refused_by_name() {
    let mesh_name = a_mesh_name_of_its_own("refused");
    let receiving_endpoint = a_listen_endpoint_of_its_own();
    let receiving_runtime_directory = a_runtime_directory_of_its_own("ref-recv");
    let sending_runtime_directory = a_runtime_directory_of_its_own("ref-send");

    let mut receiving = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-refused-receiver".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![receiving_endpoint.clone()],
        peer_endpoints: Vec::new(),
        runtime_directory: receiving_runtime_directory.path().to_path_buf(),
    });
    receiving.wait_until_it_is_up();
    receiving.add_a_processor_displayed_as(THE_DESTINATIONS_DISPLAY_NAME);

    let mut sending = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-refused-sender".to_string(),
        mesh_name,
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![receiving_endpoint],
        runtime_directory: sending_runtime_directory.path().to_path_buf(),
    });
    sending.wait_until_it_is_up();
    sending.add_a_processor_displayed_as(THE_SOURCES_DISPLAY_NAME);

    sending.ask_it_to(serde_json::json!({
        "command": "request_link",
        "from_display_name": THE_SOURCES_DISPLAY_NAME,
        "to_runtime_name": "xr-refused-receiver",
        "to_display_name": "NoSuchProcessor",
    }));

    sending.wait_until("the refusal to reach the runtime that asked", || {
        sending
            .the_requests_it_is_waiting_on()
            .first()
            .and_then(|request| request["state"].as_str())
            == Some("refused")
    });

    let [request] = sending
        .the_requests_it_is_waiting_on()
        .try_into()
        .unwrap_or_else(|requests| panic!("expected exactly one request, got {requests:?}"));
    let reason = request["reason"].as_str().expect("a refusal has a reason");
    assert!(
        reason.contains("xr-refused-receiver"),
        "the refusal names who refused: {reason}"
    );
    assert!(
        reason.contains("NoSuchProcessor"),
        "the refusal names what was asked for: {reason}"
    );
    assert!(
        reason.contains(THE_DESTINATIONS_DISPLAY_NAME),
        "the refusal lists what that runtime does display: {reason}"
    );
    assert!(
        receiving.the_links_it_last_reported().is_empty(),
        "a refused request leaves no link behind"
    );

    sending.ask_it_to_leave();
    receiving.ask_it_to_leave();
}

/// A request to a runtime that is not on the mesh waits, says so, and is sent
/// the moment that runtime appears.
///
/// What it catches: a request sent once and dropped, and one whose reason
/// blames the runtime it names before the mesh has looked.
#[test]
#[serial]
fn a_request_to_a_runtime_that_is_not_on_the_mesh_waits_and_lands_when_it_appears() {
    let mesh_name = a_mesh_name_of_its_own("awaiting");
    let sending_endpoint = a_listen_endpoint_of_its_own();
    let sending_runtime_directory = a_runtime_directory_of_its_own("await-send");
    let receiving_runtime_directory = a_runtime_directory_of_its_own("await-recv");

    let mut sending = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-awaiting-sender".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![sending_endpoint.clone()],
        peer_endpoints: Vec::new(),
        runtime_directory: sending_runtime_directory.path().to_path_buf(),
    });
    sending.wait_until_it_is_up();
    sending.add_a_processor_displayed_as(THE_SOURCES_DISPLAY_NAME);

    sending.ask_it_to(serde_json::json!({
        "command": "request_link",
        "from_display_name": THE_SOURCES_DISPLAY_NAME,
        "to_runtime_name": "xr-awaiting-receiver",
        "to_display_name": THE_DESTINATIONS_DISPLAY_NAME,
    }));

    sending.wait_until("the request to report what it is waiting on", || {
        sending
            .the_requests_it_is_waiting_on()
            .first()
            .and_then(|request| request["state"].as_str())
            == Some("awaiting_runtime")
    });
    let [request] = sending
        .the_requests_it_is_waiting_on()
        .try_into()
        .unwrap_or_else(|requests| panic!("expected exactly one request, got {requests:?}"));
    assert!(
        request["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("xr-awaiting-receiver")),
        "the reason names the runtime that is missing: {request}"
    );
    assert!(
        request["link_request_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("LR")),
        "the request renders under its own id: {request}"
    );

    // Now the runtime it names turns up.
    let mut receiving = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-awaiting-receiver".to_string(),
        mesh_name,
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![sending_endpoint],
        runtime_directory: receiving_runtime_directory.path().to_path_buf(),
    });
    receiving.wait_until_it_is_up();
    receiving.add_a_processor_displayed_as(THE_DESTINATIONS_DISPLAY_NAME);

    receiving.wait_until(
        "the waiting request to land once its runtime appears",
        || !receiving.the_links_it_last_reported().is_empty(),
    );
    assert_eq!(
        receiving.the_links_it_last_reported()[0]["created_by_runtime_name"],
        "xr-awaiting-sender"
    );

    sending.ask_it_to_leave();
    receiving.ask_it_to_leave();
}

/// A cancelled request is never sent, even once the runtime it names appears.
///
/// What it catches: a cancel that only stops the rendering, leaving the pass
/// free to send the request on its next tick.
#[test]
#[serial]
fn a_cancelled_request_is_never_sent_even_once_its_runtime_appears() {
    let mesh_name = a_mesh_name_of_its_own("cancel");
    let sending_endpoint = a_listen_endpoint_of_its_own();
    let sending_runtime_directory = a_runtime_directory_of_its_own("canc-send");
    let receiving_runtime_directory = a_runtime_directory_of_its_own("canc-recv");

    let mut sending = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-cancel-sender".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![sending_endpoint.clone()],
        peer_endpoints: Vec::new(),
        runtime_directory: sending_runtime_directory.path().to_path_buf(),
    });
    sending.wait_until_it_is_up();
    sending.add_a_processor_displayed_as(THE_SOURCES_DISPLAY_NAME);

    sending.ask_it_to(serde_json::json!({
        "command": "request_link",
        "from_display_name": THE_SOURCES_DISPLAY_NAME,
        "to_runtime_name": "xr-cancel-receiver",
        "to_display_name": THE_DESTINATIONS_DISPLAY_NAME,
    }));
    sending.wait_until("the request to be waiting on its absent runtime", || {
        !sending.the_requests_it_is_waiting_on().is_empty()
    });

    let link_request_id = sending.the_last_link_request_it_made();
    sending.ask_it_to(serde_json::json!({
        "command": "cancel_request",
        "link_request_id": link_request_id,
    }));
    sending.wait_until("the cancelled request to leave the graph", || {
        sending.the_requests_it_is_waiting_on().is_empty()
    });

    let mut receiving = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-cancel-receiver".to_string(),
        mesh_name,
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![sending_endpoint],
        runtime_directory: receiving_runtime_directory.path().to_path_buf(),
    });
    receiving.wait_until_it_is_up();
    receiving.add_a_processor_displayed_as(THE_DESTINATIONS_DISPLAY_NAME);

    // Long enough that an uncancelled request would have been sent several
    // times over: the sending pass runs every two seconds and on every token.
    receiving.wait_until("the two runtimes to see each other", || {
        receiving.the_graph_it_last_reported()["mesh"]["peers"]
            .as_array()
            .is_some_and(|peers| !peers.is_empty())
    });
    std::thread::sleep(Duration::from_secs(6));
    assert!(
        receiving.the_links_it_last_reported().is_empty(),
        "a cancelled request must never be sent: {:?}",
        receiving.the_links_it_last_reported()
    );

    sending.ask_it_to_leave();
    receiving.ask_it_to_leave();
}

/// A third runtime asks the runtime holding a link to remove it, and it goes.
///
/// What it catches: a disconnect applied on the runtime that asked — which
/// holds no such link — and a disconnect request read as a connect for want of
/// its operation.
#[test]
#[serial]
fn a_link_another_runtime_holds_is_removed_by_asking_that_runtime() {
    let mesh_name = a_mesh_name_of_its_own("remotedisconnect");
    let receiving_endpoint = a_listen_endpoint_of_its_own();
    let receiving_runtime_directory = a_runtime_directory_of_its_own("rd-recv");
    let sending_runtime_directory = a_runtime_directory_of_its_own("rd-send");
    let wiring_runtime_directory = a_runtime_directory_of_its_own("rd-agent");

    let mut receiving = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-rd-receiver".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![receiving_endpoint.clone()],
        peer_endpoints: Vec::new(),
        runtime_directory: receiving_runtime_directory.path().to_path_buf(),
    });
    receiving.wait_until_it_is_up();
    receiving.add_a_processor_displayed_as(THE_DESTINATIONS_DISPLAY_NAME);

    let mut sending = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-rd-sender".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![receiving_endpoint.clone()],
        runtime_directory: sending_runtime_directory.path().to_path_buf(),
    });
    sending.wait_until_it_is_up();
    sending.add_a_processor_displayed_as(THE_SOURCES_DISPLAY_NAME);
    sending.ask_it_to(serde_json::json!({
        "command": "request_link",
        "from_display_name": THE_SOURCES_DISPLAY_NAME,
        "to_runtime_name": "xr-rd-receiver",
        "to_display_name": THE_DESTINATIONS_DISPLAY_NAME,
    }));
    receiving.wait_until("the link to land before it is asked to go", || {
        !receiving.the_links_it_last_reported().is_empty()
    });
    let the_link = receiving.the_one_link_it_holds();

    let mut wiring = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-rd-agent".to_string(),
        mesh_name,
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![receiving_endpoint],
        runtime_directory: wiring_runtime_directory.path().to_path_buf(),
    });
    wiring.wait_until_it_is_up();
    wiring.ask_it_to(serde_json::json!({
        "command": "request_disconnect",
        "input_runtime_name": "xr-rd-receiver",
        "link_id": the_link,
    }));

    // Marked rather than gone: `disconnect` takes a link out of the graph at
    // the next compile, and a runtime that was never started never runs one.
    // The mark is what says the disconnect was applied — by the runtime that
    // holds the link, on a request from a runtime that is neither of its ends.
    receiving.wait_until("the link to be marked for deletion", || {
        receiving
            .the_links_it_last_reported()
            .first()
            .is_some_and(|link| link["components"]["pending_deletion"] == true)
    });
    assert!(
        wiring.the_links_it_last_reported().is_empty(),
        "the runtime that asked never held the link it asked to go"
    );

    wiring.ask_it_to_leave();
    sending.ask_it_to_leave();
    receiving.ask_it_to_leave();
}

/// A runtime that is on the mesh and answers nothing leaves the request
/// `unanswered` rather than refused, and keeps sending it.
///
/// Silence is not a refusal, and the two are not told apart by shape: Zenoh
/// answers a query that timed out with an error reply carrying the string
/// `Timeout`, delivered exactly as a real `reply_err` is. Reading that as a
/// refusal would make one lost packet permanent.
///
/// The silent runtime is the *pull* fixture's peer, which joins the mesh —
/// so it is present, and the requester's peer table sees it — and never
/// declares a link-request queryable, so a request to it matches nothing and
/// comes back with no replies at all. That is the lost-reply case made
/// deterministic, which a dropped packet cannot be.
#[test]
#[serial]
fn a_runtime_that_is_present_and_answers_nothing_leaves_the_request_unanswered() {
    let mesh_name = a_mesh_name_of_its_own("silent");
    let silent_endpoint = a_listen_endpoint_of_its_own();
    let silent_runtime_directory = a_runtime_directory_of_its_own("sil-peer");
    let sending_runtime_directory = a_runtime_directory_of_its_own("sil-send");

    let mut silent = Command::new(env!("CARGO_BIN_EXE_cross_runtime_link_peer"))
        .arg("--reader")
        .arg("--runtime-name")
        .arg("xr-silent-receiver")
        .arg("--mesh-name")
        .arg(&mesh_name)
        .arg("--display-name")
        .arg(THE_DESTINATIONS_DISPLAY_NAME)
        // It links from a runtime that never exists, so it waits forever and
        // does nothing else — which is the point.
        .arg("--link-from")
        .arg("xr-silent-nobody")
        .arg("--mesh-listen")
        .arg(&silent_endpoint)
        .arg("--multicast-discovery")
        .arg("off")
        .arg("--iceoryx2-domain-root")
        .arg(silent_runtime_directory.path())
        .env(
            MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE,
            LOOPBACK_INTERFACE,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the pull fixture's peer launches");

    let mut sending = LinkRequestPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "xr-silent-sender".to_string(),
        mesh_name,
        listen_endpoints: vec![a_listen_endpoint_of_its_own()],
        peer_endpoints: vec![silent_endpoint],
        runtime_directory: sending_runtime_directory.path().to_path_buf(),
    });
    sending.wait_until_it_is_up();
    sending.add_a_processor_displayed_as(THE_SOURCES_DISPLAY_NAME);
    sending.wait_until("the sender to see the silent runtime", || {
        sending.the_graph_it_last_reported()["mesh"]["peers"]
            .as_array()
            .is_some_and(|peers| {
                peers
                    .iter()
                    .any(|peer| peer["runtime_name"] == "xr-silent-receiver")
            })
    });

    sending.ask_it_to(serde_json::json!({
        "command": "request_link",
        "from_display_name": THE_SOURCES_DISPLAY_NAME,
        "to_runtime_name": "xr-silent-receiver",
        "to_display_name": THE_DESTINATIONS_DISPLAY_NAME,
    }));

    sending.wait_until("the request to read as unanswered", || {
        sending
            .the_requests_it_is_waiting_on()
            .first()
            .and_then(|request| request["state"].as_str())
            == Some("unanswered")
    });
    let [request] = sending
        .the_requests_it_is_waiting_on()
        .try_into()
        .unwrap_or_else(|requests| panic!("expected exactly one request, got {requests:?}"));
    assert!(
        request["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("xr-silent-receiver")),
        "the reason names the runtime that said nothing: {request}"
    );

    // And it is still held rather than given up on: a refusal is final, this
    // is not, and the request is sent again on its backoff.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        sending
            .the_requests_it_is_waiting_on()
            .first()
            .and_then(|request| request["state"].as_str()),
        Some("unanswered"),
        "an unanswered request is kept and resent, never discarded"
    );

    sending.ask_it_to_leave();
    let _ = silent.kill();
    let _ = silent.wait();
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Links between runtimes in separate OS processes — the proof a remote link
//! carries anything at all, that a runtime does no work for a port nobody
//! reads, that one egress serves every runtime reading a port, and that a
//! source renders only the ports it is really sending.
//!
//! Most arms are one source and one reader; the last is one source and two
//! readers, because "the source stops sending when the *last* reader leaves"
//! says nothing that can be checked with one.
//!
//! Serial, and each arm takes its own mesh name, its own runtime names, its own
//! loopback port and its own iceoryx2 domain. The arms at once would put a
//! dozen runtimes with real network endpoints on one loopback interface, which
//! tests the harness rather than the engine.
//!
//! GPU-free: neither peer builds a `Runner`, because `Runner::start()` needs a
//! GPU and CI has none. Each stands up the mesh half a runtime stands up, over
//! a real iceoryx2 channel and a real Zenoh session. Each arm takes its own mesh
//! name, so arms never see each other even while the transport connects them,
//! and multicast is pinned to `127.0.0.1` so a test never joins whatever network
//! the machine is on.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serial_test::serial;
use zenoh::Wait;

/// How long an arm waits for something it is expecting. Generous: a scouting
/// delay, a description round trip and an offered-ports query on a loaded CI
/// runner.
const HOW_LONG_AN_ARM_WAITS: Duration = Duration::from_secs(40);

/// How long a peer asked to leave has to be gone. The ladder it walks — undeclare,
/// stop each egress and ingress, join their threads, close the session — is
/// bounded by design, so this is generous against a loaded machine rather than
/// a guess at the budget.
const HOW_LONG_A_PEER_HAS_TO_LEAVE: Duration = Duration::from_secs(30);

/// The variable that pins multicast scouting, so a test never scouts on the
/// machine's real network.
const MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_MESH_MULTICAST_INTERFACE";

/// Where scouting is pinned to.
const LOOPBACK_INTERFACE: &str = "127.0.0.1";

/// The display name the source gives its processor, and the reader addresses.
/// A space in it on purpose: a display name is legal on the mesh and illegal in
/// a channel name, which is why the ingress channel is hashed from the address.
const THE_DISPLAY_NAME: &str = "Camera Source 2";

/// The output port the source peer publishes, spelled here too because the
/// peer binary is a separate crate and this is what the source's own `graph`
/// has to name.
const THE_PORT: &str = "video";

/// The runtime names one arm's source and reader take.
///
/// Per arm rather than shared: an arm's peers leave at the end of it, but a
/// session that has not finished tearing down is still on the transport when
/// the next arm's peers come up, and a name reused across arms is one a peer
/// can see twice. Discovery is off throughout for the same reason.
fn the_two_runtimes_of(arm: &str) -> (String, String) {
    (format!("x-source-{arm}"), format!("x-reader-{arm}"))
}

/// A mesh name no other arm and no other machine uses.
fn a_mesh_name_of_its_own(arm: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "x-{arm}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A loopback port no other arm will be handed.
///
/// The TCP listener that found it is kept for the whole run rather than
/// released: a released ephemeral port is handed straight back out, so a later
/// arm can be given the port an earlier arm's peers are still on — which puts a
/// reader on another arm's source. Holding it costs nothing, because what the
/// peers bind is the UDP port of the same number.
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

/// An iceoryx2 domain root of this arm's own, so two arms never share services.
fn a_domain_root_of_its_own(arm: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("sl-x-{arm}-"))
        .tempdir_in("/tmp")
        .expect("a temporary domain root")
}

/// One peer process, with everything it has reported.
struct CrossRuntimeLinkPeerProcess {
    child: Child,
    reported: Arc<Mutex<Vec<serde_json::Value>>>,
    came_up: Arc<Mutex<bool>>,
    why_it_refused: Arc<Mutex<Option<String>>>,
}

/// How a peer is launched.
#[derive(Default)]
struct HowToLaunchAPeer {
    reader: bool,
    runtime_name: String,
    mesh_name: String,
    peer_endpoints: Vec<String>,
    listen_endpoints: Vec<String>,
    display_name: String,
    link_from: Option<String>,
    iceoryx2_domain_root: std::path::PathBuf,
    burst_once_a_reader_arrives: Option<u64>,
    recreate_the_publisher_just_before_the_burst: bool,
    take_every_destination_slot: bool,
    refuse_to_say_how_to_read_the_port: bool,
}

impl CrossRuntimeLinkPeerProcess {
    fn launch(how: HowToLaunchAPeer) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cross_runtime_link_peer"));
        command
            .arg(if how.reader { "--reader" } else { "--source" })
            .arg("--runtime-name")
            .arg(&how.runtime_name)
            .arg("--mesh-name")
            .arg(&how.mesh_name)
            .arg("--display-name")
            .arg(&how.display_name)
            .arg("--iceoryx2-domain-root")
            .arg(&how.iceoryx2_domain_root)
            .arg("--multicast-discovery")
            .arg("off")
            .env(
                MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE,
                LOOPBACK_INTERFACE,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for peer in &how.peer_endpoints {
            command.arg("--mesh-peer").arg(peer);
        }
        for listen in &how.listen_endpoints {
            command.arg("--mesh-listen").arg(listen);
        }
        if let Some(link_from) = &how.link_from {
            command.arg("--link-from").arg(link_from);
        }
        if let Some(burst) = how.burst_once_a_reader_arrives {
            command
                .arg("--burst-once-a-reader-arrives")
                .arg(burst.to_string());
        }
        if how.recreate_the_publisher_just_before_the_burst {
            command.arg("--recreate-the-publisher-just-before-the-burst");
        }
        if how.refuse_to_say_how_to_read_the_port {
            command.arg("--refuse-to-say-how-to-read-the-port");
        }
        if how.take_every_destination_slot {
            command.arg("--take-every-destination-slot");
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
            "timed out waiting for {described}; the peer reported {:?}",
            self.everything_it_has_reported()
        );
    }

    fn everything_it_has_reported(&self) -> Vec<serde_json::Value> {
        self.reported.lock().clone()
    }

    /// Every bag this peer has reported receiving, with its stamp.
    fn every_bag_it_received(&self) -> Vec<(String, i64)> {
        self.everything_it_has_reported()
            .iter()
            .filter_map(|reported| {
                Some((
                    reported.get("received")?.as_str()?.to_string(),
                    reported.get("timestamp_ns")?.as_i64()?,
                ))
            })
            .collect()
    }

    /// The ports this peer last reported the mesh reading from it, each with
    /// its readers — `graph.mesh.egress_ports` on the source's own runtime.
    ///
    /// The last report rather than all of them: an egress comes and goes, so
    /// what the source is sending *now* is the only reading worth asserting.
    fn the_egress_ports_it_last_reported(&self) -> Vec<(String, String, Vec<String>)> {
        self.everything_it_has_reported()
            .iter()
            .rev()
            .find(|reported| reported.get("egress_ports").is_some())
            .map(the_egress_ports_one_report_names)
            .unwrap_or_default()
    }

    /// Every egress port this peer named in its last `how_many_reports`
    /// reports.
    ///
    /// A window rather than the last report alone, and never every report it
    /// ever made: the table renders an egress from the moment its thread is
    /// spawned and withdraws it when that thread says it ended, so a source
    /// whose egress fails renders the port until its table hears. What that
    /// settles to is the claim; that it was never rendered at all is not one the
    /// table makes.
    fn every_egress_port_in_its_last_reports(
        &self,
        how_many_reports: usize,
    ) -> Vec<(String, String, Vec<String>)> {
        let reported = self.everything_it_has_reported();
        reported
            .iter()
            .skip(reported.len().saturating_sub(how_many_reports))
            .flat_map(the_egress_ports_one_report_names)
            .collect()
    }

    /// Wait until this peer reports the egress ports `expected`, or fail
    /// naming what it reported last.
    fn wait_until_it_reports_egress_ports(
        &self,
        described: &str,
        expected: &[(String, String, Vec<String>)],
    ) {
        self.wait_until(described, || {
            self.the_egress_ports_it_last_reported() == expected
        });
    }

    /// The last bag index of this source's burst, once it has sent one.
    fn the_index_its_burst_ended_at(&self) -> Option<u64> {
        self.everything_it_has_reported()
            .iter()
            .rev()
            .find_map(|reported| reported.get("burst_ended_at_index")?.as_u64())
    }

    /// How many publishers this source's port has had — two once it has
    /// replaced the one it started with.
    fn how_many_publishers_its_port_has_had(&self) -> u64 {
        self.everything_it_has_reported()
            .iter()
            .rev()
            .find_map(|reported| reported.get("publishers_this_port_has_had")?.as_u64())
            .unwrap_or(0)
    }

    /// Everything this reader last reported about what reached it: how many
    /// bags, which indices they spanned, what the hop lost, and what its own
    /// local ring lost either side of its first poll.
    fn what_last_reached_it(&self) -> WhatReachedTheReader {
        let reported = self.everything_it_has_reported();
        let last_with_totals = reported
            .iter()
            .rev()
            .find(|reported| reported.get("received_count").is_some())
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let number = |key: &str| last_with_totals.get(key).and_then(|it| it.as_u64());
        WhatReachedTheReader {
            received_count: number("received_count").unwrap_or(0),
            first_bag_index: number("first_bag_index"),
            last_bag_index: number("last_bag_index"),
            bags_lost_before_its_first_poll: number("bags_lost_before_this_peers_first_poll")
                .unwrap_or(0),
            its_own_ring_lost: number("what_this_peers_own_ring_lost").unwrap_or(0),
            the_hop_lost: last_with_totals
                .get("mesh_hop_dropped_bags_by_link")
                .and_then(|by_link| by_link.as_object())
                .map(|by_link| by_link.values().filter_map(|lost| lost.as_u64()).sum())
                .unwrap_or(0),
        }
    }

    /// Every machine this peer has reported its link's stamps as being taken
    /// on, in order — `None` for a report that named none.
    fn every_stamp_clock_it_has_reported(&self) -> Vec<Option<String>> {
        self.everything_it_has_reported()
            .iter()
            .filter(|reported| reported.get("state").is_some())
            .map(|reported| {
                reported
                    .get("stamp_clock_identity")
                    .and_then(|machine| machine.as_str())
                    .map(str::to_string)
            })
            .collect()
    }

    /// Every reason this peer has reported its link waiting or refused on, in
    /// order.
    fn every_reason_it_has_reported(&self) -> Vec<String> {
        self.everything_it_has_reported()
            .iter()
            .filter_map(|reported| Some(reported.get("reason")?.as_str()?.to_string()))
            .collect()
    }

    /// Every state this peer has reported its link in, in order.
    fn every_state_it_has_reported(&self) -> Vec<String> {
        self.everything_it_has_reported()
            .iter()
            .filter_map(|reported| Some(reported.get("state")?.as_str()?.to_string()))
            .collect()
    }

    /// Ask the peer to leave cleanly, and wait for it to go.
    ///
    /// Bounded, and a timeout is a failure: a runtime that cannot finish
    /// leaving — a teardown that waits on a lock or a thread that never
    /// notices it was told to stop — is exactly what this arm is here to
    /// catch, and an unbounded wait would hang the suite instead of failing
    /// it.
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
        panic!(
            "the peer did not finish leaving within {HOW_LONG_A_PEER_HAS_TO_LEAVE:?}; its              teardown is stuck"
        );
    }

    /// End the peer the way a crash does, which nothing in the runtime can
    /// delay.
    fn kill_it(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for CrossRuntimeLinkPeerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The egress ports one report names, each with the runtimes reading it.
fn the_egress_ports_one_report_names(
    reported: &serde_json::Value,
) -> Vec<(String, String, Vec<String>)> {
    reported
        .get("egress_ports")
        .and_then(|ports| ports.as_array())
        .map(|ports| {
            ports
                .iter()
                .map(|port| {
                    (
                        port["processor_display_name"].as_str().unwrap().to_string(),
                        port["port_name"].as_str().unwrap().to_string(),
                        port["reader_runtime_names"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|name| name.as_str().unwrap().to_string())
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A session that only looks at the mesh — it announces nothing, so what it
/// sees is what the peers put there and nothing the test added.
fn a_session_that_only_looks(mesh_peer_endpoint: &str) -> zenoh::Session {
    let mut configuration = zenoh::Config::default();
    configuration
        .insert_json5("mode", "\"peer\"")
        .expect("peer mode");
    configuration
        .insert_json5("connect/endpoints", &format!("[\"{mesh_peer_endpoint}\"]"))
        .expect("the peer endpoint");
    configuration
        .insert_json5("listen/endpoints", "[]")
        .expect("no listener");
    configuration
        .insert_json5("scouting/multicast/enabled", "false")
        .expect("no multicast");
    zenoh::open(configuration)
        .wait()
        .expect("a looking session opens")
}

/// Every liveliness token live under `key`, as one look sees them.
fn every_token_under(session: &zenoh::Session, key: &str) -> BTreeSet<String> {
    let replies = session
        .liveliness()
        .get(key)
        .timeout(Duration::from_secs(2))
        .wait()
        .expect("the liveliness get is sent");
    replies
        .into_iter()
        .filter_map(|reply| Some(reply.result().ok()?.key_expr().as_str().to_string()))
        .collect()
}

/// What one reader last reported about what reached it.
///
/// `graph`'s own `metrics.mesh_hop_dropped_bags_by_link` for the hop, and this
/// peer's own accounting for the shallow local channel it polls where a real
/// destination has a counted mailbox.
#[derive(Debug)]
struct WhatReachedTheReader {
    received_count: u64,
    first_bag_index: Option<u64>,
    last_bag_index: Option<u64>,
    /// How many bags the ingress wrote before this peer's first poll of the
    /// wiring. Non-zero means the peer was not there from the ingress's first
    /// bag, so no arm can say which span the hop count covers.
    bags_lost_before_its_first_poll: u64,
    its_own_ring_lost: u64,
    the_hop_lost: u64,
}

impl WhatReachedTheReader {
    /// The stretch of published bags this reader's ingress saw, from the first
    /// bag it delivered to the last.
    ///
    /// Every bag in it either arrived, was lost on the hop, or was lost after
    /// it in this peer's own ring — which is the conservation an arm states.
    fn the_span_its_ingress_covered(&self) -> u64 {
        let (Some(first), Some(last)) = (self.first_bag_index, self.last_bag_index) else {
            return 0;
        };
        last - first + 1
    }

    /// Everything that stretch accounts for.
    fn everything_accounted_for(&self) -> u64 {
        self.received_count + self.the_hop_lost + self.its_own_ring_lost
    }
}

/// Two runtimes, one link: every bag the source published lands on the reader's
/// local channel byte-equal, under the stamp the producer wrote.
///
/// What it catches: a payload the engine re-encoded on the way across, and a
/// stamp minted at the receiving end rather than carried.
#[test]
#[serial]
fn a_bag_crosses_the_mesh_byte_equal_under_the_stamp_its_producer_wrote() {
    let mesh_name = a_mesh_name_of_its_own("carries");
    let (source_name, reader_name) = the_two_runtimes_of("carries");
    let source_domain = a_domain_root_of_its_own("carries-source");
    let reader_domain = a_domain_root_of_its_own("carries-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        ..Default::default()
    });
    source.wait_until_it_is_up();

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    reader.wait_until("three bags to cross the mesh", || {
        reader.every_bag_it_received().len() >= 3
    });

    for (bag, stamp) in reader.every_bag_it_received() {
        let published: u64 = bag
            .strip_prefix("bag-")
            .and_then(|index| index.parse().ok())
            .unwrap_or_else(|| panic!("a bag crossed as {bag:?}, which is not what was published"));
        assert_eq!(
            stamp,
            1_726_000_000_000_000_000 + published as i64,
            "bag {published} crossed under a stamp its producer did not write"
        );
    }
    // A bag crossing and the link reading `wired` are two observations, not
    // one: the reader can have a bag in hand before the pass that saw the
    // source's egress token has written the resolution down.
    reader.wait_until("the link that is carrying bags to read wired", || {
        reader
            .every_state_it_has_reported()
            .contains(&"wired".to_string())
    });
}

/// A link names no machine until a bag has crossed it, and names the machine
/// that stamped that bag afterwards.
///
/// What it catches: an identity that never reaches the ingress's cell over a
/// real Zenoh hop, and one written there before anything crossed — a link that
/// named a machine while carrying nothing would let a sink compare stamps it
/// never received.
///
/// What it cannot catch: that the identity is the *sender's* rather than this
/// machine's. Both peers run on one machine, so the two are equal here, and
/// forcing a second boot id would need a back door in library code. That half
/// is locked by `mesh_link_ingress::tests::
/// the_machine_a_message_names_is_the_senders_and_never_this_one`.
#[test]
#[serial]
fn a_link_names_no_machine_until_a_bag_has_crossed_it_and_that_bags_machine_after() {
    let mesh_name = a_mesh_name_of_its_own("clock");
    let (source_name, reader_name) = the_two_runtimes_of("clock");
    let source_domain = a_domain_root_of_its_own("clock-source");
    let reader_domain = a_domain_root_of_its_own("clock-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        ..Default::default()
    });
    source.wait_until_it_is_up();

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    assert_eq!(
        reader.every_stamp_clock_it_has_reported().first(),
        Some(&None),
        "the first report is made before any bag has crossed, so it must name no machine"
    );

    reader.wait_until("a bag to cross the mesh", || {
        !reader.every_bag_it_received().is_empty()
    });
    reader.wait_until(
        "the link to name the machine that bag was stamped on",
        || {
            reader
                .every_stamp_clock_it_has_reported()
                .iter()
                .any(Option::is_some)
        },
    );

    let this_machine =
        streamlib_engine::core::runtime::mesh::MachineClockIdentity::of_this_machine();
    assert!(
        !this_machine.is_unidentified(),
        "this platform names no clock at all, so the arm proves nothing"
    );
    for named in reader
        .every_stamp_clock_it_has_reported()
        .into_iter()
        .flatten()
    {
        assert_eq!(
            named,
            this_machine.to_string(),
            "both peers run on this machine, so the identity off the wire is this machine's"
        );
    }
}

/// A source runtime holds no egress token while nobody is reading its port, and
/// holds one the moment somebody is.
///
/// What it catches: an egress created eagerly at startup. "A sending runtime
/// does no network work and copies no frame for a port until a remote link to
/// that port exists" is the clause, and a token is the observable half of it.
#[test]
#[serial]
fn a_source_holds_no_egress_until_somebody_reads_its_port() {
    let mesh_name = a_mesh_name_of_its_own("lazy");
    let (source_name, reader_name) = the_two_runtimes_of("lazy");
    let source_domain = a_domain_root_of_its_own("lazy-source");
    let reader_domain = a_domain_root_of_its_own("lazy-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());
    let every_egress_token = format!("streamlib/{mesh_name}/@runtime/{source_name}/@egress/**");

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        ..Default::default()
    });
    source.wait_until_it_is_up();
    source.wait_until("the source to publish a few bags nobody reads", || {
        source.everything_it_has_reported().len() >= 3
    });

    let looking = a_session_that_only_looks(&source_listen);
    assert!(
        every_token_under(&looking, &every_egress_token).is_empty(),
        "a source nobody reads must hold no egress token"
    );

    let mut reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();
    reader.wait_until("a bag to cross, which means the egress is up", || {
        !reader.every_bag_it_received().is_empty()
    });

    let while_it_is_read = every_token_under(&looking, &every_egress_token);
    assert_eq!(
        while_it_is_read.len(),
        1,
        "a source somebody reads holds exactly one egress token for that port; it held {while_it_is_read:?}"
    );

    // The last reader leaving takes the egress with it.
    reader.ask_it_to_leave();
    let gave_up_at = Instant::now() + HOW_LONG_AN_ARM_WAITS;
    while Instant::now() < gave_up_at {
        if every_token_under(&looking, &every_egress_token).is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the last reader leaving must take the source's egress token with it");
}

/// The source's own `graph` names the port the mesh is reading from it and the
/// runtime reading it, and empties when that reader goes.
///
/// What it catches: an agent driving the sending node cannot otherwise tell
/// that anybody is pulling from it — every other sign of a remote link lives on
/// the runtime that owns the input. Mental-revert: never write the table from
/// the egress thread and the source renders an empty list while a reader is
/// plainly taking its bags.
///
/// What it does *not* catch is that the table is derived from the live
/// egresses rather than from the readers — both spellings agree here, because
/// this source offers the port it is asked for. The divergence is locked in
/// `mesh_port_egress_table`'s own tests, where a port with readers and no
/// egress can be built without standing up a second runtime.
#[test]
#[serial]
fn a_sources_graph_names_the_port_the_mesh_reads_and_who_reads_it() {
    let mesh_name = a_mesh_name_of_its_own("egressports");
    let (source_name, reader_name) = the_two_runtimes_of("egressports");
    let source_domain = a_domain_root_of_its_own("egressports-source");
    let reader_domain = a_domain_root_of_its_own("egressports-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        ..Default::default()
    });
    source.wait_until_it_is_up();
    source.wait_until("the source to publish a few bags nobody reads", || {
        source.everything_it_has_reported().len() >= 3
    });
    assert_eq!(
        source.the_egress_ports_it_last_reported(),
        Vec::new(),
        "a source nobody reads must render no egress port"
    );

    let mut reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name.clone(),
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    source.wait_until_it_reports_egress_ports(
        "the source to render the port its reader is pulling",
        &[(
            THE_DISPLAY_NAME.to_string(),
            THE_PORT.to_string(),
            vec![reader_name],
        )],
    );

    reader.ask_it_to_leave();

    source.wait_until_it_reports_egress_ports(
        "the source to render no egress port once its reader has gone",
        &[],
    );
}

/// How many of a source's own reports the arm below reads as its settled
/// render, and how many it waits out first.
///
/// A second of them either side, at the peer's report cadence. Against the bug
/// it catches the entry never leaves, so every report in the window names the
/// port however wide the window is; the width is against a loaded runner
/// settling slowly, never against the assertion being thin.
const HOW_MANY_REPORTS_A_SOURCES_RENDER_IS_READ_OVER: usize = 10;

/// A source whose egress cannot take a destination slot on its own channel
/// settles to rendering no egress port, and the reader waiting on that port
/// reads the refusal in the source's own words.
///
/// What it catches: `MeshPortEgress::start` succeeds the moment its thread
/// spawns, and everything that can refuse an egress happens inside that thread
/// afterwards. A table that keeps the entry has the source's own `graph` claim a
/// send while the reader's link reads `awaiting_remote` — two runtimes saying
/// opposite things about one link, with the source the one that is wrong — and
/// no later reader can replace the entry, so the port stays unsendable for the
/// run (#2346).
///
/// The refusal is the ticket's own repro and the readiest one to stage: a
/// channel's destination slots are fixed when it is created, so a source holding
/// every one of them leaves none for the egress. Nothing else in CI drives an
/// egress that fails at all — the unit tests feed the table's two maps directly,
/// which is what let this survive the surface it shipped on.
///
/// The reader's half is #2379: until it, the only account of the refusal was in
/// the *sending* runtime's log, and the reading runtime's link said the source
/// offered the port and was not sending it — true, and indistinguishable from a
/// source still coming up. Nothing else in CI carries a reason across the mesh
/// for a port a runtime does offer.
///
/// Mental-revert: stop the egress thread saying it ended, and the source renders
/// the port under its reader's name for the rest of the run; stop it saying
/// *why*, and the reader is left with the sentence it already had.
#[test]
#[serial]
fn a_source_whose_egress_cannot_take_a_slot_settles_to_no_egress_port() {
    let mesh_name = a_mesh_name_of_its_own("noslot");
    let (source_name, reader_name) = the_two_runtimes_of("noslot");
    let source_domain = a_domain_root_of_its_own("noslot-source");
    let reader_domain = a_domain_root_of_its_own("noslot-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        take_every_destination_slot: true,
        ..Default::default()
    });
    source.wait_until_it_is_up();

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name.clone(),
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name.clone()),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    // The source answering what it offers is what says the two are talking, so
    // the reader's token has reached it and its egress has been asked for.
    reader.wait_until(
        "the reader to say its source offers the port and is not sending it",
        || {
            reader.everything_it_has_reported().iter().any(|reported| {
                reported.get("state").and_then(|state| state.as_str()) == Some("awaiting_remote")
                    && reported
                        .get("reason")
                        .and_then(|reason| reason.as_str())
                        .is_some_and(|reason| reason.contains("is not sending it"))
            })
        },
    );
    // Then the source's own account of the refusal reaches it, which is the
    // thing the reading machine cannot derive: the slot was refused on the
    // *other* process's channel.
    reader.wait_until(
        "the reader's link to name the slot refusal and say nothing is retrying it",
        || {
            reader.every_reason_it_has_reported().iter().any(|reason| {
                reason.contains("destination slot") && reason.contains("Nothing is retrying it")
            })
        },
    );
    assert!(
        reader
            .every_state_it_has_reported()
            .iter()
            .all(|state| state == "awaiting_remote"),
        "a failed egress leaves the link waiting and never final, or no later reader could \
         revive the port: {:?}",
        reader.every_state_it_has_reported()
    );
    // Long enough for the table to have heard that the egress ended and to have
    // reported what it renders from then on, twice over: the first window is the
    // settling, the second is what the assertion reads.
    let reported_by_then = source.everything_it_has_reported().len();
    source.wait_until("the source to report again with its reader waiting", || {
        source.everything_it_has_reported().len()
            >= reported_by_then + HOW_MANY_REPORTS_A_SOURCES_RENDER_IS_READ_OVER * 2
    });

    assert_eq!(
        source
            .every_egress_port_in_its_last_reports(HOW_MANY_REPORTS_A_SOURCES_RENDER_IS_READ_OVER),
        Vec::new(),
        "a source that could not take a slot for its egress sends nothing, and must have settled \
         to rendering no port as being sent"
    );
    assert!(
        !reader
            .every_state_it_has_reported()
            .contains(&"wired".to_string()),
        "nothing was sent, so the reader's link must never have read wired: {:?}",
        reader.every_state_it_has_reported()
    );
}

/// A source that offers a port it has no way to read tells its reader so, rather
/// than leaving the link on the sentence a source still coming up shows.
///
/// What it catches: the arm of `a_runtime_started_reading` that fires before any
/// egress exists. A runtime's offer answers whether a port's channel can be
/// *named*, never whether it opens, so a port whose channel will not open is
/// offered, is never refused, and gets no egress — and until #2379 the reader
/// waited out the run on "is on the mesh and offers X/Y, and is not sending it",
/// with the only account in the other machine's log. It is the other half of the
/// no-slot arm above: that one starts an egress and has it refused, this one
/// never starts one at all, and they record their reason at different seams.
///
/// The peer stages it by answering `None` from its own
/// `how_to_read_an_offered_output_port` while still offering the port, which is
/// exactly what a real runtime is left holding when
/// `open_the_channel_of_an_output_port_nothing_local_reads` fails.
///
/// Mental-revert: drop the `record_why_it_stopped_sending_an_output_port` call
/// from that arm and this goes red on the reason, which is the state this branch
/// shipped in until the pre-PR reviewers caught it.
#[test]
#[serial]
fn a_reader_of_a_port_its_source_cannot_read_is_told_so() {
    let mesh_name = a_mesh_name_of_its_own("noread");
    let (source_name, reader_name) = the_two_runtimes_of("noread");
    let source_domain = a_domain_root_of_its_own("noread-source");
    let reader_domain = a_domain_root_of_its_own("noread-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        refuse_to_say_how_to_read_the_port: true,
        ..Default::default()
    });
    source.wait_until_it_is_up();

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name.clone(),
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name.clone()),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    reader.wait_until(
        "the reader's link to name what its source could not do and say nothing is retrying it",
        || {
            reader.every_reason_it_has_reported().iter().any(|reason| {
                reason.contains("could not open a way to read it")
                    && reason.contains("Nothing is retrying it")
            })
        },
    );
    assert!(
        reader
            .every_state_it_has_reported()
            .iter()
            .all(|state| state == "awaiting_remote"),
        "the port is still offered, so the link waits rather than going final: {:?}",
        reader.every_state_it_has_reported()
    );
    assert_eq!(
        source.the_egress_ports_it_last_reported(),
        Vec::new(),
        "no egress was ever started, so the source must claim no send"
    );
}

/// A link naming a runtime that is not on the mesh waits, saying so, and wires
/// when that runtime turns up.
///
/// What it catches: a `connect` that waits on the network, and a link that
/// gives up on a runtime that has not started yet.
#[test]
#[serial]
fn a_link_named_before_its_runtime_exists_waits_and_then_wires() {
    let mesh_name = a_mesh_name_of_its_own("waits");
    let (source_name, reader_name) = the_two_runtimes_of("waits");
    let source_domain = a_domain_root_of_its_own("waits-source");
    let reader_domain = a_domain_root_of_its_own("waits-reader");
    let reader_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    // The reader first, and it is the one listening: the source does not exist
    // yet, so there is nothing for the reader to dial.
    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![reader_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name.clone()),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();
    reader.wait_until("the link to say it is waiting on a runtime", || {
        reader.everything_it_has_reported().iter().any(|reported| {
            reported.get("state").and_then(|state| state.as_str()) == Some("awaiting_remote")
                && reported
                    .get("reason")
                    .and_then(|reason| reason.as_str())
                    .is_some_and(|reason| reason.contains(&source_name))
        })
    });

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name,
        mesh_name,
        peer_endpoints: vec![reader_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        ..Default::default()
    });
    source.wait_until_it_is_up();

    reader.wait_until("the link to wire once its runtime turned up", || {
        !reader.every_bag_it_received().is_empty()
    });
}

/// A source killed outright returns the link to waiting, and the link wires
/// again when a source comes back under the same name.
///
/// What it catches: a link that stays `wired` over a runtime that is gone, and
/// one that never re-wires after it returns.
#[test]
#[serial]
fn a_killed_source_returns_the_link_to_waiting_and_a_restart_re_wires_it() {
    let mesh_name = a_mesh_name_of_its_own("restarts");
    let (source_name, reader_name) = the_two_runtimes_of("restarts");
    let source_domain = a_domain_root_of_its_own("restarts-source");
    let reader_domain = a_domain_root_of_its_own("restarts-reader");
    let reader_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![reader_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name.clone()),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    // A fresh domain each time: a killed process leaves its iceoryx2 node
    // holding the channel's one publisher slot, and reclaiming a dead node's
    // services is the transport's own concern rather than this link's.
    let launch_the_source = |domain: &tempfile::TempDir| {
        CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
            runtime_name: source_name.clone(),
            mesh_name: mesh_name.clone(),
            peer_endpoints: vec![reader_listen.clone()],
            display_name: THE_DISPLAY_NAME.to_string(),
            iceoryx2_domain_root: domain.path().to_path_buf(),
            ..Default::default()
        })
    };

    let mut source = launch_the_source(&source_domain);
    source.wait_until_it_is_up();
    reader.wait_until("the link to carry before the source is killed", || {
        !reader.every_bag_it_received().is_empty()
    });

    source.kill_it();
    let states_before_the_kill = reader.every_state_it_has_reported().len();
    reader.wait_until(
        "the link to return to waiting once its source is gone",
        || {
            reader
                .every_state_it_has_reported()
                .iter()
                .skip(states_before_the_kill)
                .any(|state| state == "awaiting_remote")
        },
    );

    let bags_before_the_restart = reader.every_bag_it_received().len();
    let restarted_domain = a_domain_root_of_its_own("restarts-source-again");
    let source = launch_the_source(&restarted_domain);
    source.wait_until_it_is_up();
    reader.wait_until("the link to carry again once its source returned", || {
        reader.every_bag_it_received().len() > bags_before_the_restart
    });
}

/// One source, two readers: the source holds exactly one egress however many
/// runtimes read the port, and keeps it until the last of them leaves.
///
/// What it catches, and what no other arm can: the egress table counts its
/// readers in a set, and every other arm drives that set with one reader — so
/// "the last reader leaving takes the egress with it" is satisfied trivially by
/// the only reader leaving. A second egress per reader, an egress torn down
/// when the first of two readers leaves, or a reader whose token never joined
/// the set all read identically with one reader and all break here.
#[test]
#[serial]
fn one_egress_serves_every_reader_and_outlives_all_but_the_last() {
    let mesh_name = a_mesh_name_of_its_own("two-readers");
    let source_name = "x-source-two-readers".to_string();
    let first_reader_name = "x-first-reader-two-readers".to_string();
    let second_reader_name = "x-second-reader-two-readers".to_string();
    let source_domain = a_domain_root_of_its_own("two-readers-source");
    let first_reader_domain = a_domain_root_of_its_own("two-readers-first");
    let second_reader_domain = a_domain_root_of_its_own("two-readers-second");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());
    let every_egress_token = format!("streamlib/{mesh_name}/@runtime/{source_name}/@egress/**");
    let every_reader_token = format!("streamlib/{mesh_name}/@runtime/{source_name}/@readers/**");

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        ..Default::default()
    });
    source.wait_until_it_is_up();

    let a_reader_of_the_port = |runtime_name: String, domain: &tempfile::TempDir| {
        let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
            reader: true,
            runtime_name,
            mesh_name: mesh_name.clone(),
            peer_endpoints: vec![source_listen.clone()],
            display_name: THE_DISPLAY_NAME.to_string(),
            link_from: Some(source_name.clone()),
            iceoryx2_domain_root: domain.path().to_path_buf(),
            ..Default::default()
        });
        reader.wait_until_it_is_up();
        reader
    };

    let mut first_reader = a_reader_of_the_port(first_reader_name, &first_reader_domain);
    first_reader.wait_until("the first reader to receive a bag", || {
        !first_reader.every_bag_it_received().is_empty()
    });
    let mut second_reader = a_reader_of_the_port(second_reader_name, &second_reader_domain);
    second_reader.wait_until("the second reader to receive a bag", || {
        !second_reader.every_bag_it_received().is_empty()
    });

    let looking = a_session_that_only_looks(&source_listen);
    let while_both_read = every_token_under(&looking, &every_reader_token);
    assert_eq!(
        while_both_read.len(),
        2,
        "each runtime reading the port declares its own reader token; the source saw \
         {while_both_read:?}"
    );
    let one_egress = every_token_under(&looking, &every_egress_token);
    assert_eq!(
        one_egress.len(),
        1,
        "a port is sent once however many runtimes read it; the source held {one_egress:?}"
    );

    // The first reader leaving is not the last: the egress stays, and the
    // reader still here goes on receiving across it.
    let bags_the_second_reader_had = second_reader.every_bag_it_received().len();
    first_reader.ask_it_to_leave();
    second_reader.wait_until(
        "the remaining reader to receive a bag after the other left",
        || second_reader.every_bag_it_received().len() > bags_the_second_reader_had,
    );
    let after_the_first_left = every_token_under(&looking, &every_egress_token);
    assert_eq!(
        after_the_first_left.len(),
        1,
        "a reader leaving while another still reads must not take the egress with it; the \
         source held {after_the_first_left:?}"
    );

    // The second is the last, and takes it with it.
    second_reader.ask_it_to_leave();
    let gave_up_at = Instant::now() + HOW_LONG_AN_ARM_WAITS;
    while Instant::now() < gave_up_at {
        if every_token_under(&looking, &every_egress_token).is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the last of two readers leaving must take the source's egress token with it");
}

/// One source and one reader, with a burst far larger than either ring, and
/// the identity the count exists to make true: over the stretch of bags the
/// reader's ingress delivered, every one that did not arrive is counted.
///
/// A conservation identity rather than a number: what the rings lose under a
/// burst is not reproducible, and an arm asserting a rate would be asserting
/// this machine's scheduling.
///
/// The reader polls a subscriber where a real destination has a counted
/// mailbox, so it accounts for that shallow local channel itself. The arm
/// requires it to have been there from its ingress's first bag, and says so by
/// name if it was not — without that, no span the hop count covers is knowable
/// from here.
///
/// What it catches: a count that misses the sending runtime's own ring, which
/// is what a second numbering minted at the egress would do; a count that
/// double-charges, which is what counting in the Zenoh callback as well as on
/// the writing thread would do; and a count that charges the hop for bags lost
/// after it.
#[test]
#[serial]
fn every_bag_a_burst_lost_between_two_runtimes_is_counted_on_the_link() {
    /// Far past the 16-bag channel depth and the ingress ring, published with
    /// no pause, so the egress cannot keep up and the loss is the rings'.
    const HOW_MANY_BAGS_THE_BURST_PUBLISHES: u64 = 4_000;

    let mesh_name = a_mesh_name_of_its_own("hop-loss");
    let (source_name, reader_name) = the_two_runtimes_of("hop-loss");
    let source_domain = a_domain_root_of_its_own("hop-loss-source");
    let reader_domain = a_domain_root_of_its_own("hop-loss-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        burst_once_a_reader_arrives: Some(HOW_MANY_BAGS_THE_BURST_PUBLISHES),
        ..Default::default()
    });
    source.wait_until_it_is_up();

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    let reached = wait_until_the_burst_is_behind_the_reader(&source, &reader);

    assert_eq!(
        reached.everything_accounted_for(),
        reached.the_span_its_ingress_covered(),
        "every bag between the first and the last the ingress delivered must have arrived or \
         been counted: {reached:?}"
    );
    assert!(
        reached.the_hop_lost > 0,
        "a {HOW_MANY_BAGS_THE_BURST_PUBLISHES}-bag burst through a 16-bag channel must lose \
         something between the runtimes, or this arm proves nothing: {reached:?}"
    );
}

/// The source replaces its port's publisher mid-stream and bursts afterwards.
/// The replacement numbers its own sends from zero, and the reading runtime
/// must not read that restart as loss.
///
/// **What this arm cannot do, stated so nobody reads more into it:** it cannot
/// be made to fail by deleting `publisher_generation` from the attachment — I
/// tried. A replacement publisher always restarts at zero
/// (`iceoryx2/output.rs`, `next_sequence_number: 0`), so with the old run's
/// last received number `s_a` and the new run's first received number `s_b`,
/// the gap a generation-blind reader would compute is `s_b - s_a - 1`, while
/// the bags really lost across the boundary are the old publisher's remaining
/// sends plus `s_b` — larger by the old publisher's own total, always. Omitting
/// the generation therefore under-states a boundary rather than inventing one,
/// and no conservation bound can see it. What it produces is a number with no
/// meaning, which is why the plan makes a new generation a baseline; that
/// arithmetic is pinned where it can be made red, by
/// `bags_a_gap_in_the_numbering_says_were_lost`'s
/// `a_new_run_is_a_baseline_even_once_its_numbering_has_overtaken`.
///
/// What this arm does prove, against two real runtimes and a real publisher
/// replacement: the grown attachment crosses the wire and is read at the far
/// end, the link keeps carrying after its producer is replaced, the burst's
/// loss is still counted under the new run, and the count never exceeds the
/// stretch it covers. A bound rather than the other arm's equality, because a
/// generation boundary's own loss is deliberately uncounted and so the count
/// legitimately falls short of the span.
#[test]
#[serial]
fn a_producer_recreated_mid_stream_is_a_baseline_and_not_a_gap() {
    const HOW_MANY_BAGS_THE_BURST_PUBLISHES: u64 = 2_000;

    let mesh_name = a_mesh_name_of_its_own("recreated");
    let (source_name, reader_name) = the_two_runtimes_of("recreated");
    let source_domain = a_domain_root_of_its_own("recreated-source");
    let reader_domain = a_domain_root_of_its_own("recreated-reader");
    let source_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let source = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: source_name.clone(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![source_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        iceoryx2_domain_root: source_domain.path().to_path_buf(),
        burst_once_a_reader_arrives: Some(HOW_MANY_BAGS_THE_BURST_PUBLISHES),
        recreate_the_publisher_just_before_the_burst: true,
        ..Default::default()
    });
    source.wait_until_it_is_up();

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name,
        peer_endpoints: vec![source_listen],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    let reached = wait_until_the_burst_is_behind_the_reader(&source, &reader);

    assert_eq!(
        source.how_many_publishers_its_port_has_had(),
        2,
        "the port's publisher must actually have been replaced, or the arm proves nothing"
    );
    assert!(
        reached.everything_accounted_for() <= reached.the_span_its_ingress_covered(),
        "a publisher replaced mid-stream must not make the hop count bags nobody sent: \
         {reached:?}"
    );
    assert!(
        reached.the_hop_lost > 0,
        "the burst after the replacement must still have its loss counted, or the arm says \
         nothing about a count that survives a new generation: {reached:?}"
    );
}

/// Wait until the burst is behind the reader, then hand back what reached it.
///
/// The source publishes unhurried bags either side of its burst, so the arm
/// waits for one of the trailing ones rather than for a quiet stretch: a
/// stalled burst also looks quiet, and reading the counts during a stall is
/// how this stopped being a proof the first time it was written. A trailing
/// bag arriving says the whole burst is behind it, drained and counted.
fn wait_until_the_burst_is_behind_the_reader(
    source: &CrossRuntimeLinkPeerProcess,
    reader: &CrossRuntimeLinkPeerProcess,
) -> WhatReachedTheReader {
    source.wait_until("the burst to be published", || {
        source.the_index_its_burst_ended_at().is_some()
    });
    let burst_ended_at = source
        .the_index_its_burst_ended_at()
        .expect("the source reported the index its burst ended at");

    reader.wait_until("a bag published after the burst to arrive", || {
        reader
            .what_last_reached_it()
            .last_bag_index
            .is_some_and(|last| last > burst_ended_at + 1)
    });

    let reached = reader.what_last_reached_it();
    assert_eq!(
        reached.bags_lost_before_its_first_poll, 0,
        "the reader must have been reading from its ingress's first bag, or the stretch the hop \
         count covers is not knowable from here: {reached:?}"
    );
    assert!(
        reached
            .first_bag_index
            .is_some_and(|first| first < burst_ended_at),
        "the run must start before the burst ended, or the burst is not inside it: {reached:?}"
    );
    reached
}

/// The source is killed after a burst its hop lost bags on, and comes back.
/// The link re-wires, and its hop count starts again from zero.
///
/// The plan restarts a remote link's loss count when its runtime returns: a
/// count carried across a re-wire would name bags a different hop lost, on a
/// wiring `graph` no longer has.
///
/// What it catches: minting the returning link's counter with
/// `counter_for_inbound_link` instead of `a_counter_for_a_fresh_wiring_of` —
/// the whole of the restart, and the one call site that implements it. The
/// returning source does not burst, so the zero this asserts is the count the
/// re-wire minted and not a lull.
#[test]
#[serial]
fn a_killed_sources_return_restarts_the_hop_count_from_zero() {
    const HOW_MANY_BAGS_THE_BURST_PUBLISHES: u64 = 4_000;

    let mesh_name = a_mesh_name_of_its_own("recount");
    let (source_name, reader_name) = the_two_runtimes_of("recount");
    let reader_domain = a_domain_root_of_its_own("recount-reader");
    let reader_listen = format!("udp/{LOOPBACK_INTERFACE}:{}?rel=1", a_free_loopback_port());

    let reader = CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
        reader: true,
        runtime_name: reader_name,
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![reader_listen.clone()],
        display_name: THE_DISPLAY_NAME.to_string(),
        link_from: Some(source_name.clone()),
        iceoryx2_domain_root: reader_domain.path().to_path_buf(),
        ..Default::default()
    });
    reader.wait_until_it_is_up();

    // A fresh domain each time, the restart arm's own reason: a killed process
    // leaves its iceoryx2 node holding the channel's one publisher slot.
    let launch_the_source = |domain: &tempfile::TempDir, burst: Option<u64>| {
        CrossRuntimeLinkPeerProcess::launch(HowToLaunchAPeer {
            runtime_name: source_name.clone(),
            mesh_name: mesh_name.clone(),
            peer_endpoints: vec![reader_listen.clone()],
            display_name: THE_DISPLAY_NAME.to_string(),
            iceoryx2_domain_root: domain.path().to_path_buf(),
            burst_once_a_reader_arrives: burst,
            ..Default::default()
        })
    };

    let bursting_domain = a_domain_root_of_its_own("recount-source");
    let mut source = launch_the_source(&bursting_domain, Some(HOW_MANY_BAGS_THE_BURST_PUBLISHES));
    source.wait_until_it_is_up();
    let lost_before_the_kill = wait_until_the_burst_is_behind_the_reader(&source, &reader);
    assert!(
        lost_before_the_kill.the_hop_lost > 0,
        "the first wiring must have lost something, or a zero afterwards says nothing: \
         {lost_before_the_kill:?}"
    );

    source.kill_it();
    let states_before_the_kill = reader.every_state_it_has_reported().len();
    reader.wait_until(
        "the link to return to waiting once its source is gone",
        || {
            reader
                .every_state_it_has_reported()
                .iter()
                .skip(states_before_the_kill)
                .any(|state| state == "awaiting_remote")
        },
    );

    // Steady this time, so what the count reads after the re-wire is the
    // restart and not a stretch where nothing happened to be lost yet.
    let returned_domain = a_domain_root_of_its_own("recount-source-again");
    let bags_before_the_restart = reader.every_bag_it_received().len();
    let source = launch_the_source(&returned_domain, None);
    source.wait_until_it_is_up();
    reader.wait_until("the link to carry again once its source returned", || {
        reader.every_bag_it_received().len() > bags_before_the_restart + 2
    });

    assert_eq!(
        reader.what_last_reached_it().the_hop_lost,
        0,
        "the returning link's count must start again from zero rather than carrying what a \
         previous wiring's hop lost"
    );
}

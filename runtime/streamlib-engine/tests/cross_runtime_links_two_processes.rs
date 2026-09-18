// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One link between two runtimes, in two OS processes — the proof a remote link
//! carries anything at all, and that a runtime does no work for a port nobody
//! reads.
//!
//! Serial, and each arm takes its own mesh name, its own runtime names, its own
//! loopback port and its own iceoryx2 domain. Four arms at once put eight
//! runtimes with real network endpoints on one loopback interface, which tests
//! the harness rather than the engine.
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

/// The runtime names one arm's two peers take.
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

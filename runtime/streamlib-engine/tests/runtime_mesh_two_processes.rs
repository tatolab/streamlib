// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Two runtimes, two OS processes, one mesh — the proof the mesh exists at all,
//! and that two of them never share one name while both are live.
//!
//! GPU-free: every arm constructs a `Runner` and never starts it, so this runs
//! in CI. Each arm takes its own mesh name, so arms never see each other even
//! while the transport connects them, and multicast is pinned to `127.0.0.1`
//! so a test never joins whatever network the machine is on.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use streamlib_engine::core::runtime::RuntimeMeshConfiguration;
use streamlib_engine::core::runtime::mesh::{
    AnnouncedRuntimeIdentity, HostIdentity, ResolvedRuntimeMeshConfiguration, RuntimeMeshKeySpace,
    RuntimeMeshName,
};
use zenoh::Wait;

/// How long an arm waits for two runtimes to see each other. Generous: a
/// scouting delay plus a description round trip on a loaded CI runner.
const HOW_LONG_A_PEER_HAS_TO_APPEAR: Duration = Duration::from_secs(30);

/// The variable that pins multicast scouting, so a test never scouts on the
/// machine's real network. Engine-internal, and deliberately on no CLI flag.
const MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_MESH_MULTICAST_INTERFACE";

/// Where scouting is pinned to.
const LOOPBACK_INTERFACE: &str = "127.0.0.1";

/// How many peers stop answering while their tokens stay live.
///
/// Each costs a stuck round the engine's own two-second description timeout.
/// Five rather than one, because how far into a round the leave lands is not
/// controllable — five leave a stuck round ahead of the leave however the phase
/// falls.
const HOW_MANY_PEERS_ARE_WEDGED: usize = 5;

/// How long to wait before asking a runtime to leave, so that a re-ask round is
/// already in flight and stuck. Past the engine's own five-second cadence.
const HOW_LONG_UNTIL_A_RE_ASK_ROUND_IS_STUCK: Duration = Duration::from_millis(5_500);

/// How long a runtime may take to leave the mesh.
///
/// Measured on this arm rather than chosen: a clean leave — stdin's end, the
/// token undeclared, the session closed and the process gone — takes 1.40 s,
/// reproducing to 0.04 s across runs, and a teardown that waits out the stuck
/// round takes 4.40 s. Three seconds sits between them with about a second and
/// a half either way, so a loaded runner does not red this and the regression
/// it exists for cannot pass it.
const HOW_LONG_A_TEARDOWN_MAY_TAKE: Duration = Duration::from_secs(3);

/// A mesh name no other arm and no other machine uses.
fn a_mesh_name_of_its_own(arm: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "t-{arm}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A loopback port nothing is listening on, released before it is named.
///
/// Racy in principle and settled in practice: the kernel does not hand the same
/// ephemeral port out twice in quick succession, and an arm that lost the race
/// would fail as local-only with the reason named rather than hanging.
fn a_free_loopback_port() -> u16 {
    std::net::TcpListener::bind((LOOPBACK_INTERFACE, 0))
        .expect("the loopback has a free port")
        .local_addr()
        .expect("a bound listener has an address")
        .port()
}

/// One peer process, with whatever it last reported about its mesh.
struct RuntimeMeshPeerProcess {
    child: Child,
    what_it_last_saw: Arc<Mutex<Option<serde_json::Value>>>,
    why_it_refused: Arc<Mutex<Option<String>>>,
    what_it_logged: Arc<Mutex<Vec<String>>>,
}

/// How a peer is launched — every flag the fixture drives.
#[derive(Default)]
struct HowToLaunchAPeer {
    runtime_name: String,
    mesh_name: String,
    peer_endpoints: Vec<String>,
    listen_endpoints: Vec<String>,
    multicast_discovery: bool,
    /// Let the engine's own pretty log reach the parent beside the reports, for
    /// an arm whose subject is something the runtime only ever says in a log.
    report_what_it_logs: bool,
}

impl RuntimeMeshPeerProcess {
    fn launch(how: HowToLaunchAPeer) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_runtime_mesh_peer"));
        command
            .arg("--runtime-name")
            .arg(&how.runtime_name)
            .arg("--mesh-name")
            .arg(&how.mesh_name)
            .arg("--multicast-discovery")
            .arg(if how.multicast_discovery { "on" } else { "off" });
        for endpoint in &how.peer_endpoints {
            command.arg("--mesh-peer").arg(endpoint);
        }
        for endpoint in &how.listen_endpoints {
            command.arg("--mesh-listen").arg(endpoint);
        }

        command.env(
            MESH_MULTICAST_INTERFACE_ENVIRONMENT_VARIABLE,
            LOOPBACK_INTERFACE,
        );
        // The peer reports down a duplicate of fd 1 and the engine's own pretty
        // log mirror shares the real one, so it is quiet unless an arm's subject
        // is something the runtime only says in a log.
        if !how.report_what_it_logs {
            command.env("STREAMLIB_QUIET", "1");
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the mesh peer binary launches");

        let what_it_last_saw = Arc::new(Mutex::new(None));
        let why_it_refused = Arc::new(Mutex::new(None));
        let what_it_logged = Arc::new(Mutex::new(Vec::new()));
        let reported = child.stdout.take().expect("the peer's stdout is piped");
        let saw = Arc::clone(&what_it_last_saw);
        let refused = Arc::clone(&why_it_refused);
        let logged = Arc::clone(&what_it_logged);
        std::thread::spawn(move || {
            for line in BufReader::new(reported).lines().map_while(Result::ok) {
                if let Some(refusal) = line.strip_prefix("REFUSED ") {
                    *refused.lock() = Some(refusal.to_string());
                } else if let Ok(mesh) = serde_json::from_str::<serde_json::Value>(&line) {
                    *saw.lock() = Some(mesh);
                } else {
                    // Whatever is left is the engine's pretty log, which only
                    // reaches here for an arm that asked for it.
                    logged.lock().push(line);
                }
            }
        });

        Self {
            child,
            what_it_last_saw,
            why_it_refused,
            what_it_logged,
        }
    }

    /// The names this peer currently sees, or an empty list until it sees any.
    fn peer_names_it_sees(&self) -> Vec<String> {
        let Some(mesh) = self.what_it_last_saw.lock().clone() else {
            return Vec::new();
        };
        mesh["peers"]
            .as_array()
            .map(|peers| {
                peers
                    .iter()
                    .filter_map(|peer| peer["runtime_name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What this peer last reported about its own mesh.
    fn what_it_last_saw(&self) -> Option<serde_json::Value> {
        self.what_it_last_saw.lock().clone()
    }

    /// Everything this peer knows about `runtime_name`, once it knows it.
    fn what_it_knows_about(&self, runtime_name: &str) -> Option<serde_json::Value> {
        self.what_it_last_saw.lock().clone().and_then(|mesh| {
            mesh["peers"]
                .as_array()?
                .iter()
                .find(|peer| peer["runtime_name"] == runtime_name)
                .cloned()
        })
    }

    /// Stop the peer's process without taking it off the mesh: its transport
    /// stays up so its token stays live, and it answers nothing.
    fn stop_answering_without_leaving(&self) {
        // SAFETY: `kill` takes a pid and a signal and reads nothing through a
        // pointer; the pid is this fixture's own child, still unreaped.
        let stopped = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGSTOP) };
        assert_eq!(stopped, 0, "the peer must take a SIGSTOP");
    }

    /// Ask the peer to leave cleanly: closing its stdin stops its runtime,
    /// which undeclares its token before closing the session.
    fn ask_it_to_leave_and_wait(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }

    /// Kill the peer outright, with no chance to undeclare anything — the
    /// crash a restart races.
    fn kill_it_and_wait(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Wait until the peer is constructed and reporting what it sees.
    fn wait_until_it_is_on_the_mesh(&self) {
        wait_until("the runtime reports its mesh", || self.what_it_last_saw());
    }

    /// Why this peer refused to be constructed, once it says so.
    fn wait_until_it_refuses(&self) -> String {
        wait_until("the runtime refuses its name", || {
            self.why_it_refused.lock().clone()
        })
    }

    /// The exit status of a peer that has stopped on its own.
    fn wait_for_its_exit_code(&mut self) -> Option<i32> {
        self.child.wait().expect("the peer exits").code()
    }

    /// Every line this peer logged that carries `what_it_said`.
    fn log_lines_carrying(&self, what_it_said: &str) -> Vec<String> {
        self.what_it_logged
            .lock()
            .iter()
            .filter(|line| line.contains(what_it_said))
            .cloned()
            .collect()
    }
}

impl Drop for RuntimeMeshPeerProcess {
    fn drop(&mut self) {
        // Whatever an arm did or failed to do, no peer outlives the test — a
        // SIGSTOPped one included, which `SIGKILL` reaps without resuming.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Poll until `what_it_should_see` holds, or fail saying what was seen instead.
fn wait_until<T: std::fmt::Debug>(
    what_is_being_waited_for: &str,
    mut read_it: impl FnMut() -> Option<T>,
) -> T {
    let deadline = Instant::now() + HOW_LONG_A_PEER_HAS_TO_APPEAR;
    loop {
        if let Some(seen) = read_it() {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "{what_is_being_waited_for} did not happen within {HOW_LONG_A_PEER_HAS_TO_APPEAR:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Both peers list each other, whichever way they were told to find one.
fn each_lists_the_other(one: &RuntimeMeshPeerProcess, other: &RuntimeMeshPeerProcess) {
    wait_until("the first runtime sees the second", || {
        Some(one.peer_names_it_sees()).filter(|names| !names.is_empty())
    });
    wait_until("the second runtime sees the first", || {
        Some(other.peer_names_it_sees()).filter(|names| !names.is_empty())
    });
}

/// Multicast discovery, pinned to the loopback: two runtimes told nothing
/// about each other beyond their mesh name find each other.
#[test]
fn two_runtimes_discovering_by_multicast_each_list_the_other() {
    let mesh_name = a_mesh_name_of_its_own("multicast");
    let one = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "peer-one".to_string(),
        mesh_name: mesh_name.clone(),
        multicast_discovery: true,
        ..Default::default()
    });
    let other = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "peer-two".to_string(),
        mesh_name,
        multicast_discovery: true,
        ..Default::default()
    });

    each_lists_the_other(&one, &other);
    assert_eq!(one.peer_names_it_sees(), ["peer-two"]);
    assert_eq!(other.peer_names_it_sees(), ["peer-one"]);
}

/// An explicit QUIC-over-UDP peer, discovery off: the arm that does not depend
/// on multicast reaching anything.
#[test]
fn two_runtimes_named_as_quic_peers_each_list_the_other_with_discovery_off() {
    let mesh_name = a_mesh_name_of_its_own("quic");
    let port = a_free_loopback_port();
    let listening = format!("udp/{LOOPBACK_INTERFACE}:{port}?rel=1");

    let one = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "quic-one".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![listening.clone()],
        ..Default::default()
    });
    let other = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "quic-two".to_string(),
        mesh_name,
        peer_endpoints: vec![listening],
        ..Default::default()
    });

    each_lists_the_other(&one, &other);
}

/// An explicit TCP peer, discovery off: what a network that blocks UDP gets.
#[test]
fn two_runtimes_named_as_tcp_peers_each_list_the_other_with_discovery_off() {
    let mesh_name = a_mesh_name_of_its_own("tcp");
    let port = a_free_loopback_port();
    let listening = format!("tcp/{LOOPBACK_INTERFACE}:{port}");

    let one = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "tcp-one".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![listening.clone()],
        ..Default::default()
    });
    let other = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "tcp-two".to_string(),
        mesh_name,
        peer_endpoints: vec![listening],
        ..Default::default()
    });

    each_lists_the_other(&one, &other);
}

/// A peer answers what it is, so `graph` carries more than a name.
#[test]
fn a_peer_answers_its_id_its_host_and_the_engine_version_it_runs() {
    let mesh_name = a_mesh_name_of_its_own("described");
    let port = a_free_loopback_port();
    let listening = format!("udp/{LOOPBACK_INTERFACE}:{port}?rel=1");

    let one = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "described-one".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![listening.clone()],
        ..Default::default()
    });
    let other = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "described-two".to_string(),
        mesh_name,
        peer_endpoints: vec![listening],
        ..Default::default()
    });
    each_lists_the_other(&one, &other);

    let described = wait_until("the first runtime learns what the second is", || {
        other
            .what_it_knows_about("described-one")
            .filter(|peer| peer.get("runtime_id").is_some())
    });

    assert!(
        described["runtime_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{described}"
    );
    assert!(described["host_name"].as_str().is_some(), "{described}");
    assert_eq!(
        described["engine_version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    // No control plane was hosted, so the peer names no URL — the key is still
    // there, because the peer answered.
    assert_eq!(
        described["control_plane_urls"],
        serde_json::json!([]),
        "{described}"
    );
}

/// A peer that leaves is gone from `graph`, rather than lingering as a runtime
/// nothing can reach.
#[test]
fn a_peer_that_leaves_is_gone_from_graph() {
    let mesh_name = a_mesh_name_of_its_own("leaving");
    let port = a_free_loopback_port();
    let listening = format!("udp/{LOOPBACK_INTERFACE}:{port}?rel=1");

    let one = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "staying".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![listening.clone()],
        ..Default::default()
    });
    let mut leaving = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "leaving".to_string(),
        mesh_name,
        peer_endpoints: vec![listening],
        ..Default::default()
    });
    each_lists_the_other(&one, &leaving);

    leaving.ask_it_to_leave_and_wait();

    wait_until(
        "the runtime that left is gone from the other's graph",
        || one.peer_names_it_sees().is_empty().then_some(()),
    );
}

/// Peers whose tokens are live and whose processes have stopped answering do
/// not hold another runtime's teardown open.
///
/// The discovery thread re-asks every known peer on a cadence, and an ask waits
/// out its own timeout for a peer that never answers; `stop()` joins that
/// thread. So the leave has to land *inside* a round that is already stuck —
/// hence the wait past the cadence — and the round has to give up when the
/// subscriber feeding it goes, or the wedged peers' timeouts are added to every
/// teardown beside them.
#[test]
fn wedged_peers_do_not_hold_another_runtimes_teardown_open() {
    let mesh_name = a_mesh_name_of_its_own("wedged");
    let port = a_free_loopback_port();
    let listening = format!("udp/{LOOPBACK_INTERFACE}:{port}?rel=1");

    let mut leaving = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "leaving-beside-wedged-peers".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![listening.clone()],
        ..Default::default()
    });

    let wedged: Vec<RuntimeMeshPeerProcess> = (0..HOW_MANY_PEERS_ARE_WEDGED)
        .map(|which| {
            RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
                runtime_name: format!("wedged-{which}"),
                mesh_name: mesh_name.clone(),
                peer_endpoints: vec![listening.clone()],
                ..Default::default()
            })
        })
        .collect();
    wait_until("the leaving runtime sees every wedged peer", || {
        (leaving.peer_names_it_sees().len() == HOW_MANY_PEERS_ARE_WEDGED).then_some(())
    });

    // SIGSTOP, not a kill: each process stops answering its description query
    // while its transport stays up, so its token never leaves. That is the
    // partitioned peer, reproduced without a partition.
    for peer in &wedged {
        peer.stop_answering_without_leaving();
    }
    // Past the engine's re-ask cadence, so a round is in flight and stuck on
    // the first wedged peer when the leave arrives. Without this the leave
    // lands between rounds and the arm proves nothing.
    std::thread::sleep(HOW_LONG_UNTIL_A_RE_ASK_ROUND_IS_STUCK);

    let asked_to_leave_at = Instant::now();
    leaving.ask_it_to_leave_and_wait();
    let how_long_leaving_took = asked_to_leave_at.elapsed();

    assert!(
        how_long_leaving_took < HOW_LONG_A_TEARDOWN_MAY_TAKE,
        "leaving took {how_long_leaving_took:?}, past the {HOW_LONG_A_TEARDOWN_MAY_TAKE:?} bound"
    );
}

/// Two mesh names see nothing of each other, even scouting the same group on
/// the same interface — the isolation lever the plan states.
#[test]
fn two_mesh_names_see_nothing_of_each_other() {
    let one = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "in-one-mesh".to_string(),
        mesh_name: a_mesh_name_of_its_own("isolated"),
        multicast_discovery: true,
        ..Default::default()
    });
    let other = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "in-another-mesh".to_string(),
        mesh_name: a_mesh_name_of_its_own("isolated"),
        multicast_discovery: true,
        ..Default::default()
    });

    // Both are up and scouting; neither has anything to report.
    wait_until("both runtimes report their mesh", || {
        one.what_it_last_saw().zip(other.what_it_last_saw())
    });
    std::thread::sleep(Duration::from_secs(3));

    assert!(
        one.peer_names_it_sees().is_empty(),
        "{:?}",
        one.what_it_last_saw()
    );
    assert!(
        other.peer_names_it_sees().is_empty(),
        "{:?}",
        other.what_it_last_saw()
    );
}

/// A runtime scouting nobody is isolated rather than local-only: its session is
/// open and simply reaches nothing.
#[test]
fn a_runtime_with_discovery_off_and_no_peers_is_isolated_rather_than_local_only() {
    let alone = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "alone".to_string(),
        mesh_name: a_mesh_name_of_its_own("alone"),
        ..Default::default()
    });

    let mesh = wait_until("the runtime reports its mesh", || alone.what_it_last_saw());
    assert_eq!(mesh["session"], "open", "{mesh}");
    assert_eq!(mesh["peers"], serde_json::json!([]), "{mesh}");
    assert!(mesh.get("local_only_reason").is_none(), "{mesh}");
}

/// A listen endpoint somebody else holds leaves the runtime local-only, with
/// the reason named — and the runtime is still constructed and still running.
#[test]
fn a_taken_listen_endpoint_gives_a_local_only_runtime_that_still_starts() {
    let held =
        std::net::TcpListener::bind((LOOPBACK_INTERFACE, 0)).expect("the loopback has a free port");
    let taken = format!(
        "tcp/{LOOPBACK_INTERFACE}:{}",
        held.local_addr().expect("a bound listener").port()
    );

    let refused = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "local-only".to_string(),
        mesh_name: a_mesh_name_of_its_own("local-only"),
        listen_endpoints: vec![taken],
        ..Default::default()
    });

    let mesh = wait_until("the local-only runtime reports its mesh", || {
        refused.what_it_last_saw()
    });
    assert_eq!(mesh["session"], "local_only", "{mesh}");
    assert!(
        mesh["local_only_reason"]
            .as_str()
            .is_some_and(|it| !it.is_empty()),
        "a local-only runtime must say why: {mesh}"
    );
    assert!(
        refused.why_it_refused.lock().is_none(),
        "the runtime must still be constructed"
    );
}

/// The mesh object `graph` carries has exactly the keys the plan states, and no
/// more — the shape every reader deserializes.
#[test]
fn the_mesh_key_carries_exactly_what_the_plan_states() {
    let mesh_name = a_mesh_name_of_its_own("shape");
    let alone = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "shaped".to_string(),
        mesh_name: mesh_name.clone(),
        ..Default::default()
    });

    let mesh = wait_until("the runtime reports its mesh", || alone.what_it_last_saw());
    let keys: Vec<&String> = mesh
        .as_object()
        .expect("the mesh is an object")
        .keys()
        .collect();
    assert_eq!(keys, ["mesh_name", "runtime_name", "session", "peers"]);
    assert_eq!(mesh["mesh_name"], mesh_name);
    assert_eq!(mesh["runtime_name"], "shaped");
}

/// A `quic/` endpoint names a transport this build does not carry, and a plain
/// `udp/` one has no retransmission — both refuse the runtime at construction,
/// naming what is wrong, rather than leaving it local-only.
#[test]
fn an_endpoint_this_build_cannot_open_refuses_the_runtime_at_construction() {
    for (endpoint, what_the_refusal_must_name) in [
        ("quic/127.0.0.1:7447", "quic"),
        ("udp/127.0.0.1:7447", "rel=1"),
    ] {
        let mut refused = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
            runtime_name: "refused".to_string(),
            mesh_name: a_mesh_name_of_its_own("refused"),
            peer_endpoints: vec![endpoint.to_string()],
            ..Default::default()
        });

        let refusal = wait_until(&format!("{endpoint} is refused"), || {
            refused.why_it_refused.lock().clone()
        });
        assert!(
            refusal.contains(what_the_refusal_must_name) && refusal.contains(endpoint),
            "the refusal of {endpoint} must name {what_the_refusal_must_name}: {refusal}"
        );
        let exit = refused.child.wait().expect("the refused peer exits");
        assert_eq!(
            exit.code(),
            Some(2),
            "a refused runtime exits by its refusal"
        );
    }
}

/// A liveliness token this test holds on a mesh under a stated name, host and
/// pid — the key a runtime's duplicate-name check reads.
///
/// It is written through the engine's own key space rather than spelled here:
/// a second reading of the grammar beside the check would pass while the check
/// looked somewhere else entirely, which is the one thing these arms exist to
/// catch.
struct ATokenHeldUnderAName {
    _session: zenoh::Session,
    _token: zenoh::liveliness::LivelinessToken,
}

impl ATokenHeldUnderAName {
    /// Hold `runtime_name` on `mesh_name`, announced by `held_by`, over a
    /// session listening at `listening` for the runtime under test to dial.
    fn declared(
        mesh_name: &str,
        runtime_name: &str,
        held_by: HostIdentity,
        process_id: u32,
        listening: &str,
    ) -> Self {
        let mesh_name =
            RuntimeMeshName::from_configuration_environment_or_default(Some(mesh_name.to_string()))
                .expect("a legal mesh name");
        let key_space = RuntimeMeshKeySpace::of(mesh_name.clone());

        // Every value stated, so nothing here is read out of the test process's
        // own environment: this session must reach exactly the peer that dials
        // it and nothing else on the machine.
        let configuration = ResolvedRuntimeMeshConfiguration::resolve(RuntimeMeshConfiguration {
            mesh_name: Some(mesh_name.to_string()),
            mesh_peer_endpoints: Some(Vec::new()),
            mesh_listen_endpoints: Some(vec![listening.to_string()]),
            mesh_multicast_discovery: Some(false),
            ..Default::default()
        })
        .expect("a resolvable mesh configuration");

        let session = zenoh::open(
            configuration
                .as_a_zenoh_configuration()
                .expect("a Zenoh configuration"),
        )
        .wait()
        .expect("a session on the loopback");
        let token = session
            .liveliness()
            .declare_token(key_space.announcement_key_for(&AnnouncedRuntimeIdentity {
                runtime_name: runtime_name.to_string(),
                host_identity: held_by,
                process_id,
            }))
            .wait()
            .expect("a liveliness token");

        Self {
            _session: session,
            _token: token,
        }
    }
}

/// A pid this host has already reaped, so the same-host exception applies to it.
fn a_process_id_on_this_host_that_has_exited() -> u32 {
    let mut exited = std::process::Command::new("true")
        .spawn()
        .expect("this host runs a process");
    let process_id = exited.id();
    exited.wait().expect("the process is reaped");
    process_id
}

/// A second runtime under a name a live one already holds does not start, and
/// says enough for somebody to act on it.
#[test]
fn a_second_runtime_of_a_live_name_is_refused_naming_the_holders_host_and_pid() {
    let mesh_name = a_mesh_name_of_its_own("duplicate");
    let port = a_free_loopback_port();
    let listening = format!("udp/{LOOPBACK_INTERFACE}:{port}?rel=1");

    let holding_the_name = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "held-name".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![listening.clone()],
        ..Default::default()
    });
    holding_the_name.wait_until_it_is_on_the_mesh();

    let mut refused = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "held-name".to_string(),
        mesh_name,
        peer_endpoints: vec![listening],
        ..Default::default()
    });

    let refusal = refused.wait_until_it_refuses();
    assert!(refusal.contains("held-name"), "{refusal}");
    assert!(
        refusal.contains(&format!("pid {}", holding_the_name.child.id())),
        "the refusal must name the pid holding the name: {refusal}"
    );
    assert!(
        refusal.contains("this host"),
        "the holder is on this host and the refusal must say so: {refusal}"
    );
    assert!(refusal.contains("streamlib nodes"), "{refusal}");
    assert!(refusal.contains("--runtime-name"), "{refusal}");
    assert_eq!(
        refused.wait_for_its_exit_code(),
        Some(2),
        "a refused runtime exits by its refusal"
    );
    assert!(
        holding_the_name.what_it_last_saw().is_some(),
        "the runtime holding the name keeps running"
    );
}

/// Killing the runtime holding a name frees it at once: the exception exists
/// for exactly the restart that races its predecessor's exit.
#[test]
fn a_name_is_free_the_moment_the_runtime_holding_it_is_killed() {
    let mesh_name = a_mesh_name_of_its_own("killed");
    let port = a_free_loopback_port();
    let listening = format!("udp/{LOOPBACK_INTERFACE}:{port}?rel=1");

    let mut killed = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "restarted".to_string(),
        mesh_name: mesh_name.clone(),
        listen_endpoints: vec![listening.clone()],
        ..Default::default()
    });
    killed.wait_until_it_is_on_the_mesh();

    // Refused while it is alive, so the arm below is not passing for want of a
    // check rather than for want of a holder.
    let mut while_it_lives = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "restarted".to_string(),
        mesh_name: mesh_name.clone(),
        peer_endpoints: vec![listening.clone()],
        ..Default::default()
    });
    while_it_lives.wait_until_it_refuses();
    while_it_lives.wait_for_its_exit_code();

    killed.kill_it_and_wait();

    let restarted = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "restarted".to_string(),
        mesh_name,
        listen_endpoints: vec![listening],
        ..Default::default()
    });
    restarted.wait_until_it_is_on_the_mesh();
    assert!(
        restarted.why_it_refused.lock().is_none(),
        "the name must be free the moment its holder is gone"
    );
}

/// A token left on the mesh by a process on this host that is gone is taken
/// over, and the same token under a live pid is not — the control is what makes
/// the arm above it non-vacuous.
#[test]
fn a_token_left_by_a_dead_process_on_this_host_is_taken_over_and_a_live_one_is_not() {
    for (what_holds_it, process_id, it_may_start) in [
        (
            "a process that has exited",
            a_process_id_on_this_host_that_has_exited(),
            true,
        ),
        ("this very test process", std::process::id(), false),
    ] {
        let mesh_name = a_mesh_name_of_its_own("stale");
        let port = a_free_loopback_port();
        let listening = format!("tcp/{LOOPBACK_INTERFACE}:{port}");

        // Declared before the runtime dials, because a token declared onto an
        // existing connection takes a moment to propagate — which is the
        // discovery window the plan states as a residual.
        let _held = ATokenHeldUnderAName::declared(
            &mesh_name,
            "left-behind",
            HostIdentity::of_this_host(),
            process_id,
            &listening,
        );

        let mut taking_it_over = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
            runtime_name: "left-behind".to_string(),
            mesh_name,
            peer_endpoints: vec![listening],
            ..Default::default()
        });

        if it_may_start {
            taking_it_over.wait_until_it_is_on_the_mesh();
            assert!(
                taking_it_over.why_it_refused.lock().is_none(),
                "a name held by {what_holds_it} must be free"
            );
        } else {
            let refusal = taking_it_over.wait_until_it_refuses();
            assert!(
                refusal.contains("left-behind") && refusal.contains(&format!("pid {process_id}")),
                "a name held by {what_holds_it} must be refused naming it: {refusal}"
            );
            assert_eq!(taking_it_over.wait_for_its_exit_code(), Some(2));
        }
    }
}

/// The stated residual: two runtimes that meet only after both have started are
/// not refused. Both keep running, each says so once naming the other, and each
/// lists the other under the one name.
///
/// Arranged rather than raced — the second runtime dials a port nothing is
/// listening on yet, so its own check finds nobody, and the first appears
/// afterwards and is connected to by the retry.
///
/// This is the one arm that reads the peers' logs, because saying so once is
/// the whole of what the runtime does here: assert it on `graph` alone and the
/// production call could be deleted with every test still green.
#[test]
fn two_runtimes_that_meet_after_both_started_both_run_and_each_says_so_once() {
    let mesh_name = a_mesh_name_of_its_own("residual");
    let port = a_free_loopback_port();
    let listening = format!("tcp/{LOOPBACK_INTERFACE}:{port}");

    let dialling_nobody_yet = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "one-name-two-runtimes".to_string(),
        mesh_name: mesh_name.clone(),
        peer_endpoints: vec![listening.clone()],
        report_what_it_logs: true,
        ..Default::default()
    });
    dialling_nobody_yet.wait_until_it_is_on_the_mesh();

    let appearing_afterwards = RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
        runtime_name: "one-name-two-runtimes".to_string(),
        mesh_name,
        listen_endpoints: vec![listening],
        report_what_it_logs: true,
        ..Default::default()
    });
    appearing_afterwards.wait_until_it_is_on_the_mesh();

    each_lists_the_other(&dialling_nobody_yet, &appearing_afterwards);
    assert_eq!(
        dialling_nobody_yet.peer_names_it_sees(),
        ["one-name-two-runtimes"]
    );
    assert_eq!(
        appearing_afterwards.peer_names_it_sees(),
        ["one-name-two-runtimes"]
    );
    for runtime in [&dialling_nobody_yet, &appearing_afterwards] {
        assert!(
            runtime.why_it_refused.lock().is_none(),
            "neither runtime is refused: they never saw each other in time"
        );
    }

    // Each names the *other* process, and says it once however many re-ask
    // rounds go by — so both the naming and the once are locked.
    for (runtime, the_other) in [
        (&dialling_nobody_yet, &appearing_afterwards),
        (&appearing_afterwards, &dialling_nobody_yet),
    ] {
        let said = wait_until("the runtime says it shares its name", || {
            Some(runtime.log_lines_carrying("is also named one-name-two-runtimes"))
                .filter(|lines| !lines.is_empty())
        });
        assert_eq!(said.len(), 1, "said more than once: {said:?}");
        assert!(
            said[0].contains(&format!("pid {}", the_other.child.id())),
            "must name the other runtime's pid: {}",
            said[0]
        );
    }
}

/// Isolated runtimes never refuse each other, however many share one name:
/// with discovery off and no peers there is nobody to see, which is the
/// isolation lever working rather than a hole in the check.
#[test]
fn isolated_runtimes_sharing_one_name_never_refuse_each_other() {
    let mesh_name = a_mesh_name_of_its_own("isolated-namesakes");
    let namesakes: Vec<RuntimeMeshPeerProcess> = (0..3)
        .map(|_| {
            RuntimeMeshPeerProcess::launch(HowToLaunchAPeer {
                runtime_name: "one-name-many-isolated-runtimes".to_string(),
                mesh_name: mesh_name.clone(),
                ..Default::default()
            })
        })
        .collect();

    for runtime in &namesakes {
        runtime.wait_until_it_is_on_the_mesh();
        assert!(
            runtime.why_it_refused.lock().is_none(),
            "an isolated runtime has nobody to be refused by"
        );
        assert!(runtime.peer_names_it_sees().is_empty());
    }
}

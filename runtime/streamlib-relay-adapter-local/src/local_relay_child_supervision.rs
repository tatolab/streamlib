// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One `moq-relay` child, kept running by the runtime's
//! [`SupervisedChildProcess`] until it is stopped.
//!
//! Its environment holds nothing `tatolabd` inherited, so no `MOQ_*` variable
//! overrides its configuration file, and a child a crashed `tatolabd` left
//! behind is ended before a new one starts, so its port is never left bound.
//! The relay is joined once its HTTP listener accepts.

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use streamlib::sdk::supervised_child_process::{
    SupervisedChildProcess, SupervisedChildProcessEvent, SupervisedChildProcessLaunch,
};
use streamlib_relay_adapter_sdk::{RelayConnectionState, RelayInstanceId};

/// How long a stopped child has to end on `SIGTERM` before it is killed.
const STOPPED_CHILD_GRACE: Duration = Duration::from_secs(3);

/// How long one probe of whether the child accepts waits for its answer.
const CHILD_ACCEPT_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// What one child is started from.
#[derive(Debug, Clone)]
pub struct LocalRelayChildLaunch {
    /// The relay instance the child is.
    pub relay_id: RelayInstanceId,
    /// The `moq-relay` executable, canonical.
    pub relay_executable: PathBuf,
    /// The configuration file the child is started with.
    pub configuration_path: PathBuf,
    /// Where the running child's process id is written.
    pub pidfile_path: PathBuf,
    /// The address the child accepts TCP on once it is up — its HTTP
    /// listener — before which it is not reported joined.
    pub address_accepting_once_up: SocketAddr,
}

/// The supervisor keeping one relay child running, and the means to stop it.
pub struct LocalRelayChildSupervisor {
    supervised_child_process: SupervisedChildProcess,
}

impl LocalRelayChildSupervisor {
    /// End a child a previous run left behind, then start the child and keep
    /// it running, reporting each change to `connection_state`.
    pub fn start(
        launch: LocalRelayChildLaunch,
        connection_state: tokio::sync::watch::Sender<RelayConnectionState>,
    ) -> std::io::Result<Self> {
        let address_accepting_once_up = launch.address_accepting_once_up;
        let supervised_child_process = SupervisedChildProcess::start(
            SupervisedChildProcessLaunch {
                child_name_for_the_log: format!("the local relay {}", launch.relay_id),
                executable: launch.relay_executable,
                arguments: vec![launch.configuration_path.into()],
                environment: vec![("NO_COLOR".into(), "1".into())],
                pidfile_path: launch.pidfile_path,
                runs_user_code: false,
                grace_after_sigterm: STOPPED_CHILD_GRACE,
            },
            Box::new(move || {
                TcpStream::connect_timeout(&address_accepting_once_up, CHILD_ACCEPT_PROBE_TIMEOUT)
                    .is_ok()
            }),
            Box::new(move |event| {
                if let Some(state) = relay_connection_state_of(event) {
                    connection_state.send_replace(state);
                }
            }),
        )?;
        Ok(Self {
            supervised_child_process,
        })
    }

    /// The running child's process id, `None` between children.
    pub fn running_child_process_id(&self) -> Option<u32> {
        self.supervised_child_process.running_child_process_id()
    }

    /// Stop the child — `SIGTERM`, then `SIGKILL` once its grace runs out —
    /// and the supervisor with it, and remove the pidfile.
    pub fn stop(&mut self) {
        self.supervised_child_process.stop();
    }
}

/// The relay's connection state a supervisor event moves it to, `None` when
/// it stays where it is.
fn relay_connection_state_of(event: SupervisedChildProcessEvent) -> Option<RelayConnectionState> {
    match event {
        SupervisedChildProcessEvent::Started { .. } => None,
        SupervisedChildProcessEvent::Ready => Some(RelayConnectionState::Joined),
        SupervisedChildProcessEvent::NotRunning {
            why,
            starting_again_after,
        } => Some(RelayConnectionState::Lost {
            why,
            rejoining_after: starting_again_after,
        }),
        SupervisedChildProcessEvent::GivenUp { .. } | SupervisedChildProcessEvent::Stopped => {
            Some(RelayConnectionState::NotJoined)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Instant;

    /// A copy of `sleep` under a name of its own, so a test can tell the
    /// process it started from every other `sleep` on the machine.
    fn a_stand_in_relay_executable(directory: &Path) -> PathBuf {
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .expect("the test host has `sleep`");
        let stand_in = directory.join("moq-relay");
        std::fs::copy(sleep, &stand_in).unwrap();
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
        stand_in.canonicalize().unwrap()
    }

    /// A launch whose stand-in child is reported up once `accepting` accepts.
    fn launch_in(
        directory: &Path,
        relay_executable: PathBuf,
        accepting: &TcpListener,
    ) -> LocalRelayChildLaunch {
        LocalRelayChildLaunch {
            relay_id: RelayInstanceId("local-loopback".to_string()),
            relay_executable,
            configuration_path: PathBuf::from("600"),
            pidfile_path: directory.join("moq-relay.pid"),
            address_accepting_once_up: accepting.local_addr().unwrap(),
        }
    }

    fn a_listener_standing_in_for_the_childs() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").unwrap()
    }

    fn wait_until(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        condition()
    }

    #[test]
    fn the_relay_is_joined_once_it_accepts_and_not_joined_once_stopped() {
        let directory = tempfile::tempdir().unwrap();
        let accepting = a_listener_standing_in_for_the_childs();
        let launch = launch_in(
            directory.path(),
            a_stand_in_relay_executable(directory.path()),
            &accepting,
        );
        let (connection_state, connection_state_receiver) =
            tokio::sync::watch::channel(RelayConnectionState::Joining);
        let mut supervisor =
            LocalRelayChildSupervisor::start(launch.clone(), connection_state).unwrap();

        assert!(wait_until(Duration::from_secs(10), || {
            supervisor.running_child_process_id().is_some() && launch.pidfile_path.is_file()
        }));
        assert!(wait_until(Duration::from_secs(10), || {
            *connection_state_receiver.borrow() == RelayConnectionState::Joined
        }));

        supervisor.stop();
        assert!(!launch.pidfile_path.exists());
        assert_eq!(
            *connection_state_receiver.borrow(),
            RelayConnectionState::NotJoined
        );
    }

    #[test]
    fn a_child_that_accepts_nothing_is_never_reported_joined() {
        let directory = tempfile::tempdir().unwrap();
        let unbound_address = {
            let probe = a_listener_standing_in_for_the_childs();
            probe.local_addr().unwrap()
        };
        let mut launch = launch_in(
            directory.path(),
            a_stand_in_relay_executable(directory.path()),
            &a_listener_standing_in_for_the_childs(),
        );
        launch.address_accepting_once_up = unbound_address;
        let (connection_state, connection_state_receiver) =
            tokio::sync::watch::channel(RelayConnectionState::Joining);
        let mut supervisor = LocalRelayChildSupervisor::start(launch, connection_state).unwrap();
        assert!(wait_until(Duration::from_secs(10), || supervisor
            .running_child_process_id()
            .is_some()));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            *connection_state_receiver.borrow(),
            RelayConnectionState::Joining
        );
        supervisor.stop();
    }

    #[test]
    fn a_child_that_exits_is_lost_then_started_again_after_a_backoff() {
        let directory = tempfile::tempdir().unwrap();
        let accepting = a_listener_standing_in_for_the_childs();
        let launch = launch_in(
            directory.path(),
            a_stand_in_relay_executable(directory.path()),
            &accepting,
        );
        let (connection_state, mut connection_state_receiver) =
            tokio::sync::watch::channel(RelayConnectionState::Joining);
        let mut supervisor =
            LocalRelayChildSupervisor::start(launch.clone(), connection_state).unwrap();
        assert!(wait_until(Duration::from_secs(10), || supervisor
            .running_child_process_id()
            .is_some()));
        let first_process_id = supervisor.running_child_process_id().unwrap();
        connection_state_receiver.mark_unchanged();

        // SAFETY: `kill` takes scalars and touches no memory of this process.
        unsafe { libc::kill(first_process_id as libc::pid_t, libc::SIGKILL) };
        assert!(wait_until(Duration::from_secs(10), || matches!(
            *connection_state_receiver.borrow(),
            RelayConnectionState::Lost { .. }
        )));
        assert!(wait_until(Duration::from_secs(10), || {
            supervisor
                .running_child_process_id()
                .is_some_and(|process_id| process_id != first_process_id)
        }));
        supervisor.stop();
    }

    #[test]
    fn each_supervisor_event_moves_the_relay_to_its_connection_state() {
        assert_eq!(
            relay_connection_state_of(SupervisedChildProcessEvent::Started { process_id: 7 }),
            None
        );
        assert_eq!(
            relay_connection_state_of(SupervisedChildProcessEvent::Ready),
            Some(RelayConnectionState::Joined)
        );
        assert_eq!(
            relay_connection_state_of(SupervisedChildProcessEvent::NotRunning {
                why: "it exited with signal: 9".to_string(),
                starting_again_after: Duration::from_secs(2),
            }),
            Some(RelayConnectionState::Lost {
                why: "it exited with signal: 9".to_string(),
                rejoining_after: Duration::from_secs(2),
            })
        );
        assert_eq!(
            relay_connection_state_of(SupervisedChildProcessEvent::Stopped),
            Some(RelayConnectionState::NotJoined)
        );
    }
}

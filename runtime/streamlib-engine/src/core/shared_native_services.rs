// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Native services the runtime runs once for the whole machine and lends to
//! its nodes: the browser host every `BrowserSource` opens its tabs in.
//!
//! The runtime's host offers a service by name, executable and state
//! directory. The first lease on it starts it under one
//! [`SupervisedChildProcess`]; it is kept running while any lease is held,
//! and stopped a grace after the last is given back, so a stream that reloads
//! keeps the same service.
//!
//! Launch contract: a service is started as
//! `<executable> --listen-socket <socket> --state-directory <directory>`, with
//! an environment holding only the variables of this process that a native
//! child needs, and is ready once a connect on the socket succeeds. `SIGTERM`
//! asks it to end.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crate::core::error::{Error, Result};
use crate::core::supervised_child_process::{
    SupervisedChildProcess, SupervisedChildProcessEvent, SupervisedChildProcessLaunch,
};

/// The switch naming the socket a shared native service listens on.
pub const SHARED_NATIVE_SERVICE_LISTEN_SOCKET_SWITCH: &str = "--listen-socket";

/// The switch naming the directory a shared native service keeps its state in.
pub const SHARED_NATIVE_SERVICE_STATE_DIRECTORY_SWITCH: &str = "--state-directory";

/// How long a service is kept running after its last lease is given back.
pub const SHARED_NATIVE_SERVICE_GRACE_AFTER_THE_LAST_LEASE: Duration = Duration::from_secs(30);

/// How long a stopped service has to end on `SIGTERM` before it is killed.
const SHARED_NATIVE_SERVICE_GRACE_AFTER_SIGTERM: Duration = Duration::from_secs(10);

/// How long the first lease waits for a starting service to accept.
const SHARED_NATIVE_SERVICE_FIRST_READINESS_BUDGET: Duration = Duration::from_secs(30);

/// The variables of this process a native service is started with: what a
/// display, an audio server and the system's own libraries are found by.
const SHARED_NATIVE_SERVICE_INHERITED_ENVIRONMENT_VARIABLES: &[&str] = &[
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "PATH",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "PULSE_SERVER",
    "DBUS_SESSION_BUS_ADDRESS",
    "RUST_LOG",
];

/// A native service the runtime's host offers its nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedNativeServiceOffer {
    /// The name a node leases it by.
    pub service_name: String,
    /// The executable, from the runtime unit.
    pub executable: PathBuf,
    /// Where it keeps what outlives it, such as each stream's browser profile.
    pub state_directory: PathBuf,
}

/// Whether a shared native service is there to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedNativeServiceState {
    /// Started, and not yet accepting.
    Starting,
    /// Accepting connections.
    Running,
    /// It ended, and starts again after a backoff.
    StartingAgain,
    /// It is not started again, and why.
    Failed(String),
    /// No lease holds it, and it is not running.
    Stopped,
}

/// The services the runtime's host offered, and the running ones.
pub(crate) struct SharedNativeServicesOfTheEngine {
    services_directory: PathBuf,
    services_by_name: Mutex<BTreeMap<String, OneSharedNativeService>>,
    state_changed: Condvar,
    grace_after_the_last_lease: Duration,
    first_readiness_budget: Duration,
}

struct OneSharedNativeService {
    offer: SharedNativeServiceOffer,
    leases_held: usize,
    state: SharedNativeServiceState,
    supervisor: Option<SupervisedChildProcess>,
    /// Set while a supervisor taken out of the record is stopping, so a lease
    /// waits for it before starting a successor on the same pidfile.
    previous_supervisor_is_stopping: bool,
    /// Bumped by each start, so a stopped supervisor's last events are not
    /// taken for its successor's.
    supervisor_generation: u64,
    /// Bumped by each lease, so a grace stop scheduled before it does nothing.
    lease_generation: u64,
}

/// One lease on a shared native service; dropping it gives it back.
pub(crate) struct SharedNativeServiceLease {
    services: Weak<SharedNativeServicesOfTheEngine>,
    service_name: String,
    endpoint_socket_path: PathBuf,
}

impl SharedNativeServicesOfTheEngine {
    /// None offered yet; each service's socket and pidfile go in
    /// `services_directory`.
    pub(crate) fn in_directory(services_directory: PathBuf) -> Arc<Self> {
        Self::with_timings(
            services_directory,
            SHARED_NATIVE_SERVICE_GRACE_AFTER_THE_LAST_LEASE,
            SHARED_NATIVE_SERVICE_FIRST_READINESS_BUDGET,
        )
    }

    fn with_timings(
        services_directory: PathBuf,
        grace_after_the_last_lease: Duration,
        first_readiness_budget: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            services_directory,
            services_by_name: Mutex::new(BTreeMap::new()),
            state_changed: Condvar::new(),
            grace_after_the_last_lease,
            first_readiness_budget,
        })
    }

    /// Offer `offer` to every node, refusing a second offer of one name.
    pub(crate) fn offer(&self, offer: SharedNativeServiceOffer) -> Result<()> {
        let mut services_by_name = self.lock_services();
        if let Some(offered) = services_by_name.get(&offer.service_name) {
            return Err(Error::Runtime(format!(
                "the shared native service {} is already offered from {}, so {} was refused",
                offer.service_name,
                offered.offer.executable.display(),
                offer.executable.display()
            )));
        }
        services_by_name.insert(
            offer.service_name.clone(),
            OneSharedNativeService {
                offer,
                leases_held: 0,
                state: SharedNativeServiceState::Stopped,
                supervisor: None,
                previous_supervisor_is_stopping: false,
                supervisor_generation: 0,
                lease_generation: 0,
            },
        );
        Ok(())
    }

    /// The socket `service_name` listens on.
    pub(crate) fn endpoint_socket_path_of(&self, service_name: &str) -> PathBuf {
        self.services_directory.join(format!("{service_name}.sock"))
    }

    fn pidfile_path_of(&self, service_name: &str) -> PathBuf {
        self.services_directory.join(format!("{service_name}.pid"))
    }

    /// Lease `service_name`, starting it when no lease holds it, and wait for
    /// a service starting, for the first time or again, to accept.
    pub(crate) fn lease(self: &Arc<Self>, service_name: &str) -> Result<SharedNativeServiceLease> {
        let readiness_deadline = Instant::now() + self.first_readiness_budget;
        let mut services_by_name = self.lock_services();
        loop {
            let service = services_by_name.get(service_name).ok_or_else(|| {
                Error::Runtime(format!(
                    "no shared native service named {service_name} is offered by this runtime's \
                     host"
                ))
            })?;
            if !service.previous_supervisor_is_stopping {
                break;
            }
            let now = Instant::now();
            if now >= readiness_deadline {
                return Err(Error::Runtime(format!(
                    "the shared native service {service_name} was still stopping after {:?}",
                    self.first_readiness_budget
                )));
            }
            services_by_name = self
                .state_changed
                .wait_timeout(services_by_name, readiness_deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        let Some(service) = services_by_name.get_mut(service_name) else {
            return Err(Error::Runtime(format!(
                "no shared native service named {service_name} is offered by this runtime's host"
            )));
        };
        if let SharedNativeServiceState::Failed(why) = &service.state {
            return Err(Error::Runtime(format!(
                "the shared native service {service_name} failed and is not started again: {why}"
            )));
        }
        service.leases_held += 1;
        service.lease_generation += 1;
        if service.supervisor.is_none() {
            service.supervisor_generation += 1;
            match self.start_the_service(&service.offer, service.supervisor_generation) {
                Ok(supervisor) => {
                    service.supervisor = Some(supervisor);
                    service.state = SharedNativeServiceState::Starting;
                }
                Err(failure) => {
                    service.leases_held -= 1;
                    return Err(failure);
                }
            }
        }
        let lease = SharedNativeServiceLease {
            services: Arc::downgrade(self),
            service_name: service_name.to_string(),
            endpoint_socket_path: self.endpoint_socket_path_of(service_name),
        };
        loop {
            let state = services_by_name
                .get(service_name)
                .map(|service| service.state.clone());
            match state {
                Some(
                    SharedNativeServiceState::Starting | SharedNativeServiceState::StartingAgain,
                ) => {}
                Some(SharedNativeServiceState::Failed(why)) => {
                    drop(services_by_name);
                    drop(lease);
                    return Err(Error::Runtime(format!(
                        "the shared native service {service_name} failed as it started: {why}"
                    )));
                }
                _ => return Ok(lease),
            }
            let now = Instant::now();
            if now >= readiness_deadline {
                drop(services_by_name);
                drop(lease);
                return Err(Error::Runtime(format!(
                    "the shared native service {service_name} did not accept within {:?}",
                    self.first_readiness_budget
                )));
            }
            services_by_name = self
                .state_changed
                .wait_timeout(services_by_name, readiness_deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Whether `service_name` is there to connect to.
    pub(crate) fn state_of(&self, service_name: &str) -> SharedNativeServiceState {
        self.lock_services()
            .get(service_name)
            .map_or(SharedNativeServiceState::Stopped, |service| {
                service.state.clone()
            })
    }

    /// Stop every running service, whatever leases it, as the engine shuts
    /// down.
    pub(crate) fn stop_every_service(&self) {
        let supervisors: Vec<(String, SupervisedChildProcess)> = self
            .lock_services()
            .iter_mut()
            .filter_map(|(service_name, service)| {
                let supervisor = service.supervisor.take()?;
                service.previous_supervisor_is_stopping = true;
                Some((service_name.clone(), supervisor))
            })
            .collect();
        for (service_name, supervisor) in supervisors {
            self.stop_the_service_and_remove_its_socket(&service_name, supervisor);
        }
    }

    fn stop_the_service_and_remove_its_socket(
        &self,
        service_name: &str,
        mut supervisor: SupervisedChildProcess,
    ) {
        supervisor.stop();
        let mut services_by_name = self.lock_services();
        let Some(service) = services_by_name.get_mut(service_name) else {
            return;
        };
        if service.supervisor.is_none() {
            let endpoint_socket_path = self.endpoint_socket_path_of(service_name);
            match std::fs::remove_file(&endpoint_socket_path) {
                Ok(()) => {}
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                Err(failure) => tracing::warn!(
                    "the stopped shared native service {service_name}'s socket {} was not \
                     removed: {failure}",
                    endpoint_socket_path.display()
                ),
            }
        }
        service.previous_supervisor_is_stopping = false;
        self.state_changed.notify_all();
    }

    fn start_the_service(
        self: &Arc<Self>,
        offer: &SharedNativeServiceOffer,
        supervisor_generation: u64,
    ) -> Result<SupervisedChildProcess> {
        for directory in [&self.services_directory, &offer.state_directory] {
            std::fs::create_dir_all(directory).map_err(|failure| {
                Error::Runtime(format!(
                    "{} for the shared native service {} could not be created: {failure}",
                    directory.display(),
                    offer.service_name
                ))
            })?;
        }
        let endpoint_socket_path = self.endpoint_socket_path_of(&offer.service_name);
        let launch = SupervisedChildProcessLaunch {
            child_name_for_the_log: format!("the shared native service {}", offer.service_name),
            executable: offer.executable.clone(),
            arguments: vec![
                SHARED_NATIVE_SERVICE_LISTEN_SOCKET_SWITCH.into(),
                endpoint_socket_path.clone().into(),
                SHARED_NATIVE_SERVICE_STATE_DIRECTORY_SWITCH.into(),
                offer.state_directory.clone().into(),
            ],
            environment: the_environment_a_shared_native_service_inherits(),
            pidfile_path: self.pidfile_path_of(&offer.service_name),
            gives_up_at_the_crash_loop_limit: true,
            grace_after_sigterm: SHARED_NATIVE_SERVICE_GRACE_AFTER_SIGTERM,
        };
        let services = Arc::downgrade(self);
        let service_name = offer.service_name.clone();
        SupervisedChildProcess::start(
            launch,
            Box::new(move || a_connect_on_the_socket_succeeds(&endpoint_socket_path)),
            Box::new(move |event| {
                if let Some(services) = services.upgrade() {
                    services.record_what_became_of(&service_name, supervisor_generation, event);
                }
            }),
        )
        .map_err(|failure| {
            Error::Runtime(format!(
                "the supervisor of the shared native service {} did not start: {failure}",
                offer.service_name
            ))
        })
    }

    fn record_what_became_of(
        &self,
        service_name: &str,
        supervisor_generation: u64,
        event: SupervisedChildProcessEvent,
    ) {
        let mut services_by_name = self.lock_services();
        let Some(service) = services_by_name.get_mut(service_name) else {
            return;
        };
        if service.supervisor_generation != supervisor_generation {
            return;
        }
        service.state = match event {
            SupervisedChildProcessEvent::Started { .. } => match service.state {
                SharedNativeServiceState::Starting | SharedNativeServiceState::Stopped => {
                    SharedNativeServiceState::Starting
                }
                _ => SharedNativeServiceState::StartingAgain,
            },
            SupervisedChildProcessEvent::Ready => SharedNativeServiceState::Running,
            SupervisedChildProcessEvent::NotRunning { .. } => match service.state {
                SharedNativeServiceState::Starting => SharedNativeServiceState::Starting,
                _ => SharedNativeServiceState::StartingAgain,
            },
            SupervisedChildProcessEvent::GivenUp { why } => SharedNativeServiceState::Failed(why),
            SupervisedChildProcessEvent::Stopped => SharedNativeServiceState::Stopped,
        };
        drop(services_by_name);
        self.state_changed.notify_all();
    }

    fn give_back_one_lease_of(self: &Arc<Self>, service_name: &str) {
        let mut services_by_name = self.lock_services();
        let Some(service) = services_by_name.get_mut(service_name) else {
            return;
        };
        service.leases_held = service.leases_held.saturating_sub(1);
        if service.leases_held > 0 || service.supervisor.is_none() {
            return;
        }
        let lease_generation_at_the_last_release = service.lease_generation;
        drop(services_by_name);
        let services = Arc::downgrade(self);
        let service_name = service_name.to_string();
        let grace_after_the_last_lease = self.grace_after_the_last_lease;
        let scheduled = std::thread::Builder::new()
            .name("service-grace".to_string())
            .spawn(move || {
                std::thread::sleep(grace_after_the_last_lease);
                if let Some(services) = services.upgrade() {
                    services.stop_unless_leased_since(
                        &service_name,
                        lease_generation_at_the_last_release,
                    );
                }
            });
        if let Err(failure) = scheduled {
            tracing::warn!(
                "the stop of the unleased shared native service could not be scheduled, so it \
                 runs until the engine shuts down: {failure}"
            );
        }
    }

    fn stop_unless_leased_since(&self, service_name: &str, lease_generation: u64) {
        let supervisor = {
            let mut services_by_name = self.lock_services();
            let Some(service) = services_by_name.get_mut(service_name) else {
                return;
            };
            if service.leases_held > 0 || service.lease_generation != lease_generation {
                return;
            }
            let supervisor = service.supervisor.take();
            service.previous_supervisor_is_stopping = supervisor.is_some();
            supervisor
        };
        if let Some(supervisor) = supervisor {
            tracing::info!(
                "the shared native service {service_name} is stopped, no node having leased it \
                 for {:?}",
                self.grace_after_the_last_lease
            );
            self.stop_the_service_and_remove_its_socket(service_name, supervisor);
        }
    }

    fn lock_services(&self) -> MutexGuard<'_, BTreeMap<String, OneSharedNativeService>> {
        self.services_by_name
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SharedNativeServiceLease {
    /// The socket the service accepts connections on.
    pub(crate) fn endpoint_socket_path(&self) -> &Path {
        &self.endpoint_socket_path
    }

    /// Whether the leased service is there to connect to.
    pub(crate) fn state(&self) -> SharedNativeServiceState {
        self.services
            .upgrade()
            .map_or(SharedNativeServiceState::Stopped, |services| {
                services.state_of(&self.service_name)
            })
    }
}

impl Drop for SharedNativeServiceLease {
    fn drop(&mut self) {
        if let Some(services) = self.services.upgrade() {
            services.give_back_one_lease_of(&self.service_name);
        }
    }
}

impl std::fmt::Debug for SharedNativeServiceLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedNativeServiceLease")
            .field("service_name", &self.service_name)
            .field("endpoint_socket_path", &self.endpoint_socket_path)
            .finish()
    }
}

fn the_environment_a_shared_native_service_inherits() -> Vec<(OsString, OsString)> {
    SHARED_NATIVE_SERVICE_INHERITED_ENVIRONMENT_VARIABLES
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| ((*name).into(), value)))
        .collect()
}

fn a_connect_on_the_socket_succeeds(endpoint_socket_path: &Path) -> bool {
    UnixStream::connect(endpoint_socket_path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A shell script standing in for a service: it binds its socket through
    /// Python and serves until `SIGTERM`.
    fn a_stand_in_service_executable(directory: &Path) -> PathBuf {
        a_stand_in_service_executable_that_exits_on_sigterm_after(directory, 0.0)
    }

    fn a_stand_in_service_executable_that_exits_on_sigterm_after(
        directory: &Path,
        seconds_before_exiting: f64,
    ) -> PathBuf {
        let executable = directory.join("stand-in-service");
        std::fs::write(
            &executable,
            format!(
            "#!/usr/bin/env python3\n\
             import signal, socket, sys, os, time\n\
             listen = sys.argv[sys.argv.index('--listen-socket') + 1]\n\
             try:\n    os.unlink(listen)\nexcept FileNotFoundError:\n    pass\n\
             server = socket.socket(socket.AF_UNIX)\n\
             server.bind(listen)\n\
             server.listen(8)\n\
             signal.signal(signal.SIGTERM, lambda *_: (time.sleep({seconds_before_exiting}), sys.exit(0)))\n\
             while True:\n    server.accept()[0].close()\n",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        executable.canonicalize().unwrap()
    }

    fn services_offering_the_stand_in(
        directory: &Path,
        grace_after_the_last_lease: Duration,
    ) -> Arc<SharedNativeServicesOfTheEngine> {
        let services = SharedNativeServicesOfTheEngine::with_timings(
            directory.join("services"),
            grace_after_the_last_lease,
            SHARED_NATIVE_SERVICE_FIRST_READINESS_BUDGET,
        );
        services
            .offer(SharedNativeServiceOffer {
                service_name: "stand-in".to_string(),
                executable: a_stand_in_service_executable(directory),
                state_directory: directory.join("state"),
            })
            .unwrap();
        services
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
    fn the_first_lease_starts_the_service_and_returns_once_it_accepts() {
        let directory = tempfile::tempdir().unwrap();
        let services = services_offering_the_stand_in(directory.path(), Duration::from_secs(60));
        let lease = services.lease("stand-in").unwrap();
        assert_eq!(lease.state(), SharedNativeServiceState::Running);
        assert!(UnixStream::connect(lease.endpoint_socket_path()).is_ok());
        assert!(directory.path().join("state").is_dir());
        services.stop_every_service();
    }

    #[test]
    fn a_second_lease_shares_the_running_service() {
        let directory = tempfile::tempdir().unwrap();
        let services = services_offering_the_stand_in(directory.path(), Duration::from_secs(60));
        let first = services.lease("stand-in").unwrap();
        let process_id =
            std::fs::read_to_string(directory.path().join("services/stand-in.pid")).unwrap();
        let second = services.lease("stand-in").unwrap();
        assert_eq!(first.endpoint_socket_path(), second.endpoint_socket_path());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("services/stand-in.pid")).unwrap(),
            process_id
        );
        services.stop_every_service();
    }

    #[test]
    fn the_service_stops_a_grace_after_its_last_lease_unless_leased_again() {
        let directory = tempfile::tempdir().unwrap();
        let services = services_offering_the_stand_in(directory.path(), Duration::from_millis(300));
        let pidfile = directory.path().join("services/stand-in.pid");
        let endpoint_socket = directory.path().join("services/stand-in.sock");

        drop(services.lease("stand-in").unwrap());
        let leased_again = services.lease("stand-in").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            services.state_of("stand-in"),
            SharedNativeServiceState::Running
        );
        assert!(pidfile.is_file());

        drop(leased_again);
        assert!(wait_until(Duration::from_secs(15), || {
            services.state_of("stand-in") == SharedNativeServiceState::Stopped
        }));
        assert!(!pidfile.exists());
        assert!(wait_until(Duration::from_secs(5), || !endpoint_socket.exists()));
    }

    #[test]
    fn a_lease_taken_while_the_service_starts_again_returns_once_it_accepts() {
        let directory = tempfile::tempdir().unwrap();
        let services = services_offering_the_stand_in(directory.path(), Duration::from_secs(60));
        let first = services.lease("stand-in").unwrap();
        let process_id: i32 =
            std::fs::read_to_string(directory.path().join("services/stand-in.pid"))
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .parse()
                .unwrap();
        // SAFETY: the pid names the stand-in this test's supervisor started.
        unsafe { libc::kill(process_id, libc::SIGKILL) };
        assert!(wait_until(Duration::from_secs(10), || {
            first.state() == SharedNativeServiceState::StartingAgain
        }));

        let leased_while_starting_again = services.lease("stand-in").unwrap();

        assert_eq!(
            leased_while_starting_again.state(),
            SharedNativeServiceState::Running
        );
        assert!(UnixStream::connect(leased_while_starting_again.endpoint_socket_path()).is_ok());
        services.stop_every_service();
    }

    #[test]
    fn a_lease_taken_while_the_unleased_service_stops_starts_it_once_it_has_stopped() {
        let directory = tempfile::tempdir().unwrap();
        let services = SharedNativeServicesOfTheEngine::with_timings(
            directory.path().join("services"),
            Duration::from_millis(100),
            SHARED_NATIVE_SERVICE_FIRST_READINESS_BUDGET,
        );
        services
            .offer(SharedNativeServiceOffer {
                service_name: "stand-in".to_string(),
                executable: a_stand_in_service_executable_that_exits_on_sigterm_after(
                    directory.path(),
                    1.0,
                ),
                state_directory: directory.path().join("state"),
            })
            .unwrap();
        drop(services.lease("stand-in").unwrap());
        assert!(wait_until(Duration::from_secs(10), || {
            services
                .lock_services()
                .get("stand-in")
                .is_some_and(|service| service.previous_supervisor_is_stopping)
        }));

        let leased_while_stopping = services.lease("stand-in").unwrap();

        assert_eq!(
            leased_while_stopping.state(),
            SharedNativeServiceState::Running
        );
        assert!(UnixStream::connect(leased_while_stopping.endpoint_socket_path()).is_ok());
        services.stop_every_service();
    }

    #[test]
    fn a_service_that_was_not_offered_or_offered_twice_is_refused_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let services = services_offering_the_stand_in(directory.path(), Duration::from_secs(60));
        let refusal = services.lease("browser-host").unwrap_err().to_string();
        assert!(
            refusal.contains("no shared native service named browser-host"),
            "{refusal}"
        );
        let refusal = services
            .offer(SharedNativeServiceOffer {
                service_name: "stand-in".to_string(),
                executable: PathBuf::from("/elsewhere"),
                state_directory: PathBuf::from("/elsewhere-state"),
            })
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("already offered"), "{refusal}");
    }

    #[test]
    fn a_lease_on_a_service_that_never_accepts_is_refused_after_its_budget() {
        let directory = tempfile::tempdir().unwrap();
        let readiness_budget = Duration::from_secs(2);
        let services = SharedNativeServicesOfTheEngine::with_timings(
            directory.path().join("services"),
            Duration::from_secs(60),
            readiness_budget,
        );
        services
            .offer(SharedNativeServiceOffer {
                service_name: "missing".to_string(),
                executable: directory.path().join("no-such-executable"),
                state_directory: directory.path().join("state"),
            })
            .unwrap();
        // The supervisor keeps trying a service that ships with the runtime,
        // so the lease is refused by its readiness budget.
        let started = Instant::now();
        let refusal = services.lease("missing").unwrap_err().to_string();
        assert!(refusal.contains("did not accept within"), "{refusal}");
        assert!(started.elapsed() >= readiness_budget);
        services.stop_every_service();
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-share service over a raw Mach channel.
//!
//! A client opens a connection by sending a connect message to the service's
//! bootstrap name. The service answers an admitted sender with a request port
//! minted for that connection alone and watches the client's reply port for
//! death; everything after the connect rides that unlisted port, and every
//! message is still checked against the connection's audit identity.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use mach2::port::mach_port_name_t;
use objc2_io_surface::IOSurfaceRef;
use parking_lot::Mutex;
use streamlib_surface_client::{
    OwnedMachPortSet, OwnedMachReceiveRight, OwnedMachSendRight, ReceivedSurfaceShareMachMessage,
    ReceivedSurfaceShareMachTraffic, SURFACE_SHARE_HAS_CONSUME_DONE_PORT,
    SURFACE_SHARE_HAS_PRODUCE_DONE_PORT, SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID,
    SURFACE_SHARE_MACH_REPLY_MESSAGE_ID, SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
    SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE, SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED,
    SurfaceShareMachMessageReceiveBuffer, SurfaceShareMachSenderAuditIdentity,
    check_in_surface_share_mach_service, receive_surface_share_mach_traffic,
    request_dead_name_notification, send_surface_share_mach_message,
};

use crate::apple::iosurface::{
    RetainedIOSurfaceSharedAcrossThreads, create_iosurface_mach_send_right,
};
use crate::core::context::SurfaceCheckOutLeaseHolderId;
use crate::core::context::surface_share_wire_verbs::{
    answer_release_check_out, answer_unregister, latch_the_first_named_runtime_id,
    record_check_out_lease_or_refusal, refusal_of_a_retired_frame_id,
    release_what_a_closed_connection_held, requested_runtime_id, requested_surface_id,
};

use super::state::{IOSurfaceShareRegistration, IOSurfaceShareState, SharedTimelineSendRights};

/// How long a connect from a pid nobody has admitted yet waits before it is
/// refused. A spawner learns its child's pid only after the child is
/// running, so the child's connect can arrive before its admission does.
const UNADMITTED_CONNECTION_WAIT_BUDGET: Duration = Duration::from_secs(5);

/// Connects allowed to wait for admission at once; more are refused.
const MAX_CONNECTIONS_WAITING_FOR_ADMISSION: usize = 64;

/// How long the service thread waits on a connection's full reply queue
/// before giving the connection up — one wedged client must not stall every
/// other one.
const REPLY_SEND_TIMEOUT: Duration = Duration::from_secs(1);

/// A connect answer or refusal goes to a reply port its sender just made, so
/// a full queue there is the sender stalling the service on purpose.
const CONNECT_ANSWER_SEND_TIMEOUT: Duration = Duration::ZERO;

const SERVICE_STOP_MESSAGE_ID: i32 = 0x534C_5310;
const SERVICE_ADMISSIONS_CHANGED_MESSAGE_ID: i32 = 0x534C_5311;

/// Wire value of `handle_type` for a surface whose handle is an IOSurface
/// Mach port.
const SURFACE_HANDLE_TYPE_IOSURFACE: &str = "iosurface";

/// Who may connect besides this process: the helper processes it spawned,
/// admitted by pid.
#[derive(Clone, Default)]
pub struct SurfaceShareHelperProcessAdmissions {
    inner: Arc<SurfaceShareHelperProcessAdmissionsInner>,
}

#[derive(Default)]
struct SurfaceShareHelperProcessAdmissionsInner {
    admitted_helper_processes: Mutex<HashMap<libc::pid_t, AdmittedHelperProcess>>,
    service_to_wake: Mutex<Option<OwnedMachSendRight>>,
}

struct AdmittedHelperProcess {
    outstanding_admissions: usize,
    /// The pid version the process first connected with. Anything later
    /// claiming the pid under another version is a different process that
    /// inherited a reused pid.
    pinned_pidversion: Option<i32>,
}

/// One helper process's admission, withdrawn on drop.
///
/// Hold it until the spawner has reaped the process: from then on its pid
/// may belong to any process on the machine.
#[must_use = "dropping the admission withdraws it; hold it until the child is reaped"]
pub struct SurfaceShareHelperProcessAdmission {
    admissions: SurfaceShareHelperProcessAdmissions,
    pid: libc::pid_t,
}

impl Drop for SurfaceShareHelperProcessAdmission {
    fn drop(&mut self) {
        self.admissions.withdraw_one_admission_of(self.pid);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SurfaceShareConnectionAdmissionVerdict {
    Admitted,
    NotYetAdmitted,
    Refused(String),
}

impl SurfaceShareHelperProcessAdmissions {
    /// Admit the helper process `pid` this process spawned.
    pub fn admit_helper_process(&self, pid: u32) -> SurfaceShareHelperProcessAdmission {
        let pid = pid as libc::pid_t;
        self.inner
            .admitted_helper_processes
            .lock()
            .entry(pid)
            .or_insert(AdmittedHelperProcess {
                outstanding_admissions: 0,
                pinned_pidversion: None,
            })
            .outstanding_admissions += 1;
        self.wake_the_service();
        SurfaceShareHelperProcessAdmission {
            admissions: self.clone(),
            pid,
        }
    }

    fn withdraw_one_admission_of(&self, pid: libc::pid_t) {
        let mut admitted_helper_processes = self.inner.admitted_helper_processes.lock();
        if let Some(admitted) = admitted_helper_processes.get_mut(&pid) {
            admitted.outstanding_admissions -= 1;
            if admitted.outstanding_admissions == 0 {
                admitted_helper_processes.remove(&pid);
            }
        }
    }

    /// Whether `sender` may open a connection. This process always may; a
    /// helper process may once admitted, and only under the pid version it
    /// first connected with.
    fn verdict_for(
        &self,
        sender: SurfaceShareMachSenderAuditIdentity,
        this_process_pid: libc::pid_t,
    ) -> SurfaceShareConnectionAdmissionVerdict {
        if sender.pid == this_process_pid {
            return SurfaceShareConnectionAdmissionVerdict::Admitted;
        }
        let mut admitted_helper_processes = self.inner.admitted_helper_processes.lock();
        let Some(admitted) = admitted_helper_processes.get_mut(&sender.pid) else {
            return SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted;
        };
        match admitted.pinned_pidversion {
            None => {
                admitted.pinned_pidversion = Some(sender.pidversion);
                SurfaceShareConnectionAdmissionVerdict::Admitted
            }
            Some(pinned) if pinned == sender.pidversion => {
                SurfaceShareConnectionAdmissionVerdict::Admitted
            }
            Some(pinned) => SurfaceShareConnectionAdmissionVerdict::Refused(format!(
                "pid {} is admitted as pid version {pinned}, and this sender is pid version {} — \
                 a different process on a reused pid",
                sender.pid, sender.pidversion
            )),
        }
    }

    /// Where an admission wakes the service thread, so a connect already
    /// waiting on it is answered at once; `None` once the service stops.
    fn set_service_to_wake(&self, service_control_send_right: Option<OwnedMachSendRight>) {
        *self.inner.service_to_wake.lock() = service_control_send_right;
    }

    fn wake_the_service(&self) {
        if let Some(service) = self.inner.service_to_wake.lock().as_ref() {
            // A full control queue already holds a wake-up, which is all
            // this would add.
            let _ = send_surface_share_mach_message(
                service,
                None,
                SERVICE_ADMISSIONS_CHANGED_MESSAGE_ID,
                b"{}",
                Vec::new(),
                Some(Duration::ZERO),
            );
        }
    }
}

/// What a spawner needs from the service to start a helper process: the
/// name the child connects to, and where to admit the child's pid.
#[derive(Clone)]
pub struct MachSurfaceShareServiceRendezvous {
    service_name: String,
    helper_process_admissions: SurfaceShareHelperProcessAdmissions,
}

impl MachSurfaceShareServiceRendezvous {
    /// The bootstrap name to hand the child, as
    /// `STREAMLIB_SURFACE_MACH_SERVICE`.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Admit the helper process `pid` this process spawned; see
    /// [`SurfaceShareHelperProcessAdmission`] for how long to hold it.
    pub fn admit_helper_process(&self, pid: u32) -> SurfaceShareHelperProcessAdmission {
        self.helper_process_admissions.admit_helper_process(pid)
    }
}

impl std::fmt::Debug for MachSurfaceShareServiceRendezvous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachSurfaceShareServiceRendezvous")
            .field("service_name", &self.service_name)
            .finish()
    }
}

/// Per-runtime surface-sharing service over a raw Mach channel.
pub struct MachSurfaceShareService {
    state: IOSurfaceShareState,
    service_name: String,
    helper_process_admissions: SurfaceShareHelperProcessAdmissions,
    unadmitted_connection_wait_budget: Duration,
    service_control_send_right: Option<OwnedMachSendRight>,
    service_thread: Option<thread::JoinHandle<()>>,
}

impl MachSurfaceShareService {
    /// The bootstrap name a runtime's service registers under.
    pub fn service_name_for_runtime(runtime_id: &str) -> String {
        format!("com.tatolab.streamlib.surface-share.{runtime_id}")
    }

    /// A service over `state` that will register as `service_name`.
    pub fn new(state: IOSurfaceShareState, service_name: String) -> Self {
        Self {
            state,
            service_name,
            helper_process_admissions: SurfaceShareHelperProcessAdmissions::default(),
            unadmitted_connection_wait_budget: UNADMITTED_CONNECTION_WAIT_BUDGET,
            service_control_send_right: None,
            service_thread: None,
        }
    }

    /// Refuse a connect from a pid nobody admitted after `wait_budget`
    /// rather than the default five seconds.
    pub fn with_unadmitted_connection_wait_budget(mut self, wait_budget: Duration) -> Self {
        self.unadmitted_connection_wait_budget = wait_budget;
        self
    }

    /// The bootstrap name clients connect to.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// The name and admissions a spawner hands on, detached from the
    /// service's own lifetime.
    pub fn rendezvous(&self) -> MachSurfaceShareServiceRendezvous {
        MachSurfaceShareServiceRendezvous {
            service_name: self.service_name.clone(),
            helper_process_admissions: self.helper_process_admissions.clone(),
        }
    }

    /// Register the bootstrap name and start serving on a thread of the
    /// service's own. A name a live process already holds is
    /// [`io::ErrorKind::AddrInUse`].
    pub fn start(&mut self) -> io::Result<()> {
        let connect_receive_right = check_in_surface_share_mach_service(&self.service_name)?;
        let port_set = OwnedMachPortSet::allocate()?;
        port_set.insert_member(&connect_receive_right)?;
        let control_receive_right = OwnedMachReceiveRight::allocate()?;
        port_set.insert_member(&control_receive_right)?;
        let peer_death_notification_receive_right = OwnedMachReceiveRight::allocate()?;
        port_set.insert_member(&peer_death_notification_receive_right)?;
        let service_control_send_right = control_receive_right.make_send_right()?;
        self.helper_process_admissions
            .set_service_to_wake(Some(service_control_send_right.try_clone()?));

        let service_loop = MachSurfaceShareServiceLoop {
            state: self.state.clone(),
            helper_process_admissions: self.helper_process_admissions.clone(),
            unadmitted_connection_wait_budget: self.unadmitted_connection_wait_budget,
            this_process_pid: std::process::id() as libc::pid_t,
            port_set,
            connect_receive_right,
            control_receive_right,
            peer_death_notification_receive_right,
            connected_peers: HashMap::new(),
            connections_waiting_for_admission: Vec::new(),
            receive_buffer: SurfaceShareMachMessageReceiveBuffer::new(),
        };
        let service_thread = thread::Builder::new()
            .name("streamlib-surface-share-mach".to_string())
            .spawn(move || service_loop.run())?;
        self.service_thread = Some(service_thread);
        self.service_control_send_right = Some(service_control_send_right);

        tracing::info!(
            "[Surface share] Mach surface service registered as '{}'",
            self.service_name
        );
        Ok(())
    }

    /// Stop serving. The bootstrap name goes, and every connected client
    /// sees the service die.
    pub fn stop(&mut self) {
        self.helper_process_admissions.set_service_to_wake(None);
        if let Some(service_control_send_right) = self.service_control_send_right.take()
            && let Err(unsent) = send_surface_share_mach_message(
                &service_control_send_right,
                None,
                SERVICE_STOP_MESSAGE_ID,
                b"{}",
                Vec::new(),
                Some(REPLY_SEND_TIMEOUT),
            )
            // A service thread that already stopped left the control port
            // dead; one that cannot take the message in time is wedged, and
            // joining it would wedge this caller too.
            && unsent.kind() == io::ErrorKind::TimedOut
        {
            tracing::error!(
                "[Surface share] the Mach surface service '{}' did not take its stop message \
                 and is left running: {}",
                self.service_name,
                unsent
            );
            self.service_thread.take();
            return;
        }
        if let Some(service_thread) = self.service_thread.take() {
            let _ = service_thread.join();
            tracing::info!(
                "[Surface share] Mach surface service '{}' stopped",
                self.service_name
            );
        }
    }
}

impl Drop for MachSurfaceShareService {
    fn drop(&mut self) {
        self.stop();
    }
}

struct ConnectedSurfaceSharePeer {
    /// Destroying it is how a closed connection tells its client: the
    /// client's request port becomes a dead name.
    request_receive_right: OwnedMachReceiveRight,
    reply_send_right: OwnedMachSendRight,
    sender: SurfaceShareMachSenderAuditIdentity,
    lease_holder: SurfaceCheckOutLeaseHolderId,
    observed_runtime_id: Option<String>,
}

struct ConnectionWaitingForAdmission {
    reply_send_right: OwnedMachSendRight,
    sender: SurfaceShareMachSenderAuditIdentity,
    admission_deadline: Instant,
}

struct MachSurfaceShareServiceLoop {
    state: IOSurfaceShareState,
    helper_process_admissions: SurfaceShareHelperProcessAdmissions,
    unadmitted_connection_wait_budget: Duration,
    this_process_pid: libc::pid_t,
    port_set: OwnedMachPortSet,
    connect_receive_right: OwnedMachReceiveRight,
    control_receive_right: OwnedMachReceiveRight,
    peer_death_notification_receive_right: OwnedMachReceiveRight,
    connected_peers: HashMap<mach_port_name_t, ConnectedSurfaceSharePeer>,
    connections_waiting_for_admission: Vec<ConnectionWaitingForAdmission>,
    receive_buffer: SurfaceShareMachMessageReceiveBuffer,
}

impl MachSurfaceShareServiceLoop {
    fn run(mut self) {
        loop {
            let until_the_next_admission_deadline = self
                .connections_waiting_for_admission
                .iter()
                .map(|waiting| waiting.admission_deadline)
                .min()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()));
            match receive_surface_share_mach_traffic(
                self.port_set.as_raw_name(),
                &mut self.receive_buffer,
                until_the_next_admission_deadline,
            ) {
                Ok(ReceivedSurfaceShareMachTraffic::Message(message)) => {
                    if message.received_on_port == self.control_receive_right.as_raw_name() {
                        if message.sender.pid != self.this_process_pid {
                            tracing::warn!(
                                "[Surface share] ignored a control message from pid {}",
                                message.sender.pid
                            );
                        } else if message.message_id == SERVICE_STOP_MESSAGE_ID {
                            break;
                        } else {
                            self.settle_connections_waiting_for_admission();
                        }
                    } else if message.received_on_port == self.connect_receive_right.as_raw_name() {
                        self.answer_connect(message);
                    } else {
                        self.answer_peer_request(message);
                    }
                }
                Ok(ReceivedSurfaceShareMachTraffic::DeadName { dead_name }) => {
                    self.forget_every_client_answering_on(dead_name.as_raw_name());
                }
                Ok(_) => {}
                Err(timed_out) if timed_out.kind() == io::ErrorKind::TimedOut => {}
                Err(malformed) if malformed.kind() == io::ErrorKind::InvalidData => {
                    tracing::warn!("[Surface share] {}", malformed);
                }
                Err(receive_failure) => {
                    tracing::error!(
                        "[Surface share] the Mach surface service can no longer receive, and \
                         stops: {}",
                        receive_failure
                    );
                    break;
                }
            }
            self.refuse_connections_past_their_admission_deadline();
        }
    }

    fn answer_connect(&mut self, message: ReceivedSurfaceShareMachMessage) {
        let ReceivedSurfaceShareMachMessage {
            message_id,
            reply_send_right,
            sender,
            ..
        } = message;
        if message_id != SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID {
            tracing::warn!(
                "[Surface share] ignored message id {:#x} from pid {} on the connect port",
                message_id,
                sender.pid
            );
            return;
        }
        let Some(reply_send_right) = reply_send_right else {
            tracing::warn!(
                "[Surface share] ignored a connect from pid {} with no reply port",
                sender.pid
            );
            return;
        };
        match self
            .helper_process_admissions
            .verdict_for(sender, self.this_process_pid)
        {
            SurfaceShareConnectionAdmissionVerdict::Admitted => {
                self.accept_connection(reply_send_right, sender);
            }
            SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted
                if self.connections_waiting_for_admission.len()
                    < MAX_CONNECTIONS_WAITING_FOR_ADMISSION =>
            {
                tracing::debug!(
                    "[Surface share] pid {} connected before its admission; waiting up to {:?}",
                    sender.pid,
                    self.unadmitted_connection_wait_budget
                );
                self.connections_waiting_for_admission
                    .push(ConnectionWaitingForAdmission {
                        reply_send_right,
                        sender,
                        admission_deadline: Instant::now() + self.unadmitted_connection_wait_budget,
                    });
            }
            SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted => refuse_connection(
                reply_send_right,
                sender,
                "too many connections are already waiting for admission",
            ),
            SurfaceShareConnectionAdmissionVerdict::Refused(reason) => {
                refuse_connection(reply_send_right, sender, &reason);
            }
        }
    }

    fn accept_connection(
        &mut self,
        reply_send_right: OwnedMachSendRight,
        sender: SurfaceShareMachSenderAuditIdentity,
    ) {
        match self.open_connection_request_port(&reply_send_right) {
            Ok(request_receive_right) => {
                tracing::debug!(
                    "[Surface share] opened a connection for pid {} (pid version {})",
                    sender.pid,
                    sender.pidversion
                );
                self.connected_peers.insert(
                    request_receive_right.as_raw_name(),
                    ConnectedSurfaceSharePeer {
                        request_receive_right,
                        reply_send_right,
                        sender,
                        lease_holder: self.state.check_out_leases().mint_holder_id(),
                        observed_runtime_id: None,
                    },
                );
            }
            Err(unopened) => tracing::warn!(
                "[Surface share] could not open a connection for pid {}: {}",
                sender.pid,
                unopened
            ),
        }
    }

    fn open_connection_request_port(
        &self,
        reply_send_right: &OwnedMachSendRight,
    ) -> io::Result<OwnedMachReceiveRight> {
        let request_receive_right = OwnedMachReceiveRight::allocate()?;
        self.port_set.insert_member(&request_receive_right)?;
        request_dead_name_notification(
            reply_send_right,
            &self.peer_death_notification_receive_right,
        )?;
        send_surface_share_mach_message(
            reply_send_right,
            None,
            SURFACE_SHARE_MACH_REPLY_MESSAGE_ID,
            br#"{"success":true}"#,
            vec![request_receive_right.make_send_right()?],
            Some(CONNECT_ANSWER_SEND_TIMEOUT),
        )?;
        Ok(request_receive_right)
    }

    fn settle_connections_waiting_for_admission(&mut self) {
        let waiting = std::mem::take(&mut self.connections_waiting_for_admission);
        for connection in waiting {
            match self
                .helper_process_admissions
                .verdict_for(connection.sender, self.this_process_pid)
            {
                SurfaceShareConnectionAdmissionVerdict::Admitted => {
                    self.accept_connection(connection.reply_send_right, connection.sender);
                }
                SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted => {
                    self.connections_waiting_for_admission.push(connection);
                }
                SurfaceShareConnectionAdmissionVerdict::Refused(reason) => {
                    refuse_connection(connection.reply_send_right, connection.sender, &reason);
                }
            }
        }
    }

    fn refuse_connections_past_their_admission_deadline(&mut self) {
        let now = Instant::now();
        let (expired, still_waiting): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.connections_waiting_for_admission)
                .into_iter()
                .partition(|waiting| waiting.admission_deadline <= now);
        self.connections_waiting_for_admission = still_waiting;
        for connection in expired {
            refuse_connection(
                connection.reply_send_right,
                connection.sender,
                &format!(
                    "pid {} is neither this process nor a helper process it admitted",
                    connection.sender.pid
                ),
            );
        }
    }

    fn answer_peer_request(&mut self, message: ReceivedSurfaceShareMachMessage) {
        let Some(peer) = self.connected_peers.get_mut(&message.received_on_port) else {
            tracing::warn!(
                "[Surface share] a request from pid {} arrived on a port no connection owns",
                message.sender.pid
            );
            return;
        };
        if message.sender != peer.sender {
            tracing::warn!(
                "[Surface share] refused a request on pid {} (version {})'s connection from pid \
                 {} (version {})",
                peer.sender.pid,
                peer.sender.pidversion,
                message.sender.pid,
                message.sender.pidversion
            );
            return;
        }
        if message.message_id != SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID {
            tracing::warn!(
                "[Surface share] ignored message id {:#x} on pid {}'s connection",
                message.message_id,
                peer.sender.pid
            );
            return;
        }
        let (response, reply_ports) =
            match serde_json::from_slice::<serde_json::Value>(&message.json_payload) {
                Ok(request) => {
                    latch_the_first_named_runtime_id(&mut peer.observed_runtime_id, &request);
                    answer_surface_share_request(
                        &self.state,
                        &request,
                        message.ports,
                        peer.lease_holder,
                    )
                }
                Err(invalid_json) => (
                    serde_json::json!({"error": format!("invalid JSON: {invalid_json}")}),
                    Vec::new(),
                ),
            };
        let answered = send_surface_share_mach_message(
            &peer.reply_send_right,
            None,
            SURFACE_SHARE_MACH_REPLY_MESSAGE_ID,
            response.to_string().as_bytes(),
            reply_ports,
            Some(REPLY_SEND_TIMEOUT),
        );
        if let Err(unsent) = answered {
            tracing::warn!(
                "[Surface share] could not answer pid {}; closing its connection: {}",
                message.sender.pid,
                unsent
            );
            self.close_connection(message.received_on_port);
        }
    }

    /// Close every connection that answers on `dead_reply_port`. Send rights
    /// to one port share one name in this task, so a client that opened
    /// several connections on one reply port dies for all of them at once.
    fn forget_every_client_answering_on(&mut self, dead_reply_port: mach_port_name_t) {
        self.connections_waiting_for_admission
            .retain(|waiting| waiting.reply_send_right.as_raw_name() != dead_reply_port);
        let dead_connections: Vec<mach_port_name_t> = self
            .connected_peers
            .iter()
            .filter(|(_, peer)| peer.reply_send_right.as_raw_name() == dead_reply_port)
            .map(|(request_port, _)| *request_port)
            .collect();
        for request_port in dead_connections {
            self.close_connection(request_port);
        }
    }

    fn close_connection(&mut self, request_port: mach_port_name_t) {
        let Some(peer) = self.connected_peers.remove(&request_port) else {
            return;
        };
        let out_of_process_runtime_id = peer
            .observed_runtime_id
            .as_deref()
            .filter(|_| peer.sender.pid != self.this_process_pid);
        release_what_a_closed_connection_held(
            self.state.check_out_leases(),
            peer.lease_holder,
            &format_args!("pid {}'s connection", peer.sender.pid),
            out_of_process_runtime_id.map(|runtime_id| (&self.state as _, runtime_id)),
        );
        drop(peer.request_receive_right);
    }
}

fn refuse_connection(
    reply_send_right: OwnedMachSendRight,
    sender: SurfaceShareMachSenderAuditIdentity,
    reason: &str,
) {
    tracing::warn!(
        "[Surface share] refused a connection from pid {} (version {}): {}",
        sender.pid,
        sender.pidversion,
        reason
    );
    let refusal = serde_json::json!({ "error": reason }).to_string();
    let _ = send_surface_share_mach_message(
        &reply_send_right,
        None,
        SURFACE_SHARE_MACH_REPLY_MESSAGE_ID,
        refusal.as_bytes(),
        Vec::new(),
        Some(CONNECT_ANSWER_SEND_TIMEOUT),
    );
}

/// Answer one request, returning the reply and the ports it carries.
/// `received_ports` a verb does not consume are released on return.
fn answer_surface_share_request(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    received_ports: Vec<OwnedMachSendRight>,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let op = request
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match op {
        "register" => (handle_register(state, request, received_ports), Vec::new()),
        "check_in" => (handle_check_in(state, request, received_ports), Vec::new()),
        // `lookup` and `check_out` answer with the same surface; only
        // `check_out` claims the frame.
        "lookup" => handle_lookup(state, request),
        "check_out" => handle_check_out(state, request, lease_holder),
        "release_check_out" => (
            answer_release_check_out(state.check_out_leases(), request, lease_holder),
            Vec::new(),
        ),
        "unregister" | "release" => (answer_unregister(state, request), Vec::new()),
        SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE => {
            (answer_signal_consume_done(state, request), Vec::new())
        }
        SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED => {
            (answer_timeline_import_refused(state, request), Vec::new())
        }
        _ => (
            serde_json::json!({"error": format!("unknown operation: {op}")}),
            Vec::new(),
        ),
    }
}

/// The registration a `register` or `check_in` describes, holding the one
/// IOSurface its first port names and, when the flags announce them, the
/// timeline pair's shared-event ports after it.
fn registration_of_request(
    request: &serde_json::Value,
    surface_id: String,
    received_ports: Vec<OwnedMachSendRight>,
) -> Result<IOSurfaceShareRegistration, String> {
    let announced = |flag: &str| {
        request
            .get(flag)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let announces_timeline_pair = match (
        announced(SURFACE_SHARE_HAS_PRODUCE_DONE_PORT),
        announced(SURFACE_SHARE_HAS_CONSUME_DONE_PORT),
    ) {
        (false, false) => false,
        (true, true) => true,
        _ => {
            return Err(
                "a registration carries both timeline ports or neither, never one".to_string(),
            );
        }
    };
    let mut received_ports = received_ports.into_iter();
    let Some(iosurface_port) = received_ports.next() else {
        return Err("a registration carries exactly one IOSurface port".to_string());
    };
    let timeline_send_rights = if announces_timeline_pair {
        let (Some(produce_done), Some(consume_done)) =
            (received_ports.next(), received_ports.next())
        else {
            return Err("the announced timeline ports did not arrive".to_string());
        };
        Some(Arc::new(SharedTimelineSendRights {
            produce_done,
            consume_done,
        }))
    } else {
        None
    };
    if received_ports.next().is_some() {
        return Err(
            "a registration carries exactly one IOSurface port, then the ports its flags announce"
                .to_string(),
        );
    }
    let iosurface = IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
        .ok_or_else(|| "the registered port names no IOSurface".to_string())?;
    let requested_u32 = |key: &str| {
        request
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    };
    let requested_str = |key: &str, default: &str| {
        request
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(default)
            .to_string()
    };
    Ok(IOSurfaceShareRegistration {
        surface_id,
        runtime_id: requested_runtime_id(request).to_string(),
        width: requested_u32("width").unwrap_or(iosurface.width() as u32),
        height: requested_u32("height").unwrap_or(iosurface.height() as u32),
        format: requested_str("format", "unknown"),
        resource_type: requested_str("resource_type", "pixel_buffer"),
        iosurface: RetainedIOSurfaceSharedAcrossThreads::new(iosurface),
        timeline_send_rights,
    })
}

fn handle_register(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    received_ports: Vec<OwnedMachSendRight>,
) -> serde_json::Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    // Registrations are per-slot; the `#<generation>` suffix belongs to the
    // published frame ids minted over a slot.
    if crate::core::rhi::PublishedPixelBufferFrameId::parse(surface_id).is_some() {
        return serde_json::json!({"error": format!(
            "surface id '{surface_id}' ends in the reserved #<generation> suffix; \
             register the pool slot and let acquisition publish the frame ids"
        )});
    }
    let registration =
        match registration_of_request(request, surface_id.to_string(), received_ports) {
            Ok(registration) => registration,
            Err(refusal) => return serde_json::json!({ "error": refusal }),
        };
    let runtime_id = registration.runtime_id.clone();
    match state.register_surface(registration) {
        Ok(()) => {
            tracing::debug!(
                "[Surface share] register: surface '{}' for runtime '{}'",
                surface_id,
                runtime_id
            );
            serde_json::json!({"success": true})
        }
        Err(_) => {
            tracing::warn!(
                "[Surface share] register: surface '{}' already exists",
                surface_id
            );
            serde_json::json!({"success": false})
        }
    }
}

fn handle_check_in(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    received_ports: Vec<OwnedMachSendRight>,
) -> serde_json::Value {
    let surface_id = uuid::Uuid::new_v4().to_string();
    match registration_of_request(request, surface_id.clone(), received_ports) {
        Ok(registration) => match state.register_surface(registration) {
            Ok(()) => serde_json::json!({ "surface_id": surface_id }),
            Err(_) => serde_json::json!({"error": "a freshly minted surface id collided"}),
        },
        Err(refusal) => serde_json::json!({ "error": refusal }),
    }
}

fn handle_lookup(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), surface_id) {
        return (refusal, Vec::new());
    }
    let Some(registration) = state.registration_of(surface_id) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    let iosurface_port = match create_iosurface_mach_send_right(&registration.iosurface) {
        Ok(iosurface_port) => iosurface_port,
        Err(unminted) => {
            return (
                serde_json::json!({"error": unminted.to_string()}),
                Vec::new(),
            );
        }
    };
    let mut reply_ports = vec![iosurface_port];
    let carries_timeline_pair = match &registration.timeline_send_rights {
        Some(timeline_send_rights) => match (
            timeline_send_rights.produce_done.try_clone(),
            timeline_send_rights.consume_done.try_clone(),
        ) {
            (Ok(produce_done), Ok(consume_done)) => {
                reply_ports.extend([produce_done, consume_done]);
                true
            }
            (Err(unminted), _) | (_, Err(unminted)) => {
                return (
                    serde_json::json!({"error": unminted.to_string()}),
                    Vec::new(),
                );
            }
        },
        None => false,
    };
    (
        serde_json::json!({
            "surface_id": surface_id,
            "width": registration.width,
            "height": registration.height,
            "format": registration.format,
            "resource_type": registration.resource_type,
            "handle_type": SURFACE_HANDLE_TYPE_IOSURFACE,
            "plane_sizes": [registration.iosurface.alloc_size()],
            "plane_offsets": [0],
            "plane_strides": [registration.iosurface.bytes_per_row()],
            SURFACE_SHARE_HAS_PRODUCE_DONE_PORT: carries_timeline_pair,
            SURFACE_SHARE_HAS_CONSUME_DONE_PORT: carries_timeline_pair,
        }),
        reply_ports,
    )
}

/// A helper's host-side report that it released the frame at `value`,
/// signalled on the engine's own `consume_done`.
fn answer_signal_consume_done(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
) -> serde_json::Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    let Some(value) = request.get("value").and_then(serde_json::Value::as_u64) else {
        return serde_json::json!({"error": "missing value"});
    };
    match state
        .cross_process_timeline_pairs()
        .pair_or_refusal(surface_id)
        .and_then(|pair| pair.record_consumer_release_reported_over_the_channel(value))
    {
        Ok(()) => serde_json::json!({"success": true}),
        Err(refusal) => serde_json::json!({"error": refusal.to_string()}),
    }
}

/// A helper could not import `surface_id`'s timeline pair; the engine orders
/// that surface host-side from now on.
fn answer_timeline_import_refused(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
) -> serde_json::Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    let reason = request
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("the helper gave no reason");
    match state
        .cross_process_timeline_pairs()
        .pair_or_refusal(surface_id)
    {
        Ok(pair) => {
            pair.fall_back_to_host_side_ordering(reason);
            serde_json::json!({"success": true})
        }
        Err(refusal) => serde_json::json!({"error": refusal.to_string()}),
    }
}

/// `lookup` plus a claim: the surface is pinned against producer reuse until
/// this connection releases it or closes. A refused lease releases the port
/// already minted, unsent.
fn handle_check_out(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    let (response, reply_ports) = handle_lookup(state, request);
    if response.get("error").is_some() {
        return (response, reply_ports);
    }
    match record_check_out_lease_or_refusal(state.check_out_leases(), surface_id, lease_holder) {
        Ok(()) => (response, reply_ports),
        Err(refusal) => (refusal, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::iosurface::create_private_iosurface_with_packed_rows;
    use crate::core::rhi::PixelFormat;
    use objc2_core_foundation::CFRetained;
    use streamlib_surface_client::SurfaceShareMachServiceConnection;

    fn a_small_iosurface() -> CFRetained<IOSurfaceRef> {
        create_private_iosurface_with_packed_rows(16, 8, 4, PixelFormat::Bgra32)
            .expect("a private IOSurface")
    }

    fn a_port_to(iosurface: &IOSurfaceRef) -> OwnedMachSendRight {
        create_iosurface_mach_send_right(iosurface).expect("a port to the surface")
    }

    fn this_process() -> SurfaceShareMachSenderAuditIdentity {
        SurfaceShareMachSenderAuditIdentity {
            pid: std::process::id() as libc::pid_t,
            pidversion: 0,
        }
    }

    fn register_request(surface_id: &str) -> serde_json::Value {
        serde_json::json!({
            "op": "register",
            "surface_id": surface_id,
            "runtime_id": "R-test",
            "width": 16,
            "height": 8,
            "format": "bgra32",
        })
    }

    fn a_unique_service_name(label: &str) -> String {
        format!(
            "com.tatolab.streamlib.surface-share-test.{label}.{}.{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        )
    }

    #[test]
    fn this_process_is_admitted_and_an_unknown_pid_waits() {
        let admissions = SurfaceShareHelperProcessAdmissions::default();
        let this_pid = std::process::id() as libc::pid_t;
        assert_eq!(
            admissions.verdict_for(this_process(), this_pid),
            SurfaceShareConnectionAdmissionVerdict::Admitted
        );
        let stranger = SurfaceShareMachSenderAuditIdentity {
            pid: this_pid + 1,
            pidversion: 7,
        };
        assert_eq!(
            admissions.verdict_for(stranger, this_pid),
            SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted
        );
    }

    #[test]
    fn an_admitted_pid_is_pinned_to_the_pid_version_it_first_connects_with() {
        let admissions = SurfaceShareHelperProcessAdmissions::default();
        let this_pid = 1;
        let _admission = admissions.admit_helper_process(4242);
        let helper = SurfaceShareMachSenderAuditIdentity {
            pid: 4242,
            pidversion: 3,
        };
        assert_eq!(
            admissions.verdict_for(helper, this_pid),
            SurfaceShareConnectionAdmissionVerdict::Admitted
        );
        assert_eq!(
            admissions.verdict_for(helper, this_pid),
            SurfaceShareConnectionAdmissionVerdict::Admitted
        );
        let impostor_on_a_reused_pid = SurfaceShareMachSenderAuditIdentity {
            pid: 4242,
            pidversion: 4,
        };
        let SurfaceShareConnectionAdmissionVerdict::Refused(reason) =
            admissions.verdict_for(impostor_on_a_reused_pid, this_pid)
        else {
            panic!("a different pid version must be refused");
        };
        assert!(reason.contains("reused pid"), "{reason}");
    }

    #[test]
    fn a_withdrawn_admission_stops_admitting_the_pid() {
        let admissions = SurfaceShareHelperProcessAdmissions::default();
        let helper = SurfaceShareMachSenderAuditIdentity {
            pid: 4343,
            pidversion: 1,
        };
        let admission = admissions.admit_helper_process(4343);
        assert_eq!(
            admissions.verdict_for(helper, 1),
            SurfaceShareConnectionAdmissionVerdict::Admitted
        );
        drop(admission);
        assert_eq!(
            admissions.verdict_for(helper, 1),
            SurfaceShareConnectionAdmissionVerdict::NotYetAdmitted
        );
    }

    fn a_send_right_to_a_fresh_shared_event_at(
        value: u64,
    ) -> Option<(
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLSharedEvent>>,
        OwnedMachSendRight,
    )> {
        use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent};
        let shared_event = MTLCreateSystemDefaultDevice()?.newSharedEvent()?;
        shared_event.setSignaledValue(value);
        let send_right = streamlib_surface_client::mach_send_right_of_metal_shared_event_handle(
            &shared_event.newSharedEventHandle(),
        )
        .expect("the handle's send right");
        Some((shared_event, send_right))
    }

    fn signaled_value_behind(send_right: &OwnedMachSendRight) -> u64 {
        use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent};
        let handle =
            streamlib_surface_client::metal_shared_event_handle_of_mach_send_right(send_right)
                .expect("a handle");
        MTLCreateSystemDefaultDevice()
            .and_then(|device| device.newSharedEventWithHandle(&handle))
            .expect("the port names a live shared event")
            .signaledValue()
    }

    #[test]
    fn a_registration_with_a_timeline_pair_looks_up_with_both_ports_after_the_surface() {
        let (Some((_produce_done, produce_done_port)), Some((_consume_done, consume_done_port))) = (
            a_send_right_to_a_fresh_shared_event_at(11),
            a_send_right_to_a_fresh_shared_event_at(22),
        ) else {
            return;
        };
        let state = IOSurfaceShareState::new();
        let iosurface = a_small_iosurface();
        let holder = state.check_out_leases().mint_holder_id();
        let mut request = register_request("slot-timelines");
        request[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] = true.into();
        request[SURFACE_SHARE_HAS_CONSUME_DONE_PORT] = true.into();

        let (registered, _) = answer_surface_share_request(
            &state,
            &request,
            vec![a_port_to(&iosurface), produce_done_port, consume_done_port],
            holder,
        );
        assert_eq!(registered, serde_json::json!({"success": true}));

        let (looked_up, ports) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": "check_out", "surface_id": "slot-timelines"}),
            Vec::new(),
            holder,
        );
        assert_eq!(looked_up[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT], true);
        assert_eq!(looked_up[SURFACE_SHARE_HAS_CONSUME_DONE_PORT], true);
        assert_eq!(ports.len(), 3);
        assert_eq!(signaled_value_behind(&ports[1]), 11);
        assert_eq!(signaled_value_behind(&ports[2]), 22);
    }

    #[test]
    fn a_registration_announcing_one_timeline_port_is_refused() {
        let Some((_produce_done, produce_done_port)) = a_send_right_to_a_fresh_shared_event_at(0)
        else {
            return;
        };
        let state = IOSurfaceShareState::new();
        let iosurface = a_small_iosurface();
        let mut request = register_request("slot-half-pair");
        request[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] = true.into();

        let (refused, _) = answer_surface_share_request(
            &state,
            &request,
            vec![a_port_to(&iosurface), produce_done_port],
            state.check_out_leases().mint_holder_id(),
        );
        assert!(
            refused["error"]
                .as_str()
                .is_some_and(|error| error.contains("both timeline ports or neither")),
            "{refused}"
        );
        assert!(state.registration_of("slot-half-pair").is_none());
    }

    #[test]
    fn a_host_side_report_against_a_surface_with_no_engine_pair_is_refused() {
        let state = IOSurfaceShareState::new();
        for op in [
            SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE,
            SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED,
        ] {
            let (refused, _) = answer_surface_share_request(
                &state,
                &serde_json::json!({"op": op, "surface_id": "slot-unpaired", "value": 1}),
                Vec::new(),
                state.check_out_leases().mint_holder_id(),
            );
            assert!(
                refused["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("no engine timeline pair")),
                "{op}: {refused}"
            );
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_helpers_host_side_reports_move_the_engines_own_pair() {
        use crate::apple::surface_share::CrossProcessTimelinePair;
        use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore};

        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        let pair = Arc::new(CrossProcessTimelinePair::new(
            Arc::new(HostVulkanTimelineSemaphore::new_exportable(device.device(), 0).unwrap()),
            Arc::new(HostVulkanTimelineSemaphore::new_exportable(device.device(), 0).unwrap()),
        ));
        let state = IOSurfaceShareState::new();
        state
            .cross_process_timeline_pairs()
            .insert("slot-paired", Arc::clone(&pair));
        let holder = state.check_out_leases().mint_holder_id();

        let (refused_import, _) = answer_surface_share_request(
            &state,
            &serde_json::json!({
                "op": SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED,
                "surface_id": "slot-paired#3",
                "reason": "no VK_EXT_metal_objects",
            }),
            Vec::new(),
            holder,
        );
        assert_eq!(refused_import, serde_json::json!({"success": true}));
        assert!(pair.orders_host_side());

        pair.produce_done()
            .signal_host(4)
            .expect("produce four frames");
        let (signalled, _) = answer_surface_share_request(
            &state,
            &serde_json::json!({
                "op": SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE,
                "surface_id": "slot-paired#3",
                "value": 4,
            }),
            Vec::new(),
            holder,
        );
        assert_eq!(signalled, serde_json::json!({"success": true}));
        assert_eq!(pair.consume_done().current_value().unwrap(), 4);
    }

    #[test]
    fn a_registered_surface_looks_up_as_a_port_to_the_same_surface() {
        let state = IOSurfaceShareState::new();
        let iosurface = a_small_iosurface();
        let holder = state.check_out_leases().mint_holder_id();

        let (registered, _) = answer_surface_share_request(
            &state,
            &register_request("slot-a"),
            vec![a_port_to(&iosurface)],
            holder,
        );
        assert_eq!(registered, serde_json::json!({"success": true}));

        let (looked_up, mut ports) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": "lookup", "surface_id": "slot-a"}),
            Vec::new(),
            holder,
        );
        assert_eq!(looked_up["width"], 16);
        assert_eq!(looked_up["handle_type"], SURFACE_HANDLE_TYPE_IOSURFACE);
        assert_eq!(looked_up["plane_strides"], serde_json::json!([64]));
        assert_eq!(ports.len(), 1);
        assert!(
            iosurface.is_in_use(),
            "an outstanding port counts as the surface being in use"
        );
        let resolved = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
            .expect("the port names the surface");
        assert_eq!(resolved.id(), iosurface.id());
        ports.clear();
        assert!(!iosurface.is_in_use());
    }

    #[test]
    fn registrations_are_refused_without_exactly_one_iosurface_port_or_with_a_generation_suffix() {
        let state = IOSurfaceShareState::new();
        let holder = state.check_out_leases().mint_holder_id();
        let (portless, _) =
            answer_surface_share_request(&state, &register_request("slot-b"), Vec::new(), holder);
        assert!(portless["error"].as_str().unwrap().contains("exactly one"));

        let iosurface = a_small_iosurface();
        let (suffixed, _) = answer_surface_share_request(
            &state,
            &register_request("slot-b#1"),
            vec![a_port_to(&iosurface)],
            holder,
        );
        assert!(suffixed["error"].as_str().unwrap().contains("reserved"));
        assert!(
            !iosurface.is_in_use(),
            "a refused registration released the port it carried"
        );

        let (first, _) = answer_surface_share_request(
            &state,
            &register_request("slot-b"),
            vec![a_port_to(&iosurface)],
            holder,
        );
        let (duplicate, _) = answer_surface_share_request(
            &state,
            &register_request("slot-b"),
            vec![a_port_to(&iosurface)],
            holder,
        );
        assert_eq!(first, serde_json::json!({"success": true}));
        assert_eq!(duplicate, serde_json::json!({"success": false}));
    }

    #[test]
    fn a_check_out_leases_the_slot_until_its_holder_releases_it() {
        let state = IOSurfaceShareState::new();
        let iosurface = a_small_iosurface();
        let holder = state.check_out_leases().mint_holder_id();
        answer_surface_share_request(
            &state,
            &register_request("slot-c"),
            vec![a_port_to(&iosurface)],
            holder,
        );
        state
            .check_out_leases()
            .publish_frame_generation("slot-c", 1)
            .unwrap();

        let (checked_out, _) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": "check_out", "surface_id": "slot-c#1"}),
            Vec::new(),
            holder,
        );
        assert!(checked_out.get("error").is_none(), "{checked_out}");
        assert_eq!(
            state
                .check_out_leases()
                .outstanding_check_out_count("slot-c")
                .unwrap(),
            1
        );

        let (released, _) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": "release_check_out", "surface_id": "slot-c#1"}),
            Vec::new(),
            holder,
        );
        assert_eq!(
            released,
            serde_json::json!({"success": true, "released": true})
        );
        assert_eq!(
            state
                .check_out_leases()
                .outstanding_check_out_count("slot-c")
                .unwrap(),
            0
        );
    }

    #[test]
    fn a_retired_frame_id_is_refused_before_any_port_crosses() {
        let state = IOSurfaceShareState::new();
        let iosurface = a_small_iosurface();
        let holder = state.check_out_leases().mint_holder_id();
        answer_surface_share_request(
            &state,
            &register_request("slot-d"),
            vec![a_port_to(&iosurface)],
            holder,
        );
        state
            .check_out_leases()
            .publish_frame_generation("slot-d", 2)
            .unwrap();
        let (refused, ports) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": "check_out", "surface_id": "slot-d#1"}),
            Vec::new(),
            holder,
        );
        assert!(refused.get("error").is_some(), "{refused}");
        assert!(ports.is_empty());
        assert!(!iosurface.is_in_use());
    }

    #[test]
    fn a_registration_is_released_only_by_the_runtime_that_made_it() {
        let state = IOSurfaceShareState::new();
        let iosurface = a_small_iosurface();
        let holder = state.check_out_leases().mint_holder_id();
        answer_surface_share_request(
            &state,
            &register_request("slot-e"),
            vec![a_port_to(&iosurface)],
            holder,
        );
        let (by_a_stranger, _) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": "release", "surface_id": "slot-e", "runtime_id": "R-other"}),
            Vec::new(),
            holder,
        );
        let (by_its_runtime, _) = answer_surface_share_request(
            &state,
            &serde_json::json!({"op": "release", "surface_id": "slot-e", "runtime_id": "R-test"}),
            Vec::new(),
            holder,
        );
        assert_eq!(by_a_stranger, serde_json::json!({"success": false}));
        assert_eq!(by_its_runtime, serde_json::json!({"success": true}));
        assert!(state.surface_ids().is_empty());
    }

    fn started_service(label: &str) -> (IOSurfaceShareState, MachSurfaceShareService) {
        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(state.clone(), a_unique_service_name(label));
        service.start().expect("the service starts");
        (state, service)
    }

    fn connect_to(service: &MachSurfaceShareService) -> SurfaceShareMachServiceConnection {
        SurfaceShareMachServiceConnection::connect(service.service_name(), Duration::from_secs(5))
            .expect("this process is admitted")
    }

    fn settles_within(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::yield_now();
        }
        condition()
    }

    #[test]
    fn this_process_registers_and_looks_up_a_surface_over_the_mach_channel() {
        let (_state, service) = started_service("round-trip");
        let connection = connect_to(&service);
        let iosurface = a_small_iosurface();

        let (registered, _) = connection
            .send_request_with_ports(&register_request("slot-f"), vec![a_port_to(&iosurface)])
            .unwrap();
        assert_eq!(registered, serde_json::json!({"success": true}));

        let (looked_up, ports) = connection
            .send_request_with_ports(
                &serde_json::json!({"op": "lookup", "surface_id": "slot-f"}),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(looked_up["height"], 8);
        let resolved = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
            .expect("the answered port names the surface");
        assert_eq!(resolved.id(), iosurface.id());
    }

    #[test]
    fn a_closed_connection_releases_its_leases_but_not_this_processs_registrations() {
        let (state, service) = started_service("close");
        let iosurface = a_small_iosurface();
        let registering_connection = connect_to(&service);
        registering_connection
            .send_request_with_ports(&register_request("slot-g"), vec![a_port_to(&iosurface)])
            .unwrap();
        state
            .check_out_leases()
            .publish_frame_generation("slot-g", 1)
            .unwrap();

        let reading_connection = connect_to(&service);
        let (checked_out, _) = reading_connection
            .send_request_with_ports(
                &serde_json::json!({"op": "check_out", "surface_id": "slot-g#1"}),
                Vec::new(),
            )
            .unwrap();
        assert!(checked_out.get("error").is_none(), "{checked_out}");
        assert_eq!(
            state
                .check_out_leases()
                .outstanding_check_out_count("slot-g")
                .unwrap(),
            1
        );

        drop(reading_connection);
        drop(registering_connection);

        assert!(
            settles_within(Duration::from_secs(5), || {
                state
                    .check_out_leases()
                    .outstanding_check_out_count("slot-g")
                    .unwrap()
                    == 0
            }),
            "the closed connection's lease was released"
        );
        assert_eq!(state.surface_ids(), vec!["slot-g".to_string()]);
    }

    #[test]
    fn a_second_service_under_a_live_name_is_refused() {
        let (_state, service) = started_service("duplicate");
        let mut second =
            MachSurfaceShareService::new(IOSurfaceShareState::new(), service.service_name().into());
        let refused = second.start().unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AddrInUse);
    }

    #[test]
    fn a_stopped_service_is_seen_going_away_by_its_clients_and_its_name_frees() {
        let (_state, mut service) = started_service("stop");
        let service_name = service.service_name().to_string();
        let connection = connect_to(&service);

        service.stop();

        assert!(
            connection
                .wait_for_the_service_to_go_away(Some(Duration::from_secs(5)))
                .unwrap()
        );
        let refused = connection
            .send_request_with_ports(&serde_json::json!({"op": "lookup"}), Vec::new())
            .unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::BrokenPipe);
        let mut restarted = MachSurfaceShareService::new(IOSurfaceShareState::new(), service_name);
        restarted
            .start()
            .expect("the name is free once the service stops");
    }
}

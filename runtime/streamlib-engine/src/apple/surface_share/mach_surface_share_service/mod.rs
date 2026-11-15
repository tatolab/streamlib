// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface-share service over a raw Mach channel.
//!
//! A client opens a connection by sending a connect message to the service's
//! bootstrap name. The service answers an admitted sender with a request port
//! minted for that connection alone and watches the client's reply port for
//! death; everything after the connect rides that unlisted port, and every
//! message is still checked against the connection's audit identity.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use mach2::port::mach_port_name_t;
use objc2_io_surface::IOSurfaceRef;
use parking_lot::Mutex;
use streamlib_surface_client::{
    GpuPoolClaimTableRowLeasedToASlot, OwnedMachPortSet, OwnedMachReceiveRight, OwnedMachSendRight,
    ReceivedSurfaceShareMachMessage, ReceivedSurfaceShareMachTraffic,
    SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP, SURFACE_SHARE_CLAIMANT_COLUMN_FIELD,
    SURFACE_SHARE_GPU_POOL_FIELD, SURFACE_SHARE_HAS_CONSUME_DONE_PORT,
    SURFACE_SHARE_HAS_PRODUCE_DONE_PORT, SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD,
    SURFACE_SHARE_IMPORT_POOL_SLOT_OP, SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID,
    SURFACE_SHARE_MACH_REPLY_MESSAGE_ID, SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
    SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE, SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED,
    SURFACE_SHARE_POOL_SLOTS_FIELD, SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD,
    SurfaceShareMachMessageReceiveBuffer, SurfaceShareMachSenderAuditIdentity,
    check_in_surface_share_mach_service, gpu_pool_uuid_of_its_wire_name, gpu_pool_uuid_wire_name,
    receive_surface_share_mach_traffic, request_dead_name_notification,
    send_surface_share_mach_message, split_pool_slot_and_frame_generation,
};

use crate::apple::iosurface::{
    RetainedIOSurfaceSharedAcrossThreads, create_iosurface_mach_send_right,
};
use crate::core::context::surface_share_wire_verbs::{
    SURFACE_RESOURCE_TYPE_PIXEL_BUFFER, SURFACE_RESOURCE_TYPE_TEXTURE, answer_release_check_out,
    answer_unregister, echo_the_tensor_layout_onto_a_lookup_reply,
    latch_the_first_named_runtime_id, parse_vk_image_create_info_fields,
    record_check_out_lease_or_refusal, refusal_of_a_retired_frame_id,
    release_what_a_closed_connection_held, requested_runtime_id, requested_surface_id,
    stated_current_image_layout, tensor_layout_of_a_storage_buffer_registration,
};
use crate::core::context::{GpuPoolClaimantProcessId, SurfaceCheckOutLeaseHolderId};

use super::gpu_pool_claimant_process::GpuPoolClaimantProcess;
use super::state::{
    IOSurfaceShareRegistration, IOSurfaceShareState, RegisteredTextureImage,
    SharedTimelineSendRights,
};

mod mach_surface_share_service_loop;
mod surface_share_request_decoding_and_answers;
mod surface_share_request_dispatch_and_gpu_pool_handlers;
#[cfg(test)]
mod tests;

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
    pool_slot_keys_handed_an_iosurface: PoolSlotKeysHandedAnIOSurfaceOnThisConnection,
    claimant_process_handed_a_column: Option<GpuPoolClaimantProcess>,
}

impl ConnectedSurfaceSharePeer {
    /// The claimant this connection's peer claims as, held the first time it
    /// is handed a column; refused when its process cannot be held.
    fn claimant_handed_columns_on_this_connection(
        &mut self,
    ) -> Result<GpuPoolClaimantProcessId, String> {
        if let Some(claimant_process) = &self.claimant_process_handed_a_column {
            return Ok(claimant_process.claimant());
        }
        let claimant_process = GpuPoolClaimantProcess::hold_the_peer(self.sender)?;
        let claimant = claimant_process.claimant();
        self.claimant_process_handed_a_column = Some(claimant_process);
        Ok(claimant)
    }
}

/// The pool slot keys this connection was handed an IOSurface port for, so
/// the connection can tell its peer when the engine unregisters one.
#[derive(Default)]
struct PoolSlotKeysHandedAnIOSurfaceOnThisConnection {
    pool_slot_keys: HashSet<String>,
}

impl PoolSlotKeysHandedAnIOSurfaceOnThisConnection {
    fn remember(&mut self, pool_slot_key: &str) {
        if !self.pool_slot_keys.contains(pool_slot_key) {
            self.pool_slot_keys.insert(pool_slot_key.to_string());
        }
    }

    /// Forget every remembered key `state` no longer registers and list them
    /// on `reply` under `retired_pool_slots`. Keys are never reused, so a key
    /// absent from the table is retired for good.
    fn list_the_retired_ones_on_the_reply(
        &mut self,
        state: &IOSurfaceShareState,
        reply: &mut serde_json::Value,
    ) {
        let mut retired_pool_slots: Vec<String> = Vec::new();
        self.pool_slot_keys.retain(|pool_slot_key| {
            let still_registered = state.is_registered(pool_slot_key);
            if !still_registered {
                retired_pool_slots.push(pool_slot_key.clone());
            }
            still_registered
        });
        if retired_pool_slots.is_empty() {
            return;
        }
        if let Some(reply_fields) = reply.as_object_mut() {
            reply_fields.insert(
                SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD.to_string(),
                retired_pool_slots.into(),
            );
        }
    }
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

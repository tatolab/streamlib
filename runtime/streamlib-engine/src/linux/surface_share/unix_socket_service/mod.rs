// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Per-runtime Unix socket surface-sharing service.
//!
//! Each `Runner` owns one of these listening on a unique socket in the
//! runtime directory. Polyglot subprocesses connect via `connect_to_surface_share_socket`
//! / `send_request_with_fds` (from [`streamlib_surface_client`]) and exchange
//! DMA-BUF fds over `SCM_RIGHTS`. Surfaces may carry up to
//! [`streamlib_surface_client::MAX_DMA_BUF_PLANES`] plane fds — one per plane
//! for multi-plane DMA-BUFs (e.g. NV12 with separate Y and UV allocations).

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use streamlib_surface_client::{
    GpuPoolClaimTableRowLeasedToASlot, MAX_DMA_BUF_PLANES, MAX_SCM_RIGHTS_FDS,
    SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP, SURFACE_SHARE_CLAIMANT_COLUMN_FIELD,
    SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD, SURFACE_SHARE_GPU_POOL_FIELD,
    SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD, SURFACE_SHARE_IMPORT_POOL_SLOT_OP,
    SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP, SURFACE_SHARE_POOL_SLOTS_FIELD,
    SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD, SURFACE_SHARE_VIEWED_FRAME_IMPORTS_HELD_FIELD,
    SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD, ViewedFrameReadBy, gpu_pool_uuid_of_its_wire_name,
    gpu_pool_uuid_wire_name, recv_message_with_fds, send_message_with_fds,
    split_pool_slot_and_frame_generation,
};

use super::gpu_pool_claimant_process::GpuPoolClaimantProcess;
use crate::core::context::surface_share_wire_verbs::{
    SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID, SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX,
    answer_release_check_out, answer_unregister, echo_the_tensor_layout_onto_a_lookup_reply,
    latch_the_first_named_runtime_id, parse_vk_image_create_info_fields,
    pixel_planes_of_a_pixel_buffer_registration, record_check_out_lease_or_refusal,
    refusal_of_a_retired_frame_id, release_what_a_closed_connection_held, requested_runtime_id,
    requested_surface_id, stated_vk_memory_type_index,
    tensor_layout_of_a_storage_buffer_registration,
};
use crate::core::context::{GpuPoolClaimantProcessId, SurfaceCheckOutLeaseHolderId};

use super::state::{SurfacePlaneCheckout, SurfaceRegistration, SurfaceShareState};
#[cfg(test)]
use crate::core::context::surface_share_wire_verbs::{
    VK_IMAGE_ALLOCATION_SIZE_DEFAULT, VK_IMAGE_ARRAY_LAYERS_DEFAULT, VK_IMAGE_MIP_LEVELS_DEFAULT,
    VK_IMAGE_SAMPLES_DEFAULT, VK_IMAGE_TILING_DEFAULT, VK_IMAGE_TYPE_DEFAULT,
    VK_IMAGE_USAGE_DEFAULT,
};
use streamlib_runtime_client_contract::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};

mod request_handlers;

use request_handlers::{
    PoolSlotKeysHandedMemoryOnThisConnection, handle_check_in, handle_check_out,
    handle_claim_table_of_gpu_pool, handle_import_pool_slot, handle_import_viewed_frame,
    handle_lookup, handle_register, handle_release_check_out, handle_unregister,
    handle_update_layout,
};
#[cfg(test)]
mod tests;

pub struct UnixSocketSurfaceService {
    state: SurfaceShareState,
    socket_path: PathBuf,
    listener_thread: Option<thread::JoinHandle<()>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl UnixSocketSurfaceService {
    pub fn new(state: SurfaceShareState, socket_path: PathBuf) -> Self {
        Self {
            state,
            socket_path,
            listener_thread: None,
            shutdown_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)
                .map_err(|e| format!("Failed to remove stale socket: {}", e))?;
        }

        if let Some(parent) = self.socket_path.parent() {
            create_directory_and_its_missing_parents_at_mode(parent, OWNER_ONLY_DIRECTORY_MODE)
                .map_err(|e| format!("Failed to create socket directory: {}", e))?;
        }

        let listener = UnixListener::bind(&self.socket_path).map_err(|e| {
            format!(
                "Failed to bind Unix socket at {:?}: {}",
                self.socket_path, e
            )
        })?;

        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to set non-blocking: {}", e))?;

        let state = self.state.clone();
        let shutdown_flag = self.shutdown_flag.clone();

        let handle = thread::spawn(move || {
            run_listener(listener, state, shutdown_flag);
        });

        self.listener_thread = Some(handle);

        tracing::info!(
            "[Surface share] Unix socket surface service listening on {:?}",
            self.socket_path
        );

        Ok(())
    }

    /// Serve viewers' asks for viewed frames from `source`.
    pub fn serve_viewed_frames_from(&self, source: Arc<dyn super::ViewedFrameSource>) {
        self.state.serve_viewed_frames_from(source);
    }

    pub fn stop(&mut self) {
        // Before the join: a connection thread outlives the listener and
        // holds the state, so a source left in it would outlive the engine.
        self.state.stop_serving_viewed_frames();
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);

        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }

        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        tracing::info!("[Surface share] Unix socket surface service stopped");
    }
}

impl Drop for UnixSocketSurfaceService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_listener(
    listener: UnixListener,
    state: SurfaceShareState,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                // SO_PEERCRED tells us the connecting process's pid. Connections
                // from the host runtime's own process are diagnostic / test
                // harnesses publishing surfaces over the wire — they intentionally
                // disconnect after registering, and the surfaces must persist for
                // subprocess consumers. The watchdog must skip these and only fire
                // on out-of-process subprocess connections.
                let is_subprocess_peer = is_out_of_process_peer(&stream);
                let state = state.clone();
                thread::spawn(move || {
                    let mut connection_runtime_id: Option<String> = None;
                    // Every connection is a potential lease holder, in-process
                    // or not: a checkout carries no runtime_id, so the
                    // connection is the only identity its leases can be
                    // charged to.
                    let lease_holder = state.check_out_leases().mint_holder_id();
                    let mut claimant_process_handed_a_column: Option<GpuPoolClaimantProcess> = None;
                    let conn_result = handle_client_connection(
                        stream,
                        state.clone(),
                        &mut connection_runtime_id,
                        lease_holder,
                        &mut claimant_process_handed_a_column,
                    );
                    let the_service_dropped_the_connection = conn_result.is_err();
                    if let Err(e) = conn_result {
                        tracing::debug!("[Surface share] Client connection ended: {}", e);
                    }
                    // EPOLLHUP-equivalent watchdog: when the kernel closes the
                    // socket (typical on subprocess SIGKILL), the per-connection
                    // read loop above exits with `UnexpectedEof`, and a crashed
                    // subprocess's leases and registrations are released.
                    let subprocess_runtime_id =
                        connection_runtime_id.filter(|_| is_subprocess_peer);
                    release_what_a_closed_connection_held(
                        state.check_out_leases(),
                        lease_holder,
                        &lease_holder,
                        subprocess_runtime_id
                            .as_deref()
                            .map(|runtime_id| (&state as _, runtime_id)),
                    );
                    // Every connection the service drops is its claimant's
                    // fail-stop, and a column clears only once its process has
                    // exited, never on a connection closing alone.
                    if let Some(claimant_process) = claimant_process_handed_a_column {
                        claimant_process.clear_its_columns_once_it_has_exited(
                            state.check_out_leases().gpu_pool_claim_tables(),
                            the_service_dropped_the_connection,
                        );
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_until_a_connection_is_pending_or_shutdown_is_due(&listener);
            }
            Err(e) => {
                if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                tracing::warn!("[Surface share] Unix socket accept error: {}", e);
                thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// How long the idle accept loop goes before it looks at the shutdown flag.
/// It bounds only how late [`UnixSocketSurfaceService::stop`] is noticed — a
/// connection wakes the wait at once.
const ACCEPT_LOOP_SHUTDOWN_CHECK_INTERVAL_MS: libc::c_int = 50;

/// Block until the listener has a connection to accept, or the shutdown
/// check interval passes.
///
/// A helper opens its connection lazily on its first claim, and until the
/// accept that claim cannot be answered — a frame from a shallow output pool
/// can be recycled in that time, so the wait wakes on the connection itself.
fn wait_until_a_connection_is_pending_or_shutdown_is_due(listener: &UnixListener) {
    let mut listener_readiness = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd for the call's duration, over a listener fd
    // the caller keeps open. Its result is not inspected: readiness, the
    // timeout, EINTR and a poll error all go back to `accept`, which says
    // which one it was.
    unsafe {
        libc::poll(
            &mut listener_readiness,
            1,
            ACCEPT_LOOP_SHUTDOWN_CHECK_INTERVAL_MS,
        )
    };
}

fn handle_client_connection(
    mut stream: UnixStream,
    state: SurfaceShareState,
    observed_runtime_id: &mut Option<String>,
    lease_holder: SurfaceCheckOutLeaseHolderId,
    claimant_process_handed_a_column: &mut Option<GpuPoolClaimantProcess>,
) -> Result<(), std::io::Error> {
    stream.set_nonblocking(false)?;
    let mut pool_slot_keys_handed_memory = PoolSlotKeysHandedMemoryOnThisConnection::default();
    let peer_pid = peer_process_id(&stream);

    loop {
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(e) => return Err(e),
        }

        let msg_len = u32::from_be_bytes(len_buf) as usize;
        if msg_len > 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Message too large",
            ));
        }

        let (json_bytes, received_fds) =
            recv_message_with_fds(&stream, msg_len, MAX_SCM_RIGHTS_FDS)?;

        let request: serde_json::Value = serde_json::from_slice(&json_bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid JSON: {}", e),
            )
        })?;

        let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");

        // Latch the runtime whose registrations the watchdog releases on
        // disconnect. Pure consumers (check_out only) carry none; a runtime
        // that registers and crashes is exactly the leak the watchdog cleans up.
        latch_the_first_named_runtime_id(observed_runtime_id, &request);

        let (mut response, reply_fds) = match op {
            "register" => handle_register(&state, &request, &received_fds),
            // `lookup` and `check_out` return the same planes; only
            // `check_out` claims the frame. A lookup is the host reading its
            // own table — it holds nothing, so leasing it would pin a pool
            // slot no consumer is reading.
            "lookup" => handle_lookup(&state, &request),
            "check_out" => handle_check_out(
                &state,
                &request,
                lease_holder,
                &mut pool_slot_keys_handed_memory,
            ),
            "release_check_out" => handle_release_check_out(&state, &request, lease_holder),
            SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP => {
                match the_claimant_handed_columns_on_this_connection(
                    peer_pid,
                    claimant_process_handed_a_column,
                ) {
                    Ok(claimant) => handle_claim_table_of_gpu_pool(&state, &request, claimant),
                    Err(refusal) => (serde_json::json!({"error": refusal}), Vec::new()),
                }
            }
            SURFACE_SHARE_IMPORT_POOL_SLOT_OP => {
                handle_import_pool_slot(&state, &request, &mut pool_slot_keys_handed_memory)
            }
            SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP => {
                handle_import_viewed_frame(&state, &request, &mut pool_slot_keys_handed_memory)
            }
            "unregister" | "release" => handle_unregister(&state, &request),
            "check_in" => handle_check_in(&state, &request, &received_fds),
            "update_layout" => handle_update_layout(&state, &request, lease_holder),
            _ => (
                serde_json::json!({"error": format!("unknown operation: {}", op)}),
                Vec::new(),
            ),
        };

        // Close every fd the peer sent us — handlers that wanted to keep
        // ownership `dup`'d during registration.
        for fd in &received_fds {
            unsafe { libc::close(*fd) };
        }

        pool_slot_keys_handed_memory.list_the_retired_ones_on_the_reply(&state, &mut response);

        let response_bytes = serde_json::to_vec(&response)
            .map_err(|e| std::io::Error::other(format!("Failed to serialize response: {}", e)))?;

        let sent = send_message_with_fds(&stream, &response_bytes, &reply_fds);

        for fd in &reply_fds {
            // SAFETY: each reply fd was dup'd for this reply and is owned here.
            unsafe { libc::close(*fd) };
        }
        sent?;
    }
}

/// The claimant this connection's peer claims as, held the first time it is
/// handed a column; refused when its process cannot be named or held.
fn the_claimant_handed_columns_on_this_connection(
    peer_pid: Option<u32>,
    claimant_process_handed_a_column: &mut Option<GpuPoolClaimantProcess>,
) -> Result<GpuPoolClaimantProcessId, String> {
    if let Some(claimant_process) = claimant_process_handed_a_column {
        return Ok(claimant_process.claimant());
    }
    let peer_pid = peer_pid.ok_or_else(|| {
        "the peer's process could not be named, so it is given no claimant column".to_string()
    })?;
    let claimant_process = GpuPoolClaimantProcess::hold_the_peer(peer_pid)?;
    let claimant = claimant_process.claimant();
    *claimant_process_handed_a_column = Some(claimant_process);
    Ok(claimant)
}

/// True if the peer of `stream` is a different process than the host
/// running this service — i.e. a polyglot subprocess. Linux `SO_PEERCRED`
/// returns the connecting process's pid; comparing against `getpid()` lets
/// the watchdog distinguish "subprocess crashed mid-flight" from "host
/// runtime opened a same-process diagnostic connection." On query failure
/// we conservatively classify the peer as a subprocess so the watchdog
/// does not silently skip a real cleanup.
fn is_out_of_process_peer(stream: &UnixStream) -> bool {
    let Some(peer_pid) = peer_process_id(stream) else {
        return true;
    };
    let host_pid = unsafe { libc::getpid() };
    peer_pid != host_pid as u32
}

/// The pid of the process on the other end of `stream`, per `SO_PEERCRED`.
fn peer_process_id(stream: &UnixStream) -> Option<u32> {
    let mut ucred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a valid socket fd and an out-parameter sized to `ucred`.
    let ret = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut ucred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (ret == 0).then_some(ucred.pid as u32)
}

unsafe impl Send for UnixSocketSurfaceService {}
unsafe impl Sync for UnixSocketSurfaceService {}

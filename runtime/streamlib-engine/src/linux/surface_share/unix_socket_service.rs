// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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

/// The raw-handle export contract an OPAQUE_FD producer states beside its
/// fd, as a registration or check-in request carries it.
struct RawHandleExportContractFields {
    vk_memory_type_index: Option<u32>,
    exporting_device_uuid: Option<String>,
}

/// Read the raw-handle export contract fields from `request`. No default
/// exists: `0` is a valid memory type index, so absence stays absent through
/// the table and the checkout echo.
fn parse_raw_handle_export_contract_fields(
    request: &serde_json::Value,
) -> RawHandleExportContractFields {
    RawHandleExportContractFields {
        vk_memory_type_index: stated_vk_memory_type_index(request),
        exporting_device_uuid: request
            .get(SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID)
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// Extract `plane_sizes`, `plane_offsets`, and `plane_strides` from a JSON
/// request body. Returns vecs whose length matches `expected_plane_count`,
/// falling back to `[0]` for single-plane registrations that omit the arrays.
///
/// Returning `None` is an explicit wire-protocol violation (mismatched
/// arrays, negative values); the handler should error out instead of
/// guessing.
fn parse_plane_arrays(
    request: &serde_json::Value,
    expected_plane_count: usize,
) -> Option<(Vec<u64>, Vec<u64>, Vec<u64>)> {
    let parse_arr = |key: &str| -> Option<Option<Vec<u64>>> {
        match request.get(key) {
            None => Some(None),
            Some(v) => v
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|el| el.as_u64())
                        .collect::<Option<Vec<u64>>>()
                })
                .map(Some)
                .unwrap_or(Some(None)),
        }
    };

    let resolve = |opt: Option<Vec<u64>>| -> Option<Vec<u64>> {
        match opt {
            Some(v) if v.len() == expected_plane_count => Some(v),
            Some(_) => None,
            None if expected_plane_count <= 1 => Some(vec![0u64; expected_plane_count.max(1)]),
            None => None,
        }
    };

    let sizes = resolve(parse_arr("plane_sizes")?)?;
    let offsets = resolve(parse_arr("plane_offsets")?)?;
    let strides = resolve(parse_arr("plane_strides")?)?;
    Some((sizes, offsets, strides))
}

fn handle_register(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    received_fds: &[RawFd],
) -> (serde_json::Value, Vec<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            return (
                serde_json::json!({"error": "missing surface_id"}),
                Vec::new(),
            );
        }
    };

    // Registrations are per-slot; the `#<generation>` suffix belongs to the
    // published frame ids minted *over* a slot. Accepting one here would
    // fork the table's key grammar and let a registration collide with a
    // frame id's slot key.
    if crate::core::rhi::PublishedPixelBufferFrameId::parse(surface_id).is_some() {
        return (
            serde_json::json!({"error": format!(
                "surface id '{surface_id}' ends in the reserved #<generation> suffix; \
                 register the pool slot and let acquisition publish the frame ids"
            )}),
            Vec::new(),
        );
    }

    let runtime_id = requested_runtime_id(request);

    // Peel off the optional trailing timeline FDs in the order the host
    // attached them: plane FDs first, then `produce_done` (if present),
    // then `consume_done` (if present). The wire signals each timeline's
    // presence via a JSON boolean (`has_produce_done_fd`,
    // `has_consume_done_fd`); the SCM_RIGHTS slot count matches the sum.
    // The single-writer-per-edge model is documented in
    // the single-writer-per-edge timeline rule.
    let has_produce_done_fd = request
        .get("has_produce_done_fd")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_consume_done_fd = request
        .get("has_consume_done_fd")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let trailing_fd_count = (has_produce_done_fd as usize) + (has_consume_done_fd as usize);
    let plane_count = received_fds.len().saturating_sub(trailing_fd_count);
    let plane_fds: &[RawFd] = &received_fds[..plane_count];
    let mut trailing_idx = plane_count;
    let produce_done_src_fd: Option<RawFd> = if has_produce_done_fd {
        let fd = received_fds.get(trailing_idx).copied();
        trailing_idx += 1;
        fd
    } else {
        None
    };
    let consume_done_src_fd: Option<RawFd> = if has_consume_done_fd {
        received_fds.get(trailing_idx).copied()
    } else {
        None
    };

    if plane_fds.is_empty() {
        return (
            serde_json::json!({"error": "missing DMA-BUF fd(s)"}),
            Vec::new(),
        );
    }
    if plane_fds.len() > MAX_DMA_BUF_PLANES {
        return (
            serde_json::json!({
                "error": format!(
                    "too many plane fds: {} > MAX_DMA_BUF_PLANES ({})",
                    plane_fds.len(), MAX_DMA_BUF_PLANES
                )
            }),
            Vec::new(),
        );
    }
    if has_produce_done_fd && produce_done_src_fd.is_none() {
        return (
            serde_json::json!({
                "error": "has_produce_done_fd=true but no trailing produce_done FD attached"
            }),
            Vec::new(),
        );
    }
    if has_consume_done_fd && consume_done_src_fd.is_none() {
        return (
            serde_json::json!({
                "error": "has_consume_done_fd=true but no trailing consume_done FD attached"
            }),
            Vec::new(),
        );
    }

    let (plane_sizes, plane_offsets, plane_strides) =
        match parse_plane_arrays(request, plane_fds.len()) {
            Some(arrays) => arrays,
            None => {
                return (
                    serde_json::json!({
                        "error": "plane_sizes/plane_offsets/plane_strides length mismatch"
                    }),
                    Vec::new(),
                );
            }
        };
    let drm_format_modifier = request
        .get("drm_format_modifier")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");
    let handle_type = request
        .get("handle_type")
        .and_then(|v| v.as_str())
        .unwrap_or("dma_buf");
    // Producer's declared layout (i32 per `VkImageLayout`). Absent or
    // unparseable defaults to `VK_IMAGE_LAYOUT_UNDEFINED` (0) — the
    // back-compat behaviour for clients that haven't been updated to
    // publish layout state through the surface-share wire format.
    let current_image_layout = request
        .get("current_image_layout")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .unwrap_or(0);
    // OPAQUE_FD VkImage carries the full VkImageCreateInfo shape so a
    // CUDA-importable consumer can rebuild a matching VkImage; every
    // field absent-defaults to the OPAQUE_FD constructor's hardcoded
    // shape so legacy callers see no behavior change.
    let vk_image = parse_vk_image_create_info_fields(request);
    let RawHandleExportContractFields {
        vk_memory_type_index,
        exporting_device_uuid,
    } = parse_raw_handle_export_contract_fields(request);
    let tensor_layout = match tensor_layout_of_a_storage_buffer_registration(request) {
        Ok(tensor_layout) => tensor_layout,
        Err(refusal) => return (serde_json::json!({"error": refusal}), Vec::new()),
    };
    let pixel_planes_within_one_allocation =
        match pixel_planes_of_a_pixel_buffer_registration(request, &plane_sizes) {
            Ok(pixel_planes_within_one_allocation) => pixel_planes_within_one_allocation,
            Err(refusal) => return (serde_json::json!({"error": refusal}), Vec::new()),
        };

    let mut dup_plane_fds: Vec<RawFd> = Vec::with_capacity(plane_fds.len());
    for fd in plane_fds {
        let dup_fd = unsafe { libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, 0) };
        if dup_fd < 0 {
            for d in &dup_plane_fds {
                unsafe { libc::close(*d) };
            }
            return (
                serde_json::json!({"error": "failed to dup memory fd"}),
                Vec::new(),
            );
        }
        dup_plane_fds.push(dup_fd);
    }

    let dup_produce_done_fd: Option<RawFd> = match produce_done_src_fd {
        Some(src) => {
            let dup = unsafe { libc::fcntl(src, libc::F_DUPFD_CLOEXEC, 0) };
            if dup < 0 {
                for d in &dup_plane_fds {
                    unsafe { libc::close(*d) };
                }
                return (
                    serde_json::json!({"error": "failed to dup produce_done FD"}),
                    Vec::new(),
                );
            }
            Some(dup)
        }
        None => None,
    };
    let dup_consume_done_fd: Option<RawFd> = match consume_done_src_fd {
        Some(src) => {
            let dup = unsafe { libc::fcntl(src, libc::F_DUPFD_CLOEXEC, 0) };
            if dup < 0 {
                for d in &dup_plane_fds {
                    unsafe { libc::close(*d) };
                }
                if let Some(fd) = dup_produce_done_fd {
                    unsafe { libc::close(fd) };
                }
                return (
                    serde_json::json!({"error": "failed to dup consume_done FD"}),
                    Vec::new(),
                );
            }
            Some(dup)
        }
        None => None,
    };

    match state.register_surface(SurfaceRegistration {
        surface_id,
        runtime_id,
        dma_buf_fds: dup_plane_fds,
        plane_sizes,
        plane_offsets,
        plane_strides,
        width,
        height,
        format,
        resource_type,
        handle_type,
        drm_format_modifier,
        produce_done_fd: dup_produce_done_fd,
        consume_done_fd: dup_consume_done_fd,
        current_image_layout,
        vk_image_type: vk_image.vk_image_type,
        vk_image_mip_levels: vk_image.vk_image_mip_levels,
        vk_image_array_layers: vk_image.vk_image_array_layers,
        vk_image_samples: vk_image.vk_image_samples,
        vk_image_tiling: vk_image.vk_image_tiling,
        vk_image_usage: vk_image.vk_image_usage,
        vk_image_allocation_size: vk_image.vk_image_allocation_size,
        vk_memory_type_index,
        exporting_device_uuid,
        tensor_layout,
        pixel_planes_within_one_allocation,
    }) {
        Ok(()) => {
            tracing::debug!(
                "[Surface share] register: surface '{}' for runtime '{}' ({} plane(s), \
                 produce_done={}, consume_done={})",
                surface_id,
                runtime_id,
                plane_fds.len(),
                has_produce_done_fd,
                has_consume_done_fd,
            );
            (serde_json::json!({"success": true}), Vec::new())
        }
        Err((leftover_planes, leftover_produce, leftover_consume)) => {
            for fd in &leftover_planes {
                unsafe { libc::close(*fd) };
            }
            if let Some(fd) = leftover_produce {
                unsafe { libc::close(fd) };
            }
            if let Some(fd) = leftover_consume {
                unsafe { libc::close(fd) };
            }
            tracing::warn!(
                "[Surface share] register: surface '{}' already exists",
                surface_id
            );
            (serde_json::json!({"success": false}), Vec::new())
        }
    }
}

/// The pool slot keys one connection was handed memory for, so the
/// connection can tell its peer when the engine unregisters one.
#[derive(Default)]
struct PoolSlotKeysHandedMemoryOnThisConnection {
    pool_slot_keys: HashSet<String>,
}

impl PoolSlotKeysHandedMemoryOnThisConnection {
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
        state: &SurfaceShareState,
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

/// The pool slot a published frame id (`<slot>#<generation>`) names; `None`
/// for every other surface id.
fn pool_slot_of_a_published_frame_id(surface_id: &str) -> Option<&str> {
    split_pool_slot_and_frame_generation(surface_id).map(|(pool_slot, _)| pool_slot)
}

/// `check_out` is `lookup` plus a claim: the surface is pinned against
/// producer reuse until this connection releases it or drops.
///
/// A peer that states it holds its own import of a published frame's pool
/// slot gets the frame's metadata and no handle. Either way the retired-frame
/// refusal answers first, and a lease that cannot be recorded refuses the
/// checkout outright: handing a consumer memory the pool believes is free is
/// exactly the silent wrongness the lease exists to remove.
fn handle_check_out(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
    pool_slot_keys_handed_memory: &mut PoolSlotKeysHandedMemoryOnThisConnection,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), surface_id) {
        return (refusal, Vec::new());
    }
    let Some(checkout) = state.get_surface_planes(surface_id) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    let published_pool_slot = pool_slot_of_a_published_frame_id(surface_id);
    let peer_holds_the_pool_slot_import = published_pool_slot.is_some()
        && request
            .get(SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

    let reply_fds = if peer_holds_the_pool_slot_import {
        Vec::new()
    } else {
        match duplicate_the_handles_of_a_check_out(&checkout) {
            Ok(duplicated_fds) => duplicated_fds,
            Err(refusal) => return (refusal, Vec::new()),
        }
    };

    if let Err(refusal) =
        record_check_out_lease_or_refusal(state.check_out_leases(), surface_id, lease_holder)
    {
        for fd in &reply_fds {
            unsafe { libc::close(*fd) };
        }
        return (refusal, Vec::new());
    }

    if !peer_holds_the_pool_slot_import && let Some(pool_slot) = published_pool_slot {
        pool_slot_keys_handed_memory.remember(pool_slot);
    }
    (
        surface_share_check_out_metadata(state, surface_id, checkout),
        reply_fds,
    )
}

/// Hand a peer the claim table a `GpuPoolSlotFrame` descriptor names: the
/// table's memfd, the column the peer's process claims in, and the pool slot
/// key leased to each row. A peer past the table's last column is refused.
fn handle_claim_table_of_gpu_pool(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    claimant: GpuPoolClaimantProcessId,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(pool_uuid) = request
        .get(SURFACE_SHARE_GPU_POOL_FIELD)
        .and_then(|value| value.as_str())
        .and_then(gpu_pool_uuid_of_its_wire_name)
    else {
        return (
            serde_json::json!({"error": "missing or malformed pool"}),
            Vec::new(),
        );
    };
    let Some(table) = state
        .check_out_leases()
        .gpu_pool_claim_tables()
        .table_named(&pool_uuid)
    else {
        return (
            serde_json::json!({"error": "no GPU pool holds that claim table"}),
            Vec::new(),
        );
    };
    let claimant_column = match table.claimant_column_of(claimant) {
        Ok(column) => column,
        Err(refusal) => {
            return (
                serde_json::json!({"error": refusal.to_string()}),
                Vec::new(),
            );
        }
    };
    // SAFETY: a valid memfd the table keeps open for its life.
    let duplicated_memfd = unsafe {
        libc::fcntl(
            table.shared_rows().memfd().as_raw_fd(),
            libc::F_DUPFD_CLOEXEC,
            0,
        )
    };
    if duplicated_memfd < 0 {
        return (
            serde_json::json!({"error": "failed to dup the claim table memfd"}),
            Vec::new(),
        );
    }
    let pool_slots: Vec<serde_json::Value> = table
        .rows_leased_to_slots()
        .iter()
        .map(GpuPoolClaimTableRowLeasedToASlot::pool_slots_entry)
        .collect();
    (
        serde_json::json!({
            SURFACE_SHARE_GPU_POOL_FIELD: gpu_pool_uuid_wire_name(&pool_uuid),
            SURFACE_SHARE_CLAIMANT_COLUMN_FIELD: claimant_column.index(),
            SURFACE_SHARE_POOL_SLOTS_FIELD: pool_slots,
        }),
        vec![duplicated_memfd],
    )
}

/// Hand a peer one pool slot's memory under its pool slot key, with no lease:
/// what keeps the slot from reuse is the peer's claim in the pool's claim
/// table, per frame.
fn handle_import_pool_slot(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    pool_slot_keys_handed_memory: &mut PoolSlotKeysHandedMemoryOnThisConnection,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(pool_slot_key) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if pool_slot_of_a_published_frame_id(pool_slot_key).is_some() {
        return (
            serde_json::json!({"error": "import_pool_slot names a pool slot key, never a \
                                published frame id"}),
            Vec::new(),
        );
    }
    let Some(checkout) = state.get_surface_planes(pool_slot_key) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    let reply_fds = match duplicate_the_handles_of_a_check_out(&checkout) {
        Ok(duplicated_fds) => duplicated_fds,
        Err(refusal) => return (refusal, Vec::new()),
    };
    pool_slot_keys_handed_memory.remember(pool_slot_key);
    (
        surface_share_check_out_metadata(state, pool_slot_key, checkout),
        reply_fds,
    )
}

/// Hand a viewer memory holding the published frame the request names, the
/// way the request says the viewer reads it, with no lease: the viewer's claim in the pool's claim table
/// keeps the frame's slot from reuse while the engine copies it. The engine
/// names the registration — the slot itself, or its GPU copy — and its
/// handles ride the reply unless the viewer lists that id as imported.
fn handle_import_viewed_frame(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    pool_slot_keys_handed_memory: &mut PoolSlotKeysHandedMemoryOnThisConnection,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(frame_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if pool_slot_of_a_published_frame_id(frame_id).is_none() {
        return (
            serde_json::json!({"error": format!(
                "{SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP} names a published frame id \
                 (<pool slot>#<generation>), never {frame_id:?}"
            )}),
            Vec::new(),
        );
    }
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), frame_id) {
        return (refusal, Vec::new());
    }
    let requested_read_by = request
        .get(SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD)
        .and_then(serde_json::Value::as_str);
    let Some(read_by) = requested_read_by.and_then(ViewedFrameReadBy::from_wire_name) else {
        return (
            serde_json::json!({"error": format!(
                "{SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP} states {SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD} \
                 as {:?} or {:?}, not {requested_read_by:?}",
                ViewedFrameReadBy::DisplayImport.wire_name(),
                ViewedFrameReadBy::CpuRead.wire_name(),
            )}),
            Vec::new(),
        );
    };
    let Some(viewed_frame_source) = state.viewed_frame_source() else {
        return (
            serde_json::json!({"error": "the engine has no GPU context yet, so no frame can be \
                                copied for a viewer; one exists once a stream has started"}),
            Vec::new(),
        );
    };
    let layout_the_producer_left_the_frame_in = request
        .get(SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD)
        .and_then(serde_json::Value::as_i64)
        .and_then(|raw_layout| i32::try_from(raw_layout).ok())
        .filter(|raw_layout| *raw_layout != 0)
        .map(streamlib_consumer_rhi::VulkanLayout);
    let import_id = match viewed_frame_source.surface_share_id_a_viewer_reads_the_frame_through(
        frame_id,
        read_by,
        layout_the_producer_left_the_frame_in,
    ) {
        Ok(import_id) => import_id,
        Err(refusal) => {
            return (
                serde_json::json!({"error": refusal.to_string()}),
                Vec::new(),
            );
        }
    };
    let Some(checkout) = state.get_surface_planes(&import_id) else {
        return (
            serde_json::json!({"error": format!(
                "the engine named {import_id:?} for frame {frame_id:?}, which surface-share does \
                 not hold"
            )}),
            Vec::new(),
        );
    };
    let the_viewer_holds_the_import = request
        .get(SURFACE_SHARE_VIEWED_FRAME_IMPORTS_HELD_FIELD)
        .and_then(serde_json::Value::as_array)
        .is_some_and(|held| {
            held.iter()
                .any(|held_id| held_id.as_str() == Some(&import_id))
        });
    let reply_fds = if the_viewer_holds_the_import {
        Vec::new()
    } else {
        match duplicate_the_handles_of_a_check_out(&checkout) {
            Ok(duplicated_fds) => duplicated_fds,
            Err(refusal) => return (refusal, Vec::new()),
        }
    };
    pool_slot_keys_handed_memory.remember(&import_id);
    (
        surface_share_check_out_metadata(state, &import_id, checkout),
        reply_fds,
    )
}

fn handle_release_check_out(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<RawFd>) {
    (
        answer_release_check_out(state.check_out_leases(), request, lease_holder),
        Vec::new(),
    )
}

fn handle_lookup(
    state: &SurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), surface_id) {
        return (refusal, Vec::new());
    }
    let Some(checkout) = state.get_surface_planes(surface_id) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    match duplicate_the_handles_of_a_check_out(&checkout) {
        Ok(duplicated_fds) => (
            surface_share_check_out_metadata(state, surface_id, checkout),
            duplicated_fds,
        ),
        Err(refusal) => (refusal, Vec::new()),
    }
}

/// Dup every plane fd, then `produce_done` and `consume_done` when the
/// registration carries them, in that order — the order a peer peels the
/// trailing timeline fds off in. The dups are independent of the table's own
/// copies; on a partial failure every dup already taken is closed.
fn duplicate_the_handles_of_a_check_out(
    checkout: &SurfacePlaneCheckout,
) -> Result<Vec<RawFd>, serde_json::Value> {
    let mut dup_fds: Vec<RawFd> = Vec::with_capacity(checkout.dma_buf_fds.len() + 2);
    let handles = checkout
        .dma_buf_fds
        .iter()
        .map(|fd| (*fd, "DMA-BUF"))
        .chain(checkout.produce_done_fd.map(|fd| (fd, "produce_done")))
        .chain(checkout.consume_done_fd.map(|fd| (fd, "consume_done")));
    for (source_fd, handle_name) in handles {
        let dup = unsafe { libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            for d in &dup_fds {
                unsafe { libc::close(*d) };
            }
            return Err(serde_json::json!({"error": format!("failed to dup {handle_name} FD")}));
        }
        dup_fds.push(dup);
    }
    Ok(dup_fds)
}

/// The registration metadata a `lookup` or `check_out` answers with,
/// `current_image_layout` included. `has_produce_done_fd` and
/// `has_consume_done_fd` state what the registration carries; the fds trail
/// the planes only on a reply that sends handles.
fn surface_share_check_out_metadata(
    state: &SurfaceShareState,
    surface_id: &str,
    checkout: SurfacePlaneCheckout,
) -> serde_json::Value {
    let surfaces = state.get_surfaces();
    let pool_slot_key = crate::core::rhi::pool_slot_key_of_surface_id(surface_id);
    let metadata = surfaces.iter().find(|s| s.surface_id == pool_slot_key);
    let (width, height, format, resource_type) = match metadata {
        Some(m) => (
            m.width,
            m.height,
            m.format.as_str(),
            m.resource_type.as_str(),
        ),
        None => (0, 0, "unknown", "pixel_buffer"),
    };
    let mut response = serde_json::json!({
        "surface_id": surface_id,
        "width": width,
        "height": height,
        "format": format,
        "resource_type": resource_type,
        "handle_type": checkout.handle_type,
        "plane_sizes": checkout.plane_sizes,
        "plane_offsets": checkout.plane_offsets,
        "plane_strides": checkout.plane_strides,
        "drm_format_modifier": checkout.drm_format_modifier,
        "has_produce_done_fd": checkout.produce_done_fd.is_some(),
        "has_consume_done_fd": checkout.consume_done_fd.is_some(),
        "current_image_layout": checkout.current_image_layout,
        "vk_image_type": checkout.vk_image_type,
        "vk_image_mip_levels": checkout.vk_image_mip_levels,
        "vk_image_array_layers": checkout.vk_image_array_layers,
        "vk_image_samples": checkout.vk_image_samples,
        "vk_image_tiling": checkout.vk_image_tiling,
        "vk_image_usage": checkout.vk_image_usage,
        "vk_image_allocation_size": checkout.vk_image_allocation_size,
    });
    // Echoed only when registered: absence is meaningful (see
    // `SurfaceMetadata::vk_memory_type_index`), so no default may appear
    // on the wire.
    if let Some(vk_memory_type_index) = checkout.vk_memory_type_index {
        response[SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX] = vk_memory_type_index.into();
    }
    if let Some(exporting_device_uuid) = checkout.exporting_device_uuid {
        response[SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID] = exporting_device_uuid.into();
    }
    echo_the_tensor_layout_onto_a_lookup_reply(&mut response, checkout.tensor_layout.as_ref());
    if let Some(pixel_planes_within_one_allocation) = &checkout.pixel_planes_within_one_allocation {
        pixel_planes_within_one_allocation.write_surface_share_fields(&mut response);
    }
    response
}

/// Publish the layout a peer left a texture in. A peer that holds the slot
/// checked out has released it to the external queue family in that layout,
/// and that release is recorded for the slot's owner to acquire.
fn handle_update_layout(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            return (
                serde_json::json!({"error": "missing surface_id"}),
                Vec::new(),
            );
        }
    };

    let layout = match request.get("current_image_layout").and_then(|v| v.as_i64()) {
        Some(v) => v as i32,
        None => {
            return (
                serde_json::json!({"error": "missing current_image_layout"}),
                Vec::new(),
            );
        }
    };

    let updated = state.update_image_layout(surface_id, layout);
    if updated
        && let Err(unrecorded_release) = state
            .check_out_leases()
            .record_image_layout_a_check_out_holder_released_in(surface_id, lease_holder, layout)
    {
        return (
            serde_json::json!({"error": unrecorded_release.to_string()}),
            Vec::new(),
        );
    }
    (serde_json::json!({"success": updated}), Vec::new())
}

fn handle_unregister(
    state: &SurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<RawFd>) {
    (answer_unregister(state, request), Vec::new())
}

fn handle_check_in(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    received_fds: &[RawFd],
) -> (serde_json::Value, Vec<RawFd>) {
    let runtime_id = requested_runtime_id(request);

    if received_fds.is_empty() {
        return (
            serde_json::json!({"error": "missing DMA-BUF fd(s)"}),
            Vec::new(),
        );
    }
    if received_fds.len() > MAX_DMA_BUF_PLANES {
        return (
            serde_json::json!({
                "error": format!(
                    "too many plane fds: {} > MAX_DMA_BUF_PLANES ({})",
                    received_fds.len(), MAX_DMA_BUF_PLANES
                )
            }),
            Vec::new(),
        );
    }

    let (plane_sizes, plane_offsets, plane_strides) =
        match parse_plane_arrays(request, received_fds.len()) {
            Some(arrays) => arrays,
            None => {
                return (
                    serde_json::json!({
                        "error": "plane_sizes/plane_offsets/plane_strides length mismatch"
                    }),
                    Vec::new(),
                );
            }
        };
    let drm_format_modifier = request
        .get("drm_format_modifier")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");
    let handle_type = request
        .get("handle_type")
        .and_then(|v| v.as_str())
        .unwrap_or("dma_buf");
    // Parse the optional VkImageCreateInfo block. `check_in` is the
    // subprocess-registers-pixel-buffer path that legacy DMA-BUF /
    // OPAQUE_FD buffer consumers ride; every field absent-defaults to
    // the OPAQUE_FD constructor's hardcoded shape so legacy callers
    // see no behavior change.
    let vk_image = parse_vk_image_create_info_fields(request);
    let RawHandleExportContractFields {
        vk_memory_type_index,
        exporting_device_uuid,
    } = parse_raw_handle_export_contract_fields(request);
    let pixel_planes_within_one_allocation =
        match pixel_planes_of_a_pixel_buffer_registration(request, &plane_sizes) {
            Ok(pixel_planes_within_one_allocation) => pixel_planes_within_one_allocation,
            Err(refusal) => return (serde_json::json!({"error": refusal}), Vec::new()),
        };

    let surface_id = uuid::Uuid::new_v4().to_string();

    let mut dup_fds: Vec<RawFd> = Vec::with_capacity(received_fds.len());
    for fd in received_fds {
        let dup = unsafe { libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            for d in &dup_fds {
                unsafe { libc::close(*d) };
            }
            return (
                serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
                Vec::new(),
            );
        }
        dup_fds.push(dup);
    }

    if let Err((leftover_planes, leftover_produce, leftover_consume)) =
        state.register_surface(SurfaceRegistration {
            surface_id: &surface_id,
            runtime_id,
            dma_buf_fds: dup_fds,
            plane_sizes,
            plane_offsets,
            plane_strides,
            width,
            height,
            format,
            resource_type,
            handle_type,
            drm_format_modifier,
            // check_in is the subprocess-registers-pixel-buffer path used by
            // CPU-readback / legacy DMA-BUF consumers. None of those carry
            // their own timelines through this op — host-registered
            // render-target paths (`handle_register`) provide the
            // single-writer-per-edge `produce_done` / `consume_done` FDs.
            produce_done_fd: None,
            consume_done_fd: None,
            // check_in surfaces are pixel buffers — `VkBuffer`s have no
            // image layout. Seed UNDEFINED; the field is unused for buffer
            // surfaces but must be populated for the wire format.
            current_image_layout: 0,
            vk_image_type: vk_image.vk_image_type,
            vk_image_mip_levels: vk_image.vk_image_mip_levels,
            vk_image_array_layers: vk_image.vk_image_array_layers,
            vk_image_samples: vk_image.vk_image_samples,
            vk_image_tiling: vk_image.vk_image_tiling,
            vk_image_usage: vk_image.vk_image_usage,
            vk_image_allocation_size: vk_image.vk_image_allocation_size,
            vk_memory_type_index,
            exporting_device_uuid,
            tensor_layout: None,
            pixel_planes_within_one_allocation,
        })
    {
        for fd in &leftover_planes {
            unsafe { libc::close(*fd) };
        }
        if let Some(fd) = leftover_produce {
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = leftover_consume {
            unsafe { libc::close(fd) };
        }
    }

    (serde_json::json!({"surface_id": surface_id}), Vec::new())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::SurfaceCheckOutLeaseRegistry;
    use crate::core::context::surface_share_wire_verbs::release_every_surface_registered_by;
    use crate::linux::surface_share::ViewedFrameSource;
    use std::os::unix::io::FromRawFd;
    use streamlib_surface_client::{connect_to_surface_share_socket, send_request_with_fds};
    use tempfile::TempDir;

    fn make_memfd_with(contents: &[u8]) -> RawFd {
        use std::io::{Seek, SeekFrom, Write};

        let name = std::ffi::CString::new("streamlib-runtime-surface-share-test").unwrap();
        let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(
            fd >= 0,
            "memfd_create failed: {}",
            std::io::Error::last_os_error()
        );
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(contents).expect("memfd write");
        file.seek(SeekFrom::Start(0)).expect("memfd rewind");
        use std::os::unix::io::IntoRawFd;
        file.into_raw_fd()
    }

    fn read_all_from_fd(fd: RawFd) -> Vec<u8> {
        use std::io::{Read, Seek, SeekFrom};

        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.seek(SeekFrom::Start(0)).expect("recv memfd rewind");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("recv memfd read");
        buf
    }

    /// A socket path in a directory of its own. `sun_path` is 108 bytes and a
    /// unique name in a shared temp dir spends most of that budget before
    /// `TMPDIR` is even accounted for; a private dir keeps the path short and
    /// unlinks it on drop. The returned [`TempDir`] owns that lifetime.
    fn tmp_socket_path() -> (TempDir, PathBuf) {
        let dir = crate::core::test_support::a_temporary_directory_at_owner_only_mode()
            .expect("temp dir for test socket");
        let path = dir.path().join("surface-share.sock");
        (dir, path)
    }

    /// Register a one-plane surface over the wire and return the id it lives
    /// under.
    fn check_in_one_surface(stream: &UnixStream) -> String {
        let send_fd = make_memfd_with(b"surface-check-out-lease-test");
        let (response, _no_reply_fds) = send_request_with_fds(
            stream,
            &serde_json::json!({
                "op": "check_in",
                "runtime_id": "lease-test-runtime",
                "width": 64,
                "height": 64,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
            }),
            &[send_fd],
            0,
        )
        .expect("check_in request");
        unsafe { libc::close(send_fd) };
        response
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in the check_in response")
            .to_string()
    }

    fn close_every_fd(fds: &[RawFd]) {
        for fd in fds {
            unsafe { libc::close(*fd) };
        }
    }

    fn ask(stream: &UnixStream, request: &serde_json::Value) -> serde_json::Value {
        let (response, reply_fds) =
            send_request_with_fds(stream, request, &[], MAX_DMA_BUF_PLANES).expect("wire request");
        close_every_fd(&reply_fds);
        response
    }

    fn started_service(state: SurfaceShareState) -> (TempDir, PathBuf, UnixSocketSurfaceService) {
        let (socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));
        (socket_dir, socket_path, service)
    }

    /// The reclaim runs on the connection's own thread once its read loop
    /// ends, so the drop is observed rather than assumed.
    fn lease_count_settling_to(
        check_out_leases: &SurfaceCheckOutLeaseRegistry,
        surface_id: &str,
        expected: u32,
    ) -> u32 {
        for _ in 0..200 {
            let outstanding = check_out_leases
                .outstanding_check_out_count(surface_id)
                .expect("the lease table stays readable");
            if outstanding == expected {
                return outstanding;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        check_out_leases
            .outstanding_check_out_count(surface_id)
            .expect("the lease table stays readable")
    }

    /// A checkout claims the frame; a lookup does not. Both hand back the
    /// same planes, but a lookup is the host reading its own table and holds
    /// nothing — leasing it would pin a pool slot no consumer is reading.
    #[test]
    fn a_check_out_claims_the_frame_and_a_lookup_leaves_it_free() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let (_socket_dir, socket_path, mut service) = started_service(state);

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        let surface_id = check_in_one_surface(&stream);

        ask(
            &stream,
            &serde_json::json!({"op": "lookup", "surface_id": surface_id}),
        );
        assert_eq!(
            check_out_leases
                .outstanding_check_out_count(&surface_id)
                .unwrap(),
            0,
            "a lookup holds nothing, so it must pin no pool slot"
        );

        ask(
            &stream,
            &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
        );
        assert_eq!(
            check_out_leases
                .outstanding_check_out_count(&surface_id)
                .unwrap(),
            1,
            "a checkout claims the frame against producer reuse"
        );

        drop(stream);
        service.stop();
    }

    /// The explicit release, and the rule that one connection can never unpin
    /// another's frame.
    #[test]
    fn release_check_out_frees_only_the_connection_that_took_it() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let (_socket_dir, socket_path, mut service) = started_service(state);

        let holder = connect_to_surface_share_socket(&socket_path).expect("connect the holder");
        let surface_id = check_in_one_surface(&holder);
        ask(
            &holder,
            &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
        );

        let bystander =
            connect_to_surface_share_socket(&socket_path).expect("connect the bystander");
        let bystander_release = ask(
            &bystander,
            &serde_json::json!({"op": "release_check_out", "surface_id": surface_id}),
        );
        assert_eq!(
            bystander_release.get("released").and_then(|v| v.as_bool()),
            Some(false),
            "a connection that never checked the surface out has nothing to release"
        );
        assert_eq!(
            check_out_leases
                .outstanding_check_out_count(&surface_id)
                .unwrap(),
            1,
            "one connection must never be able to unpin another's frame"
        );

        let holder_release = ask(
            &holder,
            &serde_json::json!({"op": "release_check_out", "surface_id": surface_id}),
        );
        assert_eq!(
            holder_release.get("released").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            check_out_leases
                .outstanding_check_out_count(&surface_id)
                .unwrap(),
            0,
            "the frame returns to its producer once its reader lets go"
        );

        drop(holder);
        drop(bystander);
        service.stop();
    }

    /// The backstop: a child killed mid-frame never sends a release, so the
    /// kernel closing its socket is what returns the slots. Unlike the
    /// registration watchdog this is not gated on the peer being out of
    /// process — a lease may never outlive its reader, whoever the reader is.
    #[test]
    fn a_dropped_connection_releases_every_lease_it_was_holding() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let (_socket_dir, socket_path, mut service) = started_service(state);

        let publisher =
            connect_to_surface_share_socket(&socket_path).expect("connect the publisher");
        let surface_id = check_in_one_surface(&publisher);

        let reader = connect_to_surface_share_socket(&socket_path).expect("connect the reader");
        for _ in 0..2 {
            ask(
                &reader,
                &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
            );
        }
        assert_eq!(
            check_out_leases
                .outstanding_check_out_count(&surface_id)
                .unwrap(),
            2
        );

        drop(reader);
        assert_eq!(
            lease_count_settling_to(&check_out_leases, &surface_id, 0),
            0,
            "a reader that died holding frames must not pin their slots forever"
        );

        drop(publisher);
        service.stop();
    }

    #[test]
    fn check_in_check_out_roundtrip_preserves_fd_content() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");

        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let pattern = b"streamlib-runtime-surface-share-test-fd-contents-0123456789";
        let send_fd = make_memfd_with(pattern);

        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 640,
            "height": 480,
            "format": "bgra32",
            "resource_type": "pixel_buffer",
        });
        let (check_in_resp, check_in_fds) =
            send_request_with_fds(&stream, &check_in_req, &[send_fd], 0).expect("check_in request");
        unsafe { libc::close(send_fd) };
        assert!(check_in_fds.is_empty(), "check_in must not return an fd");
        let surface_id = check_in_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in response")
            .to_string();
        assert!(!surface_id.is_empty());

        let check_out_req = serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        });
        let (check_out_resp, check_out_fds) =
            send_request_with_fds(&stream, &check_out_req, &[], MAX_DMA_BUF_PLANES)
                .expect("check_out request");
        assert_eq!(
            check_out_resp.get("width").and_then(|v| v.as_u64()),
            Some(640)
        );
        assert_eq!(
            check_out_resp.get("height").and_then(|v| v.as_u64()),
            Some(480)
        );
        assert_eq!(
            check_out_resp.get("format").and_then(|v| v.as_str()),
            Some("bgra32")
        );
        assert_eq!(check_out_fds.len(), 1, "single-plane: exactly one fd");
        let received_fd = check_out_fds[0];
        assert!(received_fd >= 0);
        let received = read_all_from_fd(received_fd);
        assert_eq!(received, pattern);

        let release_req = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": "test-runtime",
        });
        let _ = send_request_with_fds(&stream, &release_req, &[], 0).expect("release request");

        drop(stream);
        service.stop();
    }

    #[test]
    fn check_out_unknown_surface_id_returns_error_no_fd() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        let req = serde_json::json!({
            "op": "check_out",
            "surface_id": "never-registered",
        });
        let (resp, fds) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
            .expect("check_out request");
        assert!(fds.is_empty());
        assert!(resp.get("error").and_then(|v| v.as_str()).is_some());

        drop(stream);
        service.stop();
    }

    /// Two memfds with distinct content registered under one surface_id via
    /// `check_in` must round-trip intact through `check_out`, including the
    /// plane-layout metadata that lets the consumer mmap each plane at its
    /// own size. This is the defining multi-plane DMA-BUF test.
    #[test]
    fn check_in_check_out_multi_fd_roundtrip() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let pattern_y = b"plane-Y-bytes-A123456789";
        let pattern_uv = b"plane-UV-bytes-Z987654321";
        let fd_y = make_memfd_with(pattern_y);
        let fd_uv = make_memfd_with(pattern_uv);

        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 1920,
            "height": 1080,
            "format": "nv12_video_range",
            "resource_type": "pixel_buffer",
            "plane_sizes": [pattern_y.len() as u64, pattern_uv.len() as u64],
            "plane_offsets": [0u64, 0u64],
            "plane_strides": [1920u64, 1920u64],
            "drm_format_modifier": 0u64,
        });
        let (check_in_resp, check_in_fds) =
            send_request_with_fds(&stream, &check_in_req, &[fd_y, fd_uv], 0)
                .expect("check_in request");
        unsafe {
            libc::close(fd_y);
            libc::close(fd_uv);
        }
        assert!(check_in_fds.is_empty(), "check_in must not return any fd");
        let surface_id = check_in_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in response")
            .to_string();

        let check_out_req = serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        });
        let (check_out_resp, check_out_fds) =
            send_request_with_fds(&stream, &check_out_req, &[], MAX_DMA_BUF_PLANES)
                .expect("check_out request");
        assert_eq!(
            check_out_fds.len(),
            2,
            "both planes delivered via SCM_RIGHTS"
        );
        assert_eq!(
            read_all_from_fd(check_out_fds[0]),
            pattern_y,
            "plane 0 content preserved"
        );
        assert_eq!(
            read_all_from_fd(check_out_fds[1]),
            pattern_uv,
            "plane 1 content preserved"
        );
        let sizes = check_out_resp
            .get("plane_sizes")
            .and_then(|v| v.as_array())
            .expect("plane_sizes array in response");
        assert_eq!(
            sizes
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![pattern_y.len() as u64, pattern_uv.len() as u64],
        );

        let _ = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "release",
                "surface_id": surface_id,
                "runtime_id": "test-runtime",
            }),
            &[],
            0,
        );

        drop(stream);
        service.stop();
    }

    /// `handle_type` rides along through the register/lookup wire path.
    /// Subprocess consumers read
    /// it to dispatch on the import API: `"dma_buf"` →
    /// `ConsumerVulkanBuffer::from_dma_buf_fds`, `"opaque_fd"` →
    /// `ConsumerVulkanBuffer::from_opaque_fd`. The default-when-absent
    /// behavior (`"dma_buf"`) preserves backward compatibility with every
    /// adapter that predates this field. Locking the round-trip contract
    /// for both halves: explicit-OPAQUE_FD and default-DMA-BUF.
    #[test]
    fn handle_type_round_trips_explicit_opaque_fd_and_default_dma_buf() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        // Half 1: explicit OPAQUE_FD round-trip. The wire treats the FD
        // opaquely — a memfd stands in for what the host RHI would emit
        // from `HostVulkanBuffer::export_opaque_fd_memory`.
        let send_fd = make_memfd_with(b"opaque-fd-fixture");
        let opaque_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 64,
            "format": "bgra8_unorm",
            "resource_type": "pixel_buffer",
            "handle_type": "opaque_fd",
            "plane_sizes": [64u64 * 64 * 4],
            "plane_offsets": [0u64],
            "plane_strides": [0u64],
        });
        let (opaque_resp, _) =
            send_request_with_fds(&stream, &opaque_req, &[send_fd], 0).expect("check_in opaque_fd");
        unsafe { libc::close(send_fd) };
        let opaque_surface_id = opaque_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id")
            .to_string();

        let lookup_resp = {
            let req = serde_json::json!({
                "op": "check_out",
                "surface_id": opaque_surface_id,
            });
            let (resp, fds) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
                .expect("check_out opaque_fd");
            for fd in &fds {
                unsafe { libc::close(*fd) };
            }
            resp
        };
        assert_eq!(
            lookup_resp.get("handle_type").and_then(|v| v.as_str()),
            Some("opaque_fd"),
            "OPAQUE_FD round-trip: lookup must echo registered handle_type",
        );

        // Half 2: handle_type-absent register defaults to dma_buf on both
        // wire-parse and lookup-emit. Lock the back-compat contract so a
        // future schema change can't silently flip the default.
        let send_fd2 = make_memfd_with(b"legacy-dma-buf-fixture");
        let legacy_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 64,
            "format": "bgra8_unorm",
            "resource_type": "pixel_buffer",
            "plane_sizes": [64u64 * 64 * 4],
            "plane_offsets": [0u64],
            "plane_strides": [0u64],
            // no handle_type — must default to "dma_buf"
        });
        let (legacy_resp, _) =
            send_request_with_fds(&stream, &legacy_req, &[send_fd2], 0).expect("check_in legacy");
        unsafe { libc::close(send_fd2) };
        let legacy_surface_id = legacy_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id")
            .to_string();

        let legacy_lookup = {
            let req = serde_json::json!({
                "op": "check_out",
                "surface_id": legacy_surface_id,
            });
            let (resp, fds) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
                .expect("check_out legacy");
            for fd in &fds {
                unsafe { libc::close(*fd) };
            }
            resp
        };
        assert_eq!(
            legacy_lookup.get("handle_type").and_then(|v| v.as_str()),
            Some("dma_buf"),
            "absent handle_type must default to dma_buf on both register and lookup",
        );

        for sid in [&opaque_surface_id, &legacy_surface_id] {
            let _ = send_request_with_fds(
                &stream,
                &serde_json::json!({
                    "op": "release",
                    "surface_id": sid,
                    "runtime_id": "test-runtime",
                }),
                &[],
                0,
            );
        }

        drop(stream);
        service.stop();
    }

    /// `VkImageTiling::DRM_FORMAT_MODIFIER_EXT` as the wire carries it.
    /// `src/linux/` deliberately takes no vulkanalia dependency, so the raw
    /// value is spelled here rather than read off `vk::ImageTiling`.
    const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i64 = 1_000_158_000;

    /// `drm_format_modifier` and `plane_strides` ride along through the
    /// register/lookup path. The host adapter writes the modifier into the
    /// `SurfaceTransportHandle` field defined in `streamlib-surface-adapter`; the
    /// consumer reads it from the lookup response and passes it to
    /// `EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT` (or the Vulkan equivalent).
    /// A round-trip through the wire is the test that locks the contract.
    #[test]
    fn drm_format_modifier_and_strides_round_trip() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let send_fd = make_memfd_with(b"rt-render-target-payload");
        // An NVIDIA-tiled modifier from the live probe (one of the values
        // NVIDIA EGL samples linear DMA-BUFs only).
        // The wire treats it opaquely — no need to validate the exact bits.
        let chosen_modifier: u64 = 0x0300_0000_0060_6014;
        let pitch: u64 = 1920 * 4;
        let req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 1920,
            "height": 1080,
            "format": "bgra8_unorm",
            "resource_type": "texture",
            "plane_sizes": [pitch * 1080],
            "plane_offsets": [0u64],
            "plane_strides": [pitch],
            "drm_format_modifier": chosen_modifier,
        });
        let (resp, _) =
            send_request_with_fds(&stream, &req, &[send_fd], 0).expect("check_in request");
        unsafe { libc::close(send_fd) };
        let surface_id = resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in response")
            .to_string();

        let lookup_req = serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        });
        let (lookup_resp, lookup_fds) =
            send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
                .expect("check_out request");
        for fd in &lookup_fds {
            unsafe { libc::close(*fd) };
        }

        assert_eq!(
            lookup_resp
                .get("drm_format_modifier")
                .and_then(|v| v.as_u64()),
            Some(chosen_modifier),
            "modifier round-trip: lookup must echo the registered value verbatim",
        );
        assert_eq!(
            lookup_resp
                .get("plane_strides")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_u64()).collect::<Vec<_>>()),
            Some(vec![pitch]),
            "plane_strides round-trip: lookup must echo registered values verbatim",
        );

        let _ = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "release",
                "surface_id": surface_id,
                "runtime_id": "test-runtime",
            }),
            &[],
            0,
        );
        drop(stream);
        service.stop();
    }

    /// A zero `drm_format_modifier` is `DRM_FORMAT_MOD_LINEAR` when the image
    /// was created with `DRM_FORMAT_MODIFIER_EXT` tiling and "no modifier at
    /// all" otherwise. The consumer can only tell the two apart if the tiling
    /// crosses the wire beside the modifier, so both must survive the
    /// round trip.
    #[test]
    fn a_linear_modifier_stays_distinguishable_from_no_modifier_across_the_wire() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let send_fd = make_memfd_with(b"linear-modifier-payload");
        let pitch: u64 = 64 * 4;
        let req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 64,
            "format": "bgra8_unorm",
            "resource_type": "texture",
            "plane_sizes": [pitch * 64],
            "plane_offsets": [0u64],
            "plane_strides": [pitch],
            // DRM_FORMAT_MOD_LINEAR — a real modifier the driver chose,
            // whose numeric value is indistinguishable from "unset".
            "drm_format_modifier": 0u64,
            "vk_image_tiling": VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
        });
        let (resp, _) =
            send_request_with_fds(&stream, &req, &[send_fd], 0).expect("check_in request");
        unsafe { libc::close(send_fd) };
        let surface_id = resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in response")
            .to_string();

        let (lookup_resp, lookup_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("check_out request");
        for fd in &lookup_fds {
            unsafe { libc::close(*fd) };
        }

        assert_eq!(
            lookup_resp
                .get("drm_format_modifier")
                .and_then(|v| v.as_u64()),
            Some(0),
            "the LINEAR modifier is zero and must echo back as zero",
        );
        assert_eq!(
            lookup_resp.get("vk_image_tiling").and_then(|v| v.as_i64()),
            Some(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT),
            "without the tiling beside it, a consumer reads the zero modifier \
             as an image that never went through VK_EXT_image_drm_format_modifier",
        );

        let _ = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "release",
                "surface_id": surface_id,
                "runtime_id": "test-runtime",
            }),
            &[],
            0,
        );
        drop(stream);
        service.stop();
    }

    /// A helper connects lazily on its first claim, and the claim is what
    /// keeps a shallow output pool from recycling the frame it names — so
    /// time spent before the service accepts is time the frame can vanish in.
    #[test]
    fn a_new_connections_first_request_is_answered_without_waiting_on_the_accept_loop() {
        const FRESH_CONNECTIONS: u32 = 40;
        // Forty accepts that each wait on a 50 ms nap take about two seconds;
        // forty prompt accepts take a few ms, leaving room for a loaded CI
        // worker.
        const ALL_FRESH_CONNECTIONS_ANSWERED_WITHIN: std::time::Duration =
            std::time::Duration::from_millis(250);
        let (_socket_dir, socket_path, mut service) = started_service(SurfaceShareState::new());

        let started_at = std::time::Instant::now();
        for connection_index in 0..FRESH_CONNECTIONS {
            let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
            let response = ask(
                &stream,
                &serde_json::json!({
                    "op": "check_out",
                    "surface_id": format!("no-such-surface-{connection_index}"),
                }),
            );
            assert!(
                response.get("error").is_some(),
                "an unknown id is refused, got {response}"
            );
        }
        let elapsed = started_at.elapsed();

        assert!(
            elapsed < ALL_FRESH_CONNECTIONS_ANSWERED_WITHIN,
            "{FRESH_CONNECTIONS} fresh connections took {elapsed:?} to be answered; the accept \
             loop is making each new connection wait"
        );
        service.stop();
    }

    /// Same-process wire connections (the host runtime opening a diagnostic
    /// connection to its own surface-share socket to publish surfaces) must NOT
    /// trigger the watchdog. SO_PEERCRED reports the same pid, so the disconnect
    /// classification rejects it as a subprocess and cleanup is skipped.
    /// Wire-level coverage of the actual subprocess-crash path lives in the
    /// `surface_share_subprocess_crash` integration test.
    #[test]
    fn same_process_disconnect_does_not_trigger_watchdog() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        let runtime_id = "host-publishes-then-disconnects";
        let send_fd = make_memfd_with(b"host-published-fixture");
        let (resp, _) = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "check_in",
                "runtime_id": runtime_id,
                "width": 16,
                "height": 16,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
            }),
            &[send_fd],
            0,
        )
        .expect("check_in request");
        unsafe { libc::close(send_fd) };
        let surface_id = resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id")
            .to_string();
        assert_eq!(state.surface_ids_by_runtime(runtime_id).len(), 1);

        drop(stream);

        // Give the per-connection thread plenty of time to observe EOF and,
        // if the watchdog were to misfire, run cleanup. The surface MUST
        // survive: same-process publishers intentionally disconnect.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            state.surface_ids_by_runtime(runtime_id),
            vec![surface_id],
            "same-process disconnect must not release host-published surfaces",
        );

        service.stop();
    }

    /// Watchdog primitive (pure-function): given a state populated with
    /// surfaces under multiple runtime_ids, `release_every_surface_registered_by`
    /// releases only the targeted runtime's surfaces and is idempotent on
    /// second call.
    #[test]
    fn release_every_surface_registered_by_is_scoped_and_idempotent() {
        let state = SurfaceShareState::new();
        // Use real memfds so release_surface's libc::close calls operate on
        // valid fds (no fd-table corruption from -1 sentinels).
        let mk = |label: &str| -> Vec<RawFd> {
            let name = std::ffi::CString::new(label).unwrap();
            let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
            assert!(fd >= 0);
            vec![fd]
        };
        for (sid, rid, label) in [
            ("victim-1", "victim-runtime", "v1"),
            ("victim-2", "victim-runtime", "v2"),
            ("survivor", "survivor-runtime", "s"),
        ] {
            state
                .register_surface(SurfaceRegistration {
                    surface_id: sid,
                    runtime_id: rid,
                    dma_buf_fds: mk(label),
                    plane_sizes: vec![0],
                    plane_offsets: vec![0],
                    plane_strides: vec![0],
                    width: 1,
                    height: 1,
                    format: "bgra32",
                    resource_type: "pixel_buffer",
                    handle_type: "dma_buf",
                    drm_format_modifier: 0,
                    produce_done_fd: None,
                    consume_done_fd: None,
                    current_image_layout: 0,
                    vk_image_type: VK_IMAGE_TYPE_DEFAULT,
                    vk_image_mip_levels: VK_IMAGE_MIP_LEVELS_DEFAULT,
                    vk_image_array_layers: VK_IMAGE_ARRAY_LAYERS_DEFAULT,
                    vk_image_samples: VK_IMAGE_SAMPLES_DEFAULT,
                    vk_image_tiling: VK_IMAGE_TILING_DEFAULT,
                    vk_image_usage: VK_IMAGE_USAGE_DEFAULT,
                    vk_image_allocation_size: VK_IMAGE_ALLOCATION_SIZE_DEFAULT,
                    vk_memory_type_index: None,
                    exporting_device_uuid: None,
                    tensor_layout: None,
                    pixel_planes_within_one_allocation: None,
                })
                .expect("register");
        }

        release_every_surface_registered_by(&state, "victim-runtime");
        assert!(state.surface_ids_by_runtime("victim-runtime").is_empty());
        assert_eq!(
            state.surface_ids_by_runtime("survivor-runtime"),
            vec!["survivor".to_string()]
        );

        // Idempotent second call: nothing left for the victim, survivor
        // unaffected. Nothing panics.
        release_every_surface_registered_by(&state, "victim-runtime");
        assert_eq!(
            state.surface_ids_by_runtime("survivor-runtime"),
            vec!["survivor".to_string()]
        );

        // Cleanup of a runtime with no registrations is a no-op.
        release_every_surface_registered_by(&state, "never-registered");
        assert_eq!(
            state.surface_ids_by_runtime("survivor-runtime"),
            vec!["survivor".to_string()]
        );
    }

    /// The service must refuse a check_in whose fd count exceeds the wire
    /// helper's SCM_RIGHTS cap instead of truncating silently. The check
    /// runs in the wire helper before the handler, so this exercises the
    /// shared back-pressure path every caller relies on.
    ///
    /// Cap shape:
    /// - `MAX_DMA_BUF_PLANES = 4` — the data-plane cap that adapters
    ///   enforce; "this many plane fds, no more."
    /// - `MAX_SCM_RIGHTS_FDS = MAX_DMA_BUF_PLANES + 1 = 5` — the wire-level
    ///   cap that leaves a slot for an optional timeline-semaphore fd
    ///   alongside the plane fds.
    ///   Sentinel: `MAX_SCM_RIGHTS_FDS + 1 = 6` is "one over the cap."
    #[test]
    fn oversize_fd_vec_rejected() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        // MAX_SCM_RIGHTS_FDS + 1 fds — one over the wire-level cap.
        let fds: Vec<RawFd> = (0..=MAX_SCM_RIGHTS_FDS)
            .map(|i| make_memfd_with(format!("plane-{}", i).as_bytes()))
            .collect();

        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 640,
            "height": 480,
            "format": "Custom",
            "plane_sizes": vec![0u64; fds.len()],
            "plane_offsets": vec![0u64; fds.len()],
        });

        // The wire helper rejects with InvalidInput *before* any syscall,
        // without closing the caller-owned fds.
        let err = send_request_with_fds(&stream, &check_in_req, &fds, 0)
            .expect_err("oversize vec must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        // Every caller fd is still valid; no leaks, no double-closes.
        for fd in &fds {
            let rc = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
            assert!(rc >= 0, "caller fd {} must still be valid", fd);
            unsafe { libc::close(*fd) };
        }

        drop(stream);
        service.stop();
    }

    /// Positive-case sibling of [`oversize_fd_vec_rejected`] — the wire
    /// helper MUST accept exactly [`MAX_SCM_RIGHTS_FDS`] fds. Locks in
    /// the high edge of the cap so a future cap shift fails loudly on
    /// whichever edge actually moved (without this, only the
    /// over-the-cap edge is regression-tested and a cap *increase* could
    /// silently widen the wire surface).
    #[test]
    fn at_cap_fd_vec_accepted_by_wire_helper() {
        // The wire helper itself is what we're gating — no daemon is
        // needed beyond a connected socket so the helper's syscall path
        // can run. Spinning up a real service-and-handler combo would
        // also work but introduces ordering noise; the unit-level helper
        // gate is the narrowest test.
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        // Exactly MAX_SCM_RIGHTS_FDS fds — the upper edge of the cap.
        // Send-only: we don't care whether the daemon parses the JSON
        // payload (it won't — `Custom` is a sentinel format string), only
        // that the wire helper itself doesn't reject for fd-count.
        let fds: Vec<RawFd> = (0..MAX_SCM_RIGHTS_FDS)
            .map(|i| make_memfd_with(format!("at-cap-{}", i).as_bytes()))
            .collect();
        assert_eq!(fds.len(), MAX_SCM_RIGHTS_FDS);

        let req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime-at-cap",
            "width": 640,
            "height": 480,
            "format": "Custom",
            "plane_sizes": vec![0u64; fds.len()],
            "plane_offsets": vec![0u64; fds.len()],
        });

        // The wire helper must NOT reject for fd-count. The daemon may
        // still error on payload parsing — we don't gate on the response,
        // only on the helper's own input-validation step. A successful
        // send (regardless of subsequent response shape) proves the cap
        // edge is accepted.
        let send_result = send_message_with_fds(&stream, req.to_string().as_bytes(), &fds);
        assert!(
            send_result.is_ok(),
            "wire helper must accept exactly MAX_SCM_RIGHTS_FDS={} fds; got {:?}",
            MAX_SCM_RIGHTS_FDS,
            send_result
        );

        // Caller fds are independent of what the daemon does — close them.
        for fd in &fds {
            let rc = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
            assert!(rc >= 0, "caller fd {} must still be valid", fd);
            unsafe { libc::close(*fd) };
        }

        drop(stream);
        service.stop();
    }

    /// The raw-handle export contract fields round-trip through the wire,
    /// and their absence stays absence: a producer that registers
    /// `vk_memory_type_index` / `exporting_device_uuid` sees both echoed
    /// on lookup, and a producer that never states them gets a response
    /// with neither key — `0` is a valid memory type index, so no default
    /// may be conjured on the wire.
    #[test]
    fn raw_handle_export_fields_round_trip_and_absence_stays_absent() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let send_fd = make_memfd_with(b"raw-handle-export-fixture");
        let register_req = serde_json::json!({
            "op": "register",
            "surface_id": "raw-handle-export-rt",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 64,
            "format": "rgba16_float",
            "resource_type": "texture",
            "handle_type": "opaque_fd",
            "vk_image_allocation_size": 65_536u64,
            "vk_memory_type_index": 7u32,
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
        });
        let (register_resp, _) =
            send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
        assert_eq!(
            register_resp.get("success").and_then(|v| v.as_bool()),
            Some(true),
            "register must succeed: {register_resp:?}",
        );

        let (lookup_resp, lookup_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "lookup", "surface_id": "raw-handle-export-rt"}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("lookup request");
        close_every_fd(&lookup_fds);
        assert_eq!(
            lookup_resp
                .get("vk_memory_type_index")
                .and_then(|v| v.as_u64()),
            Some(7),
        );
        assert_eq!(
            lookup_resp
                .get("exporting_device_uuid")
                .and_then(|v| v.as_str()),
            Some("00112233445566778899aabbccddeeff"),
        );

        let bare_register_req = serde_json::json!({
            "op": "register",
            "surface_id": "raw-handle-export-bare",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 64,
            "format": "bgra32",
            "resource_type": "pixel_buffer",
        });
        let (bare_register_resp, _) =
            send_request_with_fds(&stream, &bare_register_req, &[send_fd], 0)
                .expect("bare register request");
        unsafe { libc::close(send_fd) };
        assert_eq!(
            bare_register_resp.get("success").and_then(|v| v.as_bool()),
            Some(true),
        );
        let (bare_lookup_resp, bare_lookup_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "lookup", "surface_id": "raw-handle-export-bare"}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("bare lookup request");
        close_every_fd(&bare_lookup_fds);
        assert!(bare_lookup_resp.get("vk_memory_type_index").is_none());
        assert!(bare_lookup_resp.get("exporting_device_uuid").is_none());

        drop(stream);
        service.stop();
    }

    /// A `check_in` keeps the raw-handle export contract the same way a
    /// `register` does: an OPAQUE_FD pixel buffer checked in with both fields
    /// sees them echoed on lookup and check-out — index `0` included — and a
    /// check-in that states neither gets neither back.
    #[test]
    fn a_check_in_echoes_the_raw_handle_export_fields_it_states_and_no_others() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        let send_fd = make_memfd_with(b"check-in-raw-handle-export-fixture");

        let check_in = |request: serde_json::Value| -> String {
            let (check_in_resp, _) =
                send_request_with_fds(&stream, &request, &[send_fd], 0).expect("check_in request");
            check_in_resp
                .get("surface_id")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("check_in must answer a surface id: {check_in_resp:?}"))
                .to_string()
        };
        let stated_surface_id = check_in(serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 32,
            "format": "rgba32",
            "handle_type": "opaque_fd",
            "plane_sizes": [8192u64],
            "plane_offsets": [0u64],
            "vk_memory_type_index": 0u32,
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
        }));
        let bare_surface_id = check_in(serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 32,
            "format": "rgba32",
            "handle_type": "dma_buf",
            "plane_sizes": [8192u64],
            "plane_offsets": [0u64],
        }));
        unsafe { libc::close(send_fd) };

        for op in ["lookup", "check_out"] {
            let (stated_resp, stated_fds) = send_request_with_fds(
                &stream,
                &serde_json::json!({"op": op, "surface_id": stated_surface_id}),
                &[],
                MAX_DMA_BUF_PLANES,
            )
            .expect("request for the stated surface");
            close_every_fd(&stated_fds);
            assert_eq!(
                stated_resp
                    .get("vk_memory_type_index")
                    .and_then(|v| v.as_u64()),
                Some(0),
                "{op} must echo the stated index, zero included: {stated_resp:?}"
            );
            assert_eq!(
                stated_resp
                    .get("exporting_device_uuid")
                    .and_then(|v| v.as_str()),
                Some("00112233445566778899aabbccddeeff"),
                "{op}: {stated_resp:?}"
            );

            let (bare_resp, bare_fds) = send_request_with_fds(
                &stream,
                &serde_json::json!({"op": op, "surface_id": bare_surface_id}),
                &[],
                MAX_DMA_BUF_PLANES,
            )
            .expect("request for the bare surface");
            close_every_fd(&bare_fds);
            assert!(bare_resp.get("vk_memory_type_index").is_none(), "{op}");
            assert!(bare_resp.get("exporting_device_uuid").is_none(), "{op}");
        }

        drop(stream);
        service.stop();
    }

    /// A `storage_buffer` registration's tensor shape and dtype round-trip
    /// through a checkout, and one that omits them is refused before the table
    /// takes its fd.
    #[test]
    fn a_tensor_storage_buffer_registration_round_trips_its_shape_and_dtype() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path, mut service) = started_service(state.clone());
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        let send_fd = make_memfd_with(b"tensor-storage-buffer-fixture");

        let (register_resp, _) = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "register",
                "surface_id": "tensor-storage-buffer",
                "runtime_id": "test-runtime",
                "resource_type": "storage_buffer",
                "handle_type": "opaque_fd",
                "plane_sizes": [462u64],
                "plane_offsets": [0u64],
                "plane_strides": [0u64],
                "shape": [3u64, 7, 11],
                "dtype": "float16",
                "vk_memory_type_index": 2u32,
                "exporting_device_uuid": "00112233445566778899aabbccddeeff",
            }),
            &[send_fd],
            0,
        )
        .expect("register request");
        assert_eq!(
            register_resp.get("success").and_then(|v| v.as_bool()),
            Some(true),
            "register must succeed: {register_resp:?}",
        );

        let (check_out_resp, check_out_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "check_out", "surface_id": "tensor-storage-buffer"}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("check_out request");
        assert_eq!(
            check_out_fds.len(),
            1,
            "the tensor's memory crosses as one fd"
        );
        close_every_fd(&check_out_fds);
        assert_eq!(check_out_resp["resource_type"], "storage_buffer");
        assert_eq!(check_out_resp["shape"], serde_json::json!([3, 7, 11]));
        assert_eq!(check_out_resp["dtype"], "float16");
        assert_eq!(check_out_resp["plane_sizes"], serde_json::json!([462]));

        let (refused_resp, _) = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "register",
                "surface_id": "tensor-without-a-shape",
                "runtime_id": "test-runtime",
                "resource_type": "storage_buffer",
                "handle_type": "opaque_fd",
                "dtype": "float32",
            }),
            &[send_fd],
            0,
        )
        .expect("register request");
        unsafe { libc::close(send_fd) };
        assert!(
            refused_resp
                .get("error")
                .and_then(|v| v.as_str())
                .is_some_and(|refusal| refusal.contains("no shape")),
            "a tensor registration without its shape must be refused: {refused_resp:?}",
        );
        assert!(state.get_surface_planes("tensor-without-a-shape").is_none());

        drop(stream);
        service.stop();
    }

    /// Full `VkImageCreateInfo` round-trip through the wire: a
    /// producer ships every `vk_image_*` field on register; the consumer
    /// reads them off lookup. Locks the OPAQUE_FD VkImage consumer's
    /// ability to rebuild a matching `VkImage` whose
    /// `cudaExternalMemoryMipmappedArrayDesc` equals the host's
    /// byte-for-byte. Mentally revert any of the seven echo writes in
    /// `handle_lookup` and this test fails — the seven assertions are
    /// independent and field-specific.
    #[test]
    fn vk_image_create_info_round_trip_through_register_lookup() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let send_fd = make_memfd_with(b"opaque-fd-vkimage-fixture");
        // Non-default values for every field so a regression that zeroes
        // one is caught against a deliberate non-zero assert.
        let register_req = serde_json::json!({
            "op": "register",
            "surface_id": "vk-image-wire-rt",
            "runtime_id": "test-runtime",
            "width": 256,
            "height": 256,
            "format": "rgba16_float",
            "resource_type": "texture",
            "handle_type": "opaque_fd",
            "vk_image_type": 2,               // VK_IMAGE_TYPE_3D
            "vk_image_mip_levels": 9,
            "vk_image_array_layers": 6,
            "vk_image_samples": 4,            // _4
            "vk_image_tiling": VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
            "vk_image_usage": 0x4Fu32,
            "vk_image_allocation_size": 16_777_216u64,
        });
        let (register_resp, _) =
            send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
        unsafe { libc::close(send_fd) };
        assert_eq!(
            register_resp.get("success").and_then(|v| v.as_bool()),
            Some(true),
            "register must succeed: {:?}",
            register_resp
        );

        let lookup_req = serde_json::json!({
            "op": "lookup",
            "surface_id": "vk-image-wire-rt",
        });
        let (lookup_resp, lookup_fds) =
            send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
                .expect("lookup request");
        for fd in &lookup_fds {
            unsafe { libc::close(*fd) };
        }

        assert_eq!(
            lookup_resp.get("vk_image_type").and_then(|v| v.as_i64()),
            Some(2),
        );
        assert_eq!(
            lookup_resp
                .get("vk_image_mip_levels")
                .and_then(|v| v.as_u64()),
            Some(9),
        );
        assert_eq!(
            lookup_resp
                .get("vk_image_array_layers")
                .and_then(|v| v.as_u64()),
            Some(6),
        );
        assert_eq!(
            lookup_resp.get("vk_image_samples").and_then(|v| v.as_i64()),
            Some(4),
        );
        assert_eq!(
            lookup_resp.get("vk_image_tiling").and_then(|v| v.as_i64()),
            Some(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT),
        );
        assert_eq!(
            lookup_resp.get("vk_image_usage").and_then(|v| v.as_u64()),
            Some(0x4F),
        );
        assert_eq!(
            lookup_resp
                .get("vk_image_allocation_size")
                .and_then(|v| v.as_u64()),
            Some(16_777_216),
        );

        let _ = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "release",
                "surface_id": "vk-image-wire-rt",
                "runtime_id": "test-runtime",
            }),
            &[],
            0,
        );
        drop(stream);
        service.stop();
    }

    /// Register a one-plane pool-slot surface under `slot_id` over the wire.
    fn register_pool_slot(stream: &UnixStream, slot_id: &str) {
        let send_fd = make_memfd_with(b"frame-generation-test-backing");
        let (response, _no_reply_fds) = send_request_with_fds(
            stream,
            &serde_json::json!({
                "op": "register",
                "surface_id": slot_id,
                "runtime_id": "frame-generation-test-runtime",
                "width": 64,
                "height": 64,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
            }),
            &[send_fd],
            0,
        )
        .expect("register request");
        unsafe { libc::close(send_fd) };
        assert_eq!(
            response.get("success").and_then(|v| v.as_bool()),
            Some(true),
            "register must succeed: {response:?}"
        );
    }

    fn wire_error_of(response: &serde_json::Value) -> &str {
        response
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    }

    /// Over the wire, the slot registers once, its frames publish
    /// generations, and only the current generation's id checks out. The
    /// retired id fails with an error naming the recycling — never with the
    /// recycled slot's planes.
    #[test]
    fn a_retired_frame_id_is_refused_and_the_current_one_serves() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        register_pool_slot(&stream, "pool-slot-a");
        check_out_leases
            .publish_frame_generation("pool-slot-a", 1)
            .expect("the pool publishes the first frame");

        let (current, current_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "check_out", "surface_id": "pool-slot-a#1"}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("check_out of the live frame");
        close_every_fd(&current_fds);
        assert!(
            current.get("error").is_none(),
            "the live frame must check out: {current:?}"
        );
        let (_, released) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "release_check_out", "surface_id": "pool-slot-a#1"}),
            &[],
            0,
        )
        .expect("release of the live frame");
        close_every_fd(&released);

        // The producer recycles the slot: generation 2 publishes, 1 retires.
        check_out_leases
            .publish_frame_generation("pool-slot-a", 2)
            .expect("the pool recycles the slot");

        for op in ["check_out", "lookup"] {
            let (refused, refused_fds) = send_request_with_fds(
                &stream,
                &serde_json::json!({"op": op, "surface_id": "pool-slot-a#1"}),
                &[],
                MAX_DMA_BUF_PLANES,
            )
            .expect("the request itself goes through");
            assert!(refused_fds.is_empty(), "no planes may accompany a refusal");
            close_every_fd(&refused_fds);
            assert!(
                wire_error_of(&refused).contains("recycled"),
                "{op} of a retired frame id must fail naming the recycling, got: {refused:?}"
            );
        }

        let (served, served_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "check_out", "surface_id": "pool-slot-a#2"}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("check_out of the new frame");
        close_every_fd(&served_fds);
        assert!(
            served.get("error").is_none(),
            "the new generation must check out: {served:?}"
        );
        assert_eq!(
            lease_count_settling_to(&check_out_leases, "pool-slot-a", 1),
            1,
            "exactly the surviving checkout holds the slot"
        );

        drop(stream);
        service.stop();
    }

    fn check_out_on_the_wire(
        stream: &UnixStream,
        surface_id: &str,
        holds_pool_slot_import: bool,
    ) -> (serde_json::Value, usize) {
        let (response, reply_fds) = send_request_with_fds(
            stream,
            &serde_json::json!({
                "op": "check_out",
                "surface_id": surface_id,
                SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD: holds_pool_slot_import,
            }),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("check_out request");
        close_every_fd(&reply_fds);
        (response, reply_fds.len())
    }

    fn retired_pool_slots_of(response: &serde_json::Value) -> Vec<String> {
        response
            .get(SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD)
            .and_then(|v| v.as_array())
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| key.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A helper that holds a slot's import is answered the frame's metadata
    /// and no fd, and still takes the frame's lease; a recycled frame is
    /// refused by name whether or not it says it holds the slot.
    #[test]
    fn a_peer_holding_the_pool_slot_import_takes_the_lease_and_receives_no_fds() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        register_pool_slot(&stream, "pool-slot-held");
        check_out_leases
            .publish_frame_generation("pool-slot-held", 1)
            .expect("the pool publishes the first frame");
        let (first, first_fd_count) = check_out_on_the_wire(&stream, "pool-slot-held#1", false);
        assert!(first.get("error").is_none(), "{first:?}");
        assert_eq!(
            first_fd_count, 1,
            "the slot's first check-out carries its memory"
        );

        check_out_leases
            .publish_frame_generation("pool-slot-held", 2)
            .expect("the pool publishes the next frame");
        let (second, second_fd_count) = check_out_on_the_wire(&stream, "pool-slot-held#2", true);
        assert!(second.get("error").is_none(), "{second:?}");
        assert_eq!(second_fd_count, 0, "a held slot is answered without an fd");
        assert_eq!(second.get("width").and_then(|v| v.as_u64()), Some(64));
        assert!(second.get("current_image_layout").is_some());
        assert_eq!(
            lease_count_settling_to(&check_out_leases, "pool-slot-held", 2),
            2,
            "both check-outs take a lease"
        );

        check_out_leases
            .publish_frame_generation("pool-slot-held", 3)
            .expect("the pool recycles the slot");
        for holds_pool_slot_import in [false, true] {
            let (refused, refused_fd_count) =
                check_out_on_the_wire(&stream, "pool-slot-held#1", holds_pool_slot_import);
            assert_eq!(refused_fd_count, 0);
            assert!(
                wire_error_of(&refused).contains("recycled"),
                "holds_pool_slot_import={holds_pool_slot_import}: {refused:?}"
            );
        }

        drop(stream);
        service.stop();
    }

    /// A peer handed a pool's claim table gets the table's memfd, a column for
    /// its process, and every leased row's key; its claim through the mapped
    /// fd holds the slot in the engine's own table.
    #[test]
    fn a_claim_table_hand_over_maps_the_engines_rows_and_names_every_leased_slot() {
        use crate::core::context::gpu_pool_claim_table::GpuPoolClaimTablesOfOnePool;
        use std::os::fd::OwnedFd;
        use streamlib_ipc_types::GpuPoolClaimTableMemfd;
        use streamlib_ipc_types::GpuPoolClaimantColumn;
        use streamlib_surface_client::GpuPoolClaimTableHandOver;

        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let mut claim_tables = GpuPoolClaimTablesOfOnePool::named("handed-over");
        let leased_rows: Vec<_> = ["slot-a", "slot-b"]
            .into_iter()
            .map(|pool_slot_key| {
                claim_tables
                    .lease_a_row_for_a_slot(check_out_leases.gpu_pool_claim_tables(), pool_slot_key)
                    .unwrap()
            })
            .collect();
        let pool_uuid = leased_rows[0].table().pool_uuid();
        leased_rows[1].row().publish_generation(4);
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let (reply, reply_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP,
                SURFACE_SHARE_GPU_POOL_FIELD: gpu_pool_uuid_wire_name(&pool_uuid),
            }),
            &[],
            MAX_SCM_RIGHTS_FDS,
        )
        .expect("claim table request");

        assert!(reply.get("error").is_none(), "{reply:?}");
        let hand_over = GpuPoolClaimTableHandOver::from_reply(&reply).unwrap();
        assert_eq!(
            hand_over
                .rows_leased_to_slots
                .iter()
                .map(|leased_row| (leased_row.row_index, leased_row.pool_slot_key.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "slot-a"), (1, "slot-b")]
        );
        assert!(
            hand_over
                .rows_leased_to_slots
                .iter()
                .all(|leased_row| leased_row.row_lease_number
                    == leased_rows[leased_row.row_index as usize]
                        .row()
                        .lease_number()),
            "each row is listed under the lease its shared row carries"
        );
        let [memfd] = reply_fds.as_slice() else {
            panic!("one memfd, got {reply_fds:?}");
        };
        // SAFETY: the fd arrived over SCM_RIGHTS and is owned by nothing else.
        let mapped =
            GpuPoolClaimTableMemfd::adopt(unsafe { OwnedFd::from_raw_fd(*memfd) }).unwrap();
        let column = GpuPoolClaimantColumn::at(hand_over.claimant_column_index).unwrap();
        mapped.rows()[1].claim_frame(column, 4).unwrap();
        assert!(leased_rows[1].row().is_claimed());

        drop(stream);
        service.stop();
    }

    /// A claimant process that sends an oversized message has its connection
    /// dropped, is stopped, and only once it has exited is its column
    /// cleared, freeing the slot it left claimed for a rehand.
    #[test]
    fn a_claimant_sending_an_oversized_message_is_stopped_and_its_column_cleared() {
        a_claimant_breaking_the_protocol_is_stopped_and_its_column_cleared("oversized");
    }

    /// A claimant process that sends a message that is not JSON is stopped
    /// the same way, and its column cleared once it has exited.
    #[test]
    fn a_claimant_sending_a_malformed_message_is_stopped_and_its_column_cleared() {
        a_claimant_breaking_the_protocol_is_stopped_and_its_column_cleared("malformed");
    }

    fn a_claimant_breaking_the_protocol_is_stopped_and_its_column_cleared(
        how_it_breaks_the_protocol: &str,
    ) {
        use crate::core::context::gpu_pool_claim_table::GpuPoolClaimTablesOfOnePool;
        use std::io::{BufRead as _, Write as _};
        use streamlib_ipc_types::{
            GPU_POOL_CLAIM_TABLE_ROW_BYTES, GPU_POOL_CLAIM_TABLE_ROW_CLAIM_COUNTS_OFFSET,
            GpuPoolSlotRehandTest,
        };

        const CLAIMANT_THAT_BREAKS_THE_PROTOCOL: &str = r#"
import json, mmap, socket, struct, sys, time
path, pool, row, row_bytes, counts_offset = sys.argv[1], sys.argv[2], *map(int, sys.argv[3:6])
how_it_breaks_the_protocol = sys.argv[6]
connection = socket.socket(socket.AF_UNIX)
connection.connect(path)
request = json.dumps({"op": "claim_table_of_gpu_pool", "pool": pool}).encode()
connection.sendall(struct.pack(">I", len(request)) + request)
(reply_len,) = struct.unpack(">I", connection.recv(4))
reply, fds, _, _ = socket.recv_fds(connection, reply_len, 1)
column = json.loads(reply)["claimant_column"]
table = mmap.mmap(fds[0], 0)
count_at = row * row_bytes + counts_offset + 4 * column
table[count_at:count_at + 4] = struct.pack("<I", 1)
print("claimed", flush=True)
sys.stdin.readline()
if how_it_breaks_the_protocol == "oversized":
    connection.sendall(struct.pack(">I", 0xFFFFFFFF))
else:
    not_json = b"{not json"
    connection.sendall(struct.pack(">I", len(not_json)) + not_json)
time.sleep(30)
"#;
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let mut claim_tables = GpuPoolClaimTablesOfOnePool::named("fail-stop");
        let leased_row = claim_tables
            .lease_a_row_for_a_slot(check_out_leases.gpu_pool_claim_tables(), "fail-stop-slot")
            .unwrap();
        leased_row.row().publish_generation(1);
        let (_socket_dir, socket_path, mut service) = started_service(state);

        let Ok(mut claimant) = std::process::Command::new("python3")
            .arg("-c")
            .arg(CLAIMANT_THAT_BREAKS_THE_PROTOCOL)
            .arg(&socket_path)
            .arg(gpu_pool_uuid_wire_name(&leased_row.table().pool_uuid()))
            .arg(leased_row.row_index().to_string())
            .arg(GPU_POOL_CLAIM_TABLE_ROW_BYTES.to_string())
            .arg(GPU_POOL_CLAIM_TABLE_ROW_CLAIM_COUNTS_OFFSET.to_string())
            .arg(how_it_breaks_the_protocol)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            tracing::warn!("skipping: no python3 to stand in for a claimant process");
            service.stop();
            return;
        };
        let mut claimant_says = String::new();
        std::io::BufReader::new(claimant.stdout.take().unwrap())
            .read_line(&mut claimant_says)
            .unwrap();
        assert_eq!(claimant_says.trim(), "claimed");
        assert_eq!(
            leased_row.row().retire_for_a_rehand(1),
            GpuPoolSlotRehandTest::HeldByAClaim
        );

        writeln!(claimant.stdin.take().unwrap()).unwrap();
        let exit = claimant.wait().unwrap();
        assert!(
            std::os::unix::process::ExitStatusExt::signal(&exit) == Some(libc::SIGKILL),
            "the service stopped the claimant: {exit:?}"
        );
        let column_cleared_within = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while leased_row.row().is_claimed() && std::time::Instant::now() < column_cleared_within {
            std::thread::yield_now();
        }
        assert_eq!(
            leased_row.row().retire_for_a_rehand(1),
            GpuPoolSlotRehandTest::RetiredForTheRehand
        );

        service.stop();
    }

    /// A pool the engine holds no table for, or a malformed pool name, is
    /// refused with no fd.
    #[test]
    fn a_claim_table_hand_over_of_an_unknown_or_malformed_pool_is_refused() {
        let (_socket_dir, socket_path, mut service) = started_service(SurfaceShareState::new());
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        for pool in ["00112233445566778899aabbccddeeff", "not-a-pool"] {
            let (reply, reply_fds) = send_request_with_fds(
                &stream,
                &serde_json::json!({
                    "op": SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP,
                    SURFACE_SHARE_GPU_POOL_FIELD: pool,
                }),
                &[],
                MAX_SCM_RIGHTS_FDS,
            )
            .expect("claim table request");
            assert!(reply.get("error").is_some(), "{pool}: {reply:?}");
            assert!(reply_fds.is_empty());
        }

        drop(stream);
        service.stop();
    }

    /// A slot imported by its key carries its memory and takes no lease, and
    /// its retirement is listed on the importing connection like a check-out's.
    #[test]
    fn importing_a_pool_slot_carries_its_memory_and_takes_no_lease() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        register_pool_slot(&stream, "pool-slot-imported");

        let (imported, imported_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": SURFACE_SHARE_IMPORT_POOL_SLOT_OP,
                "surface_id": "pool-slot-imported",
            }),
            &[],
            MAX_SCM_RIGHTS_FDS,
        )
        .expect("import request");
        close_every_fd(&imported_fds);

        assert!(imported.get("error").is_none(), "{imported:?}");
        assert_eq!(imported_fds.len(), 1);
        assert_eq!(imported.get("width").and_then(|v| v.as_u64()), Some(64));
        assert_eq!(
            lease_count_settling_to(&check_out_leases, "pool-slot-imported", 0),
            0
        );
        let (refused, _) = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": SURFACE_SHARE_IMPORT_POOL_SLOT_OP,
                "surface_id": "pool-slot-imported#1",
            }),
            &[],
            MAX_SCM_RIGHTS_FDS,
        )
        .expect("import request");
        assert!(
            wire_error_of(&refused).contains("pool slot key"),
            "{refused:?}"
        );

        drop(stream);
        service.stop();
    }

    /// One ask the stand-in source was given: the frame, how the viewer
    /// reads it, and the layout the frame's message stated.
    type ViewedFrameAsked = (
        String,
        ViewedFrameReadBy,
        Option<streamlib_consumer_rhi::VulkanLayout>,
    );

    /// Stands in for the engine's GPU copy: answers a fixed registration for
    /// every frame, and records each ask.
    struct ViewedFrameSourceAnsweringOneRegistration {
        registration_it_answers: String,
        frames_asked: parking_lot::Mutex<Vec<ViewedFrameAsked>>,
    }

    impl ViewedFrameSource for ViewedFrameSourceAnsweringOneRegistration {
        fn surface_share_id_a_viewer_reads_the_frame_through(
            &self,
            frame_id: &str,
            read_by: ViewedFrameReadBy,
            layout_the_producer_left_the_frame_in: Option<streamlib_consumer_rhi::VulkanLayout>,
        ) -> crate::core::error::Result<String> {
            self.frames_asked.lock().push((
                frame_id.to_string(),
                read_by,
                layout_the_producer_left_the_frame_in,
            ));
            Ok(self.registration_it_answers.clone())
        }
    }

    fn ask_for_a_viewed_frame(
        stream: &UnixStream,
        frame_id: &str,
        viewed_frame_imports_held: &[&str],
        frame_image_layout: i32,
    ) -> (serde_json::Value, Vec<RawFd>) {
        ask_for_a_viewed_frame_read_by(
            stream,
            frame_id,
            ViewedFrameReadBy::DisplayImport.wire_name(),
            viewed_frame_imports_held,
            frame_image_layout,
        )
    }

    fn ask_for_a_viewed_frame_read_by(
        stream: &UnixStream,
        frame_id: &str,
        read_by_wire_name: &str,
        viewed_frame_imports_held: &[&str],
        frame_image_layout: i32,
    ) -> (serde_json::Value, Vec<RawFd>) {
        let (reply, reply_fds) = send_request_with_fds(
            stream,
            &serde_json::json!({
                "op": SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP,
                "surface_id": frame_id,
                SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD: read_by_wire_name,
                SURFACE_SHARE_VIEWED_FRAME_IMPORTS_HELD_FIELD: viewed_frame_imports_held,
                SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD: frame_image_layout,
            }),
            &[],
            MAX_SCM_RIGHTS_FDS,
        )
        .expect("viewed frame request");
        close_every_fd(&reply_fds);
        (reply, reply_fds)
    }

    /// A viewer's ask is answered with the registration the engine names for
    /// the frame, its handles riding the reply until the viewer lists it as
    /// held, the frame's stated layout reaching the engine, and no lease taken.
    #[test]
    fn a_viewed_frame_answers_the_registration_the_engine_names() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let viewed_frame_source = Arc::new(ViewedFrameSourceAnsweringOneRegistration {
            registration_it_answers: "pool-slot-viewed-display-importable-staging".to_string(),
            frames_asked: parking_lot::Mutex::new(Vec::new()),
        });
        state.serve_viewed_frames_from(viewed_frame_source.clone());
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        register_pool_slot(&stream, "pool-slot-viewed");
        register_pool_slot(&stream, "pool-slot-viewed-display-importable-staging");
        check_out_leases
            .publish_frame_generation("pool-slot-viewed", 3)
            .expect("the pool publishes the frame");

        let (first, first_fds) = ask_for_a_viewed_frame(&stream, "pool-slot-viewed#3", &[], 1);
        assert!(first.get("error").is_none(), "{first:?}");
        assert_eq!(
            first.get("surface_id").and_then(|v| v.as_str()),
            Some("pool-slot-viewed-display-importable-staging")
        );
        assert_eq!(first_fds.len(), 1);

        let (held, held_fds) = ask_for_a_viewed_frame(
            &stream,
            "pool-slot-viewed#3",
            &["pool-slot-viewed-display-importable-staging"],
            0,
        );
        assert!(held.get("error").is_none(), "{held:?}");
        assert!(held_fds.is_empty(), "a held import is sent no handle");

        let (cpu_read, _) = ask_for_a_viewed_frame_read_by(
            &stream,
            "pool-slot-viewed#3",
            ViewedFrameReadBy::CpuRead.wire_name(),
            &[],
            0,
        );
        assert!(cpu_read.get("error").is_none(), "{cpu_read:?}");

        assert_eq!(
            *viewed_frame_source.frames_asked.lock(),
            vec![
                (
                    "pool-slot-viewed#3".to_string(),
                    ViewedFrameReadBy::DisplayImport,
                    Some(streamlib_consumer_rhi::VulkanLayout(1))
                ),
                (
                    "pool-slot-viewed#3".to_string(),
                    ViewedFrameReadBy::DisplayImport,
                    None
                ),
                (
                    "pool-slot-viewed#3".to_string(),
                    ViewedFrameReadBy::CpuRead,
                    None
                ),
            ]
        );
        assert_eq!(
            lease_count_settling_to(&check_out_leases, "pool-slot-viewed#3", 0),
            0
        );
        drop(stream);
        service.stop();
    }

    /// The ask names a published frame, the engine must have a GPU context to
    /// serve it, and a frame its slot moved past is refused before the
    /// engine is asked.
    #[test]
    fn a_viewed_frame_ask_is_refused_by_name_where_it_cannot_be_served() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let state_the_test_keeps = state.clone();
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        register_pool_slot(&stream, "pool-slot-unserved");
        check_out_leases
            .publish_frame_generation("pool-slot-unserved", 2)
            .expect("the pool publishes the frame");

        let (not_a_frame, _) = ask_for_a_viewed_frame(&stream, "pool-slot-unserved", &[], 0);
        assert!(
            wire_error_of(&not_a_frame).contains("published frame id"),
            "{not_a_frame:?}"
        );
        let (no_gpu, _) = ask_for_a_viewed_frame(&stream, "pool-slot-unserved#2", &[], 0);
        assert!(
            wire_error_of(&no_gpu).contains("no GPU context"),
            "{no_gpu:?}"
        );

        let viewed_frame_source = Arc::new(ViewedFrameSourceAnsweringOneRegistration {
            registration_it_answers: "pool-slot-unserved".to_string(),
            frames_asked: parking_lot::Mutex::new(Vec::new()),
        });
        state_the_test_keeps.serve_viewed_frames_from(viewed_frame_source.clone());
        let (retired, _) = ask_for_a_viewed_frame(&stream, "pool-slot-unserved#1", &[], 0);
        assert!(retired.get("error").is_some(), "{retired:?}");
        let (unknown_reading, _) =
            ask_for_a_viewed_frame_read_by(&stream, "pool-slot-unserved#2", "dma_buf", &[], 0);
        assert!(
            wire_error_of(&unknown_reading).contains(SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD),
            "{unknown_reading:?}"
        );
        assert!(viewed_frame_source.frames_asked.lock().is_empty());

        service.stop();
        assert!(
            state_the_test_keeps.viewed_frame_source().is_none(),
            "a stopped service lets go of what serves viewed frames"
        );
        drop(stream);
    }

    /// An unregistered slot the connection was handed memory for is listed
    /// on that connection's next reply, once, and on no other connection.
    #[test]
    fn unregistering_a_slot_lists_it_on_the_next_reply_of_the_connection_it_was_handed_to() {
        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let reader = connect_to_surface_share_socket(&socket_path).expect("connect reader");
        let producer = connect_to_surface_share_socket(&socket_path).expect("connect producer");

        register_pool_slot(&producer, "pool-slot-retiring");
        check_out_leases
            .publish_frame_generation("pool-slot-retiring", 1)
            .expect("the pool publishes the first frame");
        let (checked_out, _) = check_out_on_the_wire(&reader, "pool-slot-retiring#1", false);
        assert!(retired_pool_slots_of(&checked_out).is_empty());
        let released = ask(
            &reader,
            &serde_json::json!({"op": "release_check_out", "surface_id": "pool-slot-retiring#1"}),
        );
        assert!(retired_pool_slots_of(&released).is_empty());

        let unregistered = ask(
            &producer,
            &serde_json::json!({
                "op": "unregister",
                "surface_id": "pool-slot-retiring",
                "runtime_id": "frame-generation-test-runtime",
            }),
        );
        assert_eq!(
            unregistered.get("success").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(
            retired_pool_slots_of(&unregistered).is_empty(),
            "the producer was never handed the slot's memory"
        );

        let next_reply = ask(
            &reader,
            &serde_json::json!({"op": "lookup", "surface_id": "no-such-surface"}),
        );
        assert_eq!(
            retired_pool_slots_of(&next_reply),
            vec!["pool-slot-retiring".to_string()]
        );
        let reply_after = ask(
            &reader,
            &serde_json::json!({"op": "lookup", "surface_id": "no-such-surface"}),
        );
        assert!(
            retired_pool_slots_of(&reply_after).is_empty(),
            "a retired slot is listed once"
        );

        drop(reader);
        drop(producer);
        service.stop();
    }

    /// A generation suffix over a slot the pool never published fails
    /// closed — nothing can prove that frame exists.
    #[test]
    fn a_generation_over_an_unpublished_slot_is_refused_on_the_wire() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        register_pool_slot(&stream, "pool-slot-b");
        let (refused, refused_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": "check_out", "surface_id": "pool-slot-b#1"}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("the request itself goes through");
        close_every_fd(&refused_fds);
        assert!(
            wire_error_of(&refused).contains("refused"),
            "an unpublished generation must fail closed, got: {refused:?}"
        );

        drop(stream);
        service.stop();
    }

    /// The `#<generation>` suffix is the published-frame grammar; a
    /// registration carrying one would fork the table's key space.
    #[test]
    fn registering_an_id_with_a_generation_suffix_is_refused() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path, mut service) = started_service(state);
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let send_fd = make_memfd_with(b"reserved-grammar");
        let (response, _no_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "register",
                "surface_id": "looks-like-a-frame#3",
                "runtime_id": "frame-generation-test-runtime",
                "width": 64,
                "height": 64,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
            }),
            &[send_fd],
            0,
        )
        .expect("register request");
        unsafe { libc::close(send_fd) };
        assert!(
            wire_error_of(&response).contains("reserved"),
            "a #<generation> registration must be refused: {response:?}"
        );

        drop(stream);
        service.stop();
    }

    /// Back-compat sibling of [`vk_image_create_info_round_trip_through_register_lookup`]:
    /// a register payload that omits the seven `vk_image_*` fields must
    /// surface the documented defaults on lookup. This is the contract
    /// every legacy producer (DMA-BUF VkImage, OPAQUE_FD VkBuffer)
    /// relies on — without the defaults, a daemon serving an old client
    /// would echo zero / unknown values and break the consumer's
    /// VkImageCreateInfo reconstruction. Mentally revert one of the
    /// `unwrap_or(...)` defaults in `parse_vk_image_create_info_fields`
    /// and this test fails on the corresponding assertion.
    ///
    /// Default constants live in `surface_share_wire_verbs::VK_IMAGE_*_DEFAULT` —
    /// the test mirrors their literal values so a default change
    /// surfaces here too.
    #[test]
    fn vk_image_create_info_absent_fields_surface_documented_defaults() {
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let send_fd = make_memfd_with(b"legacy-no-vk-image-fields");
        let register_req = serde_json::json!({
            "op": "register",
            "surface_id": "legacy-defaults",
            "runtime_id": "test-runtime",
            "width": 64,
            "height": 64,
            "format": "rgba8_unorm",
            "resource_type": "texture",
            // No vk_image_* fields — exercise the defaults path.
        });
        let (register_resp, _) =
            send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
        unsafe { libc::close(send_fd) };
        assert_eq!(
            register_resp.get("success").and_then(|v| v.as_bool()),
            Some(true),
            "legacy register must succeed: {:?}",
            register_resp
        );

        let lookup_req = serde_json::json!({
            "op": "lookup",
            "surface_id": "legacy-defaults",
        });
        let (lookup_resp, lookup_fds) =
            send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
                .expect("lookup request");
        for fd in &lookup_fds {
            unsafe { libc::close(*fd) };
        }

        // VK_IMAGE_TYPE_2D = 1, mip = 1, layers = 1, samples = 1,
        // tiling = OPTIMAL = 0,
        // usage = TRANSFER_SRC | TRANSFER_DST | SAMPLED | STORAGE = 0x0F,
        // allocation_size = 0 (consumer derives from width * height * bpp).
        assert_eq!(
            lookup_resp.get("vk_image_type").and_then(|v| v.as_i64()),
            Some(1),
            "absent vk_image_type defaults to VK_IMAGE_TYPE_2D",
        );
        assert_eq!(
            lookup_resp
                .get("vk_image_mip_levels")
                .and_then(|v| v.as_u64()),
            Some(1),
        );
        assert_eq!(
            lookup_resp
                .get("vk_image_array_layers")
                .and_then(|v| v.as_u64()),
            Some(1),
        );
        assert_eq!(
            lookup_resp.get("vk_image_samples").and_then(|v| v.as_i64()),
            Some(1),
        );
        assert_eq!(
            lookup_resp.get("vk_image_tiling").and_then(|v| v.as_i64()),
            Some(0),
            "absent vk_image_tiling defaults to VK_IMAGE_TILING_OPTIMAL",
        );
        assert_eq!(
            lookup_resp.get("vk_image_usage").and_then(|v| v.as_u64()),
            Some(0x0F),
            "absent vk_image_usage defaults to TRANSFER_SRC|TRANSFER_DST|SAMPLED|STORAGE",
        );
        assert_eq!(
            lookup_resp
                .get("vk_image_allocation_size")
                .and_then(|v| v.as_u64()),
            Some(0),
            "absent vk_image_allocation_size defaults to 0 (consumer-derived)",
        );

        let _ = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "release",
                "surface_id": "legacy-defaults",
                "runtime_id": "test-runtime",
            }),
            &[],
            0,
        );
        drop(stream);
        service.stop();
    }

    /// Round-trip `current_image_layout` through register → lookup, and
    /// flip it through `update_layout`. Producers seed the layout at registration and
    /// re-publish it after their QFOT release barrier records, so the
    /// next consumer's lookup sees the post-release layout instead of
    /// the synthesized UNDEFINED.
    #[test]
    fn current_image_layout_round_trip_through_register_lookup_update() {
        // VK_IMAGE_LAYOUT_GENERAL = 1, SHADER_READ_ONLY_OPTIMAL = 5.
        let state = SurfaceShareState::new();
        let (_socket_dir, socket_path) = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let send_fd = make_memfd_with(b"layout-test");
        let register_req = serde_json::json!({
            "op": "register",
            "surface_id": "layout-rt",
            "runtime_id": "test-runtime",
            "width": 16,
            "height": 16,
            "format": "rgba8_unorm",
            "resource_type": "texture",
            "current_image_layout": 1,
        });
        let (register_resp, _) =
            send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
        unsafe { libc::close(send_fd) };
        assert_eq!(
            register_resp.get("success").and_then(|v| v.as_bool()),
            Some(true),
            "register must succeed: {:?}",
            register_resp
        );

        let lookup_req = serde_json::json!({
            "op": "lookup",
            "surface_id": "layout-rt",
        });
        let (lookup_resp, lookup_fds) =
            send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
                .expect("lookup request");
        for fd in &lookup_fds {
            unsafe { libc::close(*fd) };
        }
        assert_eq!(
            lookup_resp
                .get("current_image_layout")
                .and_then(|v| v.as_i64()),
            Some(1),
            "lookup must echo the producer-declared layout"
        );

        let update_req = serde_json::json!({
            "op": "update_layout",
            "surface_id": "layout-rt",
            "current_image_layout": 5,
        });
        let (update_resp, _) =
            send_request_with_fds(&stream, &update_req, &[], 0).expect("update_layout request");
        assert_eq!(
            update_resp.get("success").and_then(|v| v.as_bool()),
            Some(true),
            "update_layout must succeed: {:?}",
            update_resp
        );

        let (lookup_resp, lookup_fds) =
            send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
                .expect("lookup after update");
        for fd in &lookup_fds {
            unsafe { libc::close(*fd) };
        }
        assert_eq!(
            lookup_resp
                .get("current_image_layout")
                .and_then(|v| v.as_i64()),
            Some(5),
            "subsequent lookup must see post-update layout"
        );

        // Updating an unknown surface_id reports failure rather than a
        // crash — handle_update_layout must be idempotent against the
        // race with release_every_surface_registered_by.
        let bad_update = serde_json::json!({
            "op": "update_layout",
            "surface_id": "missing",
            "current_image_layout": 0,
        });
        let (bad_resp, _) =
            send_request_with_fds(&stream, &bad_update, &[], 0).expect("bad update");
        assert_eq!(
            bad_resp.get("success").and_then(|v| v.as_bool()),
            Some(false),
            "missing surface_id must report success=false"
        );

        drop(stream);
        service.stop();
    }
}

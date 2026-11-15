// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Pool slots, generations, GPU pool claim tables, and the frames a viewer asks for.

use super::*;

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
    let mapped = GpuPoolClaimTableMemfd::adopt(unsafe { OwnedFd::from_raw_fd(*memfd) }).unwrap();
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

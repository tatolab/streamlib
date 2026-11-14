// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use super::*;

/// A pool slot import stands in for the importer's own.
type PoolSlotImportUnderTest = Arc<u32>;

fn read_one_request(stream: &mut UnixStream) {
    let mut length_prefix = [0u8; 4];
    stream
        .read_exact(&mut length_prefix)
        .expect("read the length");
    let mut request = vec![0u8; u32::from_be_bytes(length_prefix) as usize];
    stream.read_exact(&mut request).expect("read the request");
}

fn answer(stream: &mut UnixStream, reply: &serde_json::Value) {
    let reply_bytes = serde_json::to_vec(reply).expect("serialize the reply");
    stream
        .write_all(&(reply_bytes.len() as u32).to_be_bytes())
        .expect("write the length");
    stream.write_all(&reply_bytes).expect("write the reply");
}

/// A connection is opened with the response timeout, so a service that stops
/// answering never holds the calling thread, or every thread waiting on the
/// connection's lock behind it, for good.
#[test]
fn a_surface_share_connection_is_opened_with_the_response_timeout_both_ways() {
    let socket_directory = tempfile::tempdir().expect("a socket directory");
    let socket_path = socket_directory.path().join("answers.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        read_one_request(&mut stream);
        answer(&mut stream, &serde_json::json!({ "ok": true }));
        stream
    });
    let connection =
        SurfaceShareConnectionOfThisImporter::<PoolSlotImportUnderTest>::new(socket_path);

    connection
        .request(&serde_json::json!({ "op": "check_out" }))
        .expect("the round trip");

    let open_connection = connection.open_connection.lock();
    let open_stream = open_connection
        .as_ref()
        .expect("a completed exchange keeps its connection");
    assert_eq!(
        open_stream
            .read_timeout()
            .expect("the connection's read timeout is readable"),
        Some(SURFACE_SHARE_RESPONSE_TIMEOUT)
    );
    assert_eq!(
        open_stream
            .write_timeout()
            .expect("the connection's write timeout is readable"),
        Some(SURFACE_SHARE_RESPONSE_TIMEOUT)
    );
    drop(open_connection);
    drop(server.join().expect("server thread"));
}

/// A reply listing a retired pool slot drops its import, so no later frame
/// reads the retired slot's memory.
#[test]
fn a_reply_listing_a_retired_pool_slot_drops_its_import() {
    let socket_directory = tempfile::tempdir().expect("a socket directory");
    let socket_path = socket_directory.path().join("retires.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        read_one_request(&mut stream);
        answer(&mut stream, &serde_json::json!({ "ok": true }));
        read_one_request(&mut stream);
        answer(
            &mut stream,
            &serde_json::json!({
                streamlib_surface_client::SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD: ["slot-a"]
            }),
        );
        stream
    });
    let connection =
        SurfaceShareConnectionOfThisImporter::<PoolSlotImportUnderTest>::new(socket_path);
    let (_, _, reservation) = connection
        .exchange(&serde_json::json!({ "op": "check_out" }), &[], |_| {
            connection
                .pool_slot_imports_by_pool_slot()
                .reserve_for_a_fresh_import("slot-a")
        })
        .expect("the first round trip");
    reservation.set(Arc::new(1)).expect("a fresh reservation");
    assert!(
        connection
            .pool_slot_imports_by_pool_slot()
            .import_held_for("slot-a")
            .is_some()
    );

    connection
        .request(&serde_json::json!({ "op": "check_out" }))
        .expect("the second round trip");

    assert!(
        connection
            .pool_slot_imports_by_pool_slot()
            .import_held_for("slot-a")
            .is_none()
    );
    drop(server.join().expect("server thread"));
}

/// Waits out the whole response timeout against a server that never
/// answers.
///
/// A timed-out connection stays open: the service releases every claim a
/// connection holds once it closes, and frames the importer still reads were
/// claimed on it.
#[test]
fn a_connection_whose_answer_outwaits_the_timeout_is_set_aside_rather_than_closed() {
    let socket_directory = tempfile::tempdir().expect("a socket directory");
    let socket_path = socket_directory.path().join("never-answers.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    let (the_client_gave_up, the_server_hears_the_client_gave_up) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        read_one_request(&mut stream);
        the_server_hears_the_client_gave_up
            .recv()
            .expect("the client gives up");
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .expect("set the probe's timeout");
        let mut probe = [0u8; 1];
        match stream.read(&mut probe) {
            Ok(0) => false,
            Err(error) => error.kind() == std::io::ErrorKind::WouldBlock,
            Ok(_) => true,
        }
    });
    let connection =
        SurfaceShareConnectionOfThisImporter::<PoolSlotImportUnderTest>::new(socket_path);

    let refusal = connection
        .request(&serde_json::json!({ "op": "release_check_out" }))
        .expect_err("nothing answers");
    the_client_gave_up.send(()).expect("the server is waiting");

    assert!(
        refusal.contains("did not answer within"),
        "the refusal names the timeout: {refusal}"
    );
    assert!(
        server.join().expect("server thread"),
        "the importer closed the connection its claims were held on"
    );
    assert_eq!(
        connection
            .connections_set_aside_after_a_failure
            .lock()
            .len(),
        1
    );
}

/// Past its limit of connections set aside, an importer asks the service
/// nothing more, so a service that never answers holds a bounded number of
/// descriptors, service threads and claims.
#[test]
fn an_importer_that_set_aside_its_limit_of_connections_asks_the_service_nothing_more() {
    let connection = SurfaceShareConnectionOfThisImporter::<PoolSlotImportUnderTest>::new(
        PathBuf::from("/nonexistent/streamlib-surface.sock"),
    );
    for _ in 0..SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST {
        let (set_aside_end, _peer_end) = UnixStream::pair().expect("a socket pair");
        connection
            .connections_set_aside_after_a_failure
            .lock()
            .push(set_aside_end);
    }

    let refusal = connection
        .request(&serde_json::json!({ "op": "release_check_out" }))
        .expect_err("the importer has given up on the service");

    assert!(
        refusal.contains("asks it nothing more"),
        "the refusal names the limit rather than trying to connect: {refusal}"
    );
    assert_eq!(
        connection
            .connections_set_aside_after_a_failure
            .lock()
            .len(),
        SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST
    );
}

/// An answer this importer cannot take in leaves the connection open on the
/// service's side, so it is set aside rather than closed: the claims made on
/// it stay held.
#[test]
fn a_connection_whose_answer_cannot_be_taken_in_is_set_aside_rather_than_closed() {
    let socket_directory = tempfile::tempdir().expect("a socket directory");
    let socket_path = socket_directory.path().join("garbles.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        read_one_request(&mut stream);
        let not_json = b"not json";
        stream
            .write_all(&(not_json.len() as u32).to_be_bytes())
            .expect("write the length");
        stream.write_all(not_json).expect("write the answer");
        stream
    });
    let connection =
        SurfaceShareConnectionOfThisImporter::<PoolSlotImportUnderTest>::new(socket_path);

    let refusal = connection
        .request(&serde_json::json!({ "op": "check_out" }))
        .expect_err("an answer that is not JSON fails the request");

    assert!(refusal.contains("set that connection aside"), "{refusal}");
    assert_eq!(
        connection
            .connections_set_aside_after_a_failure
            .lock()
            .len(),
        1
    );
    assert!(connection.open_connection.lock().is_none());
    drop(server.join().expect("server thread"));
}

/// A connection the service closed holds nothing on the service's side, so
/// it is dropped rather than set aside.
#[test]
fn a_connection_the_service_closed_is_dropped_rather_than_set_aside() {
    let socket_directory = tempfile::tempdir().expect("a socket directory");
    let socket_path = socket_directory.path().join("closes.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        read_one_request(&mut stream);
    });
    let connection =
        SurfaceShareConnectionOfThisImporter::<PoolSlotImportUnderTest>::new(socket_path);

    let refusal = connection
        .request(&serde_json::json!({ "op": "check_out" }))
        .expect_err("a closed connection fails the request");

    assert!(refusal.contains("closed the connection"), "{refusal}");
    assert!(
        connection
            .connections_set_aside_after_a_failure
            .lock()
            .is_empty()
    );
    server.join().expect("server thread");
}

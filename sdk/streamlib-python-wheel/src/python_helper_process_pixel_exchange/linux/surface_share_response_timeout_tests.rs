use std::io::Read as _;
use std::os::unix::net::UnixListener;

use super::*;
use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, SURFACE_SHARE_RESPONSE_TIMEOUT,
};
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

fn exchange_client_reaching(socket_path: PathBuf) -> HelperProcessGpuExchangeClient {
    Python::initialize();
    Python::attach(|python| {
        HelperProcessGpuExchangeClient::new(
            python.None(),
            python.None(),
            socket_path,
            "helper:response-timeout-under-test".to_string(),
        )
    })
}

/// Fail-without-fix: the connection had no read timeout, so a service that
/// stopped answering held the calling thread — and every thread waiting on
/// this client's connection lock behind it — for good.
#[test]
fn a_surface_share_connection_is_opened_with_the_response_timeout() {
    let share = SurfaceShareUnderTest::start("timeout");
    let surface_id = share.publish_one_surface();
    let exchange_client = exchange_client_reaching(share.socket_path.clone());

    exchange_client
        .check_out_surface(&surface_id)
        .expect("the checkout round trip");

    let read_timeout = exchange_client
        .surface_share_connection
        .lock()
        .as_ref()
        .expect("a completed exchange keeps its connection")
        .read_timeout()
        .expect("the connection's read timeout is readable");
    assert_eq!(read_timeout, Some(SURFACE_SHARE_RESPONSE_TIMEOUT));
}

/// Waits out the whole response timeout against a server that never
/// answers.
///
/// Fail-without-fix: the timed-out connection was closed, and the service
/// releases every claim a connection holds once it closes — so frames the
/// helper was still reading lost their claims under it.
#[test]
fn a_connection_whose_answer_outwaits_the_timeout_is_set_aside_rather_than_closed() {
    // Only for its short socket directory, which it removes on drop.
    let share = SurfaceShareUnderTest::start("never-answers");
    let socket_path = share.socket_path.with_file_name("never-answers.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    let (the_client_gave_up, the_server_hears_the_client_gave_up) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut length_prefix = [0u8; 4];
        stream
            .read_exact(&mut length_prefix)
            .expect("read the length");
        let mut request = vec![0u8; u32::from_be_bytes(length_prefix) as usize];
        stream.read_exact(&mut request).expect("read the request");
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

    let exchange_client = exchange_client_reaching(socket_path);
    let refusal = exchange_client
        .release_check_out("surface-under-test#1")
        .expect_err("nothing answers");
    the_client_gave_up.send(()).expect("the server is waiting");

    assert!(
        refusal.to_string().contains("did not answer within"),
        "the refusal names the timeout: {refusal}"
    );
    assert!(
        server.join().expect("server thread"),
        "the helper closed the connection its claims were held on"
    );
    assert_eq!(
        exchange_client
            .surface_share_connections_set_aside_after_a_timeout
            .lock()
            .len(),
        1
    );
}

/// Fail-without-fix: every timeout set one more connection aside, so a
/// service that never answered held a descriptor, a service thread and
/// their claims per request until the helper stopped.
#[test]
fn a_helper_that_set_aside_its_limit_of_connections_asks_the_service_nothing_more() {
    let exchange_client =
        exchange_client_reaching(PathBuf::from("/nonexistent/streamlib-surface.sock"));
    for _ in 0..SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST {
        let (set_aside_end, _peer_end) = UnixStream::pair().expect("a socket pair");
        exchange_client
            .surface_share_connections_set_aside_after_a_timeout
            .lock()
            .push(set_aside_end);
    }

    let refusal = exchange_client
        .release_check_out("surface-under-test#1")
        .expect_err("the helper has given up on the service");

    assert!(
        refusal.to_string().contains("asks it nothing more"),
        "the refusal names the limit rather than trying to connect: {refusal}"
    );
    assert_eq!(
        exchange_client
            .surface_share_connections_set_aside_after_a_timeout
            .lock()
            .len(),
        SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST
    );
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A track declared on one gateway and written by a send reaches a receive
//! asking another gateway for it, through a relay running in this process.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use _native::moq_gateway::MoqGatewayThread;
use _native::moq_gateway_relay_settings::MoqGatewayRelaySettings;
use _native::moq_node_ipc::{MoqNodeIpc, MoqStreamServiceNames};
use _native::moq_track_receive::{
    MoqReceivedTrackOutput, MoqTrackReceiveAsked, MoqTrackReceiveThread,
};
use _native::moq_track_send::{MoqTrackDeclarationThread, MoqTrackDeclared, MoqTrackSendPublisher};

#[derive(Debug, Clone, PartialEq)]
struct ObjectWritten {
    payload: Vec<u8>,
    timestamp_ns: i64,
    starts_group: bool,
}

#[derive(Default)]
struct WhatTheReceiveWrote {
    objects: Vec<ObjectWritten>,
    catalog_entries: Vec<serde_json::Value>,
    given_up_groups: u64,
}

struct RecordedReceiveOutput(Arc<Mutex<WhatTheReceiveWrote>>);

impl MoqReceivedTrackOutput for RecordedReceiveOutput {
    fn write_object(&mut self, payload: &[u8], timestamp_ns: i64, starts_group: bool) {
        self.0.lock().unwrap().objects.push(ObjectWritten {
            payload: payload.to_vec(),
            timestamp_ns,
            starts_group,
        });
    }

    fn declare_groups_given_up(&mut self, given_up_group_count: u64) {
        self.0.lock().unwrap().given_up_groups += given_up_group_count;
    }

    fn set_catalog_entry(&mut self, section_and_entry: &serde_json::Value) {
        self.0
            .lock()
            .unwrap()
            .catalog_entries
            .push(section_and_entry.clone());
    }

    fn stream_time_now_ns(&self) -> i64 {
        10_000_000_000
    }
}

/// A public relay on ephemeral IPv4 loopback ports: `moq_relay::test_relay`
/// with its own dial socket on IPv4 too, for a host with no IPv6.
async fn a_relay_on_loopback() -> anyhow::Result<(moq_relay::Relay, url::Url, String)> {
    let mut config = moq_relay::Config::default();
    config.drain_timeout = Duration::ZERO;
    config.listen.bind = Some("127.0.0.1:0".parse()?);
    config.listen.tls.generate = vec!["localhost".into()];
    config.connect.bind = Some("127.0.0.1:0".parse()?);
    config.web.http.listen = Some("127.0.0.1:0".parse()?);
    config.auth.public = vec![moq_auth::Pattern::all()];
    let relay = moq_relay::Relay::load(config).await?;
    let quic = relay.quic_addr()?;
    let http = relay
        .web_addrs()
        .http
        .ok_or_else(|| anyhow::anyhow!("the relay serves no HTTP"))?;
    Ok((relay, format!("https://{quic}/").parse()?, http.to_string()))
}

/// The fingerprint the relay at `http` serves at `/certificate.sha256`.
fn the_certificate_fingerprint_served_at(http: &str) -> String {
    use std::io::{Read, Write};
    let mut connection = std::net::TcpStream::connect(http).unwrap();
    write!(
        connection,
        "GET /certificate.sha256 HTTP/1.0\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    connection.read_to_string(&mut response).unwrap();
    response
        .split("\r\n\r\n")
        .nth(1)
        .expect("the response has a body")
        .trim()
        .to_string()
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_track_written_by_a_send_reaches_a_receive_through_the_relay_in_group_order() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let relay_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (relay, relay_url, relay_http) = relay_runtime
        .block_on(a_relay_on_loopback())
        .expect("a relay binds on loopback");
    relay_runtime.spawn(async move {
        let _ = relay.run().await;
    });
    let fingerprint = the_certificate_fingerprint_served_at(&relay_http);

    let domain_root = tempfile::Builder::new()
        .prefix("moq")
        .tempdir_in("/tmp")
        .unwrap();
    let ipc = MoqNodeIpc::open_under_the_domain_root(
        domain_root.path(),
        MoqStreamServiceNames::of_the_stream("run", "studio"),
    )
    .expect("the iceoryx2 node opens");

    let a_gateway_publishing = |gateway_node: &str, broadcast: &str| {
        let mut settings_handed_over = Some(
            MoqGatewayRelaySettings::read_from(
                serde_json::json!({
                    "kind": "moq-relay",
                    "relay_url": relay_url.as_str(),
                    "certificate_fingerprint": fingerprint,
                    "broadcast": broadcast,
                })
                .as_object()
                .unwrap(),
            )
            .expect("the settings read"),
        );
        MoqGatewayThread::spawn(
            ipc.clone(),
            gateway_node.into(),
            Box::new(move || settings_handed_over.take()),
        )
        .expect("the gateway starts")
    };
    let mut gateway = a_gateway_publishing("gateway", "studio");
    let mut receiving_gateway = a_gateway_publishing("receiving-gateway", "audience");
    let catalog_entry = serde_json::json!({"section": "json", "entry": {}});
    let mut declaration = MoqTrackDeclarationThread::spawn(
        ipc.clone(),
        MoqTrackDeclared {
            track_node: "telemetry".into(),
            gateway_node: "gateway".into(),
            track: "telemetry".into(),
            catalog_entry: Some(catalog_entry.clone()),
        },
    )
    .expect("the track is declared");
    let mut send = MoqTrackSendPublisher::open(ipc.clone(), "telemetry").expect("the send opens");

    let received = Arc::new(Mutex::new(WhatTheReceiveWrote::default()));
    let mut receive = MoqTrackReceiveThread::spawn(
        ipc.clone(),
        MoqTrackReceiveAsked {
            receive_node: "receive".into(),
            gateway_node: "receiving-gateway".into(),
            broadcast: "studio".into(),
            track: "telemetry".into(),
            max_age: Duration::from_secs(2),
        },
        Box::new(RecordedReceiveOutput(Arc::clone(&received))),
    )
    .expect("the receive starts");

    let mut written_group_starts = 0u64;
    wait_until(
        "the first objects to arrive",
        Duration::from_secs(20),
        || {
            send.send_message(
                format!("record {written_group_starts}").as_bytes(),
                1_000_000_000 + i64::try_from(written_group_starts).unwrap() * 33_000_000,
                true,
                0,
            )
            .unwrap();
            written_group_starts += 1;
            received.lock().unwrap().objects.len() >= 3
        },
    );

    let received = received.lock().unwrap();
    assert_eq!(received.catalog_entries.first(), Some(&catalog_entry));
    let indices: Vec<u64> = received
        .objects
        .iter()
        .map(|object| {
            assert!(object.starts_group);
            String::from_utf8(object.payload.clone())
                .unwrap()
                .trim_start_matches("record ")
                .parse()
                .unwrap()
        })
        .collect();
    assert!(
        indices.windows(2).all(|pair| pair[0] < pair[1]),
        "the objects arrive in group order: {indices:?}"
    );
    let spacing = received.objects[1].timestamp_ns - received.objects[0].timestamp_ns;
    assert_eq!(
        spacing,
        i64::try_from(indices[1] - indices[0]).unwrap() * 33_000_000,
        "the track's spacing is kept"
    );
    drop(received);

    assert!(receive.stop_and_join(Duration::from_secs(2)));
    declaration.stop_and_join();
    assert!(gateway.stop_and_join(Duration::from_secs(5)));
    assert!(receiving_gateway.stop_and_join(Duration::from_secs(5)));
}

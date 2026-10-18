// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local relay's instances against the real `moq-relay` the runtime unit
//! carries: each child serves its certificate's fingerprint and admits a MoQ
//! client; only the runtime's token publishes and anyone subscribes; new
//! certificate files are served with no restart; leaving frees the port.
//!
//! The relay is read from `cargo xtask stage-local-relay`'s install root, or
//! from `TATOLAB_TEST_LOCAL_RELAY_EXECUTABLE`.

mod common;

use std::time::Duration;

use common::{
    HTTP_ANSWER_BUDGET, JoinedLocalRelayUnderTest, a_moq_client, http_get_body, http_get_body_at,
    the_port_is_free, this_hosts_address_off_loopback, wait_until,
    wait_until_the_served_fingerprint_is,
};
use streamlib_relay_adapter_local::{LocalRelayCertificateAndKeyFile, LocalRelayListener};
use streamlib_relay_adapter_sdk::RelayAdapter;
use streamlib_relay_adapter_sdk::secrecy::ExposeSecret;
use streamlib_relay_adapter_sdk::url::Url;

/// How long a MoQ session and an announcement have to arrive.
const MOQ_BUDGET: Duration = Duration::from_secs(15);

/// How long an announcement that must never arrive is waited for, once one
/// that must has.
const NEVER_ANNOUNCED_GRACE: Duration = Duration::from_secs(2);

#[test]
fn each_listener_serves_its_certificates_fingerprint_and_admits_a_moq_client() {
    for listener in LocalRelayListener::BOTH {
        let relay = JoinedLocalRelayUnderTest::join(listener);
        let fingerprint = relay
            .adapter
            .served_certificate_sha256_fingerprint_hex()
            .expect("a joined instance serves a certificate");
        wait_until_the_served_fingerprint_is(relay.port, &fingerprint);

        let dial_url = relay.joined.dial_url.clone();
        relay.async_runtime.block_on(async {
            let client = a_moq_client();
            let _session =
                tokio::time::timeout(MOQ_BUDGET, client.connect(dial_url.clone()).established())
                    .await
                    .unwrap_or_else(|_| panic!("{listener:?}: no session within {MOQ_BUDGET:?}"))
                    .unwrap_or_else(|refusal| panic!("{listener:?}: {refusal}"));
        });
    }
}

#[test]
fn only_the_runtimes_token_publishes_and_an_anonymous_session_subscribes() {
    for listener in LocalRelayListener::BOTH {
        let relay = JoinedLocalRelayUnderTest::join(listener);
        wait_until_the_served_fingerprint_is(
            relay.port,
            &relay
                .adapter
                .served_certificate_sha256_fingerprint_hex()
                .unwrap(),
        );
        let dial_url = relay.joined.dial_url.clone();
        let dial_url_with_the_token: Url = relay
            .adapter
            .credential_for_dial()
            .expect("the local relay is dialed with the runtime's token")
            .dial_url_carrying_the_credential(&dial_url)
            .expose_secret()
            .parse()
            .unwrap();
        let port = relay.port;

        relay.async_runtime.block_on(async {
            let published_with_the_token = moq_tokio::origin::spawn();
            let broadcast_with_the_token = published_with_the_token
                .create_broadcast("desk/published-with-the-token.hang")
                .unwrap();
            let _track_with_the_token = broadcast_with_the_token
                .create_track("camera/video", None)
                .unwrap();
            broadcast_with_the_token
                .announce(Default::default())
                .unwrap();
            let _publisher_with_the_token = a_moq_client()
                .with_publisher(&published_with_the_token)
                .connect(dial_url_with_the_token.clone());

            let published_anonymously = moq_tokio::origin::spawn();
            let broadcast_anonymously = published_anonymously
                .create_broadcast("desk/published-anonymously.hang")
                .unwrap();
            let _track_anonymously = broadcast_anonymously
                .create_track("camera/video", None)
                .unwrap();
            broadcast_anonymously.announce(Default::default()).unwrap();
            let _anonymous_publisher = a_moq_client()
                .with_publisher(&published_anonymously)
                .connect(dial_url.clone());

            let subscribed_anonymously = moq_tokio::origin::spawn();
            let mut announced_to_the_anonymous_subscriber =
                subscribed_anonymously.consume().announced();
            let _anonymous_subscriber = a_moq_client()
                .with_subscriber(subscribed_anonymously.clone())
                .connect(dial_url.clone());

            let first_announced = tokio::time::timeout(MOQ_BUDGET, async {
                loop {
                    let update = announced_to_the_anonymous_subscriber
                        .next()
                        .await
                        .expect("the anonymous subscriber's announcements stay open");
                    if update.kind.is_active() {
                        return update.prefix.to_string();
                    }
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("{listener:?}: the anonymous subscriber saw nothing within {MOQ_BUDGET:?}")
            });
            assert_eq!(
                first_announced, "desk/published-with-the-token.hang",
                "{listener:?}: the token's broadcast reaches an anonymous subscriber"
            );

            tokio::time::sleep(NEVER_ANNOUNCED_GRACE).await;
            while let Some(update) = announced_to_the_anonymous_subscriber.try_next() {
                assert_ne!(
                    update.prefix.to_string(),
                    "desk/published-anonymously.hang",
                    "{listener:?}: an anonymous publish reached a subscriber"
                );
            }

            let announced_over_http =
                tokio::task::spawn_blocking(move || http_get_body(port, "/announced"))
                    .await
                    .unwrap()
                    .expect("the relay answers /announced");
            assert_eq!(
                announced_over_http, "desk/published-with-the-token.hang",
                "{listener:?}: an anonymous HTTP read lists the token's broadcast alone"
            );
        });
    }
}

#[test]
fn new_certificate_files_are_served_with_no_restart() {
    let relay = JoinedLocalRelayUnderTest::join(LocalRelayListener::Loopback);
    wait_until_the_served_fingerprint_is(
        relay.port,
        &relay
            .adapter
            .served_certificate_sha256_fingerprint_hex()
            .unwrap(),
    );
    let running_child = relay.adapter.running_child_process_id().unwrap();

    let replaced =
        LocalRelayCertificateAndKeyFile::in_the_directory(&relay.adapter.instance_directory())
            .write_a_fresh_certificate_issued_now(&["localhost".to_string()])
            .unwrap();
    wait_until_the_served_fingerprint_is(relay.port, &replaced.sha256_fingerprint_hex);
    assert_eq!(
        relay.adapter.running_child_process_id(),
        Some(running_child),
        "the child serves the new certificate without restarting"
    );
}

#[test]
fn leaving_ends_the_child_and_frees_its_port() {
    for listener in LocalRelayListener::BOTH {
        let mut relay = JoinedLocalRelayUnderTest::join(listener);
        wait_until_the_served_fingerprint_is(
            relay.port,
            &relay
                .adapter
                .served_certificate_sha256_fingerprint_hex()
                .unwrap(),
        );
        let instance_directory = relay.adapter.instance_directory();

        relay.adapter.leave();
        assert_eq!(relay.adapter.running_child_process_id(), None);
        assert!(
            wait_until(HTTP_ANSWER_BUDGET, || the_port_is_free(
                listener, relay.port
            )),
            "{listener:?}: port {} is still bound after leaving",
            relay.port
        );
        assert!(!instance_directory.join("moq-relay.pid").exists());
        assert!(!instance_directory.join("admission.sock").exists());
    }
}

#[test]
fn off_loopback_the_every_interface_child_answers_and_the_loopback_child_is_unreachable() {
    let loopback = JoinedLocalRelayUnderTest::join(LocalRelayListener::Loopback);
    let every_interface = JoinedLocalRelayUnderTest::join(LocalRelayListener::EveryInterface);
    wait_until_the_served_fingerprint_is(
        loopback.port,
        &loopback
            .adapter
            .served_certificate_sha256_fingerprint_hex()
            .unwrap(),
    );
    let every_interface_fingerprint = every_interface
        .adapter
        .served_certificate_sha256_fingerprint_hex()
        .unwrap();
    wait_until_the_served_fingerprint_is(every_interface.port, &every_interface_fingerprint);
    let off_loopback = this_hosts_address_off_loopback();

    assert_eq!(
        http_get_body_at(
            std::net::SocketAddr::new(off_loopback, every_interface.port),
            "/certificate.sha256"
        )
        .as_deref(),
        Some(every_interface_fingerprint.as_str()),
        "the every-interface child answers at {off_loopback}"
    );
    assert!(
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::new(off_loopback, loopback.port),
            Duration::from_secs(2)
        )
        .is_err(),
        "the loopback child is reachable at {off_loopback}"
    );
}

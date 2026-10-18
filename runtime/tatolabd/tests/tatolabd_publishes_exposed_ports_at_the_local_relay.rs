// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd`'s publisher against the real local relay: a MoQ client reads a
//! public port's track at both listeners and a private port's at loopback
//! alone, each object the bytes the node wrote with groups where it opened
//! them; the broadcast's catalog is hang's and lists the port carrying an
//! entry; an internal port is at no relay; lowering a port's level ends its
//! subscribers; and a `RelayTrackSource` in another stream reads a private
//! port through loopback, its groups and spacing kept, and feeds a decoder
//! another stream's private H.264 port. A stream's start
//! creates the GPU context: `cargo test -p tatolabd --features
//! hardware-tests,machine-directories-under-a-test-root`, after
//! `cargo xtask stage-local-relay`.

#![cfg(all(
    feature = "hardware-tests",
    feature = "machine-directories-under-a-test-root"
))]

mod common;
#[path = "../../streamlib-engine/tests/support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;

use std::time::Duration;

use common::{
    LocalRelayPortsOfATest, SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit,
    a_kept_stream_record, an_executable_standing_in_for_the_interpreter,
    call_a_tool_over_the_local_api, the_local_apis_answer_to_get,
};
use streamlib_relay_adapter_local::LocalRelayListener;
use streamlib_relay_adapter_sdk::url::Url;

const STREAM: &str = "desk";
/// A stream reading `STREAM`'s ports through the local relay.
const READER_STREAM: &str = "reader";
const WRITER_NODE_TYPE: &str =
    "tatolabd::numbered_message_writer_test_node::NumberedMessageWriterTestNode";
/// Exposed public, and carrying a catalog entry.
const PUBLIC_PORT: &str = "numbers";
/// Exposed private, and carrying none.
const PRIVATE_PORT: &str = "more_numbers";
const MESSAGES_PER_GROUP: u64 = 3;
const NANOSECONDS_BETWEEN_MESSAGE_STAMPS: u128 = 10_000_000;

const THE_STREAM_STARTS_WITHIN: Duration = Duration::from_secs(60);
/// How long a session, a broadcast, a track or an object has to arrive.
const MOQ_BUDGET: Duration = Duration::from_secs(20);
/// How long a track that must never be served is asked for.
const NEVER_SERVED_GRACE: Duration = Duration::from_secs(3);

fn a_stream_writing_numbered_messages_on_a_public_and_a_private_port() -> serde_json::Value {
    serde_json::json!({
        "stream": STREAM,
        "nodes": [{"name": "writer", "type": WRITER_NODE_TYPE, "config": {}}],
        "exposed": [
            {"node": "writer", "port": PUBLIC_PORT, "level": "public"},
            {"node": "writer", "port": PRIVATE_PORT, "level": "private"},
        ],
    })
}

/// A `tatolabd` with both local relay listeners, running the stream.
struct TatolabdPublishingTheStream {
    runtime: SpawnedTatolabd,
    machine_root: TatolabTestMachineRoot,
    ports: LocalRelayPortsOfATest,
    machine: String,
    async_runtime: tokio::runtime::Runtime,
    _runtime_unit: TemporaryRuntimeUnit,
}

impl TatolabdPublishingTheStream {
    fn start() -> Self {
        Self::start_with_the_streams(&[(
            STREAM,
            a_stream_writing_numbered_messages_on_a_public_and_a_private_port(),
        )])
    }

    /// The stream, and beside it [`READER_STREAM`] holding one writer it
    /// exposes nothing of, since a stream holding no node does not load.
    fn start_beside_a_reader_stream() -> Self {
        Self::start_with_the_streams(&[
            (
                STREAM,
                a_stream_writing_numbered_messages_on_a_public_and_a_private_port(),
            ),
            (
                READER_STREAM,
                serde_json::json!({
                    "stream": READER_STREAM,
                    "nodes": [{"name": "unexposed", "type": WRITER_NODE_TYPE, "config": {}}],
                }),
            ),
        ])
    }

    fn start_with_the_streams(streams: &[(&str, serde_json::Value)]) -> Self {
        let runtime_unit = TemporaryRuntimeUnit::with_its_lend_and_the_local_relay();
        let machine_root = TatolabTestMachineRoot::new();
        let ports = LocalRelayPortsOfATest::free_now();
        for (stream_name, graph) in streams {
            machine_root
                .kept_stream_records()
                .write(&a_kept_stream_record(
                    stream_name,
                    graph.clone(),
                    &machine_root.project_directory(),
                    &an_executable_standing_in_for_the_interpreter(),
                    false,
                ))
                .unwrap();
        }
        let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
        ports.given_to(&mut tatolabd_command);
        let mut runtime = SpawnedTatolabd::spawn(tatolabd_command);
        for (stream_name, _) in streams {
            runtime.wait_until_standard_error_carries(
                &format!("[start] The stream `{stream_name}` started"),
                THE_STREAM_STARTS_WITHIN,
            );
        }
        let graph =
            the_local_apis_answer_to_get(&machine_root.local_api_socket_path(), "/api/graph");
        let machine = graph["machine"]
            .as_str()
            .unwrap_or_else(|| panic!("graph names the machine: {graph}"))
            .to_string();
        Self {
            runtime,
            machine_root,
            ports,
            machine,
            async_runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            _runtime_unit: runtime_unit,
        }
    }

    fn dial_url_of(&self, listener: LocalRelayListener) -> Url {
        let port = match listener {
            LocalRelayListener::Loopback => self.ports.loopback,
            LocalRelayListener::EveryInterface => self.ports.every_interface,
        };
        format!("http://{}/", listener.address_dialed_on_this_machine(port))
            .parse()
            .unwrap()
    }

    /// The stream's broadcast as an anonymous client of `listener` reads it.
    fn the_broadcast_at(&self, listener: LocalRelayListener) -> BroadcastReadAtAListener {
        self.the_broadcast_of_the_stream_at(STREAM, listener)
    }

    /// `stream_name`'s broadcast as an anonymous client of `listener` reads it.
    fn the_broadcast_of_the_stream_at(
        &self,
        stream_name: &str,
        listener: LocalRelayListener,
    ) -> BroadcastReadAtAListener {
        let dial_url = self.dial_url_of(listener);
        let broadcast_path = format!("{}/{stream_name}.hang", self.machine);
        self.async_runtime.block_on(async move {
            let subscribed = moq_tokio::origin::spawn();
            let session = moq_tokio::connect::Config::default()
                .init(moq_tokio::quic::Config::default())
                .expect("a MoQ client builds")
                .with_subscriber(subscribed.clone())
                .connect(dial_url);
            let broadcast = tokio::time::timeout(
                MOQ_BUDGET,
                subscribed
                    .consume()
                    .routed_broadcast(broadcast_path.as_str()),
            )
            .await
            .unwrap_or_else(|_| {
                panic!("{listener:?}: `{broadcast_path}` is not announced within {MOQ_BUDGET:?}")
            })
            .unwrap_or_else(|refusal| panic!("{listener:?}: {refusal}"));
            BroadcastReadAtAListener {
                listener,
                broadcast,
                _session: session,
                _subscribed: subscribed,
            }
        })
    }

    fn put_the_port_at(&self, port: &str, level: &str) {
        self.put_the_port_of_the_node_at("writer", port, level);
    }

    fn put_the_port_of_the_node_at(&self, node: &str, port: &str, level: &str) {
        self.call(
            "expose_port",
            serde_json::json!({"stream": STREAM, "node": node, "port": port, "level": level}),
        );
    }

    /// Add a numbered-message writer named `node` to the running stream.
    fn add_a_writer_named(&self, node: &str) {
        self.call(
            "add_node",
            serde_json::json!({
                "stream": STREAM,
                "type": WRITER_NODE_TYPE,
                "name": node,
            }),
        );
    }

    /// Add to [`READER_STREAM`] a `RelayTrackSource` named `node` reading
    /// `port` of the writer in [`STREAM`] through the loopback listener, and
    /// expose its output private.
    fn add_a_relay_track_source_reading(&self, node: &str, port: &str) {
        self.add_a_relay_track_source_reading_the_port_of(node, STREAM, "writer", port);
        self.expose_in_the_reader_stream(node, "track");
    }

    /// Add to [`READER_STREAM`] a `RelayTrackSource` named `node` reading
    /// `source_stream`'s `source_node`/`source_port` through the loopback
    /// listener.
    fn add_a_relay_track_source_reading_the_port_of(
        &self,
        node: &str,
        source_stream: &str,
        source_node: &str,
        source_port: &str,
    ) {
        self.add_a_relay_track_source_at(
            &self.dial_url_of(LocalRelayListener::Loopback),
            node,
            &format!(
                "{}/{source_stream}/{source_node}/{source_port}",
                self.machine
            ),
        );
    }

    /// Add to [`READER_STREAM`] a `RelayTrackSource` named `node` reading
    /// `track_address` at `relay_url`.
    fn add_a_relay_track_source_at(&self, relay_url: &Url, node: &str, track_address: &str) {
        self.call(
            "add_node",
            serde_json::json!({
                "stream": READER_STREAM,
                "type": "tatolab.stream:RelayTrackSource",
                "name": node,
                "config": {"relay_url": relay_url.as_str(), "track_address": track_address},
            }),
        );
    }

    fn expose_in_the_reader_stream(&self, node: &str, port: &str) {
        self.call(
            "expose_port",
            serde_json::json!({
                "stream": READER_STREAM, "node": node, "port": port, "level": "private",
            }),
        );
    }

    /// The bags `node` of `stream_name` has written on `port`, read off `graph`.
    fn bags_written_by(&self, stream_name: &str, node: &str, port: &str) -> u64 {
        let graph = the_local_apis_answer_to_get(
            &self.machine_root.local_api_socket_path(),
            &format!("/api/graph?stream={stream_name}"),
        );
        graph["nodes"]
            .as_array()
            .and_then(|nodes| nodes.iter().find(|graph_node| graph_node["name"] == node))
            .and_then(|graph_node| {
                graph_node["components"]["metrics"]["written_bags_by_output_port"][port].as_u64()
            })
            .unwrap_or(0)
    }

    fn remove_the_node_named(&self, node: &str) {
        self.call(
            "remove_node",
            serde_json::json!({"stream": STREAM, "name": node}),
        );
    }

    fn call(&self, tool_name: &str, tool_arguments: serde_json::Value) {
        let answer = call_a_tool_over_the_local_api(
            &self.machine_root.local_api_socket_path(),
            tool_name,
            tool_arguments,
        );
        assert!(!answer.is_error, "`{tool_name}`: {}", answer.text);
    }
}

struct BroadcastReadAtAListener {
    listener: LocalRelayListener,
    broadcast: moq_net::broadcast::Consumer,
    _session: moq_tokio::Connection,
    _subscribed: moq_net::origin::Producer,
}

/// One object as a subscriber read it, with the group it arrived in.
#[derive(Debug)]
struct ObjectRead {
    group_sequence: u64,
    payload: Vec<u8>,
    timestamp_ns: u128,
}

impl BroadcastReadAtAListener {
    /// The first group of `track_name` once it is served, asking again until
    /// [`MOQ_BUDGET`] runs out; `None` when it never is.
    async fn the_first_group_of_once_it_is_served(
        &self,
        track_name: &str,
    ) -> Option<moq_net::group::Consumer> {
        let deadline = tokio::time::Instant::now() + MOQ_BUDGET;
        while tokio::time::Instant::now() < deadline {
            if let Ok(mut ordered) = self.subscribe_to(track_name).await
                && let Ok(Ok(Some(group))) =
                    tokio::time::timeout(Duration::from_secs(2), ordered.next_group()).await
            {
                return Some(group);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// `track_name` subscribed once it carries a group, asking again until
    /// [`MOQ_BUDGET`] runs out.
    async fn the_track_once_it_is_served(&self, track_name: &str) -> moq_net::track::Ordered {
        let deadline = tokio::time::Instant::now() + MOQ_BUDGET;
        while tokio::time::Instant::now() < deadline {
            if let Ok(mut ordered) = self.subscribe_to(track_name).await
                && let Ok(Ok(Some(_))) =
                    tokio::time::timeout(Duration::from_secs(2), ordered.next_group()).await
            {
                return ordered;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "{:?}: `{track_name}` is not served within {MOQ_BUDGET:?}",
            self.listener
        )
    }

    async fn subscribe_to(
        &self,
        track_name: &str,
    ) -> Result<moq_net::track::Ordered, moq_net::Error> {
        let track = self.broadcast.track(track_name)?;
        Ok(track.subscribe(None).await?.ordered())
    }

    /// The next `count` objects of `track_name`, each with its group.
    async fn the_next_objects_of(&self, track_name: &str, count: usize) -> Vec<ObjectRead> {
        let mut ordered = self
            .subscribe_to(track_name)
            .await
            .unwrap_or_else(|refusal| panic!("{:?}: `{track_name}`: {refusal}", self.listener));
        let mut objects = Vec::new();
        while objects.len() < count {
            let mut group = tokio::time::timeout(MOQ_BUDGET, ordered.next_group())
                .await
                .expect("a group arrives")
                .expect("the track is live")
                .expect("the track has not ended");
            let group_sequence = group.sequence;
            while let Some(frame) = tokio::time::timeout(MOQ_BUDGET, group.read_frame())
                .await
                .expect("an object arrives")
                .expect("the group is live")
            {
                objects.push(ObjectRead {
                    group_sequence,
                    payload: frame.payload.to_vec(),
                    timestamp_ns: frame.timestamp.as_nanos(),
                });
                if objects.len() == count {
                    break;
                }
            }
        }
        objects
    }
}

/// The message number a payload carries, `message <n>`.
fn the_number_in(object: &ObjectRead) -> u64 {
    std::str::from_utf8(&object.payload)
        .ok()
        .and_then(|text| text.strip_prefix("message "))
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("an object the writer did not write: {object:?}"))
}

/// Every object is the bytes the node wrote, stamped from its envelope, in
/// the groups the node opened.
fn assert_the_objects_are_the_messages_as_written(objects: &[ObjectRead]) {
    for object in objects {
        assert_eq!(
            object.timestamp_ns,
            u128::from(the_number_in(object)) * NANOSECONDS_BETWEEN_MESSAGE_STAMPS
        );
    }
    assert_the_objects_are_grouped_as_the_node_wrote_them(objects);
}

/// Each group starts at a multiple of three and holds the messages after it
/// until the next. A reader that falls behind may be handed a later group in
/// place of an earlier one, as live delivery does, but never part of a group.
fn assert_the_objects_are_grouped_as_the_node_wrote_them(objects: &[ObjectRead]) {
    let mut groups: Vec<(u64, Vec<u64>)> = Vec::new();
    for object in objects {
        match groups.last_mut() {
            Some((group_sequence, numbers)) if *group_sequence == object.group_sequence => {
                numbers.push(the_number_in(object))
            }
            _ => groups.push((object.group_sequence, vec![the_number_in(object)])),
        }
    }
    for (group_sequence, numbers) in &groups {
        assert_eq!(
            numbers[0] % MESSAGES_PER_GROUP,
            0,
            "group {group_sequence} starts where the writer opened one: {numbers:?}"
        );
        assert!(
            numbers
                .iter()
                .enumerate()
                .all(|(offset, number)| *number == numbers[0] + offset as u64)
                && numbers.len() as u64 <= MESSAGES_PER_GROUP,
            "group {group_sequence} holds the messages of one group the writer opened, in \
             order: {numbers:?}"
        );
    }
    for pair in groups.windows(2) {
        assert!(
            pair[1].0 > pair[0].0 && pair[1].1[0] > *pair[0].1.last().unwrap(),
            "groups arrive in the writer's order: {groups:?}"
        );
    }
}

#[test]
fn a_public_port_is_read_at_both_listeners_and_a_private_one_at_loopback_alone() {
    let mut publishing = TatolabdPublishingTheStream::start();
    let at_loopback = publishing.the_broadcast_at(LocalRelayListener::Loopback);
    let at_every_interface = publishing.the_broadcast_at(LocalRelayListener::EveryInterface);
    let public_track = format!("writer/{PUBLIC_PORT}");
    let private_track = format!("writer/{PRIVATE_PORT}");

    publishing.async_runtime.block_on(async {
        for (broadcast, track) in [
            (&at_loopback, &public_track),
            (&at_every_interface, &public_track),
            (&at_loopback, &private_track),
        ] {
            assert_the_objects_are_the_messages_as_written(
                &broadcast.the_next_objects_of(track, 10).await,
            );
        }
        let private_track_off_loopback = tokio::time::timeout(
            NEVER_SERVED_GRACE,
            at_every_interface.subscribe_to(&private_track),
        )
        .await;
        assert!(
            !matches!(private_track_off_loopback, Ok(Ok(_))),
            "the every-interface listener served a private port"
        );
    });
    publishing.runtime.interrupt_and_expect_a_clean_exit();
}

#[test]
fn the_catalog_is_hangs_and_lists_the_port_carrying_an_entry_alone() {
    let mut publishing = TatolabdPublishingTheStream::start();
    let at_loopback = publishing.the_broadcast_at(LocalRelayListener::Loopback);
    publishing.async_runtime.block_on(async {
        for (catalog_track, compressed) in [
            (hang::Catalog::DEFAULT_NAME, false),
            (hang::Catalog::COMPRESSED_NAME, true),
        ] {
            let track = at_loopback
                .broadcast
                .track(catalog_track)
                .unwrap_or_else(|refusal| panic!("`{catalog_track}`: {refusal}"));
            let subscriber = track.subscribe(None).await.unwrap();
            let mut catalog_consumer = if compressed {
                hang::catalog::Consumer::<()>::compressed(subscriber)
            } else {
                hang::catalog::Consumer::<()>::new(subscriber)
            };
            let catalog = loop {
                let catalog = tokio::time::timeout(MOQ_BUDGET, catalog_consumer.next())
                    .await
                    .expect("a catalog arrives")
                    .expect("hang reads the catalog")
                    .expect("the catalog track is live");
                if !catalog.binary.tracks.is_empty() {
                    break catalog;
                }
            };
            assert_eq!(
                catalog.binary.tracks.keys().collect::<Vec<_>>(),
                [&format!("writer/{PUBLIC_PORT}")],
                "`{catalog_track}` lists the port that carries an entry, and no other"
            );
            assert!(catalog.video.renditions.is_empty());
        }
    });
    publishing.runtime.interrupt_and_expect_a_clean_exit();
}

#[test]
fn lowering_a_port_ends_its_subscribers_and_raising_it_announces_it_again() {
    let mut publishing = TatolabdPublishingTheStream::start();
    let at_every_interface = publishing.the_broadcast_at(LocalRelayListener::EveryInterface);
    let at_loopback = publishing.the_broadcast_at(LocalRelayListener::Loopback);
    let public_track = format!("writer/{PUBLIC_PORT}");

    let mut off_loopback = publishing.async_runtime.block_on(async {
        let mut ordered = at_every_interface
            .subscribe_to(&public_track)
            .await
            .unwrap();
        tokio::time::timeout(MOQ_BUDGET, ordered.next_group())
            .await
            .expect("the public port carries groups off loopback")
            .unwrap();
        ordered
    });
    let mut on_loopback = publishing
        .async_runtime
        .block_on(async { at_loopback.subscribe_to(&public_track).await.unwrap() });

    publishing.put_the_port_at(PUBLIC_PORT, "private");
    publishing.async_runtime.block_on(async {
        let ended = tokio::time::timeout(MOQ_BUDGET, async {
            loop {
                match off_loopback.next_group().await {
                    Ok(Some(_)) => continue,
                    _ => return,
                }
            }
        })
        .await;
        assert!(
            ended.is_ok(),
            "a port lowered to private still reaches a subscriber off loopback"
        );
        tokio::time::timeout(MOQ_BUDGET, on_loopback.next_group())
            .await
            .expect("a port lowered to private still reaches loopback")
            .expect("the loopback track stays live")
            .expect("the loopback track has not ended");
    });

    publishing.put_the_port_at(PUBLIC_PORT, "public");
    let at_every_interface_again = publishing.the_broadcast_at(LocalRelayListener::EveryInterface);
    publishing.async_runtime.block_on(async {
        assert!(
            at_every_interface_again
                .the_first_group_of_once_it_is_served(&public_track)
                .await
                .is_some(),
            "a port raised to public again is not announced off loopback"
        );
    });
    publishing.runtime.interrupt_and_expect_a_clean_exit();
}

/// The graph changed while it runs, as an agent changes it through the local
/// API: a node added and exposed is published, its subscribers end when it is
/// removed, one added again under its name is published again, and the port
/// that was there all along carries on throughout.
#[test]
fn a_node_added_exposed_removed_and_added_again_live_is_published_withdrawn_and_published_again() {
    let mut publishing = TatolabdPublishingTheStream::start();
    let at_loopback = publishing.the_broadcast_at(LocalRelayListener::Loopback);
    let added_track = format!("added/{PUBLIC_PORT}");
    let original_track = format!("writer/{PUBLIC_PORT}");

    publishing.add_a_writer_named("added");
    publishing.put_the_port_of_the_node_at("added", PUBLIC_PORT, "public");
    let mut added_read = publishing.async_runtime.block_on(async {
        at_loopback
            .the_first_group_of_once_it_is_served(&added_track)
            .await
            .expect("a node added and exposed live is published");
        let objects = at_loopback.the_next_objects_of(&added_track, 6).await;
        assert_the_objects_are_the_messages_as_written(&objects);
        at_loopback.subscribe_to(&added_track).await.unwrap()
    });

    publishing.remove_the_node_named("added");
    publishing.async_runtime.block_on(async {
        let ended = tokio::time::timeout(MOQ_BUDGET, async {
            loop {
                match added_read.next_group().await {
                    Ok(Some(_)) => continue,
                    _ => return,
                }
            }
        })
        .await;
        assert!(
            ended.is_ok(),
            "a removed node's track still reaches its subscriber"
        );
        assert_the_objects_are_the_messages_as_written(
            &at_loopback.the_next_objects_of(&original_track, 6).await,
        );
    });

    publishing.add_a_writer_named("added");
    publishing.put_the_port_of_the_node_at("added", PUBLIC_PORT, "public");
    publishing.async_runtime.block_on(async {
        assert!(
            at_loopback
                .the_first_group_of_once_it_is_served(&added_track)
                .await
                .is_some(),
            "a node added again under a removed one's name is not published"
        );
    });
    publishing.runtime.interrupt_and_expect_a_clean_exit();
}

#[test]
fn an_internal_port_is_at_no_relay_and_lowering_a_port_to_internal_ends_its_loopback_subscribers() {
    let mut publishing = TatolabdPublishingTheStream::start();
    let at_loopback = publishing.the_broadcast_at(LocalRelayListener::Loopback);
    let private_track = format!("writer/{PRIVATE_PORT}");
    let internal_track = format!("unexposed/{PUBLIC_PORT}");

    let mut on_loopback = publishing
        .async_runtime
        .block_on(at_loopback.the_track_once_it_is_served(&private_track));
    publishing.add_a_writer_named("unexposed");
    publishing.async_runtime.block_on(async {
        let internal_port_at_loopback = tokio::time::timeout(NEVER_SERVED_GRACE, async {
            let mut ordered = at_loopback.subscribe_to(&internal_track).await?;
            ordered.next_group().await
        })
        .await;
        assert!(
            !matches!(internal_port_at_loopback, Ok(Ok(Some(_)))),
            "the loopback listener served an internal port"
        );
    });

    publishing.put_the_port_at(PRIVATE_PORT, "internal");
    publishing.async_runtime.block_on(async {
        let ended = tokio::time::timeout(MOQ_BUDGET, async {
            loop {
                match on_loopback.next_group().await {
                    Ok(Some(_)) => continue,
                    _ => return,
                }
            }
        })
        .await;
        assert!(
            ended.is_ok(),
            "a port lowered to internal still reaches a subscriber at loopback"
        );
    });
    publishing.runtime.interrupt_and_expect_a_clean_exit();
}

/// Stream B reads stream A's private port the one way a stream reads another's:
/// a `RelayTrackSource` subscribed at the loopback listener. What it writes is
/// read back off B's own broadcast — the objects A wrote, in A's groups, A's
/// spacing kept by the re-based stamps — and the source carries the catalog
/// entry of the track it reads.
#[test]
fn a_relay_track_source_in_another_stream_reads_a_private_port_through_loopback_keeping_its_groups_and_spacing()
 {
    let mut publishing = TatolabdPublishingTheStream::start_beside_a_reader_stream();
    publishing.add_a_relay_track_source_reading("private-numbers", PRIVATE_PORT);
    publishing.add_a_relay_track_source_reading("public-numbers", PUBLIC_PORT);
    let reader_at_loopback =
        publishing.the_broadcast_of_the_stream_at(READER_STREAM, LocalRelayListener::Loopback);

    publishing.async_runtime.block_on(async {
        let read_track = "private-numbers/track";
        assert!(
            reader_at_loopback
                .the_first_group_of_once_it_is_served(read_track)
                .await
                .is_some(),
            "the reader stream's source never wrote the private port it reads"
        );
        let objects = reader_at_loopback.the_next_objects_of(read_track, 12).await;
        assert_the_objects_are_grouped_as_the_node_wrote_them(&objects);
        for pair in objects.windows(2) {
            let messages_between = the_number_in(&pair[1]) - the_number_in(&pair[0]);
            assert_eq!(
                pair[1].timestamp_ns - pair[0].timestamp_ns,
                u128::from(messages_between) * NANOSECONDS_BETWEEN_MESSAGE_STAMPS,
                "the source keeps the track's spacing whatever the arrival: {objects:?}"
            );
        }

        let catalog_track = reader_at_loopback
            .broadcast
            .track(hang::Catalog::DEFAULT_NAME)
            .unwrap();
        let mut catalog_consumer =
            hang::catalog::Consumer::<()>::new(catalog_track.subscribe(None).await.unwrap());
        let catalog = loop {
            let catalog = tokio::time::timeout(MOQ_BUDGET, catalog_consumer.next())
                .await
                .expect("the reader stream's catalog lists the source reading the public port")
                .expect("hang reads the catalog")
                .expect("the catalog track is live");
            if !catalog.binary.tracks.is_empty() {
                break catalog;
            }
        };
        assert_eq!(
            catalog.binary.tracks.keys().collect::<Vec<_>>(),
            [&"public-numbers/track".to_string()],
            "the source carries the entry of the track it reads, and a track with none carries none"
        );
        let carried_entry =
            serde_json::to_value(&catalog.binary.tracks["public-numbers/track"]).unwrap();
        assert_eq!(carried_entry["mode"], "stream");
        assert_eq!(carried_entry["mime"], "text/plain");
    });
    publishing.runtime.interrupt_and_expect_a_clean_exit();
}

/// Whether this machine has no Vulkan Video encode and decode, printing the
/// skip line when so.
fn this_machine_lacks_vulkan_video() -> bool {
    #[cfg(target_os = "linux")]
    {
        let device = streamlib::sdk::engine::host_rhi::HostVulkanDevice::new()
            .expect("a Vulkan device comes up on this machine");
        rig_only_capability_skip::skip_unless_this_machine_has(
            rig_only_capability_skip::RigOnlyCapability::VulkanVideo,
            device.supports_video_encode() && device.supports_video_decode(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    false
}

/// A stream encoding H.264 behind a private port: the test pattern into
/// `H264Encoder`, whose `encoded_video` is private.
fn a_stream_encoding_h264_on_a_private_port(stream_name: &str) -> serde_json::Value {
    serde_json::json!({
        "stream": stream_name,
        "nodes": [
            {"name": "testpattern", "type": "tatolab.stream:TestPatternSource",
             "config": {"width": 320, "height": 240}},
            {"name": "encoder", "type": "tatolab.stream:H264Encoder", "config": {}},
        ],
        "links": [{
            "source": {"node": "testpattern", "port": "video"},
            "target": {"node": "encoder", "port": "video"},
        }],
        "exposed": [{"node": "encoder", "port": "encoded_video", "level": "private"}],
    })
}

/// Stream B decodes stream A's private H.264 port: a `RelayTrackSource` at the
/// loopback listener feeds `H264Decoder`, which publishes pictures. The source
/// carries the encoder's `VideoEntry`, and restates each media frame's own
/// timestamp — the hang container's varint — on this machine's clock, so the
/// spacing of the stamps it writes is the spacing of the varints.
#[test]
fn a_relay_track_source_feeds_a_decoder_another_streams_private_h264_port() {
    if this_machine_lacks_vulkan_video() {
        return;
    }
    const ENCODING_STREAM: &str = "camera-rig";
    let mut publishing = TatolabdPublishingTheStream::start_with_the_streams(&[
        (
            ENCODING_STREAM,
            a_stream_encoding_h264_on_a_private_port(ENCODING_STREAM),
        ),
        (
            READER_STREAM,
            serde_json::json!({
                "stream": READER_STREAM,
                "nodes": [{"name": "unexposed", "type": WRITER_NODE_TYPE, "config": {}}],
            }),
        ),
    ]);
    publishing.add_a_relay_track_source_reading_the_port_of(
        "fromcamera",
        ENCODING_STREAM,
        "encoder",
        "encoded_video",
    );
    publishing.call(
        "add_node",
        serde_json::json!({
            "stream": READER_STREAM, "type": "tatolab.stream:H264Decoder", "name": "decoder",
        }),
    );
    publishing.call(
        "connect",
        serde_json::json!({
            "stream": READER_STREAM,
            "from_node": "fromcamera", "from_port": "track",
            "to_node": "decoder", "to_port": "encoded_video",
        }),
    );
    publishing.expose_in_the_reader_stream("fromcamera", "track");
    publishing.expose_in_the_reader_stream("decoder", "video");

    let deadline = std::time::Instant::now() + MOQ_BUDGET;
    while publishing.bags_written_by(READER_STREAM, "decoder", "video") < 30 {
        assert!(
            std::time::Instant::now() < deadline,
            "the decoder fed by the relay source published fewer than 30 pictures in {MOQ_BUDGET:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    let reader_at_loopback =
        publishing.the_broadcast_of_the_stream_at(READER_STREAM, LocalRelayListener::Loopback);
    publishing.async_runtime.block_on(async {
        let objects = reader_at_loopback
            .the_next_objects_of("fromcamera/track", 20)
            .await;
        let varint_timestamps_ns: Vec<u128> = objects
            .iter()
            .map(|object| {
                u128::from(
                    streamlib::sdk::hang_legacy_container_media_frame::read_hang_legacy_container_media_frame(
                        &object.payload,
                    )
                    .expect("the source writes the encoder's media frames as received")
                    .timestamp_microseconds,
                ) * 1_000
            })
            .collect();
        for (pair, varint_pair) in objects.windows(2).zip(varint_timestamps_ns.windows(2)) {
            assert_eq!(
                pair[1].timestamp_ns - pair[0].timestamp_ns,
                varint_pair[1] - varint_pair[0],
                "the source's stamps keep the spacing of the media frames' own timestamps"
            );
        }

        let catalog_track = reader_at_loopback
            .broadcast
            .track(hang::Catalog::DEFAULT_NAME)
            .unwrap();
        let mut catalog_consumer =
            hang::catalog::Consumer::<()>::new(catalog_track.subscribe(None).await.unwrap());
        let catalog = loop {
            let catalog = tokio::time::timeout(MOQ_BUDGET, catalog_consumer.next())
                .await
                .expect("the reader stream's catalog lists the source's video entry")
                .expect("hang reads the catalog")
                .expect("the catalog track is live");
            if catalog.video.renditions.contains_key("fromcamera/track") {
                break catalog;
            }
        };
        let video_entry = &catalog.video.renditions["fromcamera/track"];
        assert!(
            video_entry.codec.to_string().starts_with("avc1."),
            "the source carries the encoder's entry: {video_entry:?}"
        );
        assert_eq!(
            (video_entry.coded_width, video_entry.coded_height),
            (Some(320), Some(240))
        );
    });
    publishing.runtime.interrupt_and_expect_a_clean_exit();
}

/// Two runtimes, the way two machines read each other: the second's
/// `RelayTrackSource` dials the first's every-interface listener for a public
/// port, and what it writes — read back off the second runtime's own
/// broadcast — is the first's objects in their groups, their spacing kept.
#[test]
fn a_relay_track_source_in_a_second_runtime_reads_the_first_runtimes_public_port() {
    let mut first_runtime = TatolabdPublishingTheStream::start();
    let mut second_runtime = TatolabdPublishingTheStream::start_with_the_streams(&[(
        READER_STREAM,
        serde_json::json!({
            "stream": READER_STREAM,
            "nodes": [{"name": "unexposed", "type": WRITER_NODE_TYPE, "config": {}}],
        }),
    )]);
    second_runtime.add_a_relay_track_source_at(
        &first_runtime.dial_url_of(LocalRelayListener::EveryInterface),
        "fromfirst",
        &format!("{}/{STREAM}/writer/{PUBLIC_PORT}", first_runtime.machine),
    );
    second_runtime.expose_in_the_reader_stream("fromfirst", "track");
    let at_the_second_runtimes_loopback =
        second_runtime.the_broadcast_of_the_stream_at(READER_STREAM, LocalRelayListener::Loopback);

    second_runtime.async_runtime.block_on(async {
        at_the_second_runtimes_loopback
            .the_track_once_it_is_served("fromfirst/track")
            .await;
        let objects = at_the_second_runtimes_loopback
            .the_next_objects_of("fromfirst/track", 12)
            .await;
        assert_the_objects_are_grouped_as_the_node_wrote_them(&objects);
        for pair in objects.windows(2) {
            let messages_between = the_number_in(&pair[1]) - the_number_in(&pair[0]);
            assert_eq!(
                pair[1].timestamp_ns - pair[0].timestamp_ns,
                u128::from(messages_between) * NANOSECONDS_BETWEEN_MESSAGE_STAMPS,
                "the second runtime keeps the first's spacing: {objects:?}"
            );
        }
    });
    second_runtime.runtime.interrupt_and_expect_a_clean_exit();
    first_runtime.runtime.interrupt_and_expect_a_clean_exit();
}

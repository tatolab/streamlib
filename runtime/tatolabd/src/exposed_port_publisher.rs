// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's one publisher. Every exposed output port of every loaded
//! stream is forwarded as written to each joined relay whose levels include the
//! port's, as the track `<node>/<port>` of the stream's broadcast
//! `<root>/<machine>/<stream>.hang`, beside the broadcast's catalog merged
//! from the ports' entries. It decodes no payload, knows no port's encoding and
//! offers every exposed port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::Section;
use moq_net::{Timescale, Timestamp};
use streamlib::sdk::descriptors::{CatalogEntry, CatalogSection};
use streamlib::sdk::graph::ProcessorUniqueId;
use streamlib::sdk::iceoryx2::{
    MessageGroupBoundary, MessageGroupLossRecoveryDisposition, MessageGroupLossRecoveryGate,
    MessageReadFromInputPort,
};
use streamlib::sdk::runtime::{
    ExposedOutputPortOfALoadedStream, ExposedOutputPortSourceWiring, ExposedOutputPortSubscription,
    ExposureChangeReceiver, Runner, RuntimeOperations,
};
use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};
use streamlib_relay_adapter_sdk::{
    BroadcastPath, JoinedRelay, OutputPortExposureLevel, RelayAdapter, RelayInstanceId,
};

/// The suffix of a broadcast whose catalog the stack's players read.
const BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ: &str = ".hang";

/// One joined relay as the publisher dials it.
pub(crate) struct RelayTheExposedPortPublisherDials {
    pub(crate) relay_id: RelayInstanceId,
    /// The relay's URL with its credential in it; the whole URL is the secret.
    pub(crate) dial_url_carrying_the_credential: SecretString,
    pub(crate) levels_served: Vec<OutputPortExposureLevel>,
    pub(crate) broadcast_root: BroadcastPath,
}

impl RelayTheExposedPortPublisherDials {
    /// How the publisher dials `adapter`, joined at `joined`, for the machine
    /// `engine` runs on.
    pub(crate) fn of_a_joined_relay(
        adapter: &dyn RelayAdapter,
        joined: &JoinedRelay,
        engine: &Runner,
    ) -> Self {
        let dial_url_carrying_the_credential = match adapter.credential_for_dial() {
            Some(credential) => credential.dial_url_carrying_the_credential(&joined.dial_url),
            None => SecretString::from(joined.dial_url.to_string()),
        };
        Self {
            relay_id: adapter.relay_id().clone(),
            dial_url_carrying_the_credential,
            levels_served: adapter.levels_served().to_vec(),
            broadcast_root: adapter.broadcast_root(engine.machine_name()),
        }
    }
}

/// The publisher while it runs; dropping it ends every track, broadcast and
/// relay session it holds.
pub(crate) struct ExposedPortPublisherOfThisRuntime {
    publishing: tokio::task::JoinHandle<()>,
}

impl Drop for ExposedPortPublisherOfThisRuntime {
    fn drop(&mut self) {
        self.publishing.abort();
    }
}

/// Start the publisher on `engine`'s async runtime, dialing `relays`, or say
/// why none runs.
pub(crate) fn start_the_exposed_port_publisher(
    engine: &Arc<Runner>,
    relays: Vec<RelayTheExposedPortPublisherDials>,
) -> Option<ExposedPortPublisherOfThisRuntime> {
    if relays.is_empty() {
        tracing::info!("no relay is joined, so no exposed port is published");
        return None;
    }
    let exposure_changes = match engine.exposure_changes() {
        Ok(exposure_changes) => exposure_changes,
        Err(refusal) => {
            tracing::error!("no exposed port is published: {refusal}");
            return None;
        }
    };
    let engine_while_it_runs = Arc::downgrade(engine);
    let publishing = engine.tokio_handle().spawn(async move {
        publish_every_exposed_port(engine_while_it_runs, exposure_changes, relays).await;
    });
    Some(ExposedPortPublisherOfThisRuntime { publishing })
}

async fn publish_every_exposed_port(
    engine_while_it_runs: Weak<Runner>,
    mut exposure_changes: ExposureChangeReceiver,
    relays: Vec<RelayTheExposedPortPublisherDials>,
) {
    let mut publisher = ExposedPortPublisher {
        relays: relays
            .into_iter()
            .filter_map(RelayDialedByThePublisher::dial)
            .collect(),
        broadcasts: HashMap::new(),
        forwarded_ports: HashMap::new(),
        why_each_port_is_not_forwarded_yet: HashMap::new(),
    };
    while let Some(exposed_output_ports) = exposure_changes.next_exposed_output_ports().await {
        let Some(engine) = engine_while_it_runs.upgrade() else {
            return;
        };
        publisher
            .forward_exactly(&engine, exposed_output_ports)
            .await;
    }
}

/// One relay's session, through which every broadcast at it is published.
struct RelayDialedByThePublisher {
    relay_id: RelayInstanceId,
    levels_served: Vec<OutputPortExposureLevel>,
    broadcast_root: BroadcastPath,
    origin: moq_net::origin::Producer,
    _session: moq_tokio::Connection,
}

impl RelayDialedByThePublisher {
    fn dial(relay: RelayTheExposedPortPublisherDials) -> Option<Self> {
        let dial_url = match relay
            .dial_url_carrying_the_credential
            .expose_secret()
            .parse::<streamlib_relay_adapter_sdk::url::Url>()
        {
            Ok(dial_url) => dial_url,
            Err(unparsed) => {
                tracing::error!(
                    relay = %relay.relay_id,
                    "the relay's dial URL does not parse ({unparsed}), so nothing is published at it"
                );
                return None;
            }
        };
        let client = match moq_tokio::connect::Config::default()
            .init(moq_tokio::quic::Config::default())
        {
            Ok(client) => client,
            Err(refusal) => {
                tracing::error!(
                    relay = %relay.relay_id,
                    "no MoQ client builds for the relay, so nothing is published at it: {refusal}"
                );
                return None;
            }
        };
        let origin = moq_tokio::origin::spawn();
        let session = client
            .with_publisher(&origin)
            .with_reconnect(true)
            .connect(dial_url);
        tracing::info!(relay = %relay.relay_id, "the publisher dials the relay");
        Some(Self {
            relay_id: relay.relay_id,
            levels_served: relay.levels_served,
            broadcast_root: relay.broadcast_root,
            origin,
            _session: session,
        })
    }

    fn serves(&self, level: OutputPortExposureLevel) -> bool {
        self.levels_served.contains(&level)
    }
}

/// An exposed port's address in the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ExposedOutputPortAddress {
    stream_name: String,
    node_name: String,
    port_name: String,
    /// Tells a node removed and added again under its name from the one it
    /// replaced.
    node_id: ProcessorUniqueId,
}

impl ExposedOutputPortAddress {
    fn of(exposed_port: &ExposedOutputPortOfALoadedStream) -> Self {
        Self {
            stream_name: exposed_port.stream_name.clone(),
            node_name: exposed_port.node_name.clone(),
            port_name: exposed_port.port_name.clone(),
            node_id: exposed_port.node_id.clone(),
        }
    }

    fn track_name(&self) -> String {
        format!("{}/{}", self.node_name, self.port_name)
    }
}

impl std::fmt::Display for ExposedOutputPortAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.stream_name, self.node_name, self.port_name
        )
    }
}

/// One stream's broadcast at one relay, with its catalog in both spellings.
struct BroadcastOfAStreamAtARelay {
    broadcast: moq_net::broadcast::Producer,
    catalog: moq_json::snapshot::Producer<serde_json::Value>,
    compressed_catalog: moq_json::snapshot::Producer<serde_json::Value>,
}

impl BroadcastOfAStreamAtARelay {
    fn announced_at(relay: &RelayDialedByThePublisher, stream_name: &str) -> Option<Self> {
        let broadcast_path = format!(
            "{}/{stream_name}{BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ}",
            relay.broadcast_root
        );
        let created = (|| -> Result<Self, moq_net::Error> {
            let broadcast = relay.origin.create_broadcast(broadcast_path.as_str())?;
            let catalog = moq_json::snapshot::Producer::new(
                broadcast.create_track(
                    hang::Catalog::DEFAULT_NAME,
                    hang::Catalog::default_track_info(),
                )?,
                moq_json::snapshot::Config::default(),
            );
            let mut compressed = moq_json::snapshot::Config::default();
            compressed.compression = moq_json::Compression::Deflate;
            let compressed_catalog = moq_json::snapshot::Producer::new(
                broadcast.create_track(
                    hang::Catalog::COMPRESSED_NAME,
                    hang::Catalog::default_track_info(),
                )?,
                compressed,
            );
            Ok(Self {
                broadcast,
                catalog,
                compressed_catalog,
            })
        })();
        let mut broadcast = match created {
            Ok(broadcast) => broadcast,
            Err(refusal) => {
                tracing::error!(
                    relay = %relay.relay_id,
                    "the broadcast `{broadcast_path}` could not be created: {refusal}"
                );
                return None;
            }
        };
        broadcast.publish_the_catalog(&serde_json::json!({}));
        if let Err(refusal) = broadcast.broadcast.announce(Default::default()) {
            tracing::error!(
                relay = %relay.relay_id,
                "the broadcast `{broadcast_path}` could not be announced: {refusal}"
            );
            return None;
        }
        tracing::info!(relay = %relay.relay_id, "the broadcast `{broadcast_path}` is announced");
        Some(broadcast)
    }

    fn publish_the_catalog(&mut self, catalog: &serde_json::Value) {
        for (catalog_track, producer) in [
            (hang::Catalog::DEFAULT_NAME, &mut self.catalog),
            (hang::Catalog::COMPRESSED_NAME, &mut self.compressed_catalog),
        ] {
            if let Err(refusal) = producer.update(catalog) {
                tracing::warn!("the catalog track `{catalog_track}` was not updated: {refusal}");
            }
        }
    }
}

impl Drop for BroadcastOfAStreamAtARelay {
    fn drop(&mut self) {
        self.broadcast.close();
    }
}

/// The key hang's `video` and `audio` sections hold their renditions under.
const HANG_MAP_OF_THE_RENDITIONS_OF_A_MEDIA_SECTION: &str = "renditions";

/// A broadcast's catalog: each port's entry copied under its section, keyed by
/// its track name. A port with no entry is a track the catalog does not list.
fn the_catalog_merged_from_the_entries_of<'entry>(
    track_names_and_entries: impl IntoIterator<Item = (String, &'entry CatalogEntry)>,
) -> serde_json::Value {
    let mut tracks_by_section: HashMap<CatalogSection, serde_json::Map<String, serde_json::Value>> =
        HashMap::new();
    for (track_name, catalog_entry) in track_names_and_entries {
        tracks_by_section
            .entry(catalog_entry.section)
            .or_default()
            .insert(track_name, catalog_entry.entry.clone());
    }
    serde_json::Value::Object(
        CatalogSection::ALL
            .into_iter()
            .filter_map(|section| {
                let tracks = tracks_by_section.remove(&section)?;
                let map_of_the_section = match section {
                    CatalogSection::Video | CatalogSection::Audio => {
                        HANG_MAP_OF_THE_RENDITIONS_OF_A_MEDIA_SECTION
                    }
                    CatalogSection::Text => hang::catalog::Text::MAP,
                    CatalogSection::Json => hang::catalog::Json::MAP,
                    CatalogSection::Binary => hang::catalog::Binary::MAP,
                };
                Some((
                    section.as_catalog_key().to_string(),
                    serde_json::json!({ map_of_the_section: tracks }),
                ))
            })
            .collect(),
    )
}

/// One exposed port's track at one relay, and the group being written to it.
struct ExposedPortTrackAtARelay {
    relay_id: RelayInstanceId,
    track: moq_net::track::Producer,
    group_being_written: Option<moq_net::group::Producer>,
}

impl ExposedPortTrackAtARelay {
    fn finish_the_group_being_written(&mut self) {
        if let Some(group) = self.group_being_written.take() {
            let _ = group.finish();
        }
    }

    fn withdraw(mut self) {
        self.finish_the_group_being_written();
        let _ = self.track.abort(moq_net::Error::NotFound);
    }
}

/// Writes one exposed port's messages to its track at every relay it reaches:
/// each message one object stamped from its envelope, a new group at each
/// group start. After a loss the rest of the current group is abandoned and
/// writing resumes at the next group start, never inside a group.
struct ExposedPortTrackWriter {
    group_loss_recovery_gate: MessageGroupLossRecoveryGate,
    tracks_at_each_relay: Vec<ExposedPortTrackAtARelay>,
}

impl ExposedPortTrackWriter {
    fn new() -> Self {
        Self {
            group_loss_recovery_gate: MessageGroupLossRecoveryGate::default(),
            tracks_at_each_relay: Vec::new(),
        }
    }

    fn write_message(&mut self, message: MessageReadFromInputPort) {
        let disposition = self
            .group_loss_recovery_gate
            .admit_message(message.group_boundary, message.lost_since_last_read);
        if disposition == MessageGroupLossRecoveryDisposition::DiscardUntilTheNextGroupStart {
            for track in &mut self.tracks_at_each_relay {
                track.finish_the_group_being_written();
            }
            return;
        }
        let timestamp = the_object_timestamp_of(message.timestamp_ns);
        let payload = Bytes::from(message.payload_bytes);
        for track in &mut self.tracks_at_each_relay {
            if message.group_boundary == MessageGroupBoundary::StartsGroup {
                track.finish_the_group_being_written();
                track.group_being_written = match track.track.append_group() {
                    Ok(group) => Some(group),
                    Err(refusal) => {
                        tracing::debug!(
                            relay = %track.relay_id,
                            "a group was not opened, so its objects are not written: {refusal}"
                        );
                        None
                    }
                };
            }
            let Some(group) = &mut track.group_being_written else {
                continue;
            };
            if let Err(refusal) = group.write_frame(timestamp, &payload) {
                tracing::debug!(
                    relay = %track.relay_id,
                    "an object was not written, so its group ends here: {refusal}"
                );
                track.finish_the_group_being_written();
            }
        }
    }

    fn reaches(&self, relay_id: &RelayInstanceId) -> bool {
        self.tracks_at_each_relay
            .iter()
            .any(|track| &track.relay_id == relay_id)
    }

    fn add_the_track_at(&mut self, relay_id: RelayInstanceId, track: moq_net::track::Producer) {
        self.tracks_at_each_relay.push(ExposedPortTrackAtARelay {
            relay_id,
            track,
            group_being_written: None,
        });
    }

    fn withdraw_every_track_but_those_at(
        &mut self,
        relay_is_reached: impl Fn(&RelayInstanceId) -> bool,
    ) {
        let (kept, withdrawn) = std::mem::take(&mut self.tracks_at_each_relay)
            .into_iter()
            .partition(|track| relay_is_reached(&track.relay_id));
        self.tracks_at_each_relay = kept;
        for track in withdrawn {
            track.withdraw();
        }
    }
}

impl Drop for ExposedPortTrackWriter {
    fn drop(&mut self) {
        self.withdraw_every_track_but_those_at(|_| false);
    }
}

/// An envelope's stamp as an object's timestamp: nanoseconds, a stamp before
/// the clock's zero read as zero.
fn the_object_timestamp_of(timestamp_ns: i64) -> Timestamp {
    Timestamp::from_nanos(u64::try_from(timestamp_ns).unwrap_or(0)).unwrap_or(Timestamp::ZERO)
}

/// The track info of an exposed port: stamps kept at nanoseconds, as the
/// envelope carries them.
fn track_info_of_an_exposed_port() -> moq_net::track::Info {
    moq_net::track::Info::default().with_timescale(Timescale::NANO)
}

/// One exposed port being forwarded: its subscription read on a task of its
/// own into the writer its tracks hang off.
struct ForwardedExposedPort {
    catalog_entry: Option<CatalogEntry>,
    track_writer: Arc<Mutex<ExposedPortTrackWriter>>,
    reading: tokio::task::JoinHandle<()>,
}

impl Drop for ForwardedExposedPort {
    fn drop(&mut self) {
        self.reading.abort();
        // The reading task may still hold the writer, so its tracks are
        // withdrawn here rather than when the writer drops.
        lock_the_writer(&self.track_writer).withdraw_every_track_but_those_at(|_| false);
    }
}

fn lock_the_writer(
    track_writer: &Mutex<ExposedPortTrackWriter>,
) -> std::sync::MutexGuard<'_, ExposedPortTrackWriter> {
    track_writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct ExposedPortPublisher {
    relays: Vec<RelayDialedByThePublisher>,
    broadcasts: HashMap<(RelayInstanceId, String), BroadcastOfAStreamAtARelay>,
    forwarded_ports: HashMap<ExposedOutputPortAddress, ForwardedExposedPort>,
    why_each_port_is_not_forwarded_yet: HashMap<ExposedOutputPortAddress, String>,
}

impl ExposedPortPublisher {
    /// Forward exactly `exposed_output_ports`: subscribe to each not yet
    /// forwarded, move each whose level changed onto the relays it now
    /// reaches, withdraw every other, and publish each broadcast's catalog.
    async fn forward_exactly(
        &mut self,
        engine: &Runner,
        exposed_output_ports: Vec<ExposedOutputPortOfALoadedStream>,
    ) {
        let still_exposed: HashMap<ExposedOutputPortAddress, ExposedOutputPortOfALoadedStream> =
            exposed_output_ports
                .into_iter()
                .map(|exposed_port| (ExposedOutputPortAddress::of(&exposed_port), exposed_port))
                .collect();
        self.forwarded_ports.retain(|address, forwarded| {
            let keeps_forwarding =
                still_exposed.contains_key(address) && !forwarded.reading.is_finished();
            if !keeps_forwarding {
                tracing::info!("the exposed port `{address}` is withdrawn from every relay");
            }
            keeps_forwarding
        });
        self.why_each_port_is_not_forwarded_yet
            .retain(|address, _| still_exposed.contains_key(address));

        for (address, exposed_port) in &still_exposed {
            if !self.forwarded_ports.contains_key(address) {
                match subscribe_to(engine, address).await {
                    Ok(subscription) => {
                        self.why_each_port_is_not_forwarded_yet.remove(address);
                        let track_writer = Arc::new(Mutex::new(ExposedPortTrackWriter::new()));
                        let reading =
                            tokio::spawn(read_into(subscription, Arc::clone(&track_writer)));
                        self.forwarded_ports.insert(
                            address.clone(),
                            ForwardedExposedPort {
                                catalog_entry: None,
                                track_writer,
                                reading,
                            },
                        );
                    }
                    Err(why_not) => {
                        if self.why_each_port_is_not_forwarded_yet.get(address) != Some(&why_not) {
                            tracing::debug!(
                                "the exposed port `{address}` is not forwarded yet: {why_not}"
                            );
                            self.why_each_port_is_not_forwarded_yet
                                .insert(address.clone(), why_not);
                        }
                        continue;
                    }
                }
            }
            self.put_the_port_on_the_relays_its_level_reaches(address, exposed_port);
        }
        self.publish_each_broadcast_and_its_catalog();
    }

    fn put_the_port_on_the_relays_its_level_reaches(
        &mut self,
        address: &ExposedOutputPortAddress,
        exposed_port: &ExposedOutputPortOfALoadedStream,
    ) {
        let Some(forwarded) = self.forwarded_ports.get_mut(address) else {
            return;
        };
        forwarded.catalog_entry = exposed_port.catalog_entry.clone();
        let relays_reached: Vec<&RelayDialedByThePublisher> = self
            .relays
            .iter()
            .filter(|relay| relay.serves(exposed_port.level))
            .collect();
        let mut track_writer = lock_the_writer(&forwarded.track_writer);
        track_writer.withdraw_every_track_but_those_at(|relay_id| {
            relays_reached
                .iter()
                .any(|relay| &relay.relay_id == relay_id)
        });
        for relay in relays_reached {
            if track_writer.reaches(&relay.relay_id) {
                continue;
            }
            let broadcast = match self
                .broadcasts
                .entry((relay.relay_id.clone(), address.stream_name.clone()))
            {
                std::collections::hash_map::Entry::Occupied(occupied) => occupied.into_mut(),
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    match BroadcastOfAStreamAtARelay::announced_at(relay, &address.stream_name) {
                        Some(broadcast) => vacant.insert(broadcast),
                        None => continue,
                    }
                }
            };
            match broadcast
                .broadcast
                .create_track(address.track_name(), track_info_of_an_exposed_port())
            {
                Ok(track) => {
                    tracing::info!(
                        relay = %relay.relay_id,
                        "the exposed port `{address}` is announced as the track `{}`",
                        address.track_name()
                    );
                    track_writer.add_the_track_at(relay.relay_id.clone(), track);
                }
                Err(refusal) => tracing::error!(
                    relay = %relay.relay_id,
                    "the track `{}` could not be created: {refusal}",
                    address.track_name()
                ),
            }
        }
    }

    fn publish_each_broadcast_and_its_catalog(&mut self) {
        let forwarded_ports = &self.forwarded_ports;
        self.broadcasts
            .retain(|(relay_id, stream_name), broadcast| {
                let ports_at_this_relay: Vec<(&ExposedOutputPortAddress, &ForwardedExposedPort)> =
                    forwarded_ports
                        .iter()
                        .filter(|(address, forwarded)| {
                            &address.stream_name == stream_name
                                && lock_the_writer(&forwarded.track_writer).reaches(relay_id)
                        })
                        .collect();
                if ports_at_this_relay.is_empty() {
                    tracing::info!(
                        relay = %relay_id,
                        "the broadcast of the stream `{stream_name}` is withdrawn"
                    );
                    return false;
                }
                broadcast.publish_the_catalog(&the_catalog_merged_from_the_entries_of(
                    ports_at_this_relay
                        .iter()
                        .filter_map(|(address, forwarded)| {
                            forwarded
                                .catalog_entry
                                .as_ref()
                                .map(|catalog_entry| (address.track_name(), catalog_entry))
                        }),
                ));
                true
            });
    }
}

async fn subscribe_to(
    engine: &Runner,
    address: &ExposedOutputPortAddress,
) -> Result<ExposedOutputPortSubscription, String> {
    let stream = engine
        .loaded_stream_named(&address.stream_name)
        .map_err(|refusal| refusal.to_string())?;
    stream
        .subscribe_to_an_exposed_output_port(address.node_name.clone(), address.port_name.clone())
        .await
        .map_err(|refusal| refusal.to_string())
}

/// How often a subscription to a helper source is asked whether the helper
/// has opened its port.
const HOW_OFTEN_A_HELPERS_ANSWER_IS_ASKED_FOR: Duration = Duration::from_millis(50);

/// Write what `subscription` reads into `track_writer`, once its source has
/// opened its end of the channel; a helper's refusal ends the read, said once.
async fn read_into(
    mut subscription: ExposedOutputPortSubscription,
    track_writer: Arc<Mutex<ExposedPortTrackWriter>>,
) {
    loop {
        match subscription.source_wiring() {
            ExposedOutputPortSourceWiring::Wired => break,
            ExposedOutputPortSourceWiring::AwaitingItsHelpersAnswer => {
                tokio::time::sleep(HOW_OFTEN_A_HELPERS_ANSWER_IS_ASKED_FOR).await
            }
            ExposedOutputPortSourceWiring::RefusedByItsHelper { reason } => {
                tracing::warn!(
                    "the exposed port `{}/{}/{}` is not published: its helper could not open \
                     its port: {reason}",
                    subscription.stream_name(),
                    subscription.node_name(),
                    subscription.port_name()
                );
                return;
            }
        }
    }
    while let Some(message) = subscription.next_message().await {
        lock_the_writer(&track_writer).write_message(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::descriptors::{BinaryEntry, CatalogDataTrackMode};

    use MessageGroupBoundary::{ContinuesGroup, StartsGroup};

    fn a_message(
        payload: &[u8],
        group_boundary: MessageGroupBoundary,
        lost_since_last_read: u64,
    ) -> MessageReadFromInputPort {
        MessageReadFromInputPort {
            payload_bytes: payload.to_vec(),
            timestamp_ns: 1_000,
            group_boundary,
            lost_since_last_read,
        }
    }

    /// A reader of a track from before anything is written to it, taking
    /// what arrived after each write, as a live subscriber does.
    struct ReaderOfAStandaloneTrack {
        ordered: moq_net::track::Ordered,
        group_being_read: Option<moq_net::group::Consumer>,
        groups_read: Vec<Vec<Vec<u8>>>,
        async_runtime: tokio::runtime::Runtime,
    }

    impl ReaderOfAStandaloneTrack {
        fn subscribed_to(track: &moq_net::track::Producer) -> Self {
            Self {
                ordered: track.subscribe(None).ordered(),
                group_being_read: None,
                groups_read: Vec::new(),
                async_runtime: tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            }
        }

        fn take_what_arrived(&mut self) {
            let quiet = std::time::Duration::from_millis(50);
            let Self {
                ordered,
                group_being_read,
                groups_read,
                async_runtime,
            } = self;
            async_runtime.block_on(async {
                loop {
                    if let Some(group) = group_being_read {
                        loop {
                            match tokio::time::timeout(quiet, group.read_frame()).await {
                                Ok(Ok(Some(frame))) => {
                                    groups_read.last_mut().unwrap().push(frame.payload.to_vec())
                                }
                                Ok(_) => {
                                    *group_being_read = None;
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    match tokio::time::timeout(quiet, ordered.next_group()).await {
                        Ok(Ok(Some(group))) => {
                            groups_read.push(Vec::new());
                            *group_being_read = Some(group);
                        }
                        _ => return,
                    }
                }
            });
        }
    }

    fn a_standalone_track() -> moq_net::track::Producer {
        moq_net::broadcast::Info::new()
            .produce()
            .create_track("camera/video", track_info_of_an_exposed_port())
            .unwrap()
    }

    /// A loss inside a group abandons the rest of it and writing resumes at
    /// the next group start; a group start reporting a loss is itself written.
    #[test]
    fn a_loss_skips_to_the_next_group_start_never_inside_a_group() {
        let track = a_standalone_track();
        let mut reader = ReaderOfAStandaloneTrack::subscribed_to(&track);
        let mut writer = ExposedPortTrackWriter::new();
        writer.add_the_track_at(RelayInstanceId("local-loopback".into()), track);

        for message in [
            a_message(b"k0", StartsGroup, 0),
            a_message(b"d0", ContinuesGroup, 0),
            a_message(b"d1-after-a-loss", ContinuesGroup, 2),
            a_message(b"d2", ContinuesGroup, 0),
            a_message(b"k1", StartsGroup, 0),
            a_message(b"d3", ContinuesGroup, 0),
            a_message(b"k2-after-a-loss", StartsGroup, 1),
        ] {
            writer.write_message(message);
            reader.take_what_arrived();
        }
        assert_eq!(
            reader.groups_read,
            vec![
                vec![b"k0".to_vec(), b"d0".to_vec()],
                vec![b"k1".to_vec(), b"d3".to_vec()],
                vec![b"k2-after-a-loss".to_vec()],
            ]
        );
    }

    /// A track added mid-group begins at the next group start.
    #[test]
    fn a_track_added_inside_a_group_begins_at_the_next_group_start() {
        let mut writer = ExposedPortTrackWriter::new();
        writer.write_message(a_message(b"k0", StartsGroup, 0));
        let track = a_standalone_track();
        let mut reader = ReaderOfAStandaloneTrack::subscribed_to(&track);
        writer.add_the_track_at(RelayInstanceId("local-every-interface".into()), track);
        writer.write_message(a_message(b"d0", ContinuesGroup, 0));
        reader.take_what_arrived();
        writer.write_message(a_message(b"k1", StartsGroup, 0));
        reader.take_what_arrived();
        assert_eq!(reader.groups_read, vec![vec![b"k1".to_vec()]]);
    }

    /// The catalog copies each entry under its section, keyed by the track
    /// name, and hang reads it; a port with no entry is not listed.
    #[test]
    fn the_catalog_copies_each_entry_under_its_section_and_hang_reads_it() {
        let binary: CatalogEntry = BinaryEntry {
            mode: CatalogDataTrackMode::Snapshot,
            mime: Some("application/msgpack".to_string()),
        }
        .into();
        let video = CatalogEntry {
            section: CatalogSection::Video,
            entry: serde_json::json!({"codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720}),
        };
        let catalog = the_catalog_merged_from_the_entries_of([
            ("camera/video".to_string(), &binary),
            ("encoder/encoded_video".to_string(), &video),
        ]);
        assert_eq!(
            catalog["binary"]["tracks"]["camera/video"], binary.entry,
            "the entry is copied as written"
        );
        let read_by_hang = hang::Catalog::<()>::from_slice(&serde_json::to_vec(&catalog).unwrap())
            .expect("hang reads the catalog");
        assert_eq!(
            read_by_hang.video.renditions["encoder/encoded_video"]
                .codec
                .to_string(),
            "avc1.64001f"
        );
        assert!(read_by_hang.binary.tracks.contains_key("camera/video"));
        assert!(read_by_hang.audio.renditions.is_empty());
    }

    #[test]
    fn a_stamp_is_kept_in_nanoseconds_and_one_before_zero_reads_as_zero() {
        assert_eq!(the_object_timestamp_of(1_234_567).as_nanos(), 1_234_567);
        assert!(the_object_timestamp_of(-5).is_zero());
    }
}

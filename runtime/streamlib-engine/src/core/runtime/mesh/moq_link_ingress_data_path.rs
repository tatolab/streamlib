// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A remote link's data path over MoQ: subscribe to the source runtime's
//! gateway track on the relay, open each sealed object, split it back into
//! the attachment and payload the Zenoh data path carries, and hand both to
//! the ingress's ring — everything after that is the ingress unchanged.
//!
//! Control stays on Zenoh: the source runtime is found, asked what it offers
//! and told it is being read exactly as before. What Zenoh adds is where the
//! source's gateway publishes, read off its mesh description on every
//! attempt, so a gateway that reconnected under a fresh session id is followed.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::core::graph::MeshPortAddress;
use crate::core::runtime::mesh::mesh_data_message_attachment::MeshDataMessageAttachment;
use crate::core::runtime::mesh::moq_gateway_configuration::MoqGatewayDoors;
use crate::core::runtime::mesh::moq_gateway_object_framing::split_a_plaintext_moq_object;
use crate::core::runtime::mesh::moq_relay_client_connection::{
    open_a_moq_session_to_the_relay, the_moq_transport_runtime,
};
use crate::core::runtime::mesh::moq_secure_object_envelope::{
    a_moq_object_is_sealed, open_a_sealed_moq_object,
};

/// How long a MoQ ingress waits before subscribing again after its
/// subscription ended or could not start.
const HOW_LONG_A_MOQ_INGRESS_WAITS_BEFORE_SUBSCRIBING_AGAIN: Duration = Duration::from_millis(500);

/// Where the source runtime's gateway publishes right now, read off its mesh
/// description — `None` while it names none.
pub(crate) type WhereTheSourceGatewayPublishes = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// What an arriving object is handed to: the ingress's ring.
pub(crate) type WhereAnArrivingMoqObjectLands =
    Arc<dyn Fn(MeshDataMessageAttachment, Vec<u8>) + Send + Sync>;

/// Everything a MoQ ingress needs beyond the address it reads.
#[derive(Clone)]
pub(crate) struct HowAnIngressReadsOffTheRelay {
    /// This runtime's own relay and content keys.
    pub(crate) doors: Arc<MoqGatewayDoors>,
    pub(crate) where_the_source_gateway_publishes: WhereTheSourceGatewayPublishes,
}

/// One address being read off the relay. Dropping it ends the subscription.
pub(crate) struct MoqLinkIngressDataPath {
    stop: Arc<AtomicBool>,
    reading_task: tokio::task::JoinHandle<()>,
}

impl Drop for MoqLinkIngressDataPath {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.reading_task.abort();
    }
}

impl MoqLinkIngressDataPath {
    /// Start reading `address` off the relay into `where_it_lands`.
    pub(crate) fn start(
        address: &MeshPortAddress,
        how: HowAnIngressReadsOffTheRelay,
        where_it_lands: WhereAnArrivingMoqObjectLands,
    ) -> Result<Self, String> {
        let transport_runtime = the_moq_transport_runtime()?;
        let stop = Arc::new(AtomicBool::new(false));
        let reading_task = transport_runtime.spawn(read_one_address_off_the_relay_until_stopped(
            address.clone(),
            how,
            where_it_lands,
            Arc::clone(&stop),
        ));
        Ok(Self { stop, reading_task })
    }
}

/// Subscribe, drain, and subscribe again, until stopped.
async fn read_one_address_off_the_relay_until_stopped(
    address: MeshPortAddress,
    how: HowAnIngressReadsOffTheRelay,
    where_it_lands: WhereAnArrivingMoqObjectLands,
    stop: Arc<AtomicBool>,
) {
    let track_name = format!(
        "{}/{}",
        address.processor_display_name(),
        address.port_name()
    );
    let mut said_why_it_is_not_reading: Option<String> = None;
    while !stop.load(Ordering::Acquire) {
        let why_it_stopped = read_one_subscription(&track_name, &how, &where_it_lands, &stop).await;
        if said_why_it_is_not_reading.as_deref() != Some(why_it_stopped.as_str()) {
            tracing::info!("{address} is not being read off the relay: {why_it_stopped}");
            said_why_it_is_not_reading = Some(why_it_stopped);
        }
        tokio::time::sleep(HOW_LONG_A_MOQ_INGRESS_WAITS_BEFORE_SUBSCRIBING_AGAIN).await;
    }
}

/// One subscription's life, answering why it ended.
async fn read_one_subscription(
    track_name: &str,
    how: &HowAnIngressReadsOffTheRelay,
    where_it_lands: &WhereAnArrivingMoqObjectLands,
    stop: &AtomicBool,
) -> String {
    let Some(source_namespace) = (how.where_the_source_gateway_publishes)() else {
        return "the source runtime's MoQ gateway announces no namespace yet".to_string();
    };
    let Some(relay_url) = how
        .doors
        .with_what_it_is_told(|told| told.relay_publish_url.clone())
    else {
        return "this runtime has no MoQ relay configured".to_string();
    };
    let track_namespace =
        match moq_transport::coding::TrackNamespace::try_from(source_namespace.as_str()) {
            Ok(track_namespace) => track_namespace,
            Err(not_a_namespace) => {
                return format!("{source_namespace} is not a MoQ namespace: {not_a_namespace}");
            }
        };
    let relay_session =
        match open_a_moq_session_to_the_relay(&relay_url, how.doors.accept_any_relay_certificate)
            .await
        {
            Ok(relay_session) => relay_session,
            Err(why_not) => return why_not,
        };
    let moq_transport_session = relay_session.session;
    let mut subscriber = relay_session.subscriber;
    let session_task = tokio::spawn(async move { moq_transport_session.run().await });
    let (track_writer, track_reader) =
        moq_transport::serve::Track::new(track_namespace, track_name).produce();
    let subscribe_task = tokio::spawn(async move { subscriber.subscribe(track_writer).await });
    let namespace_and_track = format!("{source_namespace}/{track_name}");
    tracing::info!("Reading {namespace_and_track} off the relay");

    let why_it_ended = drain_one_track(
        track_reader,
        how,
        &namespace_and_track,
        where_it_lands,
        stop,
    )
    .await;
    subscribe_task.abort();
    session_task.abort();
    why_it_ended
}

/// Hand every object the track carries to `where_it_lands`, group by group
/// in the order the groups opened.
///
/// Every group's reader is held until its stream ends: moq-transport removes
/// the whole subscription when an object arrives for a subgroup nobody reads,
/// so dropping a superseded group whose tail is still in flight would end the
/// link. Newer groups wait, buffered, behind the one being read.
async fn drain_one_track(
    track_reader: moq_transport::serve::TrackReader,
    how: &HowAnIngressReadsOffTheRelay,
    namespace_and_track: &str,
    where_it_lands: &WhereAnArrivingMoqObjectLands,
    stop: &AtomicBool,
) -> String {
    let mut subgroups = match track_reader.mode().await {
        Ok(moq_transport::serve::TrackReaderMode::Subgroups(subgroups)) => subgroups,
        Ok(_) => return format!("{namespace_and_track} is not published as subgroups"),
        Err(failure) => return format!("{namespace_and_track} ended: {failure}"),
    };
    let mut groups_waiting_their_turn: VecDeque<moq_transport::serve::SubgroupReader> =
        VecDeque::new();
    let mut group_being_read: Option<moq_transport::serve::SubgroupReader> = None;
    let mut said_why_an_object_did_not_open = false;
    let mut the_track_has_ended = false;
    loop {
        if stop.load(Ordering::Acquire) {
            return "the ingress stopped".to_string();
        }
        if group_being_read.is_none() {
            group_being_read = groups_waiting_their_turn.pop_front();
        }
        let next_step = match group_being_read.as_mut() {
            None if the_track_has_ended => return format!("{namespace_and_track} ended"),
            None => match subgroups.next().await {
                Ok(Some(opened)) => DrainStep::ANewGroupOpened(opened),
                Ok(None) => DrainStep::TheTrackEnded,
                Err(failure) => return format!("{namespace_and_track} ended: {failure}"),
            },
            // Groups first: moq-transport hands out only its newest, so one
            // not taken before the next opens is never seen.
            Some(group) => tokio::select! {
                biased;
                opened = subgroups.next(), if !the_track_has_ended => match opened {
                    Ok(Some(opened)) => DrainStep::ANewGroupOpened(opened),
                    Ok(None) => DrainStep::TheTrackEnded,
                    Err(failure) => return format!("{namespace_and_track} ended: {failure}"),
                },
                object = group.read_next() => match object {
                    Ok(Some(object)) => DrainStep::AnObjectArrived(object),
                    Ok(None) | Err(_) => DrainStep::TheGroupEnded,
                },
            },
        };
        match next_step {
            DrainStep::AnObjectArrived(object) => {
                match an_arriving_object_opened(how, namespace_and_track, &object) {
                    Ok(Some((attachment, payload))) => where_it_lands(attachment, payload),
                    Ok(None) => {}
                    Err(why_not) => {
                        if !said_why_an_object_did_not_open {
                            tracing::warn!(
                                "an object on {namespace_and_track} was dropped: {why_not}"
                            );
                            said_why_an_object_did_not_open = true;
                        }
                    }
                }
            }
            DrainStep::ANewGroupOpened(opened) => groups_waiting_their_turn.push_back(opened),
            DrainStep::TheGroupEnded => group_being_read = None,
            DrainStep::TheTrackEnded => the_track_has_ended = true,
        }
    }
}

/// One step of the drain, produced by the `select!` so neither arm mutates
/// what the other borrows.
enum DrainStep {
    ANewGroupOpened(moq_transport::serve::SubgroupReader),
    AnObjectArrived(bytes::Bytes),
    TheGroupEnded,
    /// No group will open again; the ones already open are still read out.
    TheTrackEnded,
}

/// The attachment and payload one arriving object carries, opening it first
/// when it is sealed.
fn an_arriving_object_opened(
    how: &HowAnIngressReadsOffTheRelay,
    namespace_and_track: &str,
    object: &[u8],
) -> Result<Option<(MeshDataMessageAttachment, Vec<u8>)>, String> {
    let opened;
    let plaintext = if a_moq_object_is_sealed(object) {
        let content_keys = how
            .doors
            .with_what_it_is_told(|told| told.every_key_of_a_track(namespace_and_track));
        opened = open_a_sealed_moq_object(&content_keys, object)
            .map_err(|why_not| why_not.to_string())?;
        opened.as_slice()
    } else {
        object
    };
    let (attachment, payload) = split_a_plaintext_moq_object(plaintext)
        .ok_or_else(|| "it is not a gateway object".to_string())?;
    Ok(Some((attachment, payload.to_vec())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::mesh::machine_clock_identity::MachineClockIdentity;
    use crate::core::runtime::mesh::mesh_data_message_attachment::PublisherGenerationOnTheMesh;
    use crate::core::runtime::mesh::moq_gateway_configuration::MoqGatewayDoors;
    use crate::core::runtime::mesh::moq_gateway_object_framing::a_plaintext_moq_object_carrying;
    use parking_lot::Mutex;

    fn an_object_numbered(sequence_number: u64) -> bytes::Bytes {
        bytes::Bytes::from(a_plaintext_moq_object_carrying(
            MeshDataMessageAttachment {
                timestamp_ns: 0,
                sequence_number,
                publisher_generation: PublisherGenerationOnTheMesh(0),
                clock_identity: MachineClockIdentity::of_this_machine(),
                frame_pixel_description_bytes: 0,
            },
            b"bag",
        ))
    }

    /// Through a far relay the next group's stream opens while the last
    /// group's tail is still arriving. The tail must still be taken — an
    /// object written for a subgroup nobody reads ends the subscription — and
    /// it lands ahead of the newer group's objects.
    #[test]
    fn a_superseded_groups_late_tail_lands_in_order_and_ends_nothing() {
        let landed: Arc<Mutex<Vec<u64>>> = Arc::default();
        let transport = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        transport.block_on(async {
            let namespace = moq_transport::coding::TrackNamespace::try_from("p/rt/1").unwrap();
            let (track_writer, track_reader) =
                moq_transport::serve::Track::new(namespace, "Source/out").produce();
            let mut subgroups = track_writer.subgroups().unwrap();
            let where_it_lands: WhereAnArrivingMoqObjectLands = {
                let landed = Arc::clone(&landed);
                Arc::new(move |attachment, _payload| landed.lock().push(attachment.sequence_number))
            };
            let how = HowAnIngressReadsOffTheRelay {
                doors: MoqGatewayDoors::told_nothing("reader"),
                where_the_source_gateway_publishes: Arc::new(|| None),
            };
            let stop = AtomicBool::new(false);
            let draining = drain_one_track(
                track_reader,
                &how,
                "p/rt/1/Source/out",
                &where_it_lands,
                &stop,
            );
            let publishing = async {
                let mut older = subgroups.append(127).unwrap();
                older.write(an_object_numbered(1)).unwrap();
                while landed.lock().is_empty() {
                    tokio::task::yield_now().await;
                }
                let mut newer = subgroups.append(127).unwrap();
                newer.write(an_object_numbered(3)).unwrap();
                // Long enough for the drain to take the newer group before
                // the older one's tail arrives, as a far relay delivers it.
                for _ in 0..100 {
                    tokio::task::yield_now().await;
                }
                older
                    .write(an_object_numbered(2))
                    .expect("the superseded group still has a reader");
                drop(older);
                drop(newer);
                tokio::task::yield_now().await;
                drop(subgroups);
            };
            let (why_it_ended, ()) = tokio::join!(draining, publishing);
            assert!(why_it_ended.ends_with("ended"), "{why_it_ended}");
        });
        assert_eq!(*landed.lock(), [1, 2, 3]);
    }
}

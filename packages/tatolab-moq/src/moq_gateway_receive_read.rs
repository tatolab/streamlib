// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A gateway's read of one track at its relay for one receive node: each object
//! and each run of given-up groups handed to the node over shared memory, with
//! the track's catalog entry whenever it changes.

use std::time::{Duration, Instant};

use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::ipc_threadsafe;
use streamlib_relay_track_reader::{
    RelayTrackGroupsReleasedInSequenceOrder, ReleasedRelayTrackGroup,
};

use crate::moq_catalog::the_catalog_entry_of_the_track_named;
use crate::moq_node_ipc::{
    MoqNodeSlicePublisher, MoqReceivedContentSampleKind,
    MoqTrackContentReceivedFromTheGatewayHeader, publish_slice,
};

/// How often a read sends the track's catalog entry again, so a receive node
/// whose sample of it was overwritten learns it at the next group.
const THE_CATALOG_ENTRY_IS_SENT_AGAIN_EVERY: Duration = Duration::from_secs(1);

/// What a receive node asked its gateway to read, each name exactly as its
/// publisher wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqTrackReceiveRequest {
    pub receive_node: String,
    pub broadcast: String,
    pub track: String,
    pub max_age: Duration,
}

/// The shared-memory end a read hands the receive node what it reads through.
pub struct MoqReceiveNodeContentSentByTheGateway {
    pub publisher: MoqNodeSlicePublisher<MoqTrackContentReceivedFromTheGatewayHeader>,
    pub notifier: Notifier<ipc_threadsafe::Service>,
    sequence_number: u64,
}

impl MoqReceiveNodeContentSentByTheGateway {
    pub fn new(
        publisher: MoqNodeSlicePublisher<MoqTrackContentReceivedFromTheGatewayHeader>,
        notifier: Notifier<ipc_threadsafe::Service>,
    ) -> Self {
        Self {
            publisher,
            notifier,
            sequence_number: 0,
        }
    }

    fn send(
        &mut self,
        kind: MoqReceivedContentSampleKind,
        groups_given_up: u64,
        object_timestamp_ns: u64,
        starts_group: bool,
        payload: &[u8],
    ) {
        let user_header = MoqTrackContentReceivedFromTheGatewayHeader {
            sequence_number: self.sequence_number,
            groups_given_up,
            object_timestamp_ns,
            kind: kind as u32,
            starts_group: u32::from(starts_group),
        };
        self.sequence_number += 1;
        if let Err(refusal) = publish_slice(&self.publisher, user_header, payload) {
            tracing::debug!("MoqGateway: a sample for a receive node was not sent: {refusal}");
            return;
        }
        let _ = self.notifier.notify();
    }

    fn send_the_catalog_entry(&mut self, catalog_entry: Option<&serde_json::Value>) {
        let payload = catalog_entry
            .map(|entry| entry.to_string().into_bytes())
            .unwrap_or_default();
        self.send(
            MoqReceivedContentSampleKind::CatalogEntry,
            0,
            0,
            false,
            &payload,
        );
    }
}

/// Read `request`'s track from `announced_broadcasts` into `content` until the
/// broadcast or the track ends; why it ended.
pub async fn read_a_track_for_a_receive_node(
    announced_broadcasts: moq_net::origin::Consumer,
    request: MoqTrackReceiveRequest,
    mut content: MoqReceiveNodeContentSentByTheGateway,
) -> String {
    let broadcast_path = request.broadcast.as_str();
    let broadcast = match announced_broadcasts.routed_broadcast(broadcast_path).await {
        Ok(broadcast) => broadcast,
        Err(refusal) => return format!("the broadcast `{broadcast_path}` was refused: {refusal}"),
    };
    let track = match broadcast.track(&request.track) {
        Ok(track) => track,
        Err(refusal) => return format!("the track `{}` was refused: {refusal}", request.track),
    };
    let mut groups_of_the_track =
        match RelayTrackGroupsReleasedInSequenceOrder::subscribe(&track, request.max_age).await {
            Ok(groups) => groups,
            Err(refusal) => {
                return format!("the track `{}` was refused: {refusal}", request.track);
            }
        };
    let mut catalog_subscription = Some(Box::pin(async {
        broadcast
            .track(hang::Catalog::DEFAULT_NAME)?
            .subscribe(None)
            .await
    }));
    let mut catalog_of_the_broadcast = None;
    tracing::info!(
        "MoqGateway: reading `{broadcast_path}` track `{}` for `{}`",
        request.track,
        request.receive_node
    );

    let mut catalog_entry_sent_last: Option<(Option<serde_json::Value>, Instant)> = None;
    let mut group_being_read: Option<moq_net::group::Consumer> = None;
    let mut the_next_object_starts_a_group = false;
    loop {
        tokio::select! {
            biased;
            subscribed = the_catalog_subscription_answer_of(&mut catalog_subscription) => {
                catalog_subscription = None;
                match subscribed {
                    Ok(catalog_subscriber) => {
                        catalog_of_the_broadcast = Some(moq_json::snapshot::Consumer::new(
                            catalog_subscriber,
                            moq_json::snapshot::consumer::Config::default(),
                        ));
                    }
                    Err(refusal) => {
                        tracing::debug!("MoqGateway: `{broadcast_path}` serves no catalog: {refusal}");
                    }
                }
            }
            catalog = the_next_catalog_of(&mut catalog_of_the_broadcast) => match catalog {
                Some(catalog) => {
                    let catalog_entry = the_catalog_entry_of_the_track_named(&catalog, &request.track);
                    if catalog_entry_sent_last.as_ref().map(|(sent, _)| sent) != Some(&catalog_entry) {
                        content.send_the_catalog_entry(catalog_entry.as_ref());
                        catalog_entry_sent_last = Some((catalog_entry, Instant::now()));
                    }
                }
                None => catalog_of_the_broadcast = None,
            },
            released = groups_of_the_track.next_released_group(), if group_being_read.is_none() => {
                match released {
                    Ok(Some(ReleasedRelayTrackGroup::InSequence(group))) => {
                        if let Some((catalog_entry, sent_at)) = &mut catalog_entry_sent_last
                            && sent_at.elapsed() >= THE_CATALOG_ENTRY_IS_SENT_AGAIN_EVERY
                        {
                            content.send_the_catalog_entry(catalog_entry.as_ref());
                            *sent_at = Instant::now();
                        }
                        group_being_read = Some(*group);
                        the_next_object_starts_a_group = true;
                    }
                    Ok(Some(ReleasedRelayTrackGroup::GivenUp { groups })) => {
                        content.send(MoqReceivedContentSampleKind::GroupsGivenUp, groups, 0, false, &[]);
                    }
                    Ok(None) => return format!("the track `{}` ended", request.track),
                    Err(refusal) => {
                        return format!("the track `{}` ended: {refusal}", request.track);
                    }
                }
            }
            object = the_next_object_of(&mut group_being_read) => match object {
                Ok(Some(object)) => {
                    let object_timestamp_ns =
                        u64::try_from(object.timestamp.as_nanos()).unwrap_or(u64::MAX);
                    content.send(
                        MoqReceivedContentSampleKind::Object,
                        0,
                        object_timestamp_ns,
                        std::mem::take(&mut the_next_object_starts_a_group),
                        &object.payload,
                    );
                }
                Ok(None) => group_being_read = None,
                Err(refusal) => {
                    tracing::debug!("MoqGateway: a group ended early: {refusal}");
                    group_being_read = None;
                    content.send(MoqReceivedContentSampleKind::GroupsGivenUp, 1, 0, false, &[]);
                }
            },
        }
    }
}

async fn the_catalog_subscription_answer_of<CatalogSubscription: Future + Unpin>(
    catalog_subscription: &mut Option<CatalogSubscription>,
) -> CatalogSubscription::Output {
    match catalog_subscription {
        Some(catalog_subscription) => catalog_subscription.await,
        None => std::future::pending().await,
    }
}

async fn the_next_catalog_of(
    catalog_of_the_broadcast: &mut Option<moq_json::snapshot::Consumer<serde_json::Value>>,
) -> Option<serde_json::Value> {
    let Some(catalog) = catalog_of_the_broadcast else {
        return std::future::pending().await;
    };
    catalog.next().await.ok().flatten()
}

async fn the_next_object_of(
    group_being_read: &mut Option<moq_net::group::Consumer>,
) -> moq_net::Result<Option<moq_net::frame::Frame>> {
    match group_being_read {
        Some(group) => group.read_frame().await,
        None => std::future::pending().await,
    }
}

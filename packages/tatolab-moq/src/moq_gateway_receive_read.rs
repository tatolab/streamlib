// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A gateway's read of one track at its relay for one receive node: each object
//! and each run of given-up groups handed to the node over shared memory, with
//! the track's catalog entry whenever it changes.

use std::time::Duration;

use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::ipc_threadsafe;
use streamlib_relay_track_reader::{
    RelayTrackGroupsReleasedInSequenceOrder, ReleasedRelayTrackGroup,
};

use crate::moq_catalog::the_catalog_entry_of_the_track_named;
use crate::moq_gateway_relay_settings::BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ;
use crate::moq_node_ipc::{
    MoqNodeSlicePublisher, MoqReceivedContentSampleKind,
    MoqTrackContentReceivedFromTheGatewayHeader, publish_slice,
};

/// What a receive node asked its gateway to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqTrackReceiveRequest {
    pub receive_node: String,
    pub broadcast: String,
    pub track: String,
    pub max_age: Duration,
}

impl MoqTrackReceiveRequest {
    /// The broadcast's path at the relay: its name with hang's catalog suffix,
    /// unless the name already carries it.
    pub fn broadcast_path(&self) -> String {
        if self
            .broadcast
            .ends_with(BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ)
        {
            self.broadcast.clone()
        } else {
            format!(
                "{}{BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ}",
                self.broadcast
            )
        }
    }
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
}

/// Read `request`'s track from `announced_broadcasts` into `content` until the
/// broadcast or the track ends; why it ended.
pub async fn read_a_track_for_a_receive_node(
    announced_broadcasts: moq_net::origin::Consumer,
    request: MoqTrackReceiveRequest,
    mut content: MoqReceiveNodeContentSentByTheGateway,
) -> String {
    let broadcast_path = request.broadcast_path();
    let broadcast = match announced_broadcasts
        .routed_broadcast(broadcast_path.as_str())
        .await
    {
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
    let mut catalog_of_the_broadcast = match async {
        broadcast
            .track(hang::Catalog::DEFAULT_NAME)?
            .subscribe(None)
            .await
    }
    .await
    {
        Ok(catalog_subscriber) => Some(moq_json::snapshot::Consumer::new(
            catalog_subscriber,
            moq_json::snapshot::consumer::Config::default(),
        )),
        Err(refusal) => {
            tracing::debug!("MoqGateway: `{broadcast_path}` serves no catalog: {refusal}");
            None
        }
    };
    tracing::info!(
        "MoqGateway: reading `{broadcast_path}` track `{}` for `{}`",
        request.track,
        request.receive_node
    );

    let mut catalog_entry_sent_last: Option<Option<serde_json::Value>> = None;
    let mut group_being_read: Option<moq_net::group::Consumer> = None;
    let mut the_next_object_starts_a_group = false;
    loop {
        tokio::select! {
            biased;
            catalog = the_next_catalog_of(&mut catalog_of_the_broadcast) => match catalog {
                Some(catalog) => {
                    let catalog_entry = the_catalog_entry_of_the_track_named(&catalog, &request.track);
                    if catalog_entry_sent_last.as_ref() != Some(&catalog_entry) {
                        let payload = catalog_entry
                            .as_ref()
                            .map(|entry| entry.to_string().into_bytes())
                            .unwrap_or_default();
                        content.send(MoqReceivedContentSampleKind::CatalogEntry, 0, 0, false, &payload);
                        catalog_entry_sent_last = Some(catalog_entry);
                    }
                }
                None => catalog_of_the_broadcast = None,
            },
            released = groups_of_the_track.next_released_group(), if group_being_read.is_none() => {
                match released {
                    Ok(Some(ReleasedRelayTrackGroup::InSequence(group))) => {
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
                }
            },
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn a_request_for(broadcast: &str) -> MoqTrackReceiveRequest {
        MoqTrackReceiveRequest {
            receive_node: "receive".into(),
            broadcast: broadcast.into(),
            track: "video".into(),
            max_age: Duration::from_millis(100),
        }
    }

    #[test]
    fn a_broadcast_is_read_with_hangs_suffix_once() {
        assert_eq!(a_request_for("studio").broadcast_path(), "studio.hang");
        assert_eq!(a_request_for("studio.hang").broadcast_path(), "studio.hang");
    }
}

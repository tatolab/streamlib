// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link into a setting read at setup is read by the engine, never by the setting's node: a
//! reader of the link's channel on a thread of its own hands each value the wire carries to the
//! stream's values carried into settings, which rebuild the node with it. However the two ends
//! are hosted, the node is handed the value as it is handed a value set on the setting.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use iceoryx2::port::listener::Listener;
use iceoryx2::prelude::ipc_threadsafe;
use parking_lot::Mutex;
use uuid::Uuid;

use super::{
    DestinationOfALinkAsItsWiringReadsIt, RoomForAWindowedReaderTheSourcesGraphCannotSee,
    notify_service_name_for, open_the_source_side_of_a_link,
};
use crate::core::NodeRunId;
use crate::core::descriptors::PinType;
use crate::core::error::{Error, Result};
use crate::core::graph::input_values::ValuesWiresCarryIntoSettings;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, InputLinkPortRef, LinkUniqueId, OutputLinkPortRef,
};
use crate::core::pin_value_packing::{PackedPinValueRef, unpack_pin_value};
use crate::iceoryx2::{
    BundleMembersAtTheRead, DeliveryProfile, Iceoryx2Node, InboundLinkName, InputMailboxesInner,
};
use streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION;

/// The longest a reader waits for its source to write before it looks whether it is to stop.
const A_READER_LOOKS_WHETHER_TO_STOP_AT_LEAST_EVERY: Duration = Duration::from_millis(50);

/// The engine's reader of a link into a setting read at setup, on the link while it is wired:
/// dropping it stops and joins its thread.
pub(crate) struct ReaderOfALinkIntoASettingComponent {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for ReaderOfALinkIntoASettingComponent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.lock().take()
            && thread.join().is_err()
        {
            tracing::warn!("the reader of a link into a setting ended in a panic");
        }
    }
}

/// Whether link `to`, read at `to` itself, ends at a setting its node reads at setup, which the
/// engine reads in its node's place.
pub(super) fn is_read_by_the_engine(graph: &Graph, to: &InputLinkPortRef) -> bool {
    graph
        .traversal()
        .v(to.node_run_id())
        .first()
        .is_some_and(|node| {
            node.input_port(to.port_name())
                .is_some_and(|port| port.is_a_setting_read_at_setup())
        })
}

/// The type the value a link from `from` into the setting `to` carries is read as: the
/// setting's own, else, on a setting that states none exactly, the output's.
fn the_type_its_values_are_read_as(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<PinType> {
    let pin_type_of = |node_run_id: &NodeRunId, port: &str, input: bool| {
        let node = graph.traversal().v(node_run_id).first()?;
        let port = if input {
            node.input_port(port)
        } else {
            node.output_port(port)
        }?;
        port.pin.as_ref().map(|pin| pin.pin_type.clone())
    };
    pin_type_of(to.node_run_id(), to.port_name(), true)
        .or_else(|| pin_type_of(from.node_run_id(), from.port_name(), false))
        .ok_or_else(|| {
            Error::InvalidLink(format!(
                "the link from `{from}` into setting `{to}` is not wired: neither end states a \
                 pin type, so the value it carries cannot be read"
            ))
        })
}

/// Open link `link_id`, from `from` into the setting `to` its node reads at setup, for the
/// engine to read: the source's side as any link's, and on this side a reader of the newest
/// message, which hands each value it reads to `graph`'s values carried into settings. The
/// setting is held as built with the value it reads with no wire, until the wire carries
/// another.
pub(super) fn open_a_link_into_a_setting_read_by_the_engine(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let pin_type = the_type_its_values_are_read_as(graph, from, to)?;
    let node_uuid = graph
        .traversal()
        .v(to.node_run_id())
        .first()
        .map(|node| node.node_uuid)
        .ok_or_else(|| Error::NodeNotFound(to.node_run_id().to_string()))?;
    let the_engines_reader = NodeRunId::new();
    let delivery = DeliveryProfile::Newest.resolve();
    let destination = DestinationOfALinkAsItsWiringReadsIt {
        notify_service_name: notify_service_name_for(&the_engines_reader),
        dest_proc_id: the_engines_reader,
        dest_port: to.port_name().to_string(),
        link_wiring_of_a_destination_out_of_process: None,
        audio_windowing: None,
        input_port_delivery: delivery,
        max_notifiers: MAX_INBOUND_LINKS_PER_DESTINATION,
        room_for_a_windowed_reader_joining_later:
            RoomForAWindowedReaderTheSourcesGraphCannotSee::NotNeeded,
        bundle_members_at_the_read: BundleMembersAtTheRead::default(),
    };
    let opened = open_the_source_side_of_a_link(graph, from, link_id, &destination, iceoryx2_node)?;
    let mailboxes = Arc::new(InputMailboxesInner::new());
    mailboxes.add_port(to.port_name(), delivery.depth, delivery.drain_order);
    mailboxes.add_channel_subscriber(
        to.port_name(),
        link_id.as_str(),
        &InboundLinkName::from(opened.channel_service_name.as_str()),
        opened
            .channel_data_service
            .create_subscriber(delivery.depth)?,
        BundleMembersAtTheRead::default(),
    );
    let wake = opened.destination_notify_service.create_listener()?;
    let carried = Arc::clone(graph.values_wires_carry_into_settings());
    if let Some(node) = graph.traversal().v(to.node_run_id()).first() {
        let read_with_no_wire =
            crate::core::graph::input_values::the_value_a_setting_reads_with_no_wire(
                graph,
                node,
                to.port_name(),
            );
        if let Some(value) = read_with_no_wire {
            carried.built_with_unless_its_wire_carried_one(node_uuid, to.port_name(), value);
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let read = TheReadOfALinkIntoASetting {
        mailboxes,
        wake,
        carried,
        node: node_uuid,
        setting: to.port_name().to_string(),
        pin_type,
        stop: Arc::clone(&stop),
    };
    let thread = std::thread::Builder::new()
        .name(format!("setting-wire-{}", to.port_name()))
        .spawn(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                read.until_it_is_stopped()
            }),
        )
        .map_err(|spawn_failure| {
            Error::Runtime(format!(
                "the reader of the link into setting `{to}` did not start: {spawn_failure}"
            ))
        })?;
    opened.send_each_kept_message_to_a_reader_it_missed();
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    link.insert_component_without_rendering_it(ReaderOfALinkIntoASettingComponent {
        stop,
        thread: Mutex::new(Some(thread)),
    });
    opened.hold_its_services_on_the_link_and_stamp_it(link, None);
    Ok(())
}

/// What one reader's thread holds: the mailbox its link's channel lands in, the wake its
/// source notifies, and where it hands each value.
struct TheReadOfALinkIntoASetting {
    mailboxes: Arc<InputMailboxesInner>,
    wake: Listener<ipc_threadsafe::Service>,
    carried: Arc<ValuesWiresCarryIntoSettings>,
    node: Uuid,
    setting: String,
    pin_type: PinType,
    stop: Arc<AtomicBool>,
}

impl TheReadOfALinkIntoASetting {
    /// Hand the newest value the link carried each time its source writes, until stopped. A
    /// message that does not read as the setting's type is logged once and passed over.
    fn until_it_is_stopped(self) {
        let mut a_message_was_passed_over = false;
        while !self.stop.load(Ordering::Acquire) {
            if let Err(wait_failure) = self
                .wake
                .timed_wait(|_| {}, A_READER_LOOKS_WHETHER_TO_STOP_AT_LEAST_EVERY)
            {
                tracing::warn!(
                    setting = %self.setting,
                    "the reader of a link into a setting stops: its wake failed: {wait_failure:?}"
                );
                return;
            }
            let newest = match self.the_newest_message() {
                Ok(newest) => newest,
                Err(read_failure) => {
                    tracing::warn!(
                        setting = %self.setting,
                        "the reader of a link into a setting stops: {read_failure}"
                    );
                    return;
                }
            };
            let Some((payload_descriptor, payload)) = newest else {
                continue;
            };
            match unpack_pin_value(
                &self.pin_type,
                PackedPinValueRef {
                    payload_descriptor: &payload_descriptor,
                    payload: &payload,
                },
            ) {
                Ok(value) => self.carried.carried(self.node, &self.setting, value),
                Err(why) if !a_message_was_passed_over => {
                    a_message_was_passed_over = true;
                    tracing::warn!(
                        setting = %self.setting,
                        "a message on the link into setting `{}` does not read as a {}, so it \
                         is passed over: {why}",
                        self.setting,
                        self.pin_type
                    );
                }
                Err(_) => {}
            }
        }
    }

    /// The newest message the link carried since the last read, its older ones passed over.
    fn the_newest_message(
        &self,
    ) -> Result<Option<(streamlib_ipc_types::DataChannelPayloadDescriptor, Vec<u8>)>> {
        let mut newest = None;
        while let Some(message) = self.mailboxes.read_raw(&self.setting)? {
            newest = Some((message.payload_descriptor, message.payload.to_vec()));
        }
        Ok(newest)
    }
}

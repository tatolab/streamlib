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
    DestinationOfALinkAsItsWiringReadsIt, OpenedSourceSideOfALink,
    RoomForAWindowedReaderTheSourcesGraphCannotSee, notify_service_name_for,
    open_the_source_side_of_a_link, release_the_source_side_of_a_link,
};
use crate::core::NodeRunId;
use crate::core::descriptors::PinType;
use crate::core::error::{Error, Result};
use crate::core::graph::input_values::{
    ValuesWiresCarryIntoSettings, the_value_a_setting_reads_with_no_wire,
};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, InputLinkPortRef, LinkUniqueId, OutputLinkPortRef,
};
use crate::core::pin_value_packing::{PackedPinValueRef, unpack_pin_value};
use crate::iceoryx2::{
    BundleMembersAtTheRead, DeliveryProfile, Iceoryx2Node, InboundLinkName, InputMailboxesInner,
    MessageReadFromInputPort,
};
use streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION;

/// The longest a reader waits for its source to write before it looks whether it is to stop.
const A_READER_LOOKS_WHETHER_TO_STOP_AT_LEAST_EVERY: Duration = Duration::from_millis(50);

/// The engine's reader of a link into a setting read at setup, on the link while it is wired:
/// dropping it stops and joins its thread.
pub(crate) struct ReaderOfALinkIntoASettingComponent {
    read: Arc<TheReadOfALinkIntoASetting>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ReaderOfALinkIntoASettingComponent {
    /// Hand over the newest value the link carried that its thread has not read yet, now.
    pub(crate) fn hand_over_what_it_carried(&self) {
        self.read.hand_over_the_newest_message();
    }
}

impl Drop for ReaderOfALinkIntoASettingComponent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the reader of a link into a setting ended in a panic");
        }
    }
}

/// The type the value a link from `from` into the setting `to` carries is read as: the
/// setting's own, else, on a setting that states none exactly, the output's.
fn the_type_its_values_are_read_as(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<PinType> {
    let the_settings = graph
        .traversal()
        .v(to.node_run_id())
        .first()
        .and_then(|node| node.input_port(to.port_name()))
        .and_then(|port| port.pin.as_ref());
    let the_outputs = || {
        graph
            .traversal()
            .v(from.node_run_id())
            .first()
            .and_then(|node| node.output_port(from.port_name()))
            .and_then(|port| port.pin.as_ref())
    };
    the_settings
        .or_else(the_outputs)
        .map(|pin| pin.pin_type.clone())
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
/// setting is held as built with the value it reads with no wire until the wire carries
/// another, and a value the source kept is handed over before this returns, so a node built
/// after it is built with it. The source's side is released again when the rest does not open.
pub(super) fn open_a_link_into_a_setting_read_by_the_engine(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let pin_type = the_type_its_values_are_read_as(graph, from, to)?;
    let (node_uuid, read_with_no_wire) = graph
        .traversal()
        .v(to.node_run_id())
        .first()
        .map(|node| {
            (
                node.node_uuid,
                the_value_a_setting_reads_with_no_wire(graph, node, to.port_name()),
            )
        })
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
    let carried = Arc::clone(graph.values_wires_carry_into_settings());
    if let Some(value) = read_with_no_wire {
        carried.built_with_unless_its_wire_carried_one(node_uuid, to.port_name(), value);
    }
    let reader = TheSettingAndWhereItsValuesGo {
        carried,
        node: node_uuid,
        setting: to.port_name().to_string(),
        pin_type,
    };
    let opened_reader = match start_the_reader(&opened, link_id, delivery, reader) {
        Ok(opened_reader) => opened_reader,
        Err(not_started) => {
            release_the_source_side_of_a_link(graph, from.node_run_id(), from.port_name(), link_id);
            return Err(not_started);
        }
    };
    opened.send_each_kept_message_to_a_reader_it_missed();
    opened_reader.hand_over_what_it_carried();
    let Some(link) = graph.traversal_mut().e(link_id).first_mut() else {
        drop(opened_reader);
        release_the_source_side_of_a_link(graph, from.node_run_id(), from.port_name(), link_id);
        return Err(Error::LinkNotFound(link_id.to_string()));
    };
    link.insert_component_without_rendering_it(opened_reader);
    opened.hold_its_services_on_the_link_and_stamp_it(link, None);
    Ok(())
}

/// Subscribe to the channel `opened` holds for link `link_id`, and start the thread that reads
/// it for `reader`.
fn start_the_reader(
    opened: &OpenedSourceSideOfALink,
    link_id: &LinkUniqueId,
    delivery: crate::iceoryx2::DeliveryResolution,
    reader: TheSettingAndWhereItsValuesGo,
) -> Result<ReaderOfALinkIntoASettingComponent> {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port(&reader.setting, delivery.depth, delivery.drain_order);
    mailboxes.add_channel_subscriber(
        &reader.setting,
        link_id.as_str(),
        &InboundLinkName::from(opened.channel_service_name.as_str()),
        opened
            .channel_data_service
            .create_subscriber(delivery.depth)?,
        BundleMembersAtTheRead::default(),
    );
    let wake = opened.destination_notify_service.create_listener()?;
    let read = Arc::new(TheReadOfALinkIntoASetting {
        mailboxes,
        reader,
        a_message_was_passed_over: AtomicBool::new(false),
        handing_over: Mutex::new(()),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let read = Arc::clone(&read);
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name(format!("setting-wire-{}", read.reader.setting))
            .spawn(
                crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                    read.until_it_is_stopped(&wake, &stop)
                }),
            )
            .map_err(|spawn_failure| {
                Error::Runtime(format!(
                    "the reader of the link `{link_id}` into a setting did not start: \
                     {spawn_failure}"
                ))
            })?
    };
    Ok(ReaderOfALinkIntoASettingComponent {
        read,
        stop,
        thread: Some(thread),
    })
}

/// The setting one reader feeds, the type its values are read as, and where each goes.
struct TheSettingAndWhereItsValuesGo {
    carried: Arc<ValuesWiresCarryIntoSettings>,
    node: Uuid,
    setting: String,
    pin_type: PinType,
}

/// What one reader holds: the mailbox its link's channel lands in, and the setting it feeds.
struct TheReadOfALinkIntoASetting {
    mailboxes: InputMailboxesInner,
    reader: TheSettingAndWhereItsValuesGo,
    a_message_was_passed_over: AtomicBool,
    /// Held across each hand-over, by the reader's thread or by a node about to be built.
    handing_over: Mutex<()>,
}

impl TheReadOfALinkIntoASetting {
    /// Hand over the newest value the link carried each time its source writes, until `stop`.
    fn until_it_is_stopped(&self, wake: &Listener<ipc_threadsafe::Service>, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            if let Err(wait_failure) =
                wake.timed_wait(|_| {}, A_READER_LOOKS_WHETHER_TO_STOP_AT_LEAST_EVERY)
            {
                tracing::warn!(
                    setting = %self.reader.setting,
                    "the reader of a link into a setting stops: its wake failed: {wait_failure:?}"
                );
                return;
            }
            self.hand_over_the_newest_message();
        }
    }

    /// Hand over the newest message the link carried since the last read, its older ones
    /// passed over, one hand-over at a time, so a node built once one returns is built with
    /// what it read. A message that does not read as the setting's type is logged once.
    fn hand_over_the_newest_message(&self) {
        let _one_hand_over_at_a_time = self.handing_over.lock();
        let newest = match self.the_newest_message() {
            Ok(Some(newest)) => newest,
            Ok(None) => return,
            Err(read_failure) => {
                tracing::warn!(
                    setting = %self.reader.setting,
                    "the reader of a link into a setting read nothing: {read_failure}"
                );
                return;
            }
        };
        let reader = &self.reader;
        match unpack_pin_value(
            &reader.pin_type,
            PackedPinValueRef {
                payload_descriptor: &newest.payload_descriptor,
                payload: &newest.payload,
            },
        ) {
            Ok(value) => reader.carried.carried(reader.node, &reader.setting, value),
            Err(why) if !self.a_message_was_passed_over.swap(true, Ordering::AcqRel) => {
                tracing::warn!(
                    setting = %reader.setting,
                    "a message on the link into setting `{}` does not read as a {}, so it is \
                     passed over: {why}",
                    reader.setting,
                    reader.pin_type
                );
            }
            Err(_) => {}
        }
    }

    /// The newest message the link carried since the last read, its older ones passed over.
    fn the_newest_message(&self) -> Result<Option<MessageReadFromInputPort>> {
        let mut newest = None;
        while let Some(message) = self.mailboxes.read_raw(&self.reader.setting)? {
            newest = Some(message);
        }
        Ok(newest)
    }
}

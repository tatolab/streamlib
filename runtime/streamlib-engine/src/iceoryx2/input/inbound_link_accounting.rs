// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use crate::core::error::{Error, Result};
use crate::iceoryx2::audio_window::AudioWindowStageFlush;
use crate::iceoryx2::channel_name::InboundLinkName;
use crate::iceoryx2::helper_process_loss_count_board::InboundLinkLossCountBoardSlotMirror;
use crate::iceoryx2::input_port_message_payload::SamplesOneInboundLinkHolds;
use crate::iceoryx2::loss_counters::{
    DiscardedSampleCountsByInboundLink, DroppedBagCountsByInboundLink,
};

use super::{
    ChannelDataServiceSubscriber, InputMailboxesInner, PortBoundSubscriber, WindowedPortInboundLink,
};

impl InputMailboxesInner {
    /// Whether any channel subscriber has been configured yet.
    pub fn has_subscribers(&self) -> bool {
        !self
            .inbound_link_subscribers_and_listener
            .lock()
            .subscribers
            .is_empty()
    }

    /// Bind an iceoryx2 channel Subscriber to the local input port it feeds.
    ///
    /// One call per inbound `connect()` link — a destination consuming N
    /// channels holds N subscribers. The receive path routes every frame a
    /// subscriber delivers into `local_port`'s mailbox (binding-based routing;
    /// see [`PortBoundSubscriber`]).
    ///
    /// Bind after adding the port: a link into a windowed port is given its
    /// discarded-sample count here, from the port it finds.
    pub fn add_channel_subscriber(
        &self,
        local_port: &str,
        link_id: &str,
        inbound_link_name: &InboundLinkName,
        subscriber: ChannelDataServiceSubscriber,
    ) {
        let dropped_bag_counter = self.dropped_bag_counts.counter_for_inbound_link(link_id);
        let into_a_windowed_port = self
            .ports
            .lock()
            .get(local_port)
            .is_some_and(|port| port.audio_windowing.is_windowed());
        let discarded_sample_counter = into_a_windowed_port.then(|| {
            self.discarded_sample_counts
                .counter_for_inbound_link(link_id)
        });
        self.inbound_link_subscribers_and_listener
            .lock()
            .subscribers
            .push(PortBoundSubscriber {
                link_id: link_id.to_string(),
                local_port: local_port.to_string(),
                inbound_link_name: inbound_link_name.clone(),
                subscriber,
                dropped_bag_counter,
                discarded_sample_counter,
                bags_the_ring_overwrote: Default::default(),
                samples_this_link_holds: SamplesOneInboundLinkHolds::default(),
            });
    }

    /// Write every new total of `link_id`'s dropped-bag, taken-bag and
    /// discarded-sample counts onto `slot`, as the counts move.
    ///
    /// How a helper process's counts reach its parent. Refused for a link no
    /// subscriber is bound for, and for one already mirrored.
    pub fn mirror_an_inbound_links_loss_counts_into(
        &self,
        link_id: &str,
        inbound_link_board_slot_mirror: InboundLinkLossCountBoardSlotMirror,
    ) -> Result<()> {
        let (dropped_bag_counter, discarded_sample_counter) = self
            .inbound_link_subscribers_and_listener
            .lock()
            .subscribers
            .iter()
            .find(|bound| bound.link_id == link_id)
            .map(|bound| {
                (
                    bound.dropped_bag_counter.clone(),
                    bound.discarded_sample_counter.clone(),
                )
            })
            .ok_or_else(|| {
                Error::Link(format!(
                    "inbound link '{link_id}' has no subscriber bound, so there are no loss \
                     counts of it to mirror"
                ))
            })?;
        let already_mirrored = || {
            Error::Link(format!(
                "inbound link '{link_id}' already mirrors its loss counts onto a board slot"
            ))
        };
        let dropped_bags_board_slot_mirror = inbound_link_board_slot_mirror.clone();
        dropped_bag_counter
            .mirror_every_new_total_into(Box::new(move |dropped_bags| {
                dropped_bags_board_slot_mirror.mirror_dropped_bags(dropped_bags)
            }))
            .map_err(|_| already_mirrored())?;
        let bags_off_the_mailbox_board_slot_mirror = inbound_link_board_slot_mirror.clone();
        let bags_counted_lost_after_taking_board_slot_mirror =
            inbound_link_board_slot_mirror.clone();
        dropped_bag_counter
            .mirror_every_new_taken_bag_total_into(
                Box::new(move |bags_off_the_mailbox| {
                    bags_off_the_mailbox_board_slot_mirror
                        .mirror_bags_off_the_mailbox_for_its_reader(bags_off_the_mailbox)
                }),
                Box::new(move |bags_counted_lost_after_taking| {
                    bags_counted_lost_after_taking_board_slot_mirror
                        .mirror_bags_its_reader_counted_lost_after_taking_them(
                            bags_counted_lost_after_taking,
                        )
                }),
            )
            .map_err(|_| already_mirrored())?;
        if let Some(discarded_sample_counter) = discarded_sample_counter {
            discarded_sample_counter
                .mirror_every_new_total_into(Box::new(move |discarded_samples| {
                    inbound_link_board_slot_mirror.mirror_discarded_samples(discarded_samples)
                }))
                .map_err(|_| already_mirrored())?;
        }
        Ok(())
    }

    /// Every inbound link feeding `port`, in wiring order.
    ///
    /// Readable from `setup()`: a port's mailbox exists only once a link is
    /// wired into it, and WIRE runs before `setup()`, so a destination asking
    /// here learns how many producers it owes before the first bag arrives. A
    /// port with no links lists none rather than refusing — an unconnected
    /// input is a legal graph, not an error.
    pub fn inbound_link_names(&self, port: &str) -> Vec<InboundLinkName> {
        self.inbound_link_subscribers_and_listener
            .lock()
            .bound_to_local_port(port)
            .map(|bound| bound.inbound_link_name.clone())
            .collect()
    }

    /// Count a windowed port's flush on the link feeding it, and say so.
    ///
    /// A port no single link feeds — a manually injected frame's — has nowhere
    /// to count it, and still says what went.
    pub(super) fn count_a_flush_on_the_link_feeding(
        port: &str,
        feeding_link: Option<&WindowedPortInboundLink>,
        flush: AudioWindowStageFlush,
    ) {
        let AudioWindowStageFlush {
            cause,
            discarded_per_channel_samples_at_the_declared_rate: discarded_samples,
        } = flush;
        if let Some(counter) =
            feeding_link.and_then(|feeding| feeding.discarded_sample_counter.as_ref())
        {
            counter.record_discarded_samples(discarded_samples);
        }
        tracing::warn!(
            port,
            link = feeding_link.map(|feeding| feeding.link_id.as_str()),
            channel = feeding_link.map(|feeding| feeding.inbound_link_name.as_str()),
            discarded_samples,
            "audio window stage: {cause}, so the accumulator and the resampler's filter state \
             were flushed, discarding {discarded_samples} samples no reader had received"
        );
    }

    /// This processor's per-inbound-link dropped-bag counts, shared with the
    /// graph node's [`ProcessorMetrics`] so `graph` reads them live.
    ///
    /// [`ProcessorMetrics`]: crate::core::graph::ProcessorMetrics
    pub fn dropped_bag_counts_by_inbound_link(&self) -> Arc<DroppedBagCountsByInboundLink> {
        Arc::clone(&self.dropped_bag_counts)
    }

    /// This processor's per-inbound-link counts of samples its windowed ports'
    /// flushes discarded, shared with the graph node's [`ProcessorMetrics`].
    ///
    /// [`ProcessorMetrics`]: crate::core::graph::ProcessorMetrics
    pub fn discarded_sample_counts_by_inbound_link(
        &self,
    ) -> Arc<DiscardedSampleCountsByInboundLink> {
        Arc::clone(&self.discarded_sample_counts)
    }

    /// Reclaim the destination-side ports for one disconnected `connect()` link.
    ///
    /// Drops the `link_id`-tagged subscriber; when its local input port has no
    /// remaining subscribers the mailbox is removed, and when the destination has
    /// none at all the shared listener is dropped, so this destination holds
    /// nothing against a channel or notify service no link uses.
    pub fn remove_channel_link(&self, link_id: &str) {
        let mut subscribers_and_listener = self.inbound_link_subscribers_and_listener.lock();
        let Some(local_port) = subscribers_and_listener.remove_by_link(link_id) else {
            return;
        };
        self.dropped_bag_counts.forget_inbound_link(link_id);
        self.discarded_sample_counts.forget_inbound_link(link_id);
        if !subscribers_and_listener.local_port_still_bound(&local_port) {
            self.ports.lock().remove(&local_port);
        }
        if subscribers_and_listener.subscribers.is_empty() {
            subscribers_and_listener.listener = None;
        }
    }
}

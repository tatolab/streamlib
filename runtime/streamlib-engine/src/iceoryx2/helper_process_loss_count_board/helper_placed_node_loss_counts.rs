// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;

use parking_lot::Mutex;
use streamlib_ipc_types::{
    HelperProcessFunctionNodeCallBreadcrumbBoardEntry, HelperProcessFunctionNodeOverrunBoardEntry,
    HelperProcessFunctionNodeStateBoardEntry, HelperProcessStepProgressBoardEntry,
    InboundLinkLossCountBoardSlot,
};

use crate::core::error::{Error, Result};

use super::{
    HelperProcessLossCountBoard, INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD,
    InboundLinkLossCountBoardSlotAndWiringGeneration,
};

// =============================================================================
// Which slot each link was given
// =============================================================================

/// One inbound link's slot on its helper's board, and the wiring it was given
/// for.
#[derive(Clone)]
struct InboundLinkLossCountSlotAssignment {
    link_id: String,
    wiring_generation: u64,
    into_a_windowed_port: bool,
}

/// An output port with at least one outbound link, and the generation its
/// channel was given when the first of them was noted.
struct OutputPortWithOutboundLinks {
    outbound_link_ids: Vec<String>,
    wiring_generation: u64,
}

struct HelperPlacedNodeLossCountWiring {
    inbound_link_slots: Vec<Option<InboundLinkLossCountSlotAssignment>>,
    last_assigned_wiring_generation: u64,
    output_ports_with_outbound_links: BTreeMap<String, OutputPortWithOutboundLinks>,
}

impl Default for HelperPlacedNodeLossCountWiring {
    fn default() -> Self {
        Self {
            inbound_link_slots: vec![None; INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD],
            last_assigned_wiring_generation: 0,
            output_ports_with_outbound_links: BTreeMap::new(),
        }
    }
}

impl HelperPlacedNodeLossCountWiring {
    fn next_wiring_generation(&mut self) -> u64 {
        self.last_assigned_wiring_generation += 1;
        self.last_assigned_wiring_generation
    }

    fn release_inbound_link_slot(&mut self, link_id: &str) {
        for slot in &mut self.inbound_link_slots {
            if slot
                .as_ref()
                .is_some_and(|assignment| assignment.link_id == link_id)
            {
                *slot = None;
            }
        }
    }

    /// Each assigned slot with the counts written on it for its own wiring:
    /// zero before the board exists, before the helper claimed the slot, and
    /// after a write from a wiring the slot has moved past.
    fn assigned_slots_with_their_wirings_counts<'a>(
        &'a self,
        board: Option<&'a HelperProcessLossCountBoard>,
    ) -> impl Iterator<
        Item = (
            &'a InboundLinkLossCountSlotAssignment,
            InboundLinkLossCountBoardSlot,
        ),
    > {
        self.inbound_link_slots
            .iter()
            .enumerate()
            .filter_map(move |(slot, assignment)| {
                let assignment = assignment.as_ref()?;
                let this_wirings_counts = board
                    .and_then(|board| board.inbound_link_slot(slot))
                    .filter(|written| written.wiring_generation == assignment.wiring_generation)
                    .unwrap_or_default();
                Some((assignment, this_wirings_counts))
            })
    }
}

#[derive(Default)]
pub struct HelperPlacedNodeLossCounts {
    wiring: Mutex<HelperPlacedNodeLossCountWiring>,
    board: Mutex<Option<HelperProcessLossCountBoard>>,
    counts_earlier_spawns_left: Mutex<NodeLossCountSnapshot>,
}

impl std::fmt::Debug for HelperPlacedNodeLossCounts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HelperPlacedNodeLossCounts")
            .field(
                "board",
                &self
                    .board
                    .lock()
                    .as_ref()
                    .map(HelperProcessLossCountBoard::service_name),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct NodeLossCountSnapshot {
    /// Bags lost per inbound link.
    pub dropped_bags_by_inbound_link: BTreeMap<String, u64>,
    /// Bags taken per inbound link without being counted lost: read, or passed
    /// over by a port that reads only the newest.
    pub taken_bags_by_inbound_link: BTreeMap<String, u64>,
    /// Samples discarded per inbound link into a windowed port, and no other.
    pub discarded_samples_by_inbound_link: BTreeMap<String, u64>,
    /// Bags refused at the ceiling per output port with a channel.
    pub refused_bags_by_output_port: BTreeMap<String, u64>,
    /// Bags sent per output port with a channel — what the losses downstream
    /// are counted against.
    pub written_bags_by_output_port: BTreeMap<String, u64>,
    /// Groups given up upstream per output port whose writer gave one up, and
    /// no other.
    pub given_up_groups_by_output_port: BTreeMap<String, u64>,
}

impl NodeLossCountSnapshot {
    fn add(&mut self, counts_to_add: &NodeLossCountSnapshot) {
        fn add_each(totals: &mut BTreeMap<String, u64>, counts_to_add: &BTreeMap<String, u64>) {
            for (name, count) in counts_to_add {
                if let Some(total) = totals.get_mut(name) {
                    *total += count;
                } else {
                    totals.insert(name.clone(), *count);
                }
            }
        }
        add_each(
            &mut self.dropped_bags_by_inbound_link,
            &counts_to_add.dropped_bags_by_inbound_link,
        );
        add_each(
            &mut self.taken_bags_by_inbound_link,
            &counts_to_add.taken_bags_by_inbound_link,
        );
        add_each(
            &mut self.discarded_samples_by_inbound_link,
            &counts_to_add.discarded_samples_by_inbound_link,
        );
        add_each(
            &mut self.refused_bags_by_output_port,
            &counts_to_add.refused_bags_by_output_port,
        );
        add_each(
            &mut self.written_bags_by_output_port,
            &counts_to_add.written_bags_by_output_port,
        );
        add_each(
            &mut self.given_up_groups_by_output_port,
            &counts_to_add.given_up_groups_by_output_port,
        );
    }

    /// Drop every count whose link or output port `wiring` no longer holds, so
    /// a reconnect starts from zero.
    fn forget_every_count_not_in(&mut self, wiring: &HelperPlacedNodeLossCountWiring) {
        let inbound_link_is_wired = |link_id: &String| {
            wiring
                .inbound_link_slots
                .iter()
                .flatten()
                .any(|assignment| &assignment.link_id == link_id)
        };
        let output_port_is_linked = |output_port: &String| {
            wiring
                .output_ports_with_outbound_links
                .contains_key(output_port)
        };
        self.dropped_bags_by_inbound_link
            .retain(|link_id, _| inbound_link_is_wired(link_id));
        self.taken_bags_by_inbound_link
            .retain(|link_id, _| inbound_link_is_wired(link_id));
        self.discarded_samples_by_inbound_link
            .retain(|link_id, _| inbound_link_is_wired(link_id));
        self.refused_bags_by_output_port
            .retain(|output_port, _| output_port_is_linked(output_port));
        self.written_bags_by_output_port
            .retain(|output_port, _| output_port_is_linked(output_port));
        self.given_up_groups_by_output_port
            .retain(|output_port, _| output_port_is_linked(output_port));
    }
}

impl HelperPlacedNodeLossCounts {
    /// Give `link_id` the lowest free slot and a generation no wiring has had.
    ///
    /// A link already holding a slot gives it up first, so its counts start
    /// again from zero.
    pub(crate) fn assign_inbound_link_slot(
        &self,
        link_id: &str,
        into_a_windowed_port: bool,
    ) -> Result<InboundLinkLossCountBoardSlotAndWiringGeneration> {
        let mut wiring = self.wiring.lock();
        wiring.release_inbound_link_slot(link_id);
        self.counts_earlier_spawns_left
            .lock()
            .forget_every_count_not_in(&wiring);
        let Some(slot) = wiring.inbound_link_slots.iter().position(Option::is_none) else {
            return Err(Error::Configuration(format!(
                "link '{link_id}' would be a helper-placed node's inbound link past the \
                 {INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD} its loss-count board has slots for"
            )));
        };
        let wiring_generation = wiring.next_wiring_generation();
        wiring.inbound_link_slots[slot] = Some(InboundLinkLossCountSlotAssignment {
            link_id: link_id.to_string(),
            wiring_generation,
            into_a_windowed_port,
        });
        Ok(InboundLinkLossCountBoardSlotAndWiringGeneration {
            slot,
            wiring_generation,
        })
    }

    /// Note an outbound link out of `output_port`, and hand back the generation
    /// of the port's channel: a new one when the port had no link, the one it
    /// already has otherwise.
    pub(crate) fn note_outbound_link(&self, output_port: &str, link_id: &str) -> u64 {
        let mut wiring = self.wiring.lock();
        if let Some(port) = wiring.output_ports_with_outbound_links.get_mut(output_port) {
            if !port.outbound_link_ids.iter().any(|noted| noted == link_id) {
                port.outbound_link_ids.push(link_id.to_string());
            }
            return port.wiring_generation;
        }
        let wiring_generation = wiring.next_wiring_generation();
        self.counts_earlier_spawns_left
            .lock()
            .forget_every_count_not_in(&wiring);
        wiring.output_ports_with_outbound_links.insert(
            output_port.to_string(),
            OutputPortWithOutboundLinks {
                outbound_link_ids: vec![link_id.to_string()],
                wiring_generation,
            },
        );
        wiring_generation
    }

    /// Forget a disconnected link in both directions: its slot goes free, and
    /// an output port it was the last link of stops rendering and takes a new
    /// generation when it is linked again.
    pub(crate) fn forget_link(&self, link_id: &str) {
        let mut wiring = self.wiring.lock();
        wiring.release_inbound_link_slot(link_id);
        wiring.output_ports_with_outbound_links.retain(|_, port| {
            port.outbound_link_ids.retain(|noted| noted != link_id);
            !port.outbound_link_ids.is_empty()
        });
        let mut counts_earlier_spawns_left = self.counts_earlier_spawns_left.lock();
        counts_earlier_spawns_left.forget_every_count_not_in(&wiring);
    }

    /// Hold the board this spawn's helper writes on, keeping what the board of
    /// a helper before it last held so each wiring's counts stay cumulative
    /// across a restart.
    pub(crate) fn hold_the_board_of_this_spawn(&self, board: HelperProcessLossCountBoard) {
        let wiring = self.wiring.lock();
        let mut held_board = self.board.lock();
        if let Some(earlier_spawns_board) = held_board.take() {
            let counts_the_earlier_board_left =
                Self::loss_count_snapshot_read_off(&wiring, Some(&earlier_spawns_board));
            self.counts_earlier_spawns_left
                .lock()
                .add(&counts_the_earlier_board_left);
        }
        *held_board = Some(board);
    }

    /// Every count as it stands, each read only for the wiring it belongs to,
    /// with what earlier spawns' boards left for that wiring added.
    pub fn loss_count_snapshot(&self) -> NodeLossCountSnapshot {
        let wiring = self.wiring.lock();
        let held_board = self.board.lock();
        let mut snapshot = Self::loss_count_snapshot_read_off(&wiring, held_board.as_ref());
        snapshot.add(&self.counts_earlier_spawns_left.lock());
        snapshot
    }

    fn loss_count_snapshot_read_off(
        wiring: &HelperPlacedNodeLossCountWiring,
        board: Option<&HelperProcessLossCountBoard>,
    ) -> NodeLossCountSnapshot {
        let mut snapshot = NodeLossCountSnapshot::default();
        for (assignment, this_wirings_counts) in
            wiring.assigned_slots_with_their_wirings_counts(board)
        {
            snapshot
                .dropped_bags_by_inbound_link
                .insert(assignment.link_id.clone(), this_wirings_counts.dropped_bags);
            snapshot.taken_bags_by_inbound_link.insert(
                assignment.link_id.clone(),
                this_wirings_counts
                    .bags_off_the_mailbox_for_its_reader
                    .saturating_sub(
                        this_wirings_counts.bags_its_reader_counted_lost_after_taking_them,
                    ),
            );
            if assignment.into_a_windowed_port {
                snapshot.discarded_samples_by_inbound_link.insert(
                    assignment.link_id.clone(),
                    this_wirings_counts.discarded_samples,
                );
            }
        }
        for (output_port, port) in &wiring.output_ports_with_outbound_links {
            let this_channels_entry = board
                .and_then(|board| board.output_port_entry(output_port))
                .filter(|written| written.wiring_generation == port.wiring_generation)
                .unwrap_or_default();
            snapshot
                .refused_bags_by_output_port
                .insert(output_port.clone(), this_channels_entry.refused_bags);
            snapshot
                .written_bags_by_output_port
                .insert(output_port.clone(), this_channels_entry.written_bags);
        }
        snapshot
    }

    /// What the current spawn's helper last wrote of its node's step progress;
    /// `None` before any spawn's board exists.
    pub fn step_progress_its_helper_wrote(&self) -> Option<HelperProcessStepProgressBoardEntry> {
        self.board
            .lock()
            .as_ref()
            .map(HelperProcessLossCountBoard::step_progress_entry)
    }

    /// What the current spawn's helper last wrote on its function-node overrun
    /// entry; `None` before any spawn's board exists.
    pub fn function_node_overrun_its_helper_wrote(
        &self,
    ) -> Option<HelperProcessFunctionNodeOverrunBoardEntry> {
        self.board
            .lock()
            .as_ref()
            .map(HelperProcessLossCountBoard::function_node_overrun_entry)
    }

    /// What the current spawn's helper last wrote of the function-node call it
    /// has under way, read off the board it wrote even after it died; `None`
    /// before any spawn's board exists.
    pub fn function_node_call_breadcrumb_its_helper_wrote(
        &self,
    ) -> Option<HelperProcessFunctionNodeCallBreadcrumbBoardEntry> {
        self.board
            .lock()
            .as_ref()
            .map(HelperProcessLossCountBoard::function_node_call_breadcrumb_entry)
    }

    /// What the current spawn's helper last wrote of the state of the function
    /// node numbered `function_node_breadcrumb_number`, read off the board it
    /// wrote even after it died; `None` before any spawn's board exists or for
    /// a node the board carries no state entry for.
    pub fn function_node_state_its_helper_wrote(
        &self,
        function_node_breadcrumb_number: u32,
    ) -> Option<HelperProcessFunctionNodeStateBoardEntry> {
        self.board
            .lock()
            .as_ref()?
            .function_node_state_entry(function_node_breadcrumb_number)
    }

    pub fn total_dropped_bag_count(&self) -> u64 {
        self.loss_count_snapshot()
            .dropped_bags_by_inbound_link
            .values()
            .sum()
    }
}

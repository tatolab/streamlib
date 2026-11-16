// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use iceoryx2::prelude::*;

/// The board section a [`HelperProcessLossCountBoardKey`] names an inbound-link slot in.
const HELPER_PROCESS_LOSS_COUNT_BOARD_INBOUND_LINK_SLOT_SECTION: u32 = 0;

/// The board section a [`HelperProcessLossCountBoardKey`] names an output port in.
const HELPER_PROCESS_LOSS_COUNT_BOARD_OUTPUT_PORT_SECTION: u32 = 1;

/// The board section holding the helper's one step-progress entry.
const HELPER_PROCESS_LOSS_COUNT_BOARD_STEP_PROGRESS_SECTION: u32 = 2;

/// The board section holding the helper's one function-node call breadcrumb.
const HELPER_PROCESS_LOSS_COUNT_BOARD_FUNCTION_NODE_CALL_BREADCRUMB_SECTION: u32 = 3;

/// The board section holding one state entry per stateful function node the
/// helper calls, by the node's breadcrumb number.
const HELPER_PROCESS_LOSS_COUNT_BOARD_FUNCTION_NODE_STATE_SECTION: u32 = 4;

/// The board section holding the helper's one function-node overrun entry.
const HELPER_PROCESS_LOSS_COUNT_BOARD_FUNCTION_NODE_OVERRUN_SECTION: u32 = 5;

/// The key of one entry on a helper process's loss-count blackboard: an
/// inbound-link slot, a declared output port by its index, the helper's step
/// progress, its function-node call breadcrumb, the state of one stateful
/// function node it calls, or the function node that overran its frame
/// interval.
///
/// The iceoryx2 type name is pinned for the reason
/// [`DataChannelSampleUserHeader`](crate::DataChannelSampleUserHeader)'s is: the parent creates the board
/// and the helper opens it, and both must present the same key type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ZeroCopySend)]
#[type_name("HelperProcessLossCountBoardKey")]
#[repr(C)]
pub struct HelperProcessLossCountBoardKey {
    section: u32,
    index_within_section: u32,
}

impl HelperProcessLossCountBoardKey {
    /// The key of inbound-link slot `slot`.
    pub const fn inbound_link_slot(slot: u32) -> Self {
        Self {
            section: HELPER_PROCESS_LOSS_COUNT_BOARD_INBOUND_LINK_SLOT_SECTION,
            index_within_section: slot,
        }
    }

    /// The key of the declared output port at `output_port_index`.
    pub const fn output_port(output_port_index: u32) -> Self {
        Self {
            section: HELPER_PROCESS_LOSS_COUNT_BOARD_OUTPUT_PORT_SECTION,
            index_within_section: output_port_index,
        }
    }

    /// The key of the helper's step-progress entry.
    pub const STEP_PROGRESS: Self = Self {
        section: HELPER_PROCESS_LOSS_COUNT_BOARD_STEP_PROGRESS_SECTION,
        index_within_section: 0,
    };

    /// The key of the helper's function-node call breadcrumb.
    pub const FUNCTION_NODE_CALL_BREADCRUMB: Self = Self {
        section: HELPER_PROCESS_LOSS_COUNT_BOARD_FUNCTION_NODE_CALL_BREADCRUMB_SECTION,
        index_within_section: 0,
    };

    /// The key of the helper's function-node overrun entry.
    pub const FUNCTION_NODE_OVERRUN: Self = Self {
        section: HELPER_PROCESS_LOSS_COUNT_BOARD_FUNCTION_NODE_OVERRUN_SECTION,
        index_within_section: 0,
    };

    /// The key of the state entry of the stateful function node numbered
    /// `function_node_breadcrumb_number` on the helper's setup command.
    pub const fn function_node_state(function_node_breadcrumb_number: u32) -> Self {
        Self {
            section: HELPER_PROCESS_LOSS_COUNT_BOARD_FUNCTION_NODE_STATE_SECTION,
            index_within_section: function_node_breadcrumb_number,
        }
    }
}

/// One inbound link's loss counts as a helper process last wrote them, beside
/// the wiring generation the parent assigned that link.
///
/// The parent reads a slot as its link's only while the generation matches, so
/// a write for a link since unwired never lands on the link that reused the slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("InboundLinkLossCountBoardSlot")]
#[repr(C)]
pub struct InboundLinkLossCountBoardSlot {
    /// The generation of the wiring these counts belong to; zero before any
    /// write.
    pub wiring_generation: u64,
    /// Bags lost on the link before anything read them.
    pub dropped_bags: u64,
    /// Samples a windowed port's flushes discarded on the link.
    pub discarded_samples: u64,
    /// Bags that left the link's mailbox for its reader: read, passed over by
    /// a `newest` read, or evicted from a port that may pass bags over.
    pub bags_off_the_mailbox_for_its_reader: u64,
    /// Of those, the bags the reader counted lost after taking them, which
    /// `dropped_bags` already holds.
    pub bags_its_reader_counted_lost_after_taking_them: u64,
}

/// One output port's counts of bags refused at its channel ceiling and bags
/// written onto its channel, as a helper process last wrote them, beside the
/// wiring generation the parent assigned the port's channel.
///
/// Read as the port's only while the generation matches, so a reopened
/// channel never shows the totals of the one before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("OutputPortBagCountBoardEntry")]
#[repr(C)]
pub struct OutputPortBagCountBoardEntry {
    /// The generation of the channel these counts belong to; zero before any
    /// write.
    pub wiring_generation: u64,
    /// Bags refused since the port's channel was opened.
    pub refused_bags: u64,
    /// Bags sent on the port's channel since it was opened.
    pub written_bags: u64,
}

/// A helper's callbacks in its stream's steps, as it last wrote them: what the
/// step barrier reads before it closes a step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("HelperProcessStepProgressBoardEntry")]
#[repr(C)]
pub struct HelperProcessStepProgressBoardEntry {
    /// One past the last step the helper's node finished; step `k` is finished
    /// once this exceeds `k`.
    pub steps_finished: u64,
    /// Callbacks of the node's started and not yet returned.
    pub callbacks_in_flight: u32,
    /// Always zero; keeps the entry a multiple of 8 bytes.
    pub reserved: u32,
}

/// The function-node call a helper process has under way, as it last wrote it:
/// what its parent reads after the helper dies, and what the parent's watchdog
/// times.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("HelperProcessFunctionNodeCallBreadcrumbBoardEntry")]
#[repr(C)]
pub struct HelperProcessFunctionNodeCallBreadcrumbBoardEntry {
    /// The machine's monotonic time the call started at, in nanoseconds; zero
    /// while no function-node call is under way.
    pub call_started_at_monotonic_ns: u64,
    /// The number the parent gave the called function node on the helper's
    /// setup command; read only while a call is under way.
    pub function_node_breadcrumb_number: u32,
    /// How many function-node calls the helper has under way, this oldest one
    /// among them; zero between calls.
    pub function_node_calls_under_way: u32,
}

/// How many calls running one function node may overrun the frame interval
/// its helper was given before the helper names it on its overrun entry.
pub const FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED: u32 = 3;

/// The first function node whose calls overran the frame interval its helper
/// was given [`FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED`]
/// times running, as the helper wrote it: what its parent reads to end the
/// helper and promote the node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("HelperProcessFunctionNodeOverrunBoardEntry")]
#[repr(C)]
pub struct HelperProcessFunctionNodeOverrunBoardEntry {
    /// The longest of the overrunning calls, in nanoseconds; zero until a node
    /// is named.
    pub longest_overrunning_call_ns: u64,
    /// The number the parent gave the node on the helper's setup command; read
    /// only once a node is named.
    pub function_node_breadcrumb_number: u32,
    /// Always zero; keeps the entry a multiple of 8 bytes.
    pub reserved: u32,
}

impl HelperProcessFunctionNodeOverrunBoardEntry {
    /// The breadcrumb number of the node named, or `None` before one is.
    pub fn function_node_that_overran(&self) -> Option<u32> {
        (self.longest_overrunning_call_ns != 0).then_some(self.function_node_breadcrumb_number)
    }
}

/// The most state values one function node's state entry holds: a state record
/// of at most this many numbers.
pub const STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS: usize = 30;

/// The state one stateful function node's last call returned, as its helper
/// process last wrote it: what its parent reads to carry the state across a
/// restart, a promotion and a reload, and what `graph` shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, ZeroCopySend)]
#[type_name("HelperProcessFunctionNodeStateBoardEntry")]
#[repr(C)]
pub struct HelperProcessFunctionNodeStateBoardEntry {
    /// How many times the helper wrote this entry; zero before its first
    /// write, when the entry holds no state.
    pub state_writes: u64,
    /// How many of `state_values` hold the state, in the state record's field
    /// order.
    pub state_value_count: u32,
    /// Always zero; keeps `state_values` 8-byte aligned.
    pub reserved: u32,
    /// The state record's fields, each as an f64; past `state_value_count`
    /// always zero.
    pub state_values: [f64; STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS],
}

impl HelperProcessFunctionNodeStateBoardEntry {
    /// The entry holding `state_values` as its `state_writes`-th write;
    /// `None` when there are more values than an entry holds.
    pub fn holding(state_values: &[f64], state_writes: u64) -> Option<Self> {
        let mut entry = Self {
            state_writes,
            state_value_count: u32::try_from(state_values.len()).ok()?,
            ..Self::default()
        };
        entry
            .state_values
            .get_mut(..state_values.len())?
            .copy_from_slice(state_values);
        Some(entry)
    }

    /// The state the entry holds, or `None` before the helper's first write.
    pub fn state_values_written(&self) -> Option<&[f64]> {
        if self.state_writes == 0 {
            return None;
        }
        self.state_values.get(..self.state_value_count as usize)
    }
}

pub const BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_A_STANDALONE_NODE: u32 = 0;

impl HelperProcessFunctionNodeCallBreadcrumbBoardEntry {
    /// The breadcrumb number of the function node whose call is under way, or
    /// `None` between calls.
    pub fn function_node_whose_call_is_under_way(&self) -> Option<u32> {
        (self.call_started_at_monotonic_ns != 0).then_some(self.function_node_breadcrumb_number)
    }
}

#[cfg(test)]
mod tests;

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The board as its helper writes it, and the mirrors each link and output port writes through.

use std::collections::BTreeMap;
use std::sync::Arc;

use iceoryx2::port::writer::{EntryHandleMut, Writer};
use iceoryx2::prelude::*;
use parking_lot::Mutex;
use streamlib_ipc_types::{
    FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED,
    HelperProcessFunctionNodeCallBreadcrumbBoardEntry, HelperProcessFunctionNodeOverrunBoardEntry,
    HelperProcessFunctionNodeStateBoardEntry, HelperProcessLossCountBoardKey,
    HelperProcessStepProgressBoardEntry, InboundLinkLossCountBoardSlot,
    OutputPortBagCountBoardEntry, STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS,
};

use crate::core::error::{Error, Result};
use crate::core::media_clock::MediaClock;

use super::{
    HelperProcessLossCountBoardService, INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD,
    InboundLinkLossCountBoardSlotAndWiringGeneration, inbound_link_slot_keys, output_port_keys,
};

// =============================================================================
// The helper's board
// =============================================================================

/// A loss-count board as its helper writes it: one lock per entry, since an
/// iceoryx2 entry has exactly one producer and a count moves on whatever thread
/// read or wrote.
pub struct HelperProcessLossCountBoardWriter {
    service_name: String,
    board_write_handles: HelperProcessLossCountBoardWriteHandles,
}

/// One inbound-link slot, and what was last written on it.
struct InboundLinkLossCountBoardSlotBeingWritten {
    entry: EntryHandleMut<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        InboundLinkLossCountBoardSlot,
    >,
    last_written: InboundLinkLossCountBoardSlot,
}

/// One output port's entry, and what was last written on it.
struct OutputPortBagCountBoardEntryBeingWritten {
    entry: EntryHandleMut<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        OutputPortBagCountBoardEntry,
    >,
    last_written: OutputPortBagCountBoardEntry,
}

/// The step-progress entry, and what was last written on it.
struct HelperProcessStepProgressBoardEntryBeingWritten {
    entry: EntryHandleMut<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        HelperProcessStepProgressBoardEntry,
    >,
    last_written: HelperProcessStepProgressBoardEntry,
}

/// One stateful function node's state entry, and how many times it was
/// written.
struct HelperProcessFunctionNodeStateBoardEntryBeingWritten {
    entry: EntryHandleMut<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        HelperProcessFunctionNodeStateBoardEntry,
    >,
    state_writes: u64,
}

/// The writer and its entry handles, dropped together.
struct HelperProcessLossCountBoardWriteHandles {
    inbound_link_slots: Vec<Mutex<InboundLinkLossCountBoardSlotBeingWritten>>,
    output_ports: Vec<(String, Mutex<OutputPortBagCountBoardEntryBeingWritten>)>,
    step_progress: Mutex<HelperProcessStepProgressBoardEntryBeingWritten>,
    function_node_call_breadcrumb: Mutex<FunctionNodeCallsUnderWayInThisHelper>,
    function_node_states: Vec<(
        u32,
        Mutex<HelperProcessFunctionNodeStateBoardEntryBeingWritten>,
    )>,
    _writer: Writer<ipc_threadsafe::Service, HelperProcessLossCountBoardKey>,
    _service: HelperProcessLossCountBoardService,
}

impl HelperProcessLossCountBoardWriter {
    /// Take the write handle of every entry on the board `service` opened.
    pub(crate) fn writing(
        service_name: String,
        output_port_names: &[String],
        stateful_function_node_breadcrumb_numbers: &[u32],
        service: HelperProcessLossCountBoardService,
    ) -> Result<Self> {
        let writer = service.writer_builder().create().map_err(|refusal| {
            Error::Runtime(format!(
                "the loss-count board '{service_name}' refused its helper the writer: {refusal:?}"
            ))
        })?;
        let unwritable_entry = |refusal: iceoryx2::port::writer::EntryHandleMutError| {
            Error::Runtime(format!(
                "the loss-count board '{service_name}' has no writable entry this helper was \
                 told it declares: {refusal:?}"
            ))
        };
        let inbound_link_slots = inbound_link_slot_keys()
            .map(|key| {
                writer.entry(&key).map(|entry| {
                    Mutex::new(InboundLinkLossCountBoardSlotBeingWritten {
                        entry,
                        last_written: InboundLinkLossCountBoardSlot::default(),
                    })
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unwritable_entry)?;
        let output_ports = output_port_keys(output_port_names.len())
            .zip(output_port_names)
            .map(|(key, output_port)| {
                writer.entry(&key).map(|entry| {
                    (
                        output_port.clone(),
                        Mutex::new(OutputPortBagCountBoardEntryBeingWritten {
                            entry,
                            last_written: OutputPortBagCountBoardEntry::default(),
                        }),
                    )
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unwritable_entry)?;
        let step_progress = writer
            .entry(&HelperProcessLossCountBoardKey::STEP_PROGRESS)
            .map(|entry| {
                Mutex::new(HelperProcessStepProgressBoardEntryBeingWritten {
                    entry,
                    last_written: HelperProcessStepProgressBoardEntry::default(),
                })
            })
            .map_err(unwritable_entry)?;
        let function_node_call_breadcrumb = writer
            .entry(&HelperProcessLossCountBoardKey::FUNCTION_NODE_CALL_BREADCRUMB)
            .map_err(unwritable_entry)?;
        let overrun_entry = writer
            .entry(&HelperProcessLossCountBoardKey::FUNCTION_NODE_OVERRUN)
            .map_err(unwritable_entry)?;
        let function_node_call_breadcrumb = Mutex::new(FunctionNodeCallsUnderWayInThisHelper {
            breadcrumb_entry: function_node_call_breadcrumb,
            calls_under_way: Vec::new(),
            next_call_number: 0,
            overruns: FunctionNodeCallOverrunsInThisHelper {
                overrun_entry,
                frame_interval_ns: None,
                overrunning_calls_running_by_function_node: BTreeMap::new(),
                named_a_function_node: false,
            },
        });
        let function_node_states = stateful_function_node_breadcrumb_numbers
            .iter()
            .map(|&breadcrumb_number| {
                writer
                    .entry(&HelperProcessLossCountBoardKey::function_node_state(
                        breadcrumb_number,
                    ))
                    .map(|entry| {
                        (
                            breadcrumb_number,
                            Mutex::new(HelperProcessFunctionNodeStateBoardEntryBeingWritten {
                                entry,
                                state_writes: 0,
                            }),
                        )
                    })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unwritable_entry)?;
        Ok(Self {
            service_name,
            board_write_handles: HelperProcessLossCountBoardWriteHandles {
                inbound_link_slots,
                output_ports,
                step_progress,
                function_node_call_breadcrumb,
                function_node_states,
                _writer: writer,
                _service: service,
            },
        })
    }

    /// Claim the slot the parent assigned an inbound link, writing zero counts
    /// under its wiring's generation, and hand back the mirror that link's
    /// counters write through.
    ///
    /// Every mirror an earlier wiring of the slot handed out writes nothing from
    /// here on.
    pub fn claim_inbound_link_slot(
        self: &Arc<Self>,
        assigned: InboundLinkLossCountBoardSlotAndWiringGeneration,
    ) -> Result<InboundLinkLossCountBoardSlotMirror> {
        let InboundLinkLossCountBoardSlotAndWiringGeneration {
            slot,
            wiring_generation,
        } = assigned;
        let Some(slot_being_written) = self.board_write_handles.inbound_link_slots.get(slot) else {
            return Err(Error::Configuration(format!(
                "inbound-link slot {slot} is past the {INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD} \
                 slots the loss-count board '{}' declares",
                self.service_name
            )));
        };
        let mut slot_being_written = slot_being_written.lock();
        slot_being_written.last_written = InboundLinkLossCountBoardSlot {
            wiring_generation,
            ..InboundLinkLossCountBoardSlot::default()
        };
        slot_being_written
            .entry
            .update_with_copy(slot_being_written.last_written);
        Ok(InboundLinkLossCountBoardSlotMirror {
            board: Arc::clone(self),
            slot,
            wiring_generation,
        })
    }

    /// Claim `output_port`'s entry for the channel `wiring_generation` names,
    /// writing zero counts under it, and hand back the mirror its refused-bag
    /// and written-bag counters write through — or `None` for a port this board
    /// carries no entry for.
    ///
    /// Every mirror an earlier channel of the port handed out writes nothing
    /// from here on.
    pub fn claim_output_port_entry(
        self: &Arc<Self>,
        output_port: &str,
        wiring_generation: u64,
    ) -> Option<OutputPortBagCountBoardMirror> {
        let (output_port_index, (_, entry_being_written)) = self
            .board_write_handles
            .output_ports
            .iter()
            .enumerate()
            .find(|(_, (declared, _))| declared == output_port)?;
        let mut entry_being_written = entry_being_written.lock();
        entry_being_written.last_written = OutputPortBagCountBoardEntry {
            wiring_generation,
            refused_bags: 0,
            written_bags: 0,
        };
        entry_being_written
            .entry
            .update_with_copy(entry_being_written.last_written);
        Some(OutputPortBagCountBoardMirror {
            board: Arc::clone(self),
            output_port_index,
            wiring_generation,
        })
    }

    /// Note a callback of the helper's node started; a step cannot close until
    /// [`Self::note_a_callback_returned`] notes it returned.
    pub fn note_a_callback_started(&self) {
        self.write_on_the_step_progress_entry(|last_written| {
            last_written.callbacks_in_flight = last_written.callbacks_in_flight.saturating_add(1);
        });
    }

    /// Note a callback of the helper's node returned, and that the node
    /// finished step `finished_step_index` with it when one is named.
    pub fn note_a_callback_returned(&self, finished_step_index: Option<u64>) {
        self.write_on_the_step_progress_entry(|last_written| {
            last_written.callbacks_in_flight = last_written.callbacks_in_flight.saturating_sub(1);
            if let Some(finished_step_index) = finished_step_index {
                last_written.steps_finished = last_written
                    .steps_finished
                    .max(finished_step_index.saturating_add(1));
            }
        });
    }

    /// Note a call to the function node the parent numbered
    /// `function_node_breadcrumb_number` started at `call_started_at_monotonic_ns`,
    /// which is never zero; it stays under way until the call it answers is
    /// handed to [`Self::note_the_function_node_call_returned`].
    ///
    /// The board names the oldest call under way, so a call on another thread
    /// never hides one that stalled before it.
    pub fn note_a_function_node_call_started(
        &self,
        function_node_breadcrumb_number: u32,
        call_started_at_monotonic_ns: u64,
    ) -> FunctionNodeCallUnderWayInThisHelper {
        let mut calls = self
            .board_write_handles
            .function_node_call_breadcrumb
            .lock();
        let call = FunctionNodeCallUnderWayInThisHelper {
            call_number: calls.next_call_number,
        };
        calls.next_call_number = calls.next_call_number.wrapping_add(1);
        calls.calls_under_way.push((
            call,
            HelperProcessFunctionNodeCallBreadcrumbBoardEntry {
                call_started_at_monotonic_ns: call_started_at_monotonic_ns.max(1),
                function_node_breadcrumb_number,
                function_node_calls_under_way: 0,
            },
        ));
        calls.write_the_oldest_call_under_way();
        call
    }

    /// Note `call` returned.
    pub fn note_the_function_node_call_returned(&self, call: FunctionNodeCallUnderWayInThisHelper) {
        self.note_the_function_node_call_returned_at(call, MediaClock::now().as_nanos() as u64);
    }

    /// Note `call` returned at `call_returned_at_monotonic_ns`, and name its
    /// node on the overrun entry once its calls overran the frame interval
    /// [`FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED`]
    /// times running.
    pub fn note_the_function_node_call_returned_at(
        &self,
        call: FunctionNodeCallUnderWayInThisHelper,
        call_returned_at_monotonic_ns: u64,
    ) {
        let mut calls = self
            .board_write_handles
            .function_node_call_breadcrumb
            .lock();
        let returned = calls
            .calls_under_way
            .iter()
            .find(|(call_under_way, _)| *call_under_way == call)
            .map(|(_, breadcrumb)| *breadcrumb);
        calls
            .calls_under_way
            .retain(|(call_under_way, _)| *call_under_way != call);
        calls.write_the_oldest_call_under_way();
        if let Some(returned) = returned {
            calls.overruns.time_the_call_that_returned(
                returned.function_node_breadcrumb_number,
                call_returned_at_monotonic_ns.saturating_sub(returned.call_started_at_monotonic_ns),
            );
        }
    }

    /// Time every function-node call from now on against `frame_interval`.
    pub fn time_each_function_node_call_against_the_frame_interval(
        &self,
        frame_interval: std::time::Duration,
    ) {
        self.board_write_handles
            .function_node_call_breadcrumb
            .lock()
            .overruns
            .frame_interval_ns = Some(frame_interval.as_nanos() as u64);
    }

    /// Whether this board carries a state entry for the function node
    /// numbered `function_node_breadcrumb_number`.
    pub fn carries_a_state_entry_for(&self, function_node_breadcrumb_number: u32) -> bool {
        self.board_write_handles
            .function_node_states
            .iter()
            .any(|(breadcrumb_number, _)| *breadcrumb_number == function_node_breadcrumb_number)
    }

    /// Write the state the last call of the function node numbered
    /// `function_node_breadcrumb_number` returned, refused for a node this
    /// board carries no state entry for and for a state past what an entry
    /// holds.
    pub fn write_the_state_of_a_function_node(
        &self,
        function_node_breadcrumb_number: u32,
        state_values: &[f64],
    ) -> Result<()> {
        let Some((_, entry_being_written)) = self
            .board_write_handles
            .function_node_states
            .iter()
            .find(|(breadcrumb_number, _)| *breadcrumb_number == function_node_breadcrumb_number)
        else {
            return Err(Error::Configuration(format!(
                "the loss-count board '{}' carries no state entry for the function node \
                 numbered {function_node_breadcrumb_number}",
                self.service_name
            )));
        };
        let mut entry_being_written = entry_being_written.lock();
        let state_writes = entry_being_written.state_writes.wrapping_add(1).max(1);
        let Some(entry) =
            HelperProcessFunctionNodeStateBoardEntry::holding(state_values, state_writes)
        else {
            return Err(Error::Configuration(format!(
                "the function node numbered {function_node_breadcrumb_number} has a state of \
                 {} numbers, and a state entry holds at most \
                 {STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS}",
                state_values.len()
            )));
        };
        entry_being_written.entry.update_with_copy(entry);
        entry_being_written.state_writes = state_writes;
        Ok(())
    }

    fn write_on_the_step_progress_entry(
        &self,
        update: impl FnOnce(&mut HelperProcessStepProgressBoardEntry),
    ) {
        let mut entry_being_written = self.board_write_handles.step_progress.lock();
        update(&mut entry_being_written.last_written);
        entry_being_written
            .entry
            .update_with_copy(entry_being_written.last_written);
    }
}

/// Where one wiring of an inbound link writes its loss counts: its slot, under
/// its generation.
#[derive(Clone)]
pub struct InboundLinkLossCountBoardSlotMirror {
    board: Arc<HelperProcessLossCountBoardWriter>,
    slot: usize,
    wiring_generation: u64,
}

impl InboundLinkLossCountBoardSlotMirror {
    /// Write `dropped_bags` as this link's dropped-bag total.
    pub(crate) fn mirror_dropped_bags(&self, dropped_bags: u64) {
        self.write_on_the_slot_while_it_is_this_wirings(|last_written| {
            last_written.dropped_bags = last_written.dropped_bags.max(dropped_bags);
        });
    }

    /// Write `discarded_samples` as this link's discarded-sample total.
    pub(crate) fn mirror_discarded_samples(&self, discarded_samples: u64) {
        self.write_on_the_slot_while_it_is_this_wirings(|last_written| {
            last_written.discarded_samples = last_written.discarded_samples.max(discarded_samples);
        });
    }

    /// Write `bags_off_the_mailbox` as the total of this link's bags that left
    /// its mailbox for the reader.
    pub(crate) fn mirror_bags_off_the_mailbox_for_its_reader(&self, bags_off_the_mailbox: u64) {
        self.write_on_the_slot_while_it_is_this_wirings(|last_written| {
            last_written.bags_off_the_mailbox_for_its_reader = last_written
                .bags_off_the_mailbox_for_its_reader
                .max(bags_off_the_mailbox);
        });
    }

    /// Write `bags_counted_lost_after_taking` as the total of this link's bags
    /// its reader counted lost after taking them.
    pub(crate) fn mirror_bags_its_reader_counted_lost_after_taking_them(
        &self,
        bags_counted_lost_after_taking: u64,
    ) {
        self.write_on_the_slot_while_it_is_this_wirings(|last_written| {
            last_written.bags_its_reader_counted_lost_after_taking_them = last_written
                .bags_its_reader_counted_lost_after_taking_them
                .max(bags_counted_lost_after_taking);
        });
    }

    /// Totals are kept at their largest, because two threads that each moved a
    /// count can reach this lock in either order.
    fn write_on_the_slot_while_it_is_this_wirings(
        &self,
        update: impl FnOnce(&mut InboundLinkLossCountBoardSlot),
    ) {
        let mut slot_being_written =
            self.board.board_write_handles.inbound_link_slots[self.slot].lock();
        if slot_being_written.last_written.wiring_generation != self.wiring_generation {
            return;
        }
        update(&mut slot_being_written.last_written);
        slot_being_written
            .entry
            .update_with_copy(slot_being_written.last_written);
    }
}

/// Where one channel of an output port writes its refused-bag and written-bag
/// counts: the port's entry, under the channel's generation.
#[derive(Clone)]
pub struct OutputPortBagCountBoardMirror {
    board: Arc<HelperProcessLossCountBoardWriter>,
    output_port_index: usize,
    wiring_generation: u64,
}

impl OutputPortBagCountBoardMirror {
    /// Write `refused_bags` as the port's refused-bag total, kept at its
    /// largest for the reason an inbound slot's totals are.
    pub(crate) fn mirror_refused_bags(&self, refused_bags: u64) {
        self.write_on_the_entry_while_it_is_this_channels(|last_written| {
            last_written.refused_bags = last_written.refused_bags.max(refused_bags);
        });
    }

    /// Write `written_bags` as the port's written-bag total, kept at its
    /// largest for the same reason.
    pub(crate) fn mirror_written_bags(&self, written_bags: u64) {
        self.write_on_the_entry_while_it_is_this_channels(|last_written| {
            last_written.written_bags = last_written.written_bags.max(written_bags);
        });
    }

    fn write_on_the_entry_while_it_is_this_channels(
        &self,
        update: impl FnOnce(&mut OutputPortBagCountBoardEntry),
    ) {
        let (_, entry_being_written) =
            &self.board.board_write_handles.output_ports[self.output_port_index];
        let mut entry_being_written = entry_being_written.lock();
        if entry_being_written.last_written.wiring_generation != self.wiring_generation {
            return;
        }
        update(&mut entry_being_written.last_written);
        entry_being_written
            .entry
            .update_with_copy(entry_being_written.last_written);
    }
}

/// One function-node call a helper noted on its board, until it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionNodeCallUnderWayInThisHelper {
    call_number: u64,
}

/// Every function-node call a helper has under way, and the board entry naming
/// the oldest.
struct FunctionNodeCallsUnderWayInThisHelper {
    breadcrumb_entry: EntryHandleMut<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        HelperProcessFunctionNodeCallBreadcrumbBoardEntry,
    >,
    calls_under_way: Vec<(
        FunctionNodeCallUnderWayInThisHelper,
        HelperProcessFunctionNodeCallBreadcrumbBoardEntry,
    )>,
    next_call_number: u64,
    overruns: FunctionNodeCallOverrunsInThisHelper,
}

/// How many calls running each function node overran the frame interval its
/// helper was given, and the entry the first to reach the limit is named on.
struct FunctionNodeCallOverrunsInThisHelper {
    overrun_entry: EntryHandleMut<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        HelperProcessFunctionNodeOverrunBoardEntry,
    >,
    frame_interval_ns: Option<u64>,
    overrunning_calls_running_by_function_node: BTreeMap<u32, (u32, u64)>,
    named_a_function_node: bool,
}

impl FunctionNodeCallOverrunsInThisHelper {
    fn time_the_call_that_returned(&mut self, function_node_breadcrumb_number: u32, call_ns: u64) {
        let Some(frame_interval_ns) = self.frame_interval_ns else {
            return;
        };
        if self.named_a_function_node {
            return;
        }
        if call_ns <= frame_interval_ns {
            self.overrunning_calls_running_by_function_node
                .remove(&function_node_breadcrumb_number);
            return;
        }
        let (overrunning_calls_running, longest_overrunning_call_ns) = self
            .overrunning_calls_running_by_function_node
            .entry(function_node_breadcrumb_number)
            .or_insert((0, 0));
        *overrunning_calls_running += 1;
        *longest_overrunning_call_ns = (*longest_overrunning_call_ns).max(call_ns);
        if *overrunning_calls_running
            >= FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED
        {
            self.overrun_entry
                .update_with_copy(HelperProcessFunctionNodeOverrunBoardEntry {
                    longest_overrunning_call_ns: *longest_overrunning_call_ns,
                    function_node_breadcrumb_number,
                    reserved: 0,
                });
            self.named_a_function_node = true;
        }
    }
}

impl FunctionNodeCallsUnderWayInThisHelper {
    fn write_the_oldest_call_under_way(&mut self) {
        let function_node_calls_under_way = self.calls_under_way.len() as u32;
        let oldest_call_under_way = self
            .calls_under_way
            .iter()
            .map(|(_, breadcrumb)| *breadcrumb)
            .min_by_key(|breadcrumb| breadcrumb.call_started_at_monotonic_ns)
            .map(|oldest| HelperProcessFunctionNodeCallBreadcrumbBoardEntry {
                function_node_calls_under_way,
                ..oldest
            })
            .unwrap_or_default();
        self.breadcrumb_entry
            .update_with_copy(oldest_call_under_way);
    }
}

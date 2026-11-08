// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The blackboard a helper process mirrors its loss counts, its step progress,
//! its function-node call breadcrumb and its stateful function nodes' state
//! onto, and what its parent reads `graph`'s `metrics`, the step ledger and
//! each function node's state off.
//!
//! One board per helper spawn: the parent creates it before the child starts
//! and holds it until the next spawn's board replaces it, then adds what it
//! last held to the new board's counts, so a link's losses never go backwards
//! across a restart. Its keys are fixed at creation and links are wired
//! live, so the board declares a slot per permitted inbound link and an entry
//! per declared output port, and the parent assigns each inbound link a slot and
//! each output port's channel a wiring generation. An entry renders as its
//! link's or port's only while the generation written there is the one the
//! parent assigned.

use std::collections::BTreeMap;
use std::sync::Arc;

use iceoryx2::port::reader::{EntryHandle, Reader};
use iceoryx2::port::writer::{EntryHandleMut, Writer};
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::blackboard::PortFactory as BlackboardPortFactory;
use parking_lot::Mutex;
use streamlib_ipc_types::{
    FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED,
    HelperProcessFunctionNodeCallBreadcrumbBoardEntry, HelperProcessFunctionNodeOverrunBoardEntry,
    HelperProcessFunctionNodeStateBoardEntry, HelperProcessLossCountBoardKey,
    HelperProcessStepProgressBoardEntry, InboundLinkLossCountBoardSlot,
    MAX_INBOUND_LINKS_PER_DESTINATION, OutputPortBagCountBoardEntry,
    STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS,
};

use crate::core::error::{Error, Result};
use crate::core::media_clock::MediaClock;

/// The inbound-link slots every board declares: one per inbound link a
/// destination may hold.
pub(crate) const INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD: usize = MAX_INBOUND_LINKS_PER_DESTINATION;

/// The blackboard service a loss-count board is.
pub(crate) type HelperProcessLossCountBoardService =
    BlackboardPortFactory<ipc_threadsafe::Service, HelperProcessLossCountBoardKey>;

/// Every inbound-link slot's key, in slot order — the board's first section.
pub(super) fn inbound_link_slot_keys() -> impl Iterator<Item = HelperProcessLossCountBoardKey> {
    (0..INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD)
        .map(|slot| HelperProcessLossCountBoardKey::inbound_link_slot(slot as u32))
}

/// The keys of `output_port_count` declared output ports, in declaration order
/// — the board's second section.
pub(super) fn output_port_keys(
    output_port_count: usize,
) -> impl Iterator<Item = HelperProcessLossCountBoardKey> {
    (0..output_port_count).map(|output_port_index| {
        HelperProcessLossCountBoardKey::output_port(output_port_index as u32)
    })
}

/// Where the parent assigned an inbound link's counts to go: its slot, and the
/// generation of the wiring it holds that slot for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InboundLinkLossCountBoardSlotAndWiringGeneration {
    /// The slot's index in the board's inbound-link section.
    pub slot: usize,
    /// The wiring's generation, never repeated for the processor's life.
    pub wiring_generation: u64,
}

// =============================================================================
// The parent's board
// =============================================================================

/// A helper spawn's loss-count board as the parent holds it: the service it
/// created, and a read handle on every entry.
pub struct HelperProcessLossCountBoard {
    service_name: String,
    board_read_handles: HelperProcessLossCountBoardReadHandles,
}

/// The reader and its entry handles, dropped together.
///
/// Fields drop in declaration order: the handles, the reader they came from,
/// then the service.
struct HelperProcessLossCountBoardReadHandles {
    inbound_link_slots: Vec<
        EntryHandle<
            ipc_threadsafe::Service,
            HelperProcessLossCountBoardKey,
            InboundLinkLossCountBoardSlot,
        >,
    >,
    output_ports: Vec<(
        String,
        EntryHandle<
            ipc_threadsafe::Service,
            HelperProcessLossCountBoardKey,
            OutputPortBagCountBoardEntry,
        >,
    )>,
    step_progress: EntryHandle<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        HelperProcessStepProgressBoardEntry,
    >,
    function_node_call_breadcrumb: EntryHandle<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        HelperProcessFunctionNodeCallBreadcrumbBoardEntry,
    >,
    function_node_overrun: EntryHandle<
        ipc_threadsafe::Service,
        HelperProcessLossCountBoardKey,
        HelperProcessFunctionNodeOverrunBoardEntry,
    >,
    function_node_states: Vec<(
        u32,
        EntryHandle<
            ipc_threadsafe::Service,
            HelperProcessLossCountBoardKey,
            HelperProcessFunctionNodeStateBoardEntry,
        >,
    )>,
    _reader: Reader<ipc_threadsafe::Service, HelperProcessLossCountBoardKey>,
    _service: HelperProcessLossCountBoardService,
}

impl HelperProcessLossCountBoard {
    /// Take a read handle on every entry of the board `service` just created.
    pub(crate) fn reading(
        service_name: String,
        output_port_names: Vec<String>,
        stateful_function_node_breadcrumb_numbers: Vec<u32>,
        service: HelperProcessLossCountBoardService,
    ) -> Result<Self> {
        let reader = service.reader_builder().create().map_err(|refusal| {
            Error::Runtime(format!(
                "the loss-count board '{service_name}' was created but refused its parent a \
                 reader: {refusal:?}"
            ))
        })?;
        let unreadable_entry = |refusal: iceoryx2::port::reader::EntryHandleError| {
            Error::Runtime(format!(
                "the loss-count board '{service_name}' was created without an entry it \
                 declares: {refusal:?}"
            ))
        };
        let inbound_link_slots = inbound_link_slot_keys()
            .map(|key| reader.entry(&key))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unreadable_entry)?;
        let output_ports = output_port_keys(output_port_names.len())
            .zip(output_port_names)
            .map(|(key, output_port)| reader.entry(&key).map(|entry| (output_port, entry)))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unreadable_entry)?;
        let step_progress = reader
            .entry(&HelperProcessLossCountBoardKey::STEP_PROGRESS)
            .map_err(unreadable_entry)?;
        let function_node_call_breadcrumb = reader
            .entry(&HelperProcessLossCountBoardKey::FUNCTION_NODE_CALL_BREADCRUMB)
            .map_err(unreadable_entry)?;
        let function_node_overrun = reader
            .entry(&HelperProcessLossCountBoardKey::FUNCTION_NODE_OVERRUN)
            .map_err(unreadable_entry)?;
        let function_node_states = stateful_function_node_breadcrumb_numbers
            .into_iter()
            .map(|breadcrumb_number| {
                reader
                    .entry(&HelperProcessLossCountBoardKey::function_node_state(
                        breadcrumb_number,
                    ))
                    .map(|entry| (breadcrumb_number, entry))
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unreadable_entry)?;
        Ok(Self {
            service_name,
            board_read_handles: HelperProcessLossCountBoardReadHandles {
                inbound_link_slots,
                output_ports,
                step_progress,
                function_node_call_breadcrumb,
                function_node_overrun,
                function_node_states,
                _reader: reader,
                _service: service,
            },
        })
    }

    /// The name the helper opens this board by.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// The declared output ports this board carries an entry for, in key order.
    pub fn output_port_names(&self) -> impl Iterator<Item = &str> {
        self.board_read_handles
            .output_ports
            .iter()
            .map(|(output_port, _)| output_port.as_str())
    }

    /// What the helper last wrote on inbound-link slot `slot`.
    pub fn inbound_link_slot(&self, slot: usize) -> Option<InboundLinkLossCountBoardSlot> {
        self.board_read_handles
            .inbound_link_slots
            .get(slot)
            .map(|entry| *entry.get())
    }

    /// What the helper last wrote on `output_port`'s entry.
    pub fn output_port_entry(&self, output_port: &str) -> Option<OutputPortBagCountBoardEntry> {
        self.board_read_handles
            .output_ports
            .iter()
            .find(|(declared, _)| declared == output_port)
            .map(|(_, entry)| *entry.get())
    }

    /// What the helper last wrote of its node's step progress.
    pub fn step_progress_entry(&self) -> HelperProcessStepProgressBoardEntry {
        *self.board_read_handles.step_progress.get()
    }

    /// What the helper last wrote of the function-node call it has under way.
    pub fn function_node_call_breadcrumb_entry(
        &self,
    ) -> HelperProcessFunctionNodeCallBreadcrumbBoardEntry {
        *self.board_read_handles.function_node_call_breadcrumb.get()
    }

    /// The function node the helper named for overrunning its frame interval,
    /// if it named one.
    pub fn function_node_overrun_entry(&self) -> HelperProcessFunctionNodeOverrunBoardEntry {
        *self.board_read_handles.function_node_overrun.get()
    }

    /// What the helper last wrote of the state of the function node numbered
    /// `function_node_breadcrumb_number`; `None` for a node this board carries
    /// no state entry for.
    pub fn function_node_state_entry(
        &self,
        function_node_breadcrumb_number: u32,
    ) -> Option<HelperProcessFunctionNodeStateBoardEntry> {
        self.board_read_handles
            .function_node_states
            .iter()
            .find(|(breadcrumb_number, _)| *breadcrumb_number == function_node_breadcrumb_number)
            .map(|(_, entry)| *entry.get())
    }
}

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

/// The slots and output ports a helper-placed processor's links hold.
struct HelperPlacedProcessorLossCountWiring {
    inbound_link_slots: Vec<Option<InboundLinkLossCountSlotAssignment>>,
    last_assigned_wiring_generation: u64,
    output_ports_with_outbound_links: BTreeMap<String, OutputPortWithOutboundLinks>,
}

impl Default for HelperPlacedProcessorLossCountWiring {
    fn default() -> Self {
        Self {
            inbound_link_slots: vec![None; INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD],
            last_assigned_wiring_generation: 0,
            output_ports_with_outbound_links: BTreeMap::new(),
        }
    }
}

impl HelperPlacedProcessorLossCountWiring {
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

/// A helper-placed processor's loss counts as its parent reads them: the slot
/// each inbound link was assigned, the output ports with a link, the board the
/// current helper writes on, and what the boards of earlier helpers — ones that
/// died and were restarted — last held for wirings still in place.
#[derive(Default)]
pub struct HelperPlacedProcessorLossCounts {
    wiring: Mutex<HelperPlacedProcessorLossCountWiring>,
    board: Mutex<Option<HelperProcessLossCountBoard>>,
    counts_earlier_spawns_left: Mutex<ProcessorLossCountSnapshot>,
}

impl std::fmt::Debug for HelperPlacedProcessorLossCounts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HelperPlacedProcessorLossCounts")
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

/// Every count `graph` renders under a processor's `metrics`, taken at one
/// instant.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProcessorLossCountSnapshot {
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

impl ProcessorLossCountSnapshot {
    fn add(&mut self, counts_to_add: &ProcessorLossCountSnapshot) {
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
    fn forget_every_count_not_in(&mut self, wiring: &HelperPlacedProcessorLossCountWiring) {
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

impl HelperPlacedProcessorLossCounts {
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
                "link '{link_id}' would be a helper-placed processor's inbound link past the \
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
    pub fn loss_count_snapshot(&self) -> ProcessorLossCountSnapshot {
        let wiring = self.wiring.lock();
        let held_board = self.board.lock();
        let mut snapshot = Self::loss_count_snapshot_read_off(&wiring, held_board.as_ref());
        snapshot.add(&self.counts_earlier_spawns_left.lock());
        snapshot
    }

    fn loss_count_snapshot_read_off(
        wiring: &HelperPlacedProcessorLossCountWiring,
        board: Option<&HelperProcessLossCountBoard>,
    ) -> ProcessorLossCountSnapshot {
        let mut snapshot = ProcessorLossCountSnapshot::default();
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

    /// This processor's dropped bags across every inbound link, each read for
    /// its link's own wiring.
    pub fn total_dropped_bag_count(&self) -> u64 {
        self.loss_count_snapshot()
            .dropped_bags_by_inbound_link
            .values()
            .sum()
    }
}

/// A board as a parent creates it and a writer as its helper opens it, each
/// from its own node in this test process's domain, with the helper's node
/// handed back so a test can let it go.
#[cfg(test)]
pub(crate) fn a_loss_count_board_and_its_helpers_writer_for_this_test_process(
    output_port_names: &[&str],
    stateful_function_node_breadcrumb_numbers: &[u32],
) -> (
    HelperProcessLossCountBoard,
    Arc<HelperProcessLossCountBoardWriter>,
    crate::iceoryx2::Iceoryx2Node,
) {
    let service_name = format!(
        "streamlib-test/loss-counts/{}",
        crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix()
    );
    let output_port_names: Vec<String> = output_port_names
        .iter()
        .map(|name| name.to_string())
        .collect();
    let board = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .create_helper_process_loss_count_board(
            &service_name,
            output_port_names.clone(),
            stateful_function_node_breadcrumb_numbers.to_vec(),
        )
        .expect("the parent creates its helper's board");
    let helper_node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
    let writer = helper_node
        .open_helper_process_loss_count_board_writer(
            &service_name,
            &output_port_names,
            stateful_function_node_breadcrumb_numbers,
        )
        .expect("the helper opens the board its parent named");
    (board, Arc::new(writer), helper_node)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot_and_generation(
        slot: usize,
        wiring_generation: u64,
    ) -> InboundLinkLossCountBoardSlotAndWiringGeneration {
        InboundLinkLossCountBoardSlotAndWiringGeneration {
            slot,
            wiring_generation,
        }
    }

    #[test]
    fn a_count_written_on_a_claimed_slot_is_what_the_parent_reads() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);

        let mirror = writer
            .claim_inbound_link_slot(slot_and_generation(3, 7))
            .unwrap();
        assert_eq!(
            board.inbound_link_slot(3),
            Some(InboundLinkLossCountBoardSlot {
                wiring_generation: 7,
                ..InboundLinkLossCountBoardSlot::default()
            }),
            "a claim writes the wiring's generation and zero counts before any loss"
        );

        mirror.mirror_dropped_bags(5);
        mirror.mirror_discarded_samples(480);
        mirror.mirror_dropped_bags(4);
        assert_eq!(
            board.inbound_link_slot(3),
            Some(InboundLinkLossCountBoardSlot {
                wiring_generation: 7,
                dropped_bags: 5,
                discarded_samples: 480,
                ..InboundLinkLossCountBoardSlot::default()
            }),
            "a total that arrives late never lowers the count"
        );

        let video_mirror = writer.claim_output_port_entry("video", 8).unwrap();
        video_mirror.mirror_refused_bags(2);
        video_mirror.mirror_written_bags(60);
        video_mirror.mirror_written_bags(59);
        assert_eq!(
            board.output_port_entry("video"),
            Some(OutputPortBagCountBoardEntry {
                wiring_generation: 8,
                refused_bags: 2,
                written_bags: 60,
            }),
            "a written total that arrives late never lowers the count either"
        );
        assert_eq!(board.output_port_entry("audio"), None);
        assert!(writer.claim_output_port_entry("audio", 9).is_none());
    }

    #[test]
    fn a_write_from_a_wiring_the_slot_has_moved_past_lands_nothing() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
        let first_wiring = writer
            .claim_inbound_link_slot(slot_and_generation(0, 1))
            .unwrap();
        first_wiring.mirror_dropped_bags(9);

        let second_wiring = writer
            .claim_inbound_link_slot(slot_and_generation(0, 2))
            .unwrap();
        first_wiring.mirror_dropped_bags(12);
        second_wiring.mirror_dropped_bags(1);

        assert_eq!(
            board.inbound_link_slot(0),
            Some(InboundLinkLossCountBoardSlot {
                wiring_generation: 2,
                dropped_bags: 1,
                ..InboundLinkLossCountBoardSlot::default()
            }),
            "the slot carries only the wiring that claimed it last"
        );
    }

    #[test]
    fn an_output_port_claimed_again_starts_from_zero_and_its_earlier_mirror_lands_nothing() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
        let video_entry = || {
            board
                .output_port_entry("video")
                .map(|written| (written.wiring_generation, written.refused_bags))
        };
        let first_channel = writer.claim_output_port_entry("video", 1).unwrap();
        first_channel.mirror_refused_bags(3);

        let second_channel = writer.claim_output_port_entry("video", 2).unwrap();
        assert_eq!(video_entry(), Some((2, 0)));
        first_channel.mirror_refused_bags(4);
        assert_eq!(video_entry(), Some((2, 0)));
        second_channel.mirror_refused_bags(1);
        assert_eq!(video_entry(), Some((2, 1)));
    }

    #[test]
    fn the_last_counts_stay_readable_after_the_helpers_writer_is_gone() {
        let (board, writer, helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
        writer
            .claim_inbound_link_slot(slot_and_generation(1, 4))
            .unwrap()
            .mirror_dropped_bags(6);
        writer
            .claim_output_port_entry("video", 5)
            .unwrap()
            .mirror_refused_bags(2);

        drop(writer);
        drop(helper_node);

        assert_eq!(
            board.inbound_link_slot(1).map(|slot| slot.dropped_bags),
            Some(6)
        );
        assert_eq!(
            board
                .output_port_entry("video")
                .map(|written| written.refused_bags),
            Some(2)
        );
    }

    #[test]
    fn slots_are_reused_lowest_first_and_every_wiring_gets_a_new_generation() {
        let counts = HelperPlacedProcessorLossCounts::default();
        assert_eq!(
            counts.assign_inbound_link_slot("L-a", false).unwrap(),
            slot_and_generation(0, 1)
        );
        assert_eq!(
            counts.assign_inbound_link_slot("L-b", false).unwrap(),
            slot_and_generation(1, 2)
        );

        counts.forget_link("L-a");
        assert_eq!(
            counts.assign_inbound_link_slot("L-c", false).unwrap(),
            slot_and_generation(0, 3)
        );
        assert_eq!(
            counts.assign_inbound_link_slot("L-b", true).unwrap(),
            slot_and_generation(1, 4),
            "a link wired again gives up its slot and takes a new generation"
        );

        let snapshot = counts.loss_count_snapshot();
        assert_eq!(
            snapshot.dropped_bags_by_inbound_link,
            BTreeMap::from([("L-b".to_string(), 0), ("L-c".to_string(), 0)]),
            "an assigned link renders zero before any board exists"
        );
        assert_eq!(
            snapshot.discarded_samples_by_inbound_link,
            BTreeMap::from([("L-b".to_string(), 0)]),
            "only a link into a windowed port carries a sample count"
        );
    }

    /// Fail-without-fix: render an output port's entry whatever generation is
    /// written on it, and a port whose channel was reopened shows the three
    /// bags its earlier channel refused.
    #[test]
    fn the_parent_reads_each_entry_only_for_the_wiring_it_assigned() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
        let counts = HelperPlacedProcessorLossCounts::default();
        counts.hold_the_board_of_this_spawn(board);
        let refused_bags_of_video = || counts.loss_count_snapshot().refused_bags_by_output_port;
        let written_bags_of_video = || counts.loss_count_snapshot().written_bags_by_output_port;

        let first_channel = counts.note_outbound_link("video", "L-first-out");
        let first_channels_mirror = writer
            .claim_output_port_entry("video", first_channel)
            .unwrap();
        first_channels_mirror.mirror_refused_bags(3);
        first_channels_mirror.mirror_written_bags(90);
        assert_eq!(
            refused_bags_of_video(),
            BTreeMap::from([("video".to_string(), 3)])
        );
        assert_eq!(
            written_bags_of_video(),
            BTreeMap::from([("video".to_string(), 90)])
        );

        counts.forget_link("L-first-out");
        let second_channel = counts.note_outbound_link("video", "L-second-out");
        assert_eq!(
            refused_bags_of_video(),
            BTreeMap::from([("video".to_string(), 0)]),
            "the earlier channel's total is not the reopened channel's"
        );
        assert_eq!(
            written_bags_of_video(),
            BTreeMap::from([("video".to_string(), 0)]),
            "the earlier channel's written total is not the reopened channel's"
        );
        writer
            .claim_output_port_entry("video", second_channel)
            .unwrap()
            .mirror_refused_bags(1);
        assert_eq!(
            refused_bags_of_video(),
            BTreeMap::from([("video".to_string(), 1)])
        );

        let inbound = counts.assign_inbound_link_slot("L-in", false).unwrap();
        writer
            .claim_inbound_link_slot(inbound)
            .unwrap()
            .mirror_dropped_bags(4);
        assert_eq!(counts.total_dropped_bag_count(), 4);
        counts.assign_inbound_link_slot("L-in", false).unwrap();
        assert_eq!(
            counts.total_dropped_bag_count(),
            0,
            "the total reads each link for its own wiring"
        );
    }

    /// A restarted helper writes on a fresh board, and each wiring's counts
    /// keep counting from what the dead helper's board last held rather than
    /// going back to zero; a reconnect still starts from zero.
    ///
    /// Fail-without-fix: keep one board per processor and the restarted
    /// helper's board is refused; drop the earlier board's counts and a link's
    /// losses go backwards in `graph` at every restart.
    #[test]
    fn a_restarted_helpers_board_keeps_each_wirings_counts_cumulative() {
        let counts = HelperPlacedProcessorLossCounts::default();
        let inbound = counts.assign_inbound_link_slot("L-in", false).unwrap();
        let video_channel = counts.note_outbound_link("video", "L-out");
        let (dead_helpers_board, dead_helpers_writer, dead_helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
        counts.hold_the_board_of_this_spawn(dead_helpers_board);
        dead_helpers_writer
            .claim_inbound_link_slot(inbound)
            .unwrap()
            .mirror_dropped_bags(6);
        dead_helpers_writer
            .claim_output_port_entry("video", video_channel)
            .unwrap()
            .mirror_written_bags(100);
        drop(dead_helpers_writer);
        drop(dead_helper_node);

        let (new_helpers_board, new_helpers_writer, _new_helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
        counts.hold_the_board_of_this_spawn(new_helpers_board);
        assert_eq!(counts.total_dropped_bag_count(), 6);
        new_helpers_writer
            .claim_inbound_link_slot(inbound)
            .unwrap()
            .mirror_dropped_bags(2);
        new_helpers_writer
            .claim_output_port_entry("video", video_channel)
            .unwrap()
            .mirror_written_bags(5);

        let snapshot = counts.loss_count_snapshot();
        assert_eq!(
            snapshot.dropped_bags_by_inbound_link,
            BTreeMap::from([("L-in".to_string(), 8)])
        );
        assert_eq!(
            snapshot.written_bags_by_output_port,
            BTreeMap::from([("video".to_string(), 105)])
        );

        counts.assign_inbound_link_slot("L-in", false).unwrap();
        assert_eq!(
            counts.total_dropped_bag_count(),
            0,
            "a reconnected link starts from zero, restarts or not"
        );
    }

    #[test]
    fn a_links_taken_bags_are_read_off_its_slot_less_the_ones_counted_lost_after_taking() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
        let counts = HelperPlacedProcessorLossCounts::default();
        counts.hold_the_board_of_this_spawn(board);
        let inbound = counts.assign_inbound_link_slot("L-in", false).unwrap();
        let mirror = writer.claim_inbound_link_slot(inbound).unwrap();

        mirror.mirror_bags_off_the_mailbox_for_its_reader(7);
        mirror.mirror_bags_its_reader_counted_lost_after_taking_them(2);
        mirror.mirror_bags_off_the_mailbox_for_its_reader(6);
        assert_eq!(
            counts.loss_count_snapshot().taken_bags_by_inbound_link,
            BTreeMap::from([("L-in".to_string(), 5)])
        );
    }

    #[test]
    fn the_helpers_step_progress_is_what_its_parent_reads_off_the_current_board() {
        let counts = HelperPlacedProcessorLossCounts::default();
        assert_eq!(counts.step_progress_its_helper_wrote(), None);
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
        counts.hold_the_board_of_this_spawn(board);

        writer.note_a_callback_started();
        assert_eq!(
            counts.step_progress_its_helper_wrote(),
            Some(HelperProcessStepProgressBoardEntry {
                steps_finished: 0,
                callbacks_in_flight: 1,
                reserved: 0,
            })
        );
        writer.note_a_callback_returned(Some(4));
        writer.note_a_callback_started();
        writer.note_a_callback_returned(None);
        assert_eq!(
            counts.step_progress_its_helper_wrote(),
            Some(HelperProcessStepProgressBoardEntry {
                steps_finished: 5,
                callbacks_in_flight: 0,
                reserved: 0,
            }),
            "a callback that names no step leaves the steps finished where they were"
        );

        let (restarted_helpers_board, _restarted_writer, _restarted_helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
        counts.hold_the_board_of_this_spawn(restarted_helpers_board);
        assert_eq!(
            counts
                .step_progress_its_helper_wrote()
                .map(|entry| entry.steps_finished),
            Some(0),
            "a restarted helper has finished no step until it finishes the one open"
        );
    }

    #[test]
    fn a_function_node_call_breadcrumb_is_set_for_the_call_and_outlives_the_helper_that_set_it() {
        let counts = HelperPlacedProcessorLossCounts::default();
        assert_eq!(
            counts.function_node_call_breadcrumb_its_helper_wrote(),
            None
        );
        let (board, writer, helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
        counts.hold_the_board_of_this_spawn(board);
        let breadcrumb_under_way = || {
            counts
                .function_node_call_breadcrumb_its_helper_wrote()
                .and_then(|entry| entry.function_node_whose_call_is_under_way())
        };
        assert_eq!(breadcrumb_under_way(), None);

        let call = writer.note_a_function_node_call_started(2, 1_000);
        assert_eq!(breadcrumb_under_way(), Some(2));
        writer.note_the_function_node_call_returned(call);
        assert_eq!(
            breadcrumb_under_way(),
            None,
            "a call that returned leaves no breadcrumb a later crash could read"
        );

        writer.note_a_function_node_call_started(5, 2_000);
        drop(writer);
        drop(helper_node);
        assert_eq!(
            breadcrumb_under_way(),
            Some(5),
            "the parent reads the call a helper died in off the board it holds"
        );
    }

    /// A node is named once three of its calls running overran the frame
    /// interval: a call inside it starts the count again, another node's
    /// overruns count apart, and a board told no interval names nobody.
    #[test]
    fn a_function_node_is_named_once_three_of_its_calls_running_overrun_the_frame_interval() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
        let call_lasting = |function_node_breadcrumb_number: u32, call_ms: u64| {
            let started_at = 1_000_000_000;
            let call = writer
                .note_a_function_node_call_started(function_node_breadcrumb_number, started_at);
            writer.note_the_function_node_call_returned_at(call, started_at + call_ms * 1_000_000);
        };

        for _ in 0..4 {
            call_lasting(1, 50);
        }
        assert_eq!(
            board
                .function_node_overrun_entry()
                .function_node_that_overran(),
            None,
            "a helper told no frame interval times nothing"
        );

        writer.time_each_function_node_call_against_the_frame_interval(
            std::time::Duration::from_millis(10),
        );
        call_lasting(1, 15);
        call_lasting(1, 25);
        call_lasting(1, 5);
        call_lasting(1, 15);
        call_lasting(2, 40);
        call_lasting(1, 15);
        assert_eq!(
            board
                .function_node_overrun_entry()
                .function_node_that_overran(),
            None,
            "a call inside the interval starts the count again, and another node's overrun \
             counts apart"
        );

        call_lasting(1, 30);
        let overrun = board.function_node_overrun_entry();
        assert_eq!(overrun.function_node_that_overran(), Some(1));
        assert_eq!(overrun.longest_overrunning_call_ns, 30_000_000);

        call_lasting(2, 40);
        call_lasting(2, 40);
        assert_eq!(
            board
                .function_node_overrun_entry()
                .function_node_that_overran(),
            Some(1),
            "the first node named stays named"
        );
    }

    /// Two calls under way at once each keep their own breadcrumb: the board
    /// names the older until it returns, and the younger returning first
    /// neither clears nor replaces it.
    #[test]
    fn concurrent_function_node_calls_each_keep_their_breadcrumb_and_the_board_names_the_oldest() {
        let counts = HelperPlacedProcessorLossCounts::default();
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
        counts.hold_the_board_of_this_spawn(board);
        let breadcrumb_under_way = || {
            counts
                .function_node_call_breadcrumb_its_helper_wrote()
                .and_then(|entry| entry.function_node_whose_call_is_under_way())
        };

        let calls_under_way = || {
            counts
                .function_node_call_breadcrumb_its_helper_wrote()
                .map(|entry| entry.function_node_calls_under_way)
        };

        let older_call = writer.note_a_function_node_call_started(1, 1_000);
        let younger_call = writer.note_a_function_node_call_started(2, 2_000);
        assert_eq!(breadcrumb_under_way(), Some(1));
        assert_eq!(
            calls_under_way(),
            Some(2),
            "the board says the oldest call is not the only one, so a death blames neither"
        );
        writer.note_the_function_node_call_returned(younger_call);
        assert_eq!(
            breadcrumb_under_way(),
            Some(1),
            "a younger call returning leaves the older one under way on the board"
        );

        let youngest_call = writer.note_a_function_node_call_started(3, 3_000);
        writer.note_the_function_node_call_returned(older_call);
        assert_eq!(breadcrumb_under_way(), Some(3));
        assert_eq!(calls_under_way(), Some(1));
        writer.note_the_function_node_call_returned(youngest_call);
        assert_eq!(breadcrumb_under_way(), None);
        assert_eq!(calls_under_way(), Some(0));
    }

    #[test]
    fn a_destination_past_the_slot_count_is_refused_by_name() {
        let counts = HelperPlacedProcessorLossCounts::default();
        for link in 0..INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD {
            counts
                .assign_inbound_link_slot(&format!("L-{link}"), false)
                .unwrap();
        }
        let refusal = counts
            .assign_inbound_link_slot("L-one-too-many", false)
            .unwrap_err();
        assert!(
            refusal.to_string().contains("L-one-too-many"),
            "the refusal names the link: {refusal}"
        );
    }

    #[test]
    fn an_output_port_keeps_its_generation_while_it_has_a_link_and_takes_a_new_one_after_its_last_goes()
     {
        let counts = HelperPlacedProcessorLossCounts::default();
        let first_channel = counts.note_outbound_link("video", "L-first");
        assert_eq!(
            counts.note_outbound_link("video", "L-second"),
            first_channel
        );
        counts.forget_link("L-first");
        assert_eq!(
            counts.loss_count_snapshot().refused_bags_by_output_port,
            BTreeMap::from([("video".to_string(), 0)])
        );

        counts.forget_link("L-second");
        assert!(
            counts
                .loss_count_snapshot()
                .refused_bags_by_output_port
                .is_empty()
        );
        assert_ne!(
            counts.note_outbound_link("video", "L-third"),
            first_channel,
            "a reopened channel is a new wiring"
        );
    }
}

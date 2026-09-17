// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The blackboard a helper process mirrors its loss counts onto, and what its
//! parent reads `graph`'s `metrics` off.
//!
//! One board per helper spawn: the parent creates it before the child starts
//! and holds it until the processor is removed, so the counts a crashed helper
//! last wrote stay readable. Its keys are fixed at creation and links are wired
//! live, so the board declares a slot per permitted inbound link and an entry
//! per declared output port, and the parent assigns each inbound link a slot and
//! each output port's channel a wiring generation. An entry renders as its
//! link's or port's only while the generation written there is the one the
//! parent assigned.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use iceoryx2::port::reader::{EntryHandle, Reader};
use iceoryx2::port::writer::{EntryHandleMut, Writer};
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::blackboard::PortFactory as BlackboardPortFactory;
use parking_lot::Mutex;
use streamlib_ipc_types::{
    HelperProcessLossCountBoardKey, InboundLinkLossCountBoardSlot,
    MAX_INBOUND_LINKS_PER_DESTINATION, OutputPortRefusedBagCountBoardEntry,
};

use crate::core::error::{Error, Result};

/// The inbound-link slots every board declares: one per inbound link a
/// destination may hold.
pub(crate) const INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD: usize = MAX_INBOUND_LINKS_PER_DESTINATION;

/// The blackboard service a loss-count board is.
pub(crate) type HelperProcessLossCountBoardService =
    BlackboardPortFactory<ipc::Service, HelperProcessLossCountBoardKey>;

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
        EntryHandle<ipc::Service, HelperProcessLossCountBoardKey, InboundLinkLossCountBoardSlot>,
    >,
    output_ports: Vec<(
        String,
        EntryHandle<
            ipc::Service,
            HelperProcessLossCountBoardKey,
            OutputPortRefusedBagCountBoardEntry,
        >,
    )>,
    _reader: Reader<ipc::Service, HelperProcessLossCountBoardKey>,
    _service: HelperProcessLossCountBoardService,
}

// SAFETY: `Reader` is the one field iceoryx2 0.9.3 leaves `!Send` and `!Sync`,
// because `ipc::Service`'s `SingleThreaded` policy keeps its shared state in a
// bare `Rc`. Every `EntryHandle` holds a clone of that same `Rc`; iceoryx2
// declares the handle `Send + Sync` all the same, which is sound only while no
// handle ever leaves this struct on its own — two handles dropped on two threads
// race the count. Every clone is made while this struct is built, on one thread,
// and every one is dropped when it drops, in one `Drop`. A read,
// `EntryHandle::get`, loads the entry's lock-free atomic and never touches the
// `Rc`, which is what lets `graph` read from a tokio worker. The blackboard
// `PortFactory` is `Send + Sync` in its own right. Re-check this against the
// source on any iceoryx2 upgrade.
unsafe impl Send for HelperProcessLossCountBoardReadHandles {}
unsafe impl Sync for HelperProcessLossCountBoardReadHandles {}

impl HelperProcessLossCountBoard {
    /// Take a read handle on every entry of the board `service` just created.
    pub(crate) fn reading(
        service_name: String,
        output_port_names: Vec<String>,
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
        Ok(Self {
            service_name,
            board_read_handles: HelperProcessLossCountBoardReadHandles {
                inbound_link_slots,
                output_ports,
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
    pub fn output_port_entry(
        &self,
        output_port: &str,
    ) -> Option<OutputPortRefusedBagCountBoardEntry> {
        self.board_read_handles
            .output_ports
            .iter()
            .find(|(declared, _)| declared == output_port)
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
    entry:
        EntryHandleMut<ipc::Service, HelperProcessLossCountBoardKey, InboundLinkLossCountBoardSlot>,
    last_written: InboundLinkLossCountBoardSlot,
}

/// One output port's entry, and what was last written on it.
struct OutputPortRefusedBagCountBoardEntryBeingWritten {
    entry: EntryHandleMut<
        ipc::Service,
        HelperProcessLossCountBoardKey,
        OutputPortRefusedBagCountBoardEntry,
    >,
    last_written: OutputPortRefusedBagCountBoardEntry,
}

/// The writer and its entry handles, dropped together.
struct HelperProcessLossCountBoardWriteHandles {
    inbound_link_slots: Vec<Mutex<InboundLinkLossCountBoardSlotBeingWritten>>,
    output_ports: Vec<(
        String,
        Mutex<OutputPortRefusedBagCountBoardEntryBeingWritten>,
    )>,
    _writer: Writer<ipc::Service, HelperProcessLossCountBoardKey>,
    _service: HelperProcessLossCountBoardService,
}

// SAFETY: `Writer` is the one field iceoryx2 0.9.3 leaves `!Send` and `!Sync`,
// for the `SingleThreaded` `Rc` its shared state sits in. Every `EntryHandleMut`
// holds a clone of that `Rc` and is declared `Send + Sync` upstream regardless,
// which is sound only while no handle leaves this struct on its own. Every
// clone is made while this struct is built, on one thread, and dropped when it
// drops, in one `Drop`. A write, `EntryHandleMut::update_with_copy`, stores
// through the entry's producer and never touches the `Rc`; the producer is
// single-writer, which each entry's `Mutex` serialises. The blackboard
// `PortFactory` is `Send + Sync` in its own right.
unsafe impl Send for HelperProcessLossCountBoardWriteHandles {}
unsafe impl Sync for HelperProcessLossCountBoardWriteHandles {}

impl HelperProcessLossCountBoardWriter {
    /// Take the write handle of every entry on the board `service` opened.
    pub(crate) fn writing(
        service_name: String,
        output_port_names: &[String],
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
                        Mutex::new(OutputPortRefusedBagCountBoardEntryBeingWritten {
                            entry,
                            last_written: OutputPortRefusedBagCountBoardEntry::default(),
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
            dropped_bags: 0,
            discarded_samples: 0,
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
    /// writing a zero count under it, and hand back the mirror its refused-bag
    /// counter writes through — or `None` for a port this board carries no
    /// entry for.
    ///
    /// Every mirror an earlier channel of the port handed out writes nothing
    /// from here on.
    pub fn claim_output_port_entry(
        self: &Arc<Self>,
        output_port: &str,
        wiring_generation: u64,
    ) -> Option<OutputPortRefusedBagCountBoardMirror> {
        let (output_port_index, (_, entry_being_written)) = self
            .board_write_handles
            .output_ports
            .iter()
            .enumerate()
            .find(|(_, (declared, _))| declared == output_port)?;
        let mut entry_being_written = entry_being_written.lock();
        entry_being_written.last_written = OutputPortRefusedBagCountBoardEntry {
            wiring_generation,
            refused_bags: 0,
        };
        entry_being_written
            .entry
            .update_with_copy(entry_being_written.last_written);
        Some(OutputPortRefusedBagCountBoardMirror {
            board: Arc::clone(self),
            output_port_index,
            wiring_generation,
        })
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

/// Where one channel of an output port writes its refused-bag count: the
/// port's entry, under the channel's generation.
pub struct OutputPortRefusedBagCountBoardMirror {
    board: Arc<HelperProcessLossCountBoardWriter>,
    output_port_index: usize,
    wiring_generation: u64,
}

impl OutputPortRefusedBagCountBoardMirror {
    /// Write `refused_bags` as the port's refused-bag total, kept at its
    /// largest for the reason an inbound slot's totals are.
    pub(crate) fn mirror_refused_bags(&self, refused_bags: u64) {
        let (_, entry_being_written) =
            &self.board.board_write_handles.output_ports[self.output_port_index];
        let mut entry_being_written = entry_being_written.lock();
        if entry_being_written.last_written.wiring_generation != self.wiring_generation {
            return;
        }
        entry_being_written.last_written.refused_bags = entry_being_written
            .last_written
            .refused_bags
            .max(refused_bags);
        entry_being_written
            .entry
            .update_with_copy(entry_being_written.last_written);
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
/// each inbound link was assigned, the output ports with a link, and the board
/// the helper writes on.
#[derive(Default)]
pub struct HelperPlacedProcessorLossCounts {
    wiring: Mutex<HelperPlacedProcessorLossCountWiring>,
    board: OnceLock<HelperProcessLossCountBoard>,
}

impl std::fmt::Debug for HelperPlacedProcessorLossCounts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HelperPlacedProcessorLossCounts")
            .field(
                "board",
                &self
                    .board
                    .get()
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
    /// Samples discarded per inbound link into a windowed port, and no other.
    pub discarded_samples_by_inbound_link: BTreeMap<String, u64>,
    /// Bags refused at the ceiling per output port with a channel.
    pub refused_bags_by_output_port: BTreeMap<String, u64>,
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
    }

    /// Hold the board this spawn's helper writes on.
    pub(crate) fn hold_the_board_of_this_spawn(
        &self,
        board: HelperProcessLossCountBoard,
    ) -> Result<()> {
        self.board.set(board).map_err(|refused| {
            Error::Runtime(format!(
                "a helper-placed processor already holds a loss-count board, so the board \
                 '{}' of a second spawn has nowhere to be read",
                refused.service_name()
            ))
        })
    }

    /// Every count as it stands, each read only for the wiring it belongs to.
    pub fn loss_count_snapshot(&self) -> ProcessorLossCountSnapshot {
        let wiring = self.wiring.lock();
        let board = self.board.get();
        let mut snapshot = ProcessorLossCountSnapshot::default();
        for (assignment, this_wirings_counts) in
            wiring.assigned_slots_with_their_wirings_counts(board)
        {
            snapshot
                .dropped_bags_by_inbound_link
                .insert(assignment.link_id.clone(), this_wirings_counts.dropped_bags);
            if assignment.into_a_windowed_port {
                snapshot.discarded_samples_by_inbound_link.insert(
                    assignment.link_id.clone(),
                    this_wirings_counts.discarded_samples,
                );
            }
        }
        for (output_port, port) in &wiring.output_ports_with_outbound_links {
            let refused_bags = board
                .and_then(|board| board.output_port_entry(output_port))
                .filter(|written| written.wiring_generation == port.wiring_generation)
                .map_or(0, |written| written.refused_bags);
            snapshot
                .refused_bags_by_output_port
                .insert(output_port.clone(), refused_bags);
        }
        snapshot
    }

    /// This processor's dropped bags across every inbound link, each read for
    /// its link's own wiring.
    pub fn total_dropped_bag_count(&self) -> u64 {
        self.wiring
            .lock()
            .assigned_slots_with_their_wirings_counts(self.board.get())
            .map(|(_, this_wirings_counts)| this_wirings_counts.dropped_bags)
            .sum()
    }
}

/// A board as a parent creates it and a writer as its helper opens it, each
/// from its own node in this test process's domain, with the helper's node
/// handed back so a test can let it go.
#[cfg(test)]
pub(crate) fn a_loss_count_board_and_its_helpers_writer_for_this_test_process(
    output_port_names: &[&str],
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
        .create_helper_process_loss_count_board(&service_name, output_port_names.clone())
        .expect("the parent creates its helper's board");
    let helper_node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
    let writer = helper_node
        .open_helper_process_loss_count_board_writer(&service_name, &output_port_names)
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
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"]);

        let mirror = writer
            .claim_inbound_link_slot(slot_and_generation(3, 7))
            .unwrap();
        assert_eq!(
            board.inbound_link_slot(3),
            Some(InboundLinkLossCountBoardSlot {
                wiring_generation: 7,
                dropped_bags: 0,
                discarded_samples: 0,
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
            }),
            "a total that arrives late never lowers the count"
        );

        writer
            .claim_output_port_entry("video", 8)
            .unwrap()
            .mirror_refused_bags(2);
        assert_eq!(
            board.output_port_entry("video"),
            Some(OutputPortRefusedBagCountBoardEntry {
                wiring_generation: 8,
                refused_bags: 2,
            })
        );
        assert_eq!(board.output_port_entry("audio"), None);
        assert!(writer.claim_output_port_entry("audio", 9).is_none());
    }

    #[test]
    fn a_write_from_a_wiring_the_slot_has_moved_past_lands_nothing() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[]);
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
                discarded_samples: 0,
            }),
            "the slot carries only the wiring that claimed it last"
        );
    }

    #[test]
    fn an_output_port_claimed_again_starts_from_zero_and_its_earlier_mirror_lands_nothing() {
        let (board, writer, _helper_node) =
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"]);
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
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"]);
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
            a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"]);
        let counts = HelperPlacedProcessorLossCounts::default();
        counts.hold_the_board_of_this_spawn(board).unwrap();
        let refused_bags_of_video = || counts.loss_count_snapshot().refused_bags_by_output_port;

        let first_channel = counts.note_outbound_link("video", "L-first-out");
        writer
            .claim_output_port_entry("video", first_channel)
            .unwrap()
            .mirror_refused_bags(3);
        assert_eq!(
            refused_bags_of_video(),
            BTreeMap::from([("video".to_string(), 3)])
        );

        counts.forget_link("L-first-out");
        let second_channel = counts.note_outbound_link("video", "L-second-out");
        assert_eq!(
            refused_bags_of_video(),
            BTreeMap::from([("video".to_string(), 0)]),
            "the earlier channel's total is not the reopened channel's"
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

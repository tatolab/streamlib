// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The blackboard a helper process mirrors its loss counts onto, and what its
//! parent reads `graph`'s `metrics` off.
//!
//! One board per helper spawn: the parent creates it before the child starts
//! and holds it until the processor is removed, so the counts a crashed helper
//! last wrote stay readable. Its keys are fixed at creation and links are wired
//! live, so the board declares a slot per permitted inbound link and the parent
//! assigns each link a slot and a wiring generation; a slot renders as its link's
//! only while the generation written there is the one the parent assigned.

use std::collections::{BTreeMap, HashMap};
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
pub const INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD: usize = MAX_INBOUND_LINKS_PER_DESTINATION;

/// The blackboard service a loss-count board is.
pub(crate) type HelperProcessLossCountBoardService =
    BlackboardPortFactory<ipc::Service, HelperProcessLossCountBoardKey>;

/// The key of inbound-link slot `slot`.
fn inbound_link_slot_key(slot: usize) -> HelperProcessLossCountBoardKey {
    HelperProcessLossCountBoardKey::inbound_link_slot(slot as u32)
}

/// The key of the declared output port at `output_port_index`.
fn output_port_key(output_port_index: usize) -> HelperProcessLossCountBoardKey {
    HelperProcessLossCountBoardKey::output_port(output_port_index as u32)
}

// =============================================================================
// The parent's board
// =============================================================================

/// A helper spawn's loss-count board as the parent holds it: the service it
/// created, and a read handle on every entry.
pub struct HelperProcessLossCountBoard {
    service_name: String,
    output_port_names: Vec<String>,
    held: HelperProcessLossCountBoardReadHandles,
}

/// The reader and its entry handles, dropped together.
///
/// Fields drop in declaration order: the handles, the reader they came from,
/// then the service.
struct HelperProcessLossCountBoardReadHandles {
    inbound_link_slots: Vec<
        EntryHandle<ipc::Service, HelperProcessLossCountBoardKey, InboundLinkLossCountBoardSlot>,
    >,
    output_ports: Vec<
        EntryHandle<
            ipc::Service,
            HelperProcessLossCountBoardKey,
            OutputPortRefusedBagCountBoardEntry,
        >,
    >,
    _reader: Reader<ipc::Service, HelperProcessLossCountBoardKey>,
    _service: HelperProcessLossCountBoardService,
}

// SAFETY: `Reader` and `EntryHandle` are `!Send` only through the reader's
// shared state, which `ipc::Service`'s `SingleThreaded` policy keeps in a bare
// `Rc` (iceoryx2 0.9.3). Every clone of that `Rc` is made while this struct is
// built, on one thread, and every one is dropped when this struct drops, in one
// `Drop` — so its count is never touched from two threads at once. A read,
// `EntryHandle::get`, loads the entry's lock-free atomic and never touches the
// `Rc`, which is what lets `graph` read from a tokio worker. Re-check this
// against the source on any iceoryx2 upgrade.
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
        let inbound_link_slots = (0..INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD)
            .map(|slot| reader.entry(&inbound_link_slot_key(slot)))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unreadable_entry)?;
        let output_ports = (0..output_port_names.len())
            .map(|output_port_index| reader.entry(&output_port_key(output_port_index)))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unreadable_entry)?;
        Ok(Self {
            service_name,
            output_port_names,
            held: HelperProcessLossCountBoardReadHandles {
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
    pub fn output_port_names(&self) -> &[String] {
        &self.output_port_names
    }

    /// What the helper last wrote on inbound-link slot `slot`.
    pub fn inbound_link_slot(&self, slot: usize) -> Option<InboundLinkLossCountBoardSlot> {
        self.held
            .inbound_link_slots
            .get(slot)
            .map(|entry| *entry.get())
    }

    /// The refused-bag count the helper last wrote for `output_port`.
    pub fn output_port_refused_bags(&self, output_port: &str) -> Option<u64> {
        let output_port_index = self
            .output_port_names
            .iter()
            .position(|declared| declared == output_port)?;
        Some(self.held.output_ports[output_port_index].get().refused_bags)
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
    held: HelperProcessLossCountBoardWriteHandles,
}

/// One inbound-link slot, and what was last written on it.
struct InboundLinkLossCountBoardSlotBeingWritten {
    entry:
        EntryHandleMut<ipc::Service, HelperProcessLossCountBoardKey, InboundLinkLossCountBoardSlot>,
    last_written: InboundLinkLossCountBoardSlot,
}

/// One output port's entry, what was last written on it, and which claim of it
/// is current.
struct OutputPortRefusedBagCountBoardEntryBeingWritten {
    entry: EntryHandleMut<
        ipc::Service,
        HelperProcessLossCountBoardKey,
        OutputPortRefusedBagCountBoardEntry,
    >,
    last_written_refused_bags: u64,
    current_claim: u64,
}

/// The writer and its entry handles, dropped together.
struct HelperProcessLossCountBoardWriteHandles {
    inbound_link_slots: Vec<Mutex<InboundLinkLossCountBoardSlotBeingWritten>>,
    output_ports: HashMap<String, Mutex<OutputPortRefusedBagCountBoardEntryBeingWritten>>,
    _writer: Writer<ipc::Service, HelperProcessLossCountBoardKey>,
    _service: HelperProcessLossCountBoardService,
}

// SAFETY: as for `HelperProcessLossCountBoardReadHandles`, with the writer's
// `Rc` in place of the reader's: every clone is made while this struct is built
// and dropped when it drops. A write, `EntryHandleMut::update_with_copy`, stores
// through the entry's single producer and never touches the `Rc`; the producer
// is single-writer, which is what each entry's `Mutex` serialises.
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
        let inbound_link_slots = (0..INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD)
            .map(|slot| {
                writer.entry(&inbound_link_slot_key(slot)).map(|entry| {
                    Mutex::new(InboundLinkLossCountBoardSlotBeingWritten {
                        entry,
                        last_written: InboundLinkLossCountBoardSlot::default(),
                    })
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(unwritable_entry)?;
        let output_ports = output_port_names
            .iter()
            .enumerate()
            .map(|(output_port_index, output_port)| {
                writer
                    .entry(&output_port_key(output_port_index))
                    .map(|entry| {
                        (
                            output_port.clone(),
                            Mutex::new(OutputPortRefusedBagCountBoardEntryBeingWritten {
                                entry,
                                last_written_refused_bags: 0,
                                current_claim: 0,
                            }),
                        )
                    })
            })
            .collect::<std::result::Result<HashMap<_, _>, _>>()
            .map_err(unwritable_entry)?;
        Ok(Self {
            service_name,
            held: HelperProcessLossCountBoardWriteHandles {
                inbound_link_slots,
                output_ports,
                _writer: writer,
                _service: service,
            },
        })
    }

    /// Claim inbound-link slot `slot` for the wiring `wiring_generation` names,
    /// writing zero counts under it, and hand back the mirror that link's
    /// counters write through.
    ///
    /// Every mirror an earlier wiring of the slot handed out writes nothing from
    /// here on.
    pub fn claim_inbound_link_slot(
        self: &Arc<Self>,
        slot: usize,
        wiring_generation: u64,
    ) -> Result<InboundLinkLossCountBoardSlotMirror> {
        let Some(slot_being_written) = self.held.inbound_link_slots.get(slot) else {
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

    /// Claim `output_port`'s entry for a channel just opened, writing a zero
    /// count, and hand back the mirror its refused-bag counter writes through —
    /// or `None` for a port this board carries no entry for.
    ///
    /// Every mirror an earlier claim of the port handed out writes nothing from
    /// here on.
    pub fn claim_output_port_entry(
        self: &Arc<Self>,
        output_port: &str,
    ) -> Option<OutputPortRefusedBagCountBoardMirror> {
        let mut entry_being_written = self.held.output_ports.get(output_port)?.lock();
        entry_being_written.current_claim += 1;
        entry_being_written.last_written_refused_bags = 0;
        entry_being_written
            .entry
            .update_with_copy(OutputPortRefusedBagCountBoardEntry { refused_bags: 0 });
        Some(OutputPortRefusedBagCountBoardMirror {
            board: Arc::clone(self),
            output_port: output_port.to_string(),
            claim: entry_being_written.current_claim,
        })
    }
}

/// Where one wiring of an inbound link writes its loss counts: its slot, under
/// its generation.
pub struct InboundLinkLossCountBoardSlotMirror {
    board: Arc<HelperProcessLossCountBoardWriter>,
    slot: usize,
    wiring_generation: u64,
}

impl InboundLinkLossCountBoardSlotMirror {
    /// Write `dropped_bags` as this link's dropped-bag total.
    pub fn mirror_dropped_bags(&self, dropped_bags: u64) {
        self.write_on_the_slot_while_it_is_this_wirings(|last_written| {
            last_written.dropped_bags = last_written.dropped_bags.max(dropped_bags);
        });
    }

    /// Write `discarded_samples` as this link's discarded-sample total.
    pub fn mirror_discarded_samples(&self, discarded_samples: u64) {
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
        let mut slot_being_written = self.board.held.inbound_link_slots[self.slot].lock();
        if slot_being_written.last_written.wiring_generation != self.wiring_generation {
            return;
        }
        update(&mut slot_being_written.last_written);
        slot_being_written
            .entry
            .update_with_copy(slot_being_written.last_written);
    }
}

/// Where one channel of an output port writes its refused-bag count.
pub struct OutputPortRefusedBagCountBoardMirror {
    board: Arc<HelperProcessLossCountBoardWriter>,
    output_port: String,
    claim: u64,
}

impl OutputPortRefusedBagCountBoardMirror {
    /// Write `refused_bags` as the port's refused-bag total.
    pub fn mirror_refused_bags(&self, refused_bags: u64) {
        let Some(entry_being_written) = self.board.held.output_ports.get(&self.output_port) else {
            return;
        };
        let mut entry_being_written = entry_being_written.lock();
        if entry_being_written.current_claim != self.claim {
            return;
        }
        entry_being_written.last_written_refused_bags = entry_being_written
            .last_written_refused_bags
            .max(refused_bags);
        let refused_bags = entry_being_written.last_written_refused_bags;
        entry_being_written
            .entry
            .update_with_copy(OutputPortRefusedBagCountBoardEntry { refused_bags });
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

/// The slots and output ports a helper-placed processor's links hold.
struct HelperPlacedProcessorLossCountWiring {
    inbound_link_slots: Vec<Option<InboundLinkLossCountSlotAssignment>>,
    last_assigned_wiring_generation: u64,
    outbound_link_ids_by_output_port: BTreeMap<String, Vec<String>>,
}

impl Default for HelperPlacedProcessorLossCountWiring {
    fn default() -> Self {
        Self {
            inbound_link_slots: vec![None; INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD],
            last_assigned_wiring_generation: 0,
            outbound_link_ids_by_output_port: BTreeMap::new(),
        }
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
    /// Give `link_id` the lowest free slot and a generation no wiring has had,
    /// and hand both back.
    ///
    /// A link already holding a slot gives it up first, so its counts start
    /// again from zero.
    pub(crate) fn assign_inbound_link_slot(
        &self,
        link_id: &str,
        into_a_windowed_port: bool,
    ) -> Result<(usize, u64)> {
        let mut wiring = self.wiring.lock();
        release_inbound_link_slot(&mut wiring, link_id);
        let Some(slot) = wiring.inbound_link_slots.iter().position(Option::is_none) else {
            return Err(Error::Configuration(format!(
                "link '{link_id}' would be a helper-placed processor's inbound link past the \
                 {INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD} its loss-count board has slots for"
            )));
        };
        wiring.last_assigned_wiring_generation += 1;
        let wiring_generation = wiring.last_assigned_wiring_generation;
        wiring.inbound_link_slots[slot] = Some(InboundLinkLossCountSlotAssignment {
            link_id: link_id.to_string(),
            wiring_generation,
            into_a_windowed_port,
        });
        Ok((slot, wiring_generation))
    }

    /// Note an outbound link out of `output_port`, so the port renders its
    /// refused-bag count while it has one.
    pub(crate) fn note_outbound_link(&self, output_port: &str, link_id: &str) {
        let mut wiring = self.wiring.lock();
        let links = wiring
            .outbound_link_ids_by_output_port
            .entry(output_port.to_string())
            .or_default();
        if !links.iter().any(|noted| noted == link_id) {
            links.push(link_id.to_string());
        }
    }

    /// Forget a disconnected link in both directions: its slot goes free and an
    /// output port it was the last link of stops rendering.
    pub(crate) fn forget_link(&self, link_id: &str) {
        let mut wiring = self.wiring.lock();
        release_inbound_link_slot(&mut wiring, link_id);
        wiring.outbound_link_ids_by_output_port.retain(|_, links| {
            links.retain(|noted| noted != link_id);
            !links.is_empty()
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

    /// Every count as it stands: a slot's counts are its link's only while the
    /// generation written there is the one this link was assigned, and zero
    /// otherwise — before the board exists, before the helper wired the link,
    /// and after a write from a wiring the slot has moved past.
    pub fn loss_count_snapshot(&self) -> ProcessorLossCountSnapshot {
        let wiring = self.wiring.lock();
        let board = self.board.get();
        let mut snapshot = ProcessorLossCountSnapshot::default();
        for (slot, assignment) in wiring.inbound_link_slots.iter().enumerate() {
            let Some(assignment) = assignment else {
                continue;
            };
            let this_wirings_counts = board
                .and_then(|board| board.inbound_link_slot(slot))
                .filter(|written| written.wiring_generation == assignment.wiring_generation)
                .unwrap_or_default();
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
        for output_port in wiring.outbound_link_ids_by_output_port.keys() {
            snapshot.refused_bags_by_output_port.insert(
                output_port.clone(),
                board
                    .and_then(|board| board.output_port_refused_bags(output_port))
                    .unwrap_or(0),
            );
        }
        snapshot
    }
}

fn release_inbound_link_slot(wiring: &mut HelperPlacedProcessorLossCountWiring, link_id: &str) {
    for slot in &mut wiring.inbound_link_slots {
        if slot
            .as_ref()
            .is_some_and(|assignment| assignment.link_id == link_id)
        {
            *slot = None;
        }
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

    fn a_board_and_its_helpers_writer(
        output_port_names: &[&str],
    ) -> (
        HelperProcessLossCountBoard,
        Arc<HelperProcessLossCountBoardWriter>,
        crate::iceoryx2::Iceoryx2Node,
    ) {
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(output_port_names)
    }

    #[test]
    fn a_count_written_on_a_claimed_slot_is_what_the_parent_reads() {
        let (board, writer, _helper_node) = a_board_and_its_helpers_writer(&["video"]);

        let mirror = writer.claim_inbound_link_slot(3, 7).unwrap();
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

        let video = writer.claim_output_port_entry("video").unwrap();
        video.mirror_refused_bags(2);
        assert_eq!(board.output_port_refused_bags("video"), Some(2));
        assert_eq!(board.output_port_refused_bags("audio"), None);
        assert!(writer.claim_output_port_entry("audio").is_none());
    }

    #[test]
    fn a_write_from_a_wiring_the_slot_has_moved_past_lands_nothing() {
        let (board, writer, _helper_node) = a_board_and_its_helpers_writer(&[]);
        let first_wiring = writer.claim_inbound_link_slot(0, 1).unwrap();
        first_wiring.mirror_dropped_bags(9);

        let second_wiring = writer.claim_inbound_link_slot(0, 2).unwrap();
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
        let (board, writer, _helper_node) = a_board_and_its_helpers_writer(&["video"]);
        let first_channel = writer.claim_output_port_entry("video").unwrap();
        first_channel.mirror_refused_bags(3);

        let second_channel = writer.claim_output_port_entry("video").unwrap();
        assert_eq!(board.output_port_refused_bags("video"), Some(0));
        first_channel.mirror_refused_bags(4);
        assert_eq!(board.output_port_refused_bags("video"), Some(0));
        second_channel.mirror_refused_bags(1);
        assert_eq!(board.output_port_refused_bags("video"), Some(1));
    }

    #[test]
    fn the_last_counts_stay_readable_after_the_helpers_writer_is_gone() {
        let (board, writer, helper_node) = a_board_and_its_helpers_writer(&["video"]);
        writer
            .claim_inbound_link_slot(1, 4)
            .unwrap()
            .mirror_dropped_bags(6);
        writer
            .claim_output_port_entry("video")
            .unwrap()
            .mirror_refused_bags(2);

        drop(writer);
        drop(helper_node);

        assert_eq!(
            board.inbound_link_slot(1).map(|slot| slot.dropped_bags),
            Some(6)
        );
        assert_eq!(board.output_port_refused_bags("video"), Some(2));
    }

    #[test]
    fn slots_are_reused_lowest_first_and_every_wiring_gets_a_new_generation() {
        let counts = HelperPlacedProcessorLossCounts::default();
        assert_eq!(
            counts.assign_inbound_link_slot("L-a", false).unwrap(),
            (0, 1)
        );
        assert_eq!(
            counts.assign_inbound_link_slot("L-b", false).unwrap(),
            (1, 2)
        );

        counts.forget_link("L-a");
        assert_eq!(
            counts.assign_inbound_link_slot("L-c", false).unwrap(),
            (0, 3)
        );
        assert_eq!(
            counts.assign_inbound_link_slot("L-b", true).unwrap(),
            (1, 4),
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
    fn an_output_port_renders_while_it_has_a_link_and_not_after_its_last_goes() {
        let counts = HelperPlacedProcessorLossCounts::default();
        counts.note_outbound_link("video", "L-first");
        counts.note_outbound_link("video", "L-second");
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
    }
}

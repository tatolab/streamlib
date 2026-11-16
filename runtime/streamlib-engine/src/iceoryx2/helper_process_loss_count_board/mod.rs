// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

mod helper_placed_processor_loss_counts;
mod helper_process_loss_count_board_writer;

pub use helper_placed_processor_loss_counts::{
    HelperPlacedProcessorLossCounts, ProcessorLossCountSnapshot,
};
pub use helper_process_loss_count_board_writer::{
    FunctionNodeCallUnderWayInThisHelper, HelperProcessLossCountBoardWriter,
    InboundLinkLossCountBoardSlotMirror, OutputPortBagCountBoardMirror,
};

#[cfg(test)]
use std::sync::Arc;

use iceoryx2::port::reader::{EntryHandle, Reader};
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::blackboard::PortFactory as BlackboardPortFactory;
use streamlib_ipc_types::{
    HelperProcessFunctionNodeCallBreadcrumbBoardEntry, HelperProcessFunctionNodeOverrunBoardEntry,
    HelperProcessFunctionNodeStateBoardEntry, HelperProcessLossCountBoardKey,
    HelperProcessStepProgressBoardEntry, InboundLinkLossCountBoardSlot,
    MAX_INBOUND_LINKS_PER_DESTINATION, OutputPortBagCountBoardEntry,
};

use crate::core::error::{Error, Result};

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
mod tests;

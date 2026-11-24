// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use streamlib::sdk::iceoryx2::{
    HelperProcessLossCountBoardWriter, Iceoryx2Node, InputMailboxesInner, OutputWriterInner,
    StreamClockBoardPublishListener,
};

use super::{
    HelperProcessDataPlaneLentToNativeCode, PythonNodeLinkDataAccess,
    declared_port_name_the_spelling_names, not_a_helper_process_data_plane_error,
    unwired_port_error,
};
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;

impl PythonNodeLinkDataAccess {
    pub(crate) fn new() -> Self {
        Self {
            input_mailboxes: OnceLock::new(),
            output_writer: OnceLock::new(),
            iceoryx2_node: OnceLock::new(),
            loss_count_board_writer: OnceLock::new(),
            function_node_states_resumed_from: OnceLock::new(),
            stream_clock_board_publish_listener: OnceLock::new(),
            declared_input_ports: parking_lot::Mutex::new(HashSet::new()),
            declared_output_ports: parking_lot::Mutex::new(HashSet::new()),
            input_ports_lent_to_native_code: parking_lot::Mutex::new(HashSet::new()),
            output_ports_lent_to_native_code: parking_lot::Mutex::new(HashSet::new()),
            runtime_drives_process_from_inputs: std::sync::atomic::AtomicBool::new(false),
            values_last_read_off_links_into_its_function_node_islands: Arc::default(),
            function_node_raises_in_this_helper: Arc::default(),
            gpu_exchange_client_of_this_helper: Arc::new(OnceLock::new()),
        }
    }

    /// The control value or record each reader input port of this helper last
    /// carried into an island, which an island replacing it starts from.
    pub(crate) fn values_last_read_off_links_into_its_function_node_islands(
        &self,
    ) -> crate::python_function_node_island::ValuesLastReadOffLinksFromOutsideIslands {
        Arc::clone(&self.values_last_read_off_links_into_its_function_node_islands)
    }

    /// What each function node this helper's islands call raised at its last
    /// call, which the parent shows on the function node.
    pub(crate) fn function_node_raises_in_this_helper(
        &self,
    ) -> Arc<crate::python_function_node_island::FunctionNodeRaisesInThisHelper> {
        Arc::clone(&self.function_node_raises_in_this_helper)
    }

    /// Hand the islands this helper feeds the client its runtime context
    /// opened, which they claim the pooled GPU frames they hold through.
    pub(crate) fn note_the_gpu_exchange_client_of_this_helper(
        &self,
        gpu_exchange_client: Arc<HelperProcessGpuExchangeClient>,
    ) {
        let _ = self
            .gpu_exchange_client_of_this_helper
            .set(gpu_exchange_client);
    }

    /// Claims the pooled GPU frame a message an island or a bundle this
    /// helper's mailboxes hold names, through this helper's GPU exchange
    /// client; nothing is claimed before one opened.
    pub(crate) fn claim_of_the_pooled_gpu_frames_its_islands_hold(
        &self,
    ) -> crate::python_function_node_island::ClaimOfThePooledGpuFrameAMessageNames {
        let gpu_exchange_client_of_this_helper =
            Arc::clone(&self.gpu_exchange_client_of_this_helper);
        Box::new(move |pooled_gpu_frame| {
            let Some(gpu_exchange_client) = gpu_exchange_client_of_this_helper.get() else {
                return Ok(None);
            };
            Ok(gpu_exchange_client
                .note_and_claim_the_pooled_gpu_frame_a_read_carries_refusing_by_message(
                    pooled_gpu_frame,
                )?
                .map(|claim| {
                    Box::new(claim)
                        as crate::python_function_node_island::PooledGpuFrameClaimAnIslandHolds
                }))
        })
    }

    /// This helper's input mailboxes; `None` before a link wired one.
    pub(crate) fn input_mailboxes_of_this_helper(&self) -> Option<Arc<InputMailboxesInner>> {
        self.input_mailboxes.get().cloned()
    }

    pub(crate) fn output_writer_of_this_helper(&self) -> Option<Arc<OutputWriterInner>> {
        self.output_writer.get().cloned()
    }

    /// The loss-count board this helper notes its step callbacks on, once its
    /// setup opened it.
    pub(crate) fn loss_count_board_writer_of_this_helper(
        &self,
    ) -> Option<&HelperProcessLossCountBoardWriter> {
        self.loss_count_board_writer.get().map(Arc::as_ref)
    }

    /// The state the function node numbered `function_node_breadcrumb_number`
    /// resumes from, when its stream carries one for it.
    pub(crate) fn the_state_a_function_node_resumes_from(
        &self,
        function_node_breadcrumb_number: u32,
    ) -> Option<Vec<f64>> {
        self.function_node_states_resumed_from
            .get()?
            .get(&function_node_breadcrumb_number)
            .cloned()
    }

    /// The loss-count board this helper writes, shared with what notes on it
    /// past this call, once its setup opened it.
    pub(crate) fn shared_loss_count_board_writer_of_this_helper(
        &self,
    ) -> Option<Arc<HelperProcessLossCountBoardWriter>> {
        self.loss_count_board_writer.get().cloned()
    }

    /// The wake on each publish of this helper's stream clock, once it listens
    /// for them.
    pub(crate) fn stream_clock_board_publish_listener_of_this_helper(
        &self,
    ) -> Option<&StreamClockBoardPublishListener> {
        self.stream_clock_board_publish_listener.get()
    }

    /// Record whether the runtime drives `process()` from this node's inputs,
    /// refusing it when an input is already lent to native code.
    pub(crate) fn note_whether_the_runtime_drives_process_from_inputs(
        &self,
        runtime_drives_process_from_inputs: bool,
    ) -> PyResult<()> {
        if runtime_drives_process_from_inputs {
            let lent_input_ports = self.input_ports_lent_to_native_code.lock();
            if !lent_input_ports.is_empty() {
                let mut lent: Vec<&String> = lent_input_ports.iter().collect();
                lent.sort();
                return Err(PyValueError::new_err(format!(
                    "input ports {lent:?} are lent to native code, so the runtime cannot drive \
                     this node's `process()` from its inputs. A node that lends an input is \
                     declared `execution=\"manual\"`"
                )));
            }
        }
        self.runtime_drives_process_from_inputs.store(
            runtime_drives_process_from_inputs,
            std::sync::atomic::Ordering::Release,
        );
        Ok(())
    }

    /// Lend `input_port_names` and `output_port_names` to the node's native
    /// code, refusing a name the class never declared or one already lent, and
    /// an input a function node island feeds that runs a node as Python.
    pub(crate) fn lend_ports_to_native_code(
        &self,
        input_port_names: &[String],
        output_port_names: &[String],
    ) -> PyResult<HelperProcessDataPlaneLentToNativeCode> {
        let (Some(input_mailboxes), Some(output_writer)) =
            (self.input_mailboxes.get(), self.output_writer.get())
        else {
            return Err(not_a_helper_process_data_plane_error());
        };
        if !input_port_names.is_empty()
            && self
                .runtime_drives_process_from_inputs
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(PyValueError::new_err(format!(
                "input ports {input_port_names:?} cannot be lent to native code: the runtime \
                 drives this node's `process()` from its inputs. A node that lends an input \
                 is declared `execution=\"manual\"`"
            )));
        }
        let lent_input_port_names = Self::cast_and_check_ports_to_lend(
            "input",
            input_port_names,
            &self.declared_input_ports,
            &self.input_ports_lent_to_native_code,
        )?;
        let lent_output_port_names = Self::cast_and_check_ports_to_lend(
            "output",
            output_port_names,
            &self.declared_output_ports,
            &self.output_ports_lent_to_native_code,
        )?;
        for lent_input_port_name in &lent_input_port_names {
            if let Some(function_node_id) = input_mailboxes
                .the_function_node_run_as_python_by_the_island_feeding(lent_input_port_name)
            {
                return Err(PyValueError::new_err(format!(
                    "input port {lent_input_port_name:?} cannot be lent to native code: function \
                     node `{function_node_id}` feeds it and runs as Python, since its body does \
                     not compile, and a native read holds no Python interpreter lock to call it \
                     with. To fix: make `{function_node_id}` compile, or read the port from \
                     Python"
                )));
            }
        }
        self.input_ports_lent_to_native_code
            .lock()
            .extend(lent_input_port_names.iter().cloned());
        self.output_ports_lent_to_native_code
            .lock()
            .extend(lent_output_port_names.iter().cloned());
        Ok(HelperProcessDataPlaneLentToNativeCode {
            input_mailboxes: Arc::clone(input_mailboxes),
            output_writer: Arc::clone(output_writer),
            lent_input_port_names,
            lent_output_port_names,
        })
    }

    /// Whether `input_port_name` is lent to the node's native code, whose
    /// reads hold no Python interpreter lock.
    pub(crate) fn input_port_is_lent_to_native_code(&self, input_port_name: &str) -> bool {
        self.input_ports_lent_to_native_code
            .lock()
            .contains(input_port_name)
    }

    fn cast_and_check_ports_to_lend(
        direction: &str,
        port_names: &[String],
        declared_ports: &parking_lot::Mutex<HashSet<String>>,
        ports_already_lent: &parking_lot::Mutex<HashSet<String>>,
    ) -> PyResult<Vec<String>> {
        let declared_ports = declared_ports.lock();
        let ports_already_lent = ports_already_lent.lock();
        let mut lent_port_names: Vec<String> = Vec::with_capacity(port_names.len());
        for port_name in port_names {
            let port_name = declared_port_name_the_spelling_names(port_name)?.into_owned();
            if !declared_ports.contains(&port_name) {
                let mut declared: Vec<&String> = declared_ports.iter().collect();
                declared.sort();
                return Err(PyValueError::new_err(format!(
                    "{direction} port {port_name:?} cannot be lent to native code: the node \
                     declares no such port (it declares {declared:?})"
                )));
            }
            if ports_already_lent.contains(&port_name) || lent_port_names.contains(&port_name) {
                return Err(PyValueError::new_err(format!(
                    "{direction} port {port_name:?} is already lent to native code; a port is \
                     lent once"
                )));
            }
            lent_port_names.push(port_name);
        }
        Ok(lent_port_names)
    }

    fn refuse_a_python_read_of_a_port_lent_to_native_code(&self, port_name: &str) -> PyResult<()> {
        if self
            .input_ports_lent_to_native_code
            .lock()
            .contains(port_name)
        {
            return Err(PyRuntimeError::new_err(format!(
                "input port {port_name:?} is lent to this node's native code through its module \
                 host table, so Python does not read it"
            )));
        }
        Ok(())
    }

    fn refuse_a_python_write_to_a_port_lent_to_native_code(&self, port_name: &str) -> PyResult<()> {
        if self
            .output_ports_lent_to_native_code
            .lock()
            .contains(port_name)
        {
            return Err(PyRuntimeError::new_err(format!(
                "output port {port_name:?} is lent to this node's native code through its module \
                 host table, so Python does not write it"
            )));
        }
        Ok(())
    }

    /// A helper process's own data plane over an iceoryx2 node it already opened.
    pub(crate) fn over_helper_process_iceoryx2_node(node: Iceoryx2Node) -> Self {
        let wiring = Self::new();
        let _ = wiring.iceoryx2_node.set(node);
        let input_mailboxes = Arc::new(InputMailboxesInner::new());
        input_mailboxes.reach_the_claim_of_the_pooled_gpu_frames_its_bundles_hold(
            wiring.claim_of_the_pooled_gpu_frames_its_islands_hold(),
        );
        let _ = wiring.input_mailboxes.set(input_mailboxes);
        let _ = wiring.output_writer.set(Arc::new(OutputWriterInner::new()));
        wiring
    }

    /// The mailboxes `port_name` reads from; `None` for a declared input port
    /// no link has reached yet, which reads as empty rather than as an error.
    pub(super) fn input_mailboxes_reaching(
        &self,
        port_name: &str,
    ) -> PyResult<Option<&Arc<InputMailboxesInner>>> {
        self.refuse_a_python_read_of_a_port_lent_to_native_code(port_name)?;
        match self.input_mailboxes.get() {
            Some(input_mailboxes) if input_mailboxes.has_port(port_name) => {
                Ok(Some(input_mailboxes))
            }
            _ if self.declared_input_ports.lock().contains(port_name) => Ok(None),
            _ => Err(unwired_port_error("input", port_name)),
        }
    }

    /// The writer `port_name` publishes through, linked or not: the writer
    /// sends nothing for a declared port no link has reached, and keeps the
    /// last message of one that keeps it. `None` for a declared port of a data
    /// plane with no writer of its own, whose messages go nowhere rather than
    /// raising.
    pub(super) fn output_writer_reaching(
        &self,
        port_name: &str,
    ) -> PyResult<Option<&Arc<OutputWriterInner>>> {
        self.refuse_a_python_write_to_a_port_lent_to_native_code(port_name)?;
        let declared = self.declared_output_ports.lock().contains(port_name);
        match self.output_writer.get() {
            Some(output_writer) if declared || output_writer.has_port(port_name) => {
                Ok(Some(output_writer))
            }
            _ if declared => Ok(None),
            _ => Err(unwired_port_error("output", port_name)),
        }
    }

    /// The open loss-count board and where a link being wired mirrors onto it,
    /// or `None` with no board open, where there is nowhere to mirror.
    ///
    /// A helper with a board open must be told where every link's counts go, so
    /// a link wired without it is refused naming what was missing.
    pub(super) fn loss_count_board_writer_with_where_a_link_mirrors_to<MirrorTarget>(
        &self,
        link_direction: &str,
        link_id: &str,
        mirror_target: Option<MirrorTarget>,
        missing_arguments: &str,
    ) -> PyResult<Option<(&Arc<HelperProcessLossCountBoardWriter>, MirrorTarget)>> {
        match (self.loss_count_board_writer.get(), mirror_target) {
            (None, _) => Ok(None),
            (Some(board_writer), Some(mirror_target)) => Ok(Some((board_writer, mirror_target))),
            (Some(_), None) => Err(PyValueError::new_err(format!(
                "{link_direction} link {link_id:?} was wired without {missing_arguments}, so this \
                 helper cannot mirror the link's losses onto its loss-count board"
            ))),
        }
    }

    pub(super) fn helper_process_output_plane(
        &self,
    ) -> PyResult<(&Iceoryx2Node, &Arc<OutputWriterInner>)> {
        match (self.iceoryx2_node.get(), self.output_writer.get()) {
            (Some(node), Some(output_writer)) => Ok((node, output_writer)),
            _ => Err(not_a_helper_process_data_plane_error()),
        }
    }

    pub(super) fn helper_process_input_plane(
        &self,
    ) -> PyResult<(&Iceoryx2Node, &Arc<InputMailboxesInner>)> {
        match (self.iceoryx2_node.get(), self.input_mailboxes.get()) {
            (Some(node), Some(input_mailboxes)) => Ok((node, input_mailboxes)),
            _ => Err(not_a_helper_process_data_plane_error()),
        }
    }
}

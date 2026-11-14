// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Object-safe wrapper for GeneratedProcessor - DO NOT USE DIRECTLY.

use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use super::GeneratedProcessor;
use crate::core::ProcessorDescriptor;
use crate::core::Result;
use crate::core::context::processor_output_surface_pool::OutputFramesKeptByTheOutputLinksOfOneProcessor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::HowItsHelpersRanAFunctionNode;
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
use crate::core::runtime::FunctionNodeStatesCarriedForOneStream;
use crate::iceoryx2::{HelperPlacedProcessorLossCounts, Iceoryx2Node};
use serde_json::Value as JsonValue;

/// How a link wired after a far side's setup command reaches that far side,
/// which opens its own port for it — implemented by the far side's transport,
/// the helper bridge in production.
pub trait OutOfProcessFarSideLinkDelivery: Send {
    /// Hand the far side one link; its answer lands on `answer_cell`.
    fn hand_over_a_link_wired_after_setup(
        &self,
        port_direction: crate::core::PortDirection,
        link_wiring: &JsonValue,
        answer_cell: Arc<OutOfProcessLinkWireReply>,
    ) -> Result<()>;

    /// Ask the far side to drop the port it opened for one link being
    /// disconnected, and stop waiting on that link's answer. `local_port_name`
    /// is the port on the processor this far side hosts.
    fn tell_the_far_side_a_link_was_unwired(
        &self,
        port_direction: crate::core::PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> Result<()>;

    /// Hand the far side the function node island feeding one of its input
    /// ports, in place of any it evaluates there now; its answer lands on
    /// `answer_cell` under the island's link id.
    fn hand_over_a_function_node_island_wired_after_setup(
        &self,
        function_node_island: &JsonValue,
        answer_cell: Arc<OutOfProcessLinkWireReply>,
    ) -> Result<()>;

    /// Ask the far side to stop evaluating the function node island that
    /// link `link_id` carried into its input port `input_port`.
    fn tell_the_far_side_a_function_node_island_was_unwired(
        &self,
        input_port: &str,
        link_id: &str,
    ) -> Result<()>;

    /// Hand the far side the pin value each of its input ports reads while no
    /// link feeds it, in place of those it reads now.
    fn hand_over_the_pin_values_of_its_input_ports(
        &self,
        pin_values: &serde_json::Map<String, JsonValue>,
    ) -> Result<()>;

    /// Hand the far side the config fields its node takes as inputs, each read
    /// from the input port of its name, in place of those it takes now.
    fn hand_over_the_config_fields_its_node_exposes_as_inputs(
        &self,
        exposed_config_fields: &[String],
    ) -> Result<()>;

    /// Refuse every link the far side still owes an answer for with `reason`,
    /// because its host gave up on it.
    fn refuse_every_link_still_awaiting_the_far_sides_answer(&self, reason: &str);
}

/// A link recorded after the far side's setup began, waiting to be handed over
/// right behind the setup command.
struct LinkAwaitingHandoverBehindTheSetupCommand {
    port_direction: crate::core::PortDirection,
    link_wiring: JsonValue,
    answer_cell: Arc<OutOfProcessLinkWireReply>,
}

/// How a link recorded on an envelope reaches its far side.
enum HowALinkReachesTheFarSide {
    /// Setup has not begun: the link rides the setup command, and the far
    /// side's `ready` confirms it.
    RidesTheSetupCommand,
    /// Setup has begun, so the setup command's `ports` may already be taken:
    /// the link waits to be handed over right behind the command.
    WaitsForTheSetupCommandToGoOut(Vec<LinkAwaitingHandoverBehindTheSetupCommand>),
    /// The setup command has gone out: the link is handed over through this.
    HandedOverTo(Box<dyn OutOfProcessFarSideLinkDelivery>),
    /// The far side is gone: the link is refused for this reason.
    RefusedBecauseTheFarSideIsGone(String),
}

/// The links a far side was or will be given, and how the next reaches it —
/// under one lock, so no link lands between the setup command's snapshot and
/// the far side taking links one at a time.
struct RecordedLinksAndHowTheNextReachesTheFarSide {
    input_links: Vec<JsonValue>,
    output_links: Vec<JsonValue>,
    function_node_islands_feeding_its_input_ports: Vec<JsonValue>,
    /// The pin value each of its input ports reads while no link feeds it, by
    /// port name.
    pin_values_of_its_input_ports: serde_json::Map<String, JsonValue>,
    /// The config fields its node takes as inputs, each read from the input
    /// port of its name.
    config_fields_its_node_exposes_as_inputs: Vec<String>,
    /// Each function node its islands call, by the breadcrumb number its far
    /// side writes while calling it.
    function_nodes_by_breadcrumb_number: Vec<FunctionNodeNumberedOnTheBreadcrumb>,
    /// Each stateful function node its far side calls, whose state its board
    /// carries.
    stateful_function_nodes_its_far_side_calls: Vec<StatefulFunctionNodeItsFarSideCalls>,
    runs_a_function_node_as_its_processor: bool,
    /// Where each function node its islands call is told how its far side ran
    /// it, by the node's id.
    how_its_far_side_ran_each_function_node:
        std::collections::HashMap<String, Arc<HowItsHelpersRanAFunctionNode>>,
    answer_cells_of_links_handed_over_past_the_setup_command:
        Vec<(Option<String>, Arc<OutOfProcessLinkWireReply>)>,
    how_a_link_reaches_the_far_side: HowALinkReachesTheFarSide,
}

impl RecordedLinksAndHowTheNextReachesTheFarSide {
    fn links_facing(&mut self, port_direction: crate::core::PortDirection) -> &mut Vec<JsonValue> {
        match port_direction {
            crate::core::PortDirection::Input => &mut self.input_links,
            crate::core::PortDirection::Output => &mut self.output_links,
        }
    }

    fn record_a_link_handed_over_past_the_setup_command(
        &mut self,
        port_direction: crate::core::PortDirection,
        link_wiring: JsonValue,
        answer_cell: Arc<OutOfProcessLinkWireReply>,
    ) {
        self.answer_cells_of_links_handed_over_past_the_setup_command
            .push((link_id_of(&link_wiring), answer_cell));
        self.links_facing(port_direction).push(link_wiring);
    }

    /// Forget every link handed over past the setup command that its far side
    /// never opened, so a new far side is never handed a link `graph` reads
    /// `error`; answers the forgotten links' ids.
    fn forget_every_link_its_far_side_never_opened(&mut self) -> Vec<String> {
        let never_opened: Vec<String> = self
            .answer_cells_of_links_handed_over_past_the_setup_command
            .drain(..)
            .filter(|(_, answer_cell)| {
                answer_cell.the_far_sides_answer()
                    != Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
            })
            .filter_map(|(link_id, _)| link_id)
            .collect();
        let never_opened_by_its_far_side = |link_wiring: &JsonValue| {
            link_id_of(link_wiring).is_some_and(|link_id| never_opened.contains(&link_id))
        };
        self.input_links
            .retain(|link| !never_opened_by_its_far_side(link));
        self.output_links
            .retain(|link| !never_opened_by_its_far_side(link));
        never_opened
    }

    /// The breadcrumb number of `function_node_id`, named `function_node_name`
    /// in the graph: the one it was first given.
    fn the_breadcrumb_number_of_a_function_node(
        &mut self,
        function_node_id: &str,
        function_node_name: &str,
    ) -> u32 {
        let position = match self
            .function_nodes_by_breadcrumb_number
            .iter()
            .position(|numbered| numbered.function_node_id == function_node_id)
        {
            Some(position) => position,
            None => {
                self.function_nodes_by_breadcrumb_number.push(
                    FunctionNodeNumberedOnTheBreadcrumb {
                        function_node_id: function_node_id.to_string(),
                        function_node_name: function_node_name.to_string(),
                    },
                );
                self.function_nodes_by_breadcrumb_number.len() - 1
            }
        };
        position as u32
    }

    /// Note that the function node numbered `breadcrumb_number` keeps a state
    /// of `state_fields`, once.
    fn note_a_stateful_function_node(
        &mut self,
        breadcrumb_number: u32,
        function_node_uuid: uuid::Uuid,
        state_fields: Vec<String>,
    ) {
        if state_fields.is_empty() {
            self.forget_a_stateful_function_node(breadcrumb_number);
            return;
        }
        let noted_now = StatefulFunctionNodeItsFarSideCalls {
            breadcrumb_number,
            function_node_uuid,
            state_fields,
        };
        match self
            .stateful_function_nodes_its_far_side_calls
            .iter_mut()
            .find(|noted| noted.breadcrumb_number == breadcrumb_number)
        {
            Some(noted_before) => *noted_before = noted_now,
            None => self
                .stateful_function_nodes_its_far_side_calls
                .push(noted_now),
        }
    }

    fn forget_a_stateful_function_node(&mut self, breadcrumb_number: u32) {
        self.stateful_function_nodes_its_far_side_calls
            .retain(|noted| noted.breadcrumb_number != breadcrumb_number);
    }

    fn as_setup_command_ports(&self) -> JsonValue {
        serde_json::json!({
            "inputs": self.input_links,
            "outputs": self.output_links,
            "islands": self.function_node_islands_feeding_its_input_ports,
            "pins": self.pin_values_of_its_input_ports,
            "exposed_config_fields": self.config_fields_its_node_exposes_as_inputs,
        })
    }
}

fn link_id_of(link_wiring: &JsonValue) -> Option<String> {
    link_wiring
        .get("link_id")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

impl LinkAwaitingHandoverBehindTheSetupCommand {
    fn refuse_because(self, reason: &str) {
        self.answer_cell.note_the_far_sides_answer(
            OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: reason.to_string(),
            },
        );
    }
}

fn refuse_every_link_waiting_behind_the_setup_command(
    waiting: Vec<LinkAwaitingHandoverBehindTheSetupCommand>,
    reason: &str,
) {
    for waiting_link in waiting {
        waiting_link.refuse_because(reason);
    }
}

/// One function node a far side's islands call, by its id and its name in the
/// graph.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionNodeNumberedOnTheBreadcrumb {
    function_node_id: String,
    function_node_name: String,
}

/// One stateful function node a far side calls: its breadcrumb number, which
/// keys its state entry, its UUID, which keys the state its stream carries, its
/// state record's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatefulFunctionNodeItsFarSideCalls {
    breadcrumb_number: u32,
    function_node_uuid: uuid::Uuid,
    state_fields: Vec<String>,
}

/// Where the request to promote a function node out of one far side's islands
/// stands, which the far side's host waits on before it starts the far side
/// standing in for the one that function node crashed.
#[derive(Default)]
struct FunctionNodePromotionOutOfTheIslandsOfOneFarSide {
    progress: Mutex<FunctionNodePromotionOutOfTheIslandsProgress>,
    promotion_requests_awaited: std::sync::atomic::AtomicU64,
    settled: Condvar,
}

/// A compile plans a promotion only while the host still waits on that very
/// request, and the host stops waiting only before a compile has claimed it, so
/// a far side never starts with islands a promotion goes on to change.
#[derive(Default)]
enum FunctionNodePromotionOutOfTheIslandsProgress {
    #[default]
    NotAwaited,
    AwaitedByTheFarSidesHost {
        promotion_request_number: u64,
    },
    ClaimedByTheCompileCarryingIt,
    Settled(std::result::Result<(), String>),
}

/// A promotion of a function node out of a reader's islands that the reader's
/// host asked for, waited on without holding the reader so the compile that
/// carries it is never kept from the reader.
pub struct FunctionNodePromotionAwaitedBeforeTheReaderRestarts {
    pub(crate) reader_link_wiring: Arc<OutOfProcessLinkWiringEnvelope>,
    pub(crate) reader_name: String,
    pub(crate) function_node_name: String,
    pub(crate) longest_wait_for_a_compile_to_claim_it: std::time::Duration,
    pub(crate) stop_waiting: Box<dyn Fn() -> bool + Send>,
}

impl FunctionNodePromotionAwaitedBeforeTheReaderRestarts {
    /// Wait for the promotion to settle, and log how it ended.
    pub(crate) fn wait_until_it_settles(self) {
        let Self {
            reader_link_wiring,
            reader_name,
            function_node_name,
            longest_wait_for_a_compile_to_claim_it,
            stop_waiting,
        } = self;
        match reader_link_wiring.wait_for_the_function_node_promotion_out_of_its_islands_to_settle(
            longest_wait_for_a_compile_to_claim_it,
            stop_waiting,
        ) {
            Some(Ok(())) => tracing::info!(
                "[{reader_name}] function node `{function_node_name}` was promoted out of its \
                 islands into a helper process of its own"
            ),
            Some(Err(refusal)) => tracing::warn!(
                "[{reader_name}] function node `{function_node_name}` stays in its islands: its \
                 promotion was refused: {refusal}"
            ),
            None => tracing::warn!(
                "[{reader_name}] function node `{function_node_name}` stays in its islands: no \
                 compile took its promotion up within {} s",
                longest_wait_for_a_compile_to_claim_it.as_secs()
            ),
        }
    }
}

/// A call to one function node a far side had under way, as its breadcrumb
/// names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionNodeCallUnderWayInTheFarSide {
    pub(crate) function_node_id: String,
    /// The node's name in the graph, which a person reading a log knows it by.
    pub(crate) function_node_name: String,
    pub(crate) call_started_at_monotonic_ns: u64,
    /// How many calls the far side had under way, this oldest one among them.
    pub(crate) function_node_calls_under_way: u32,
}

/// A function node whose calls overran its far side's frame interval too many
/// times running, as the far side's overrun entry names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionNodeThatOverranItsFarSidesFrameInterval {
    pub(crate) function_node_id: String,
    /// The node's name in the graph, which a person reading a log knows it by.
    pub(crate) function_node_name: String,
    pub(crate) longest_overrunning_call: std::time::Duration,
}

impl FunctionNodeCallUnderWayInTheFarSide {
    /// Whether `other` is this same call, by its node and its start.
    pub(crate) fn is_the_same_call_as(&self, other: &Self) -> bool {
        self.function_node_id == other.function_node_id
            && self.call_started_at_monotonic_ns == other.call_started_at_monotonic_ns
    }
}

/// One out-of-process processor's link wiring, shared between the host of its
/// far side and its graph node, which is where the compiler op reaches it.
///
/// The far side reads the links recorded before its setup began as the `ports`
/// payload of its setup command and opens its own publishers, subscribers and
/// notifiers from them; every later link is handed over on its own. It also
/// holds each inbound link's loss-count board slot and the board itself.
///
/// Its lock is never the hosting processor's, which a helper holds across a
/// whole setup — a cold import — that wiring a link must not wait on.
pub struct OutOfProcessLinkWiringEnvelope {
    recorded_links_and_how_the_next_reaches_the_far_side:
        Mutex<RecordedLinksAndHowTheNextReachesTheFarSide>,
    loss_counts: Arc<HelperPlacedProcessorLossCounts>,
    output_frames_kept_by_its_output_links: Arc<OutputFramesKeptByTheOutputLinksOfOneProcessor>,
    far_side_process_execution: ProcessExecution,
    function_node_promotion_out_of_its_islands: FunctionNodePromotionOutOfTheIslandsOfOneFarSide,
    /// The stream's function-node states, which the current spawn's board
    /// writes; `None` before any spawn of a far side calling a stateful node.
    function_node_states_its_far_side_writes:
        Mutex<Option<Arc<FunctionNodeStatesCarriedForOneStream>>>,
}

impl std::fmt::Debug for OutOfProcessLinkWiringEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("OutOfProcessLinkWiringEnvelope");
        match self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .try_lock()
        {
            Some(recorded) => debug.field("ports", &recorded.as_setup_command_ports()),
            None => debug.field("ports", &format_args!("<locked>")),
        };
        debug
            .field(
                "far_side_process_execution",
                &self.far_side_process_execution,
            )
            .finish_non_exhaustive()
    }
}

impl OutOfProcessLinkWiringEnvelope {
    /// An empty envelope for a far side that drives its processor in
    /// `far_side_process_execution` — the child's declared mode, never the
    /// host thread's own.
    pub fn for_a_far_side_driven_in(far_side_process_execution: ProcessExecution) -> Self {
        Self {
            recorded_links_and_how_the_next_reaches_the_far_side: Mutex::new(
                RecordedLinksAndHowTheNextReachesTheFarSide {
                    input_links: Vec::new(),
                    output_links: Vec::new(),
                    function_node_islands_feeding_its_input_ports: Vec::new(),
                    pin_values_of_its_input_ports: serde_json::Map::new(),
                    config_fields_its_node_exposes_as_inputs: Vec::new(),
                    function_nodes_by_breadcrumb_number: Vec::new(),
                    stateful_function_nodes_its_far_side_calls: Vec::new(),
                    runs_a_function_node_as_its_processor: false,
                    how_its_far_side_ran_each_function_node: std::collections::HashMap::new(),
                    answer_cells_of_links_handed_over_past_the_setup_command: Vec::new(),
                    how_a_link_reaches_the_far_side:
                        HowALinkReachesTheFarSide::RidesTheSetupCommand,
                },
            ),
            loss_counts: Arc::default(),
            output_frames_kept_by_its_output_links: Arc::default(),
            far_side_process_execution,
            function_node_promotion_out_of_its_islands: Default::default(),
            function_node_states_its_far_side_writes: Mutex::new(None),
        }
    }

    /// What the links on this processor's outputs keep of its output frames,
    /// shared with the pools its far side publishes from.
    pub(crate) fn output_frames_kept_by_its_output_links(
        &self,
    ) -> Arc<OutputFramesKeptByTheOutputLinksOfOneProcessor> {
        Arc::clone(&self.output_frames_kept_by_its_output_links)
    }

    /// The mode the far side drives its processor in.
    pub fn far_side_process_execution(&self) -> ProcessExecution {
        self.far_side_process_execution
    }

    /// The loss counts this processor's far side writes, shared with its graph
    /// node so `graph` reads them.
    pub(crate) fn helper_placed_processor_loss_counts(
        &self,
    ) -> Arc<HelperPlacedProcessorLossCounts> {
        Arc::clone(&self.loss_counts)
    }

    /// Create the loss-count board this helper spawn writes on, before the
    /// child starts, and return the `loss_count_board` payload of its setup
    /// command: the board's name, its output ports, and each stateful function
    /// node's entry with the state the stream carries for it.
    ///
    /// Named per spawn rather than per processor, so a spawn never opens a
    /// board an earlier one's dead writer still holds. The envelope keeps the
    /// board until the next spawn's replaces it, and the counts and states it
    /// held after.
    pub fn create_the_loss_count_board_for_this_helper_spawn(
        &self,
        ctx: &RuntimeContextFullAccess<'_>,
        processor_id: &str,
        output_port_names: Vec<String>,
    ) -> Result<JsonValue> {
        self.create_the_loss_count_board_for_this_helper_spawn_on(
            ctx.host_base().iceoryx2_node(),
            ctx.host_base().this_streams_function_node_states(),
            processor_id,
            output_port_names,
        )
    }

    pub(crate) fn create_the_loss_count_board_for_this_helper_spawn_on(
        &self,
        iceoryx2_node: &Iceoryx2Node,
        function_node_states: &Arc<FunctionNodeStatesCarriedForOneStream>,
        processor_id: &str,
        output_port_names: Vec<String>,
    ) -> Result<JsonValue> {
        let service_name = format!(
            "streamlib/{processor_id}/loss-counts/{}",
            mint_machine_global_unique_name_suffix()
        );
        let stateful_function_nodes = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock()
            .stateful_function_nodes_its_far_side_calls
            .clone();
        self.keep_every_function_node_state_its_far_side_wrote();
        let board = iceoryx2_node.create_helper_process_loss_count_board(
            &service_name,
            output_port_names,
            stateful_function_nodes
                .iter()
                .map(|stateful| stateful.breadcrumb_number)
                .collect(),
        )?;
        let function_node_state_entries: Vec<JsonValue> = stateful_function_nodes
            .iter()
            .map(|stateful| {
                let resumes_from = function_node_states.the_state_a_function_node_resumes_from(
                    stateful.function_node_uuid,
                    &stateful.state_fields,
                );
                serde_json::json!({
                    "breadcrumb_number": stateful.breadcrumb_number,
                    "resumes_from_f64_bits": resumes_from.map(|state_values| {
                        state_values.iter().map(|value| value.to_bits()).collect::<Vec<u64>>()
                    }),
                })
            })
            .collect();
        let setup_command_loss_count_board = serde_json::json!({
            "service_name": board.service_name(),
            "output_ports": board.output_port_names().collect::<Vec<_>>(),
            "function_node_states": function_node_state_entries,
        });
        self.loss_counts.hold_the_board_of_this_spawn(board);
        for stateful in stateful_function_nodes {
            function_node_states.note_the_helper_writing_the_state_of_a_function_node(
                stateful.function_node_uuid,
                stateful.state_fields,
                &self.loss_counts,
                stateful.breadcrumb_number,
            );
        }
        *self.function_node_states_its_far_side_writes.lock() =
            Some(Arc::clone(function_node_states));
        Ok(setup_command_loss_count_board)
    }

    /// Keep, past this spawn's board, the state of every function node its far
    /// side last wrote: the far side went away, or a new spawn replaces it.
    pub(crate) fn keep_every_function_node_state_its_far_side_wrote(&self) {
        if let Some(function_node_states) = self
            .function_node_states_its_far_side_writes
            .lock()
            .as_ref()
        {
            function_node_states.keep_every_state_the_helper_wrote(&self.loss_counts);
        }
    }

    /// Record one link, in the direction its port faces, and get it to the far
    /// side however it reaches it now.
    ///
    /// `None` is a link riding the setup command, confirmed by the far side's
    /// `ready`; `Some` is the cell the far side's own answer lands in. A far side
    /// that is gone refuses the link, so the compile wiring it fails.
    pub fn record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
        &self,
        port_direction: crate::core::PortDirection,
        link_wiring: JsonValue,
    ) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        match &mut recorded.how_a_link_reaches_the_far_side {
            HowALinkReachesTheFarSide::RidesTheSetupCommand => {
                recorded.links_facing(port_direction).push(link_wiring);
                Ok(None)
            }
            HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(waiting) => {
                let answer_cell = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
                waiting.push(LinkAwaitingHandoverBehindTheSetupCommand {
                    port_direction,
                    link_wiring,
                    answer_cell: Arc::clone(&answer_cell),
                });
                Ok(Some(answer_cell))
            }
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => {
                let answer_cell = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
                delivery.hand_over_a_link_wired_after_setup(
                    port_direction,
                    &link_wiring,
                    Arc::clone(&answer_cell),
                )?;
                recorded.record_a_link_handed_over_past_the_setup_command(
                    port_direction,
                    link_wiring,
                    Arc::clone(&answer_cell),
                );
                Ok(Some(answer_cell))
            }
            HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(reason) => {
                Err(crate::core::error::Error::Runtime(reason.clone()))
            }
        }
    }

    /// Record the function node island feeding one of this processor's input
    /// ports, in place of any recorded under its link id, numbering each node it
    /// calls with the breadcrumb its far side writes while calling it, and get
    /// it to the far side however it reaches it now.
    ///
    /// `None` is an island riding the setup command, confirmed by the far side's
    /// `ready`; `Some` is the cell the far side's own answer lands in.
    pub(crate) fn record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
        &self,
        mut function_node_island: JsonValue,
    ) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        if let HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(reason) =
            &recorded.how_a_link_reaches_the_far_side
        {
            return Err(crate::core::error::Error::Runtime(reason.clone()));
        }
        if let Some(nodes) = function_node_island
            .get_mut("nodes")
            .and_then(JsonValue::as_array_mut)
        {
            for node in nodes {
                let Some(node_id) = node.get("node").and_then(JsonValue::as_str) else {
                    continue;
                };
                let node_name = node
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or(node_id);
                let breadcrumb_number =
                    recorded.the_breadcrumb_number_of_a_function_node(node_id, node_name);
                let state_fields: Vec<String> = node
                    .get("state_fields")
                    .and_then(|state_fields| serde_json::from_value(state_fields.clone()).ok())
                    .unwrap_or_default();
                let stateful_function_node_uuid = node
                    .get("node_uuid")
                    .and_then(JsonValue::as_str)
                    .and_then(|node_uuid| node_uuid.parse::<uuid::Uuid>().ok());
                match stateful_function_node_uuid {
                    Some(function_node_uuid) => recorded.note_a_stateful_function_node(
                        breadcrumb_number,
                        function_node_uuid,
                        state_fields,
                    ),
                    None => recorded.forget_a_stateful_function_node(breadcrumb_number),
                }
                node["breadcrumb_number"] = JsonValue::from(breadcrumb_number);
            }
        }
        let island_link_id = link_id_of(&function_node_island);
        recorded
            .function_node_islands_feeding_its_input_ports
            .retain(|island| link_id_of(island) != island_link_id);
        recorded
            .function_node_islands_feeding_its_input_ports
            .push(function_node_island.clone());
        match &recorded.how_a_link_reaches_the_far_side {
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => {
                let answer_cell = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
                delivery.hand_over_a_function_node_island_wired_after_setup(
                    &function_node_island,
                    Arc::clone(&answer_cell),
                )?;
                Ok(Some(answer_cell))
            }
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(_)
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => Ok(None),
        }
    }

    /// Record the pin value each of its input ports reads while no link feeds
    /// it, and hand them to a far side past its setup command when they differ
    /// from those it holds.
    pub(crate) fn record_the_pin_values_and_hand_them_to_a_far_side_past_its_setup_command(
        &self,
        pin_values: serde_json::Map<String, JsonValue>,
    ) -> Result<()> {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        if recorded.pin_values_of_its_input_ports == pin_values {
            return Ok(());
        }
        recorded.pin_values_of_its_input_ports = pin_values;
        match &recorded.how_a_link_reaches_the_far_side {
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => delivery
                .hand_over_the_pin_values_of_its_input_ports(
                    &recorded.pin_values_of_its_input_ports,
                ),
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(_)
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => Ok(()),
        }
    }

    /// Record the config fields its node takes as inputs, and hand them to a
    /// far side past its setup command when they differ from those it holds.
    pub(crate) fn record_the_exposed_config_fields_and_hand_them_to_a_far_side_past_its_setup_command(
        &self,
        exposed_config_fields: Vec<String>,
    ) -> Result<()> {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        if recorded.config_fields_its_node_exposes_as_inputs == exposed_config_fields {
            return Ok(());
        }
        recorded.config_fields_its_node_exposes_as_inputs = exposed_config_fields;
        match &recorded.how_a_link_reaches_the_far_side {
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => delivery
                .hand_over_the_config_fields_its_node_exposes_as_inputs(
                    &recorded.config_fields_its_node_exposes_as_inputs,
                ),
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(_)
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => Ok(()),
        }
    }

    /// The function node island recorded under `link_id`, as it was recorded
    /// before its nodes were numbered; `None` when none is.
    pub(crate) fn the_function_node_island_recorded_for_a_link(
        &self,
        link_id: &str,
    ) -> Option<JsonValue> {
        let recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        recorded
            .function_node_islands_feeding_its_input_ports
            .iter()
            .find(|island| link_id_of(island).as_deref() == Some(link_id))
            .map(|island| {
                let mut island_as_built = island.clone();
                if let Some(nodes) = island_as_built
                    .get_mut("nodes")
                    .and_then(JsonValue::as_array_mut)
                {
                    for node in nodes {
                        if let Some(node) = node.as_object_mut() {
                            node.remove("breadcrumb_number");
                        }
                    }
                }
                island_as_built
            })
    }

    /// Tell each of `function_nodes` how its far side ran it, once that far side
    /// reports it at the end of its setup.
    pub(crate) fn tell_each_function_node_how_its_far_side_ran_it(
        &self,
        function_nodes: impl IntoIterator<Item = (String, Arc<HowItsHelpersRanAFunctionNode>)>,
    ) {
        self.recorded_links_and_how_the_next_reaches_the_far_side
            .lock()
            .how_its_far_side_ran_each_function_node
            .extend(function_nodes);
    }

    /// Note how the far side ran each function node its islands call, from the
    /// `function_nodes_ran_compiled` map of node id to bool its `ready` carries.
    pub(crate) fn note_how_its_far_side_ran_its_function_nodes(
        &self,
        function_nodes_ran_compiled: Option<&JsonValue>,
    ) {
        let Some(ran_compiled_by_function_node_id) =
            function_nodes_ran_compiled.and_then(JsonValue::as_object)
        else {
            return;
        };
        let recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        for (function_node_id, ran_compiled) in ran_compiled_by_function_node_id {
            if let (Some(how_it_ran), Some(ran_compiled)) = (
                recorded
                    .how_its_far_side_ran_each_function_node
                    .get(function_node_id),
                ran_compiled.as_bool(),
            ) {
                how_it_ran.note_it_ran_compiled(ran_compiled);
            }
        }
    }

    /// Number `function_node_id` first on the breadcrumb, as the function node
    /// its far side runs as its processor once promoted out of its readers'
    /// islands; the nodes of the islands feeding it are numbered after it.
    pub(crate) fn number_the_function_node_its_far_side_runs_as_its_processor(
        &self,
        function_node_id: &str,
        function_node_name: &str,
        function_node_uuid: uuid::Uuid,
        state_fields: Vec<String>,
    ) {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        let breadcrumb_number =
            recorded.the_breadcrumb_number_of_a_function_node(function_node_id, function_node_name);
        recorded.note_a_stateful_function_node(breadcrumb_number, function_node_uuid, state_fields);
        recorded.runs_a_function_node_as_its_processor = true;
        debug_assert_eq!(
            breadcrumb_number,
            crate::iceoryx2::BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR
        );
    }

    /// The function node its far side last noted a call under way to, read
    /// off the current spawn's board even after the far side died.
    pub(crate) fn function_node_call_its_far_side_has_under_way(
        &self,
    ) -> Option<FunctionNodeCallUnderWayInTheFarSide> {
        let breadcrumb = self
            .loss_counts
            .function_node_call_breadcrumb_its_helper_wrote()?;
        let breadcrumb_number = breadcrumb.function_node_whose_call_is_under_way()?;
        let FunctionNodeNumberedOnTheBreadcrumb {
            function_node_id,
            function_node_name,
        } = self.function_node_numbered_on_the_breadcrumb(breadcrumb_number);
        Some(FunctionNodeCallUnderWayInTheFarSide {
            function_node_id,
            function_node_name,
            call_started_at_monotonic_ns: breadcrumb.call_started_at_monotonic_ns,
            function_node_calls_under_way: breadcrumb.function_node_calls_under_way,
        })
    }

    /// The function node the far side named for overrunning its frame
    /// interval, if it named one.
    pub(crate) fn function_node_that_overran_its_far_sides_frame_interval(
        &self,
    ) -> Option<FunctionNodeThatOverranItsFarSidesFrameInterval> {
        let overrun = self.loss_counts.function_node_overrun_its_helper_wrote()?;
        let breadcrumb_number = overrun.function_node_that_overran()?;
        let FunctionNodeNumberedOnTheBreadcrumb {
            function_node_id,
            function_node_name,
        } = self.function_node_numbered_on_the_breadcrumb(breadcrumb_number);
        Some(FunctionNodeThatOverranItsFarSidesFrameInterval {
            function_node_id,
            function_node_name,
            longest_overrunning_call: std::time::Duration::from_nanos(
                overrun.longest_overrunning_call_ns,
            ),
        })
    }

    fn function_node_numbered_on_the_breadcrumb(
        &self,
        breadcrumb_number: u32,
    ) -> FunctionNodeNumberedOnTheBreadcrumb {
        self.recorded_links_and_how_the_next_reaches_the_far_side
            .lock()
            .function_nodes_by_breadcrumb_number
            .get(breadcrumb_number as usize)
            .cloned()
            .unwrap_or_else(|| {
                let numbered = format!("numbered {breadcrumb_number}");
                FunctionNodeNumberedOnTheBreadcrumb {
                    function_node_id: numbered.clone(),
                    function_node_name: numbered,
                }
            })
    }

    /// Start waiting on a new request to promote a function node out of this
    /// far side's islands, forgetting how any earlier one ended; the number
    /// names the request, which only a compile carrying it may claim.
    pub(crate) fn await_a_function_node_promotion_out_of_its_islands(&self) -> u64 {
        let promotion = &self.function_node_promotion_out_of_its_islands;
        let promotion_request_number = promotion
            .promotion_requests_awaited
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        *promotion.progress.lock() =
            FunctionNodePromotionOutOfTheIslandsProgress::AwaitedByTheFarSidesHost {
                promotion_request_number,
            };
        promotion_request_number
    }

    /// Stop waiting on the promotion out of this far side's islands unless a
    /// compile has claimed it; `true` when the host may start its far side.
    pub(crate) fn give_up_the_function_node_promotion_out_of_its_islands_unless_claimed(
        &self,
    ) -> bool {
        let mut progress = self
            .function_node_promotion_out_of_its_islands
            .progress
            .lock();
        match *progress {
            FunctionNodePromotionOutOfTheIslandsProgress::ClaimedByTheCompileCarryingIt => false,
            FunctionNodePromotionOutOfTheIslandsProgress::Settled(_) => true,
            FunctionNodePromotionOutOfTheIslandsProgress::NotAwaited
            | FunctionNodePromotionOutOfTheIslandsProgress::AwaitedByTheFarSidesHost { .. } => {
                *progress = FunctionNodePromotionOutOfTheIslandsProgress::NotAwaited;
                true
            }
        }
    }

    /// Claim request `promotion_request_number` to promote a function node out
    /// of this far side's islands for the compile that plans it; `false` when
    /// the host is not waiting on that request, so the compile passes it over.
    pub(crate) fn claim_the_awaited_function_node_promotion_out_of_its_islands(
        &self,
        promotion_request_number: u64,
    ) -> bool {
        let mut progress = self
            .function_node_promotion_out_of_its_islands
            .progress
            .lock();
        if matches!(
            *progress,
            FunctionNodePromotionOutOfTheIslandsProgress::AwaitedByTheFarSidesHost {
                promotion_request_number: awaited,
            } if awaited == promotion_request_number
        ) {
            *progress = FunctionNodePromotionOutOfTheIslandsProgress::ClaimedByTheCompileCarryingIt;
            true
        } else {
            false
        }
    }

    /// Say how the promotion a compile claimed out of this far side's islands
    /// ended: `Err` names why it was refused or why the compile failed. An
    /// outcome for a promotion no compile claimed is dropped.
    pub(crate) fn settle_the_function_node_promotion_out_of_its_islands(
        &self,
        outcome: std::result::Result<(), String>,
    ) {
        let promotion = &self.function_node_promotion_out_of_its_islands;
        let mut progress = promotion.progress.lock();
        if matches!(
            *progress,
            FunctionNodePromotionOutOfTheIslandsProgress::ClaimedByTheCompileCarryingIt
        ) {
            *progress = FunctionNodePromotionOutOfTheIslandsProgress::Settled(outcome);
            promotion.settled.notify_all();
        }
    }

    /// How the awaited promotion out of this far side's islands ended. Waits
    /// up to `longest_wait` for a compile to claim it and then for as long as
    /// that compile runs, giving up early once `stop_waiting` says so; `None`
    /// when no compile claimed it in time, or the wait was stopped.
    pub(crate) fn wait_for_the_function_node_promotion_out_of_its_islands_to_settle(
        &self,
        longest_wait: std::time::Duration,
        stop_waiting: impl Fn() -> bool,
    ) -> Option<std::result::Result<(), String>> {
        const STOP_WAITING_POLL_INTERVAL: std::time::Duration =
            std::time::Duration::from_millis(100);
        let promotion = &self.function_node_promotion_out_of_its_islands;
        let wait_ends_at = std::time::Instant::now() + longest_wait;
        let mut progress = promotion.progress.lock();
        loop {
            match &*progress {
                FunctionNodePromotionOutOfTheIslandsProgress::Settled(outcome) => {
                    return Some(outcome.clone());
                }
                FunctionNodePromotionOutOfTheIslandsProgress::NotAwaited => return None,
                FunctionNodePromotionOutOfTheIslandsProgress::ClaimedByTheCompileCarryingIt
                | FunctionNodePromotionOutOfTheIslandsProgress::AwaitedByTheFarSidesHost {
                    ..
                } => {}
            }
            if stop_waiting() {
                return None;
            }
            let now = std::time::Instant::now();
            let claimed = matches!(
                *progress,
                FunctionNodePromotionOutOfTheIslandsProgress::ClaimedByTheCompileCarryingIt
            );
            if !claimed && now >= wait_ends_at {
                *progress = FunctionNodePromotionOutOfTheIslandsProgress::NotAwaited;
                return None;
            }
            let next_look = if claimed {
                STOP_WAITING_POLL_INTERVAL
            } else {
                STOP_WAITING_POLL_INTERVAL.min(wait_ends_at - now)
            };
            promotion.settled.wait_for(&mut progress, next_look);
        }
    }

    /// Forget the function node island `link_id` carries, and each stateful
    /// node no island left carries, so no later setup command or board carries
    /// them and no later spawn takes their state over from the helper that
    /// evaluates them now; a far side past its setup command is told to stop
    /// evaluating it.
    pub(crate) fn forget_a_function_node_island(&self, link_id: &str) {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        let input_port_it_fed = recorded
            .function_node_islands_feeding_its_input_ports
            .iter()
            .find(|island| link_id_of(island).as_deref() == Some(link_id))
            .and_then(|island| island.get("input_port").and_then(JsonValue::as_str))
            .map(str::to_string);
        recorded
            .function_node_islands_feeding_its_input_ports
            .retain(|island| link_id_of(island).as_deref() != Some(link_id));
        if let (HowALinkReachesTheFarSide::HandedOverTo(delivery), Some(input_port)) = (
            &recorded.how_a_link_reaches_the_far_side,
            &input_port_it_fed,
        ) && let Err(error) =
            delivery.tell_the_far_side_a_function_node_island_was_unwired(input_port, link_id)
        {
            tracing::warn!(
                %link_id,
                port = %input_port,
                %error,
                "An out-of-process reader could not be told to stop evaluating a function node \
                 island; it keeps evaluating it until it restarts"
            );
        }
        let runs_a_function_node_as_its_processor = recorded.runs_a_function_node_as_its_processor;
        let breadcrumb_numbers_still_carried: std::collections::HashSet<u64> = recorded
            .function_node_islands_feeding_its_input_ports
            .iter()
            .filter_map(|island| island.get("nodes").and_then(JsonValue::as_array))
            .flatten()
            .filter_map(|node| node.get("breadcrumb_number").and_then(JsonValue::as_u64))
            .collect();
        recorded
            .stateful_function_nodes_its_far_side_calls
            .retain(|stateful| {
                (runs_a_function_node_as_its_processor
                    && stateful.breadcrumb_number
                        == crate::iceoryx2::BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR)
                    || breadcrumb_numbers_still_carried
                        .contains(&u64::from(stateful.breadcrumb_number))
            });
    }

    /// Forget one link, in both directions, on disconnect, and ask a far side
    /// past its setup command to drop the port it opened for it.
    ///
    /// The far side is asked whether or not this end's entry was still carried:
    /// both ends of a link between one far side's own ports are forgotten under
    /// one link id, and each end's port is its own to drop.
    pub(crate) fn forget_a_link_and_tell_a_far_side_past_its_setup_command(
        &self,
        port_direction: crate::core::PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> Result<()> {
        let carries_link = |link_wiring: &JsonValue| {
            link_wiring.get("link_id").and_then(JsonValue::as_str) == Some(link_id)
        };
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        recorded.input_links.retain(|link| !carries_link(link));
        recorded.output_links.retain(|link| !carries_link(link));
        recorded
            .answer_cells_of_links_handed_over_past_the_setup_command
            .retain(|(handed_over_link_id, _)| handed_over_link_id.as_deref() != Some(link_id));
        self.loss_counts.forget_link(link_id);
        self.output_frames_kept_by_its_output_links
            .forget_an_unwired_output_link(link_id);
        match &mut recorded.how_a_link_reaches_the_far_side {
            HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(waiting) => {
                waiting.retain(|waiting_link| !carries_link(&waiting_link.link_wiring));
                Ok(())
            }
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => delivery
                .tell_the_far_side_a_link_was_unwired(port_direction, local_port_name, link_id),
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => Ok(()),
        }
    }

    /// Forget one link as [`Self::forget_a_link_and_tell_a_far_side_past_its_setup_command`]
    /// does, logging a far side that could not be told rather than failing: the
    /// link is going regardless, and only a later reconnect feels the port it keeps.
    pub(crate) fn unwire_a_link_reporting_a_far_side_that_kept_its_port(
        &self,
        port_direction: crate::core::PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) {
        match self.forget_a_link_and_tell_a_far_side_past_its_setup_command(
            port_direction,
            local_port_name,
            link_id,
        ) {
            Ok(()) => tracing::debug!(
                %link_id,
                port = %local_port_name,
                %port_direction,
                "Asked an out-of-process endpoint to reclaim its port for an unwired link"
            ),
            Err(error) => tracing::warn!(
                %link_id,
                port = %local_port_name,
                %port_direction,
                %error,
                "An out-of-process endpoint did not reclaim its port for an unwired link; a \
                 reconnect of this link may exhaust its channel's notifier or subscriber slots"
            ),
        }
    }

    /// Say the far side's setup has begun: a link recorded from now on waits to
    /// be handed over right behind the setup command, and reads `pending` until
    /// the far side answers, rather than riding a command whose `ports` may
    /// already be taken.
    pub fn hold_every_later_link_until_the_setup_command_goes_out(&self) {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        if matches!(
            recorded.how_a_link_reaches_the_far_side,
            HowALinkReachesTheFarSide::RidesTheSetupCommand
        ) {
            recorded.how_a_link_reaches_the_far_side =
                HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(Vec::new());
        }
    }

    /// Send the far side its setup command carrying every link recorded before
    /// its setup began, hand over the links waiting behind it, and hand every
    /// later link over through `later_link_delivery`.
    ///
    /// `send_the_setup_command_carrying_these_ports` runs under the envelope's
    /// lock, so it must not call back into this envelope. A command that could
    /// not be sent refuses every waiting and later link.
    pub fn send_the_setup_command_then_hand_every_later_link_over(
        &self,
        send_the_setup_command_carrying_these_ports: impl FnOnce(JsonValue) -> Result<()>,
        later_link_delivery: impl OutOfProcessFarSideLinkDelivery + 'static,
    ) -> Result<()> {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        match &recorded.how_a_link_reaches_the_far_side {
            HowALinkReachesTheFarSide::HandedOverTo(_) => {
                return Err(crate::core::error::Error::Runtime(
                    "this far side's setup command was already sent".to_string(),
                ));
            }
            HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(reason) => {
                return Err(crate::core::error::Error::Runtime(reason.clone()));
            }
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(_) => {}
        }
        let waiting = match std::mem::replace(
            &mut recorded.how_a_link_reaches_the_far_side,
            HowALinkReachesTheFarSide::RidesTheSetupCommand,
        ) {
            HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(waiting) => waiting,
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::HandedOverTo(_)
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => Vec::new(),
        };

        if let Err(send_failure) =
            send_the_setup_command_carrying_these_ports(recorded.as_setup_command_ports())
        {
            let reason = format!("the far side's setup command could not be sent: {send_failure}");
            refuse_every_link_waiting_behind_the_setup_command(waiting, &reason);
            recorded.how_a_link_reaches_the_far_side =
                HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(reason);
            return Err(send_failure);
        }

        for waiting_link in waiting {
            let handed_over = later_link_delivery.hand_over_a_link_wired_after_setup(
                waiting_link.port_direction,
                &waiting_link.link_wiring,
                Arc::clone(&waiting_link.answer_cell),
            );
            match handed_over {
                Ok(()) => recorded.record_a_link_handed_over_past_the_setup_command(
                    waiting_link.port_direction,
                    waiting_link.link_wiring,
                    waiting_link.answer_cell,
                ),
                Err(handover_failure) => waiting_link.refuse_because(&handover_failure.to_string()),
            }
        }
        recorded.how_a_link_reaches_the_far_side =
            HowALinkReachesTheFarSide::HandedOverTo(Box::new(later_link_delivery));
        Ok(())
    }

    /// Refuse every link recorded from now on with `reason`, and every link the
    /// far side still owes an answer for, because the far side is gone.
    pub fn refuse_every_later_link_because_the_far_side_is_gone(&self, reason: String) {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        let how_links_reached_it = std::mem::replace(
            &mut recorded.how_a_link_reaches_the_far_side,
            HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(reason.clone()),
        );
        match how_links_reached_it {
            HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(waiting) => {
                refuse_every_link_waiting_behind_the_setup_command(waiting, &reason);
            }
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => {
                delivery.refuse_every_link_still_awaiting_the_far_sides_answer(&reason);
            }
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => {}
        }
    }

    /// Reopen the envelope for a new far side standing in for one that died:
    /// every link the dead far side opened, or that rode its setup command,
    /// rides the new far side's setup command; one it never opened stays
    /// refused and is forgotten.
    pub fn reopen_for_a_new_far_side_in_place_of_one_that_died(&self) {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        let how_links_reached_the_dead_far_side = std::mem::replace(
            &mut recorded.how_a_link_reaches_the_far_side,
            HowALinkReachesTheFarSide::RidesTheSetupCommand,
        );
        let reason = "the helper process this link was handed to died before answering";
        match how_links_reached_the_dead_far_side {
            HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(waiting) => {
                refuse_every_link_waiting_behind_the_setup_command(waiting, reason);
            }
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => {
                delivery.refuse_every_link_still_awaiting_the_far_sides_answer(reason);
            }
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => {}
        }
        for never_opened_link_id in recorded.forget_every_link_its_far_side_never_opened() {
            self.output_frames_kept_by_its_output_links
                .forget_an_unwired_output_link(&never_opened_link_id);
        }
    }

    /// The `ports` payload a setup command sent now would carry.
    #[cfg(test)]
    pub(crate) fn as_setup_command_ports(&self) -> JsonValue {
        self.recorded_links_and_how_the_next_reaches_the_far_side
            .lock()
            .as_setup_command_ports()
    }
}

/// Object-safe version of [`GeneratedProcessor`] for dynamic dispatch.
///
/// **DO NOT USE DIRECTLY** - This is an internal implementation detail.
///
/// All lifecycle methods are synchronous per the Phase B ABI; plugins
/// that want async lifecycle work do their own `block_on` against a
/// self-owned runtime.
pub trait DynGeneratedProcessor: Send + 'static {
    /// Generated setup hook called by runtime with privileged ctx.
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Generated teardown hook called by runtime with privileged ctx.
    fn __generated_teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Generated on_pause hook — restricted ctx.
    fn __generated_on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Generated on_resume hook — restricted ctx.
    fn __generated_on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Called once to start a Manual mode processor. Privileged ctx.
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Called to stop a Manual mode processor. Privileged ctx.
    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    fn name(&self) -> &str;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    /// Returns the execution configuration for this processor.
    fn execution_config(&self) -> ExecutionConfig;

    /// Check if this processor has iceoryx2-based output ports.
    fn has_iceoryx2_outputs(&self) -> bool;

    /// Check if this processor has iceoryx2-based input ports.
    fn has_iceoryx2_inputs(&self) -> bool;

    /// Install host-allocated iceoryx2 resources (issue #894).
    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<crate::iceoryx2::OutputWriter>,
        input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> crate::core::Result<()>;

    /// Borrow the host-side `OutputWriterInner` Arc.
    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriterInner>>;

    /// Borrow the host-side `InputMailboxesInner` Arc.
    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>>;

    /// Notice a helper process that died on its own, and take its process group
    /// with it.
    ///
    /// Polled beside [`Self::has_failed_unrecoverably`] by the Manual-mode
    /// lifecycle loop. Separate from it because this one acts: a query that
    /// killed a process group would hide the kill behind a name that reads like
    /// a read. The engine puts a helper's
    /// group down at every helper exit, "a crash the engine detects by the
    /// process itself rather than by its socket" included — a descendant
    /// holding that socket keeps its EOF from ever arriving.
    ///
    /// Does nothing by default: only a processor hosting a child has one to
    /// lose.
    fn detect_and_clean_up_after_an_out_of_process_helper_that_died(&mut self) {}

    /// Whether this processor has failed in a way it cannot recover from, so
    /// the graph shows it in error while the rest of the pipeline keeps
    /// running.
    ///
    /// Polled by the Manual-mode lifecycle loop, which is the only place a
    /// processor doing its work elsewhere — on a callback thread the engine
    /// never enters, or in a helper process — can report a failure that never
    /// comes back from a callback. Reactive and continuous processors report
    /// by returning `Err` from the callback that failed.
    fn has_failed_unrecoverably(&self) -> bool {
        false
    }

    /// Why this processor's node failed itself, once it has: the graph shows
    /// it in error and its helper is not started again. `None` by default.
    fn failure_the_node_reported_of_itself(&self) -> Option<String> {
        None
    }

    /// What this processor's `process()` raised as its helper process last
    /// reported, while it has not run cleanly since; `None` by default, and for
    /// a processor whose `process()` the engine calls itself.
    fn why_process_is_raising_as_its_helper_reports(&self) -> Option<String> {
        None
    }

    /// Why this processor's node is not ready as its helper process last
    /// reported; `None` by default, and for a processor with no helper.
    fn why_the_node_is_not_ready_as_its_helper_reports(&self) -> Option<String> {
        None
    }

    /// Why this processor's helper process died on its own, once the engine
    /// has noticed and taken its group down; `None` for a processor with no
    /// helper, one whose helper is alive, or one the engine stopped itself.
    fn why_the_out_of_process_helper_died_on_its_own(&self) -> Option<String> {
        None
    }

    /// Ask to promote the function node whose call lost this processor's
    /// helper out of its islands, before the helper is restarted; `None` when
    /// no call is known to have lost it, or the promotion could not be asked.
    fn ask_to_promote_the_function_node_that_lost_the_helper_that_died(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> Option<FunctionNodePromotionAwaitedBeforeTheReaderRestarts> {
        None
    }

    /// Start a new helper process, with a new processor instance in it, in
    /// place of one that died on its own: the same node, its current config
    /// and every link it holds, then `run`.
    fn restart_the_out_of_process_helper_that_died(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> crate::core::Result<()> {
        Err(crate::core::Error::Runtime(
            "this processor runs in the app process, so it has no helper process to restart"
                .to_string(),
        ))
    }

    /// Where to record this processor's link wiring, when its iceoryx2 ports
    /// live outside the engine's address space.
    ///
    /// `Some` says two things that must never disagree: the engine cannot
    /// install a publisher or a subscriber for this processor, and this is the
    /// envelope to hand the service names and channel parameters to instead so
    /// the far side can open its own. Answering the first without the second
    /// would produce a processor the engine wires nothing into and that never
    /// learns what to wire itself — a graph that compiles, comes up, reports
    /// healthy, and moves no frames. One method, so it cannot be half
    /// implemented.
    ///
    /// Asked once, when the instance is attached to its graph node, which then
    /// carries the envelope: the compiler op reaches it there and never through
    /// this processor's lock. A host supplies the envelope and never records on
    /// it; it arms the envelope with how a later link reaches its far side once
    /// that far side's setup command is on its way.
    fn out_of_process_link_wiring(&self) -> Option<Arc<OutOfProcessLinkWiringEnvelope>> {
        None
    }

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()>;

    /// Serialize processor-specific runtime state to JSON.
    fn to_runtime_json(&self) -> serde_json::Value;

    /// Get the current config as JSON.
    fn config_json(&self) -> serde_json::Value;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Blanket implementation of DynGeneratedProcessor for all GeneratedProcessor types.
impl<T> DynGeneratedProcessor for T
where
    T: GeneratedProcessor,
{
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_setup(self, ctx)
    }

    fn __generated_teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_teardown(self, ctx)
    }

    fn __generated_on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_on_pause(self, ctx)
    }

    fn __generated_on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_on_resume(self, ctx)
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::process(self, ctx)
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::start(self, ctx)
    }

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::stop(self, ctx)
    }

    fn name(&self) -> &str {
        <Self as GeneratedProcessor>::name(self)
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <T as GeneratedProcessor>::descriptor()
    }

    fn execution_config(&self) -> ExecutionConfig {
        <Self as GeneratedProcessor>::execution_config(self)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        <Self as GeneratedProcessor>::has_iceoryx2_outputs(self)
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        <Self as GeneratedProcessor>::has_iceoryx2_inputs(self)
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<crate::iceoryx2::OutputWriter>,
        input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::set_iceoryx2_resources(self, output_writer, input_mailboxes)
    }

    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriterInner>> {
        <Self as GeneratedProcessor>::iceoryx2_output_writer_inner(self)
    }

    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>> {
        <Self as GeneratedProcessor>::iceoryx2_input_mailboxes_inner(self)
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::apply_config_json(self, config_json)
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::to_runtime_json(self)
    }

    fn config_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::config_json(self)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PortDirection;
    use crate::core::test_support::{ReclaimedLink, RecordingOutOfProcessFarSideLinkDelivery};

    fn link_wiring_entry(link_id: &str, port_name: &str) -> JsonValue {
        serde_json::json!({ "name": port_name, "link_id": link_id })
    }

    fn link_ids_the_setup_command_carries(ports: &JsonValue, direction: &str) -> Vec<String> {
        ports[direction]
            .as_array()
            .expect("the envelope renders both directions as arrays")
            .iter()
            .map(|link| link["link_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn record(envelope: &OutOfProcessLinkWiringEnvelope, direction: PortDirection, link_id: &str) {
        envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                direction,
                link_wiring_entry(link_id, "port"),
            )
            .expect("a far side that is not gone takes the link");
    }

    fn an_envelope_whose_island_calls_a_stateful_lfo() -> OutOfProcessLinkWiringEnvelope {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        envelope
            .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
                serde_json::json!({
                    "link_id": "L-island",
                    "nodes": [
                        {"node": "P-remap", "name": "remap"},
                        {
                            "node": "P-lfo",
                            "name": "lfo",
                            "node_uuid": the_lfo_uuid(),
                            "state_fields": ["phase"],
                        },
                    ],
                }),
            )
            .expect("a far side not yet set up takes its islands");
        envelope
    }

    fn the_lfo_uuid() -> uuid::Uuid {
        uuid::Uuid::from_u128(0x1f0)
    }

    fn the_lfo_at(phase: f64) -> crate::core::runtime::FunctionNodeState {
        crate::core::runtime::FunctionNodeState {
            state_fields: vec!["phase".to_string()],
            state_values: vec![phase],
        }
    }

    #[test]
    fn an_island_recorded_again_notes_each_stateful_node_as_the_live_edit_left_it() {
        let envelope = an_envelope_whose_island_calls_a_stateful_lfo();
        envelope
            .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
                serde_json::json!({
                    "link_id": "L-island",
                    "nodes": [
                        {"node": "P-remap", "name": "remap"},
                        {
                            "node": "P-lfo",
                            "name": "lfo",
                            "node_uuid": the_lfo_uuid(),
                            "state_fields": ["phase", "depth"],
                        },
                    ],
                }),
            )
            .expect("a far side not yet set up takes its islands");

        let recorded = envelope
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        let [lfo] = recorded
            .stateful_function_nodes_its_far_side_calls
            .as_slice()
        else {
            panic!("the lfo is noted once");
        };
        assert_eq!(lfo.function_node_uuid, the_lfo_uuid());
        assert_eq!(
            lfo.state_fields,
            vec!["phase".to_string(), "depth".to_string()]
        );
    }

    fn the_lfo_island_recorded_again_with(lfo: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "link_id": "L-island",
            "nodes": [{"node": "P-remap", "name": "remap"}, lfo],
        })
    }

    #[test]
    fn a_node_a_live_edit_makes_stateless_is_no_longer_noted_stateful() {
        let envelope = an_envelope_whose_island_calls_a_stateful_lfo();
        envelope
            .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
                the_lfo_island_recorded_again_with(serde_json::json!({
                    "node": "P-lfo",
                    "name": "lfo",
                })),
            )
            .expect("a far side not yet set up takes its islands");

        assert!(
            envelope
                .recorded_links_and_how_the_next_reaches_the_far_side
                .lock()
                .stateful_function_nodes_its_far_side_calls
                .is_empty()
        );
    }

    /// Each spawn's board carries an entry for the stateful node alone, and a
    /// spawn after one that wrote a state resumes the node from it.
    #[test]
    fn a_new_spawn_resumes_each_stateful_node_from_the_state_the_last_spawn_wrote() {
        let envelope = an_envelope_whose_island_calls_a_stateful_lfo();
        let function_node_states = Arc::new(FunctionNodeStatesCarriedForOneStream::default());
        let parent_node = Iceoryx2Node::for_this_test_process();

        let first_spawns_board = envelope
            .create_the_loss_count_board_for_this_helper_spawn_on(
                &parent_node,
                &function_node_states,
                "P-reader",
                Vec::new(),
            )
            .expect("the first spawn creates its board");
        assert_eq!(
            first_spawns_board["function_node_states"],
            serde_json::json!([{"breadcrumb_number": 1, "resumes_from_f64_bits": null}]),
            "only the stateful node has an entry, and nothing is carried for it yet"
        );
        let first_helper_node = Iceoryx2Node::for_this_test_process();
        let first_helpers_writer = first_helper_node
            .open_helper_process_loss_count_board_writer(
                first_spawns_board["service_name"].as_str().unwrap(),
                &[],
                &[1],
            )
            .expect("the helper opens its board");
        first_helpers_writer
            .write_the_state_of_a_function_node(1, &[0.25])
            .unwrap();
        assert_eq!(
            function_node_states.the_current_state_of(the_lfo_uuid()),
            Some(the_lfo_at(0.25)),
            "the stream reads the state live off the board its helper writes"
        );
        drop(first_helpers_writer);
        envelope.keep_every_function_node_state_its_far_side_wrote();

        let second_spawns_board = envelope
            .create_the_loss_count_board_for_this_helper_spawn_on(
                &parent_node,
                &function_node_states,
                "P-reader",
                Vec::new(),
            )
            .expect("the restart creates its own board");
        assert_eq!(
            second_spawns_board["function_node_states"],
            serde_json::json!([{
                "breadcrumb_number": 1,
                "resumes_from_f64_bits": [0.25f64.to_bits()],
            }])
        );
        assert_eq!(
            function_node_states.the_current_state_of(the_lfo_uuid()),
            Some(the_lfo_at(0.25)),
            "the state the first spawn left stands until the second writes"
        );
    }

    /// A promoted node's own helper takes over writing its state, and the
    /// reader's restart afterwards leaves the state the promoted node writes.
    #[test]
    fn a_promoted_nodes_own_helper_takes_its_state_over_from_the_reader_it_crashed() {
        let reader = an_envelope_whose_island_calls_a_stateful_lfo();
        let function_node_states = Arc::new(FunctionNodeStatesCarriedForOneStream::default());
        let parent_node = Iceoryx2Node::for_this_test_process();
        let readers_board = reader
            .create_the_loss_count_board_for_this_helper_spawn_on(
                &parent_node,
                &function_node_states,
                "P-reader",
                Vec::new(),
            )
            .unwrap();
        let readers_helper_node = Iceoryx2Node::for_this_test_process();
        readers_helper_node
            .open_helper_process_loss_count_board_writer(
                readers_board["service_name"].as_str().unwrap(),
                &[],
                &[1],
            )
            .unwrap()
            .write_the_state_of_a_function_node(1, &[0.5])
            .unwrap();

        let promoted =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        promoted.number_the_function_node_its_far_side_runs_as_its_processor(
            "P-lfo",
            "lfo",
            the_lfo_uuid(),
            vec!["phase".to_string()],
        );
        let promoted_board = promoted
            .create_the_loss_count_board_for_this_helper_spawn_on(
                &parent_node,
                &function_node_states,
                "P-lfo",
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            promoted_board["function_node_states"][0]["resumes_from_f64_bits"],
            serde_json::json!([0.5f64.to_bits()]),
            "the promoted node resumes from what its reader's helper last wrote"
        );
        let promoted_helper_node = Iceoryx2Node::for_this_test_process();
        promoted_helper_node
            .open_helper_process_loss_count_board_writer(
                promoted_board["service_name"].as_str().unwrap(),
                &[],
                &[0],
            )
            .unwrap()
            .write_the_state_of_a_function_node(0, &[0.75])
            .unwrap();

        reader.keep_every_function_node_state_its_far_side_wrote();
        assert_eq!(
            function_node_states.the_current_state_of(the_lfo_uuid()),
            Some(the_lfo_at(0.75)),
            "the reader letting its helper go never takes the state back"
        );
    }

    /// A reader whose island moved out with its stateful node carries no
    /// entry for that node on its next spawn's board, so the node's own helper
    /// stays the one whose state the stream carries.
    #[test]
    fn a_readers_spawn_after_its_island_moved_out_never_takes_the_moved_nodes_state_over() {
        let reader = an_envelope_whose_island_calls_a_stateful_lfo();
        let function_node_states = Arc::new(FunctionNodeStatesCarriedForOneStream::default());
        let parent_node = Iceoryx2Node::for_this_test_process();
        reader.forget_a_function_node_island("L-island");
        let promoted =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        promoted.number_the_function_node_its_far_side_runs_as_its_processor(
            "P-lfo",
            "lfo",
            the_lfo_uuid(),
            vec!["phase".to_string()],
        );
        promoted.forget_a_function_node_island("L-an-island-feeding-the-promoted-node");
        let promoted_board = promoted
            .create_the_loss_count_board_for_this_helper_spawn_on(
                &parent_node,
                &function_node_states,
                "P-lfo",
                Vec::new(),
            )
            .unwrap();
        let promoted_helper_node = Iceoryx2Node::for_this_test_process();
        promoted_helper_node
            .open_helper_process_loss_count_board_writer(
                promoted_board["service_name"].as_str().unwrap(),
                &[],
                &[0],
            )
            .unwrap()
            .write_the_state_of_a_function_node(0, &[0.25])
            .unwrap();

        let readers_board = reader
            .create_the_loss_count_board_for_this_helper_spawn_on(
                &parent_node,
                &function_node_states,
                "P-reader",
                Vec::new(),
            )
            .unwrap();

        assert_eq!(
            readers_board["function_node_states"],
            serde_json::json!([]),
            "the reader evaluates the moved node no more"
        );
        assert_eq!(
            function_node_states.the_current_state_of(the_lfo_uuid()),
            Some(the_lfo_at(0.25)),
            "the moved node's own helper stays the one whose state the stream carries"
        );
    }

    /// A host that stopped waiting leaves no promotion for a late compile to
    /// claim, so its restarted far side keeps the islands it started with.
    #[test]
    fn a_promotion_the_host_stopped_waiting_on_is_not_claimed_by_a_late_compile() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        let given_up = envelope.await_a_function_node_promotion_out_of_its_islands();

        let outcome = envelope.wait_for_the_function_node_promotion_out_of_its_islands_to_settle(
            std::time::Duration::ZERO,
            || false,
        );

        assert_eq!(outcome, None);
        assert!(!envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(given_up));
    }

    /// A request the host gave up cannot claim the one it awaits after the next
    /// death: only the compile carrying the awaited request plans a promotion.
    #[test]
    fn an_earlier_promotion_request_cannot_claim_the_one_the_host_now_awaits() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        let given_up = envelope.await_a_function_node_promotion_out_of_its_islands();
        envelope.give_up_the_function_node_promotion_out_of_its_islands_unless_claimed();
        let awaited_now = envelope.await_a_function_node_promotion_out_of_its_islands();

        assert!(!envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(given_up));
        assert!(envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(awaited_now));
    }

    /// Once a compile claims the promotion, the host waits past its deadline
    /// for how it ends.
    #[test]
    fn a_claimed_promotion_is_waited_on_past_the_deadline_until_it_settles() {
        let envelope = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
            ProcessExecution::Reactive,
        ));
        let awaited = envelope.await_a_function_node_promotion_out_of_its_islands();
        assert!(envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(awaited));
        assert!(
            !envelope.give_up_the_function_node_promotion_out_of_its_islands_unless_claimed(),
            "a claimed promotion is not given up"
        );
        let settling_envelope = Arc::clone(&envelope);
        let settler = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            settling_envelope
                .settle_the_function_node_promotion_out_of_its_islands(Err("refused".to_string()));
        });

        let outcome = envelope.wait_for_the_function_node_promotion_out_of_its_islands_to_settle(
            std::time::Duration::ZERO,
            || false,
        );

        settler.join().unwrap();
        assert_eq!(outcome, Some(Err("refused".to_string())));
    }

    /// A disconnected link leaves the envelope in both directions, and only
    /// that link does. Leave it behind and the next setup re-sends it beside
    /// the reconnect's own entry — two subscribers, two notifiers, one link.
    #[test]
    fn a_removed_link_leaves_the_envelope_and_its_neighbours_stay() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        record(&envelope, PortDirection::Input, "L-gone");
        record(&envelope, PortDirection::Input, "L-stays");
        record(&envelope, PortDirection::Output, "L-gone");
        record(&envelope, PortDirection::Output, "L-stays");

        envelope
            .forget_a_link_and_tell_a_far_side_past_its_setup_command(
                PortDirection::Input,
                "port",
                "L-gone",
            )
            .expect("a far side that was never set up needs no telling");

        let ports = envelope.as_setup_command_ports();
        assert_eq!(
            link_ids_the_setup_command_carries(&ports, "inputs"),
            ["L-stays"]
        );
        assert_eq!(
            link_ids_the_setup_command_carries(&ports, "outputs"),
            ["L-stays"]
        );
    }

    /// Removing a link the envelope never carried changes nothing — the
    /// compiler reclaims every link it closes, including ones whose far side
    /// was never wired.
    #[test]
    fn removing_an_unknown_link_leaves_the_envelope_alone() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        record(&envelope, PortDirection::Output, "L-only");

        envelope
            .forget_a_link_and_tell_a_far_side_past_its_setup_command(
                PortDirection::Output,
                "port",
                "L-never-recorded",
            )
            .expect("a far side that was never set up needs no telling");

        assert_eq!(
            link_ids_the_setup_command_carries(&envelope.as_setup_command_ports(), "outputs"),
            ["L-only"]
        );
    }

    /// A link recorded before the setup command goes out rides it and waits on
    /// no answer of its own; one recorded after is handed to the far side and
    /// answered for.
    ///
    /// Fail-without-fix: leave the envelope riding the setup command once it
    /// has gone out and the later link is recorded where nothing reads it — a
    /// link `graph` reports wired that no port was ever opened for.
    #[test]
    fn a_link_recorded_before_the_setup_command_rides_it_and_one_recorded_after_is_handed_over() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        let before = envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry("L-before", "in1"),
            )
            .expect("a far side not yet set up takes the link");
        assert!(before.is_none());

        let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
        let mut setup_command_ports = None;
        envelope
            .send_the_setup_command_then_hand_every_later_link_over(
                |ports| {
                    setup_command_ports = Some(ports);
                    Ok(())
                },
                far_side.clone(),
            )
            .expect("the setup command goes out");
        assert_eq!(
            link_ids_the_setup_command_carries(&setup_command_ports.unwrap(), "inputs"),
            ["L-before"]
        );

        let after = envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry("L-after", "in1"),
            )
            .expect("a far side past its setup command takes the link");
        assert!(
            after.is_some(),
            "a link handed over waits on its far side's answer"
        );
        let handed_over = far_side.late_wired_links.lock();
        let [(PortDirection::Input, entry)] = &handed_over[..] else {
            panic!("exactly the later link is handed over; got {handed_over:?}");
        };
        assert_eq!(entry["link_id"], "L-after");
    }

    /// A link recorded while the setup command is going out waits for it, then
    /// is handed over behind it — never recorded into a snapshot already taken
    /// and never handed to a far side that has not been set up.
    ///
    /// Fail-without-fix: take the setup command's `ports` outside the lock and
    /// the link recorded meanwhile is neither in them nor handed over.
    #[test]
    fn a_link_recorded_while_the_setup_command_goes_out_is_handed_over_behind_it() {
        let envelope = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
            ProcessExecution::Reactive,
        ));
        let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
        let (setup_command_is_going_out_tx, setup_command_is_going_out_rx) =
            std::sync::mpsc::channel();
        let (let_the_setup_command_finish_tx, let_the_setup_command_finish_rx) =
            std::sync::mpsc::channel::<()>();

        std::thread::scope(|scope| {
            let setup_envelope = Arc::clone(&envelope);
            let setup_far_side = far_side.clone();
            let setup_thread = scope.spawn(move || {
                let mut setup_command_ports = None;
                setup_envelope
                    .send_the_setup_command_then_hand_every_later_link_over(
                        |ports| {
                            setup_command_is_going_out_tx.send(()).unwrap();
                            let_the_setup_command_finish_rx.recv().unwrap();
                            setup_command_ports = Some(ports);
                            Ok(())
                        },
                        setup_far_side,
                    )
                    .expect("the setup command goes out");
                setup_command_ports.unwrap()
            });
            setup_command_is_going_out_rx.recv().unwrap();

            let (recorded_tx, recorded_rx) = std::sync::mpsc::channel();
            let recording_envelope = Arc::clone(&envelope);
            let recording_thread = scope.spawn(move || {
                let reply = recording_envelope
                    .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                        PortDirection::Output,
                        link_wiring_entry("L-meanwhile", "out1"),
                    )
                    .expect("the link is taken");
                recorded_tx.send(()).unwrap();
                reply
            });
            assert!(
                recorded_rx
                    .recv_timeout(std::time::Duration::from_millis(200))
                    .is_err(),
                "a link recorded while the setup command goes out waits for it"
            );

            let_the_setup_command_finish_tx.send(()).unwrap();
            let setup_command_ports = setup_thread.join().unwrap();
            let reply = recording_thread.join().unwrap();

            assert!(
                link_ids_the_setup_command_carries(&setup_command_ports, "outputs").is_empty(),
                "the snapshot was taken before the link was recorded"
            );
            assert!(
                reply.is_some(),
                "so the link is handed over behind the command"
            );
            assert_eq!(far_side.late_wired_links.lock().len(), 1);
        });
    }

    /// Once the host gives its far side up, a link recorded after is refused
    /// with the host's reason, and a disconnect tells nobody.
    #[test]
    fn a_link_recorded_once_the_far_side_is_gone_is_refused_naming_why() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
        envelope
            .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), far_side.clone())
            .expect("the setup command goes out");

        envelope.refuse_every_later_link_because_the_far_side_is_gone(
            crate::core::compiler::compiler_ops::subprocess_bridge::refusal_of_a_link_into_a_helper_process_that_failed(
                "Blur", "Pblur",
            ),
        );

        let refused = envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry("L-too-late", "in1"),
            )
            .expect_err("a far side that is gone can open no port");
        assert!(refused.to_string().contains("has failed"), "{refused}");
        envelope
            .forget_a_link_and_tell_a_far_side_past_its_setup_command(
                PortDirection::Input,
                "in1",
                "L-too-late",
            )
            .expect("a far side that is gone needs no telling");
        assert!(far_side.late_wired_links.lock().is_empty());
        assert!(far_side.reclaimed_links.lock().is_empty());
    }

    fn refusal_reason_of(answer_cell: &OutOfProcessLinkWireReply) -> Option<String> {
        match answer_cell.the_far_sides_answer() {
            Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { reason }) => Some(reason),
            _ => None,
        }
    }

    /// A link recorded once the far side's setup has begun is not folded into a
    /// setup command whose `ports` may already be taken: it waits, reads as
    /// awaiting its own answer, and is handed over right behind the command.
    ///
    /// Fail-without-fix: let it ride the setup command and `connect` reports a
    /// link `wired` that no helper has confirmed.
    #[test]
    fn a_link_recorded_once_setup_has_begun_is_handed_over_right_behind_the_setup_command() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        record(&envelope, PortDirection::Input, "L-startup");
        envelope.hold_every_later_link_until_the_setup_command_goes_out();

        let answer_cell = envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Output,
                link_wiring_entry("L-live", "out1"),
            )
            .expect("a far side setting up takes the link")
            .expect("a link recorded during setup waits on its own answer");

        let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
        let mut setup_command_ports = None;
        envelope
            .send_the_setup_command_then_hand_every_later_link_over(
                |ports| {
                    assert!(
                        far_side.late_wired_links.lock().is_empty(),
                        "nothing is handed over ahead of the setup command"
                    );
                    setup_command_ports = Some(ports);
                    Ok(())
                },
                far_side.clone(),
            )
            .expect("the setup command goes out");

        let ports = setup_command_ports.expect("the setup command was sent");
        assert_eq!(
            link_ids_the_setup_command_carries(&ports, "inputs"),
            ["L-startup"]
        );
        assert!(link_ids_the_setup_command_carries(&ports, "outputs").is_empty());
        let handed_over = far_side.late_wired_links.lock();
        let [(PortDirection::Output, entry)] = &handed_over[..] else {
            panic!("exactly the live link is handed over; got {handed_over:?}");
        };
        assert_eq!(entry["link_id"], "L-live");
        assert!(Arc::ptr_eq(
            &far_side.wire_answers_owed.lock()[0],
            &answer_cell
        ));
    }

    /// A link waiting behind a setup command that could not be sent is refused
    /// with the reason, and so is every link recorded after.
    #[test]
    fn a_setup_command_that_could_not_be_sent_refuses_the_links_waiting_behind_it() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        envelope.hold_every_later_link_until_the_setup_command_goes_out();
        let answer_cell = envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry("L-waiting", "in1"),
            )
            .expect("a far side setting up takes the link")
            .expect("a link recorded during setup waits on its own answer");

        envelope
            .send_the_setup_command_then_hand_every_later_link_over(
                |_| {
                    Err(crate::core::error::Error::Runtime(
                        "socket closed".to_string(),
                    ))
                },
                RecordingOutOfProcessFarSideLinkDelivery::default(),
            )
            .expect_err("the send failed");

        let reason = refusal_reason_of(&answer_cell).expect("the waiting link is refused");
        assert!(reason.contains("socket closed"), "{reason}");
        envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry("L-after", "in1"),
            )
            .expect_err("a far side whose setup never went out takes no link");
    }

    /// Giving the far side up before its setup command goes out refuses the
    /// links waiting behind it; a link disconnected while waiting is never
    /// handed over.
    #[test]
    fn a_waiting_link_is_refused_when_the_far_side_is_given_up_and_dropped_when_disconnected() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        envelope.hold_every_later_link_until_the_setup_command_goes_out();
        let disconnected = envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry("L-disconnected", "in1"),
            )
            .unwrap()
            .unwrap();
        envelope
            .forget_a_link_and_tell_a_far_side_past_its_setup_command(
                PortDirection::Input,
                "in1",
                "L-disconnected",
            )
            .expect("a waiting link needs no far side told");
        let given_up = envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry("L-given-up", "in1"),
            )
            .unwrap()
            .unwrap();

        envelope.refuse_every_later_link_because_the_far_side_is_gone(
            crate::core::compiler::compiler_ops::subprocess_bridge::refusal_of_a_link_into_a_helper_process_that_failed(
                "Blur", "Pblur",
            ),
        );

        assert!(refusal_reason_of(&given_up).is_some_and(|reason| reason.contains("has failed")));
        assert_eq!(
            disconnected.the_far_sides_answer(),
            None,
            "a link the graph no longer has is answered for by nobody"
        );
    }

    /// A disconnect asks a far side past its setup command to drop its port, by
    /// its own port and direction.
    #[test]
    fn forgetting_a_link_tells_a_far_side_past_its_setup_command_which_port_to_drop() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
        envelope
            .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), far_side.clone())
            .expect("the setup command goes out");
        record(&envelope, PortDirection::Output, "L-out");

        envelope
            .forget_a_link_and_tell_a_far_side_past_its_setup_command(
                PortDirection::Output,
                "out1",
                "L-out",
            )
            .expect("the far side is told");

        assert_eq!(
            *far_side.reclaimed_links.lock(),
            [ReclaimedLink {
                port_direction: PortDirection::Output,
                local_port_name: "out1".to_string(),
                link_id: "L-out".to_string(),
            }]
        );
        assert!(
            link_ids_the_setup_command_carries(&envelope.as_setup_command_ports(), "outputs")
                .is_empty()
        );
    }

    /// A new helper standing in for one that died is handed, on its own setup
    /// command, every link the dead helper opened, those that rode its setup
    /// command and those handed over past it alike, and never one it was handed
    /// and never opened, which `graph` reads `error`.
    ///
    /// Fail-without-fix: leave the envelope refusing and the new helper's setup
    /// command is refused; clear the recorded links on reopen and it opens no
    /// port; keep every recorded link and the unopened one rides along.
    #[test]
    fn a_reopened_envelope_hands_a_new_far_side_every_link_the_dead_one_held() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        record(&envelope, PortDirection::Input, "L-before-setup");
        envelope.hold_every_later_link_until_the_setup_command_goes_out();
        envelope
            .send_the_setup_command_then_hand_every_later_link_over(
                |_| Ok(()),
                RecordingOutOfProcessFarSideLinkDelivery::default(),
            )
            .expect("the dead helper's setup command went out");
        envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Output,
                link_wiring_entry("L-past-setup", "port"),
            )
            .expect("a far side that is not gone takes the link")
            .expect("a link past the setup command waits on the far side's answer")
            .note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        record(&envelope, PortDirection::Output, "L-never-opened");
        envelope.refuse_every_later_link_because_the_far_side_is_gone(
            "the helper process died".to_string(),
        );

        envelope.reopen_for_a_new_far_side_in_place_of_one_that_died();
        envelope.hold_every_later_link_until_the_setup_command_goes_out();
        let mut new_far_sides_setup_command_ports = None;
        envelope
            .send_the_setup_command_then_hand_every_later_link_over(
                |ports| {
                    new_far_sides_setup_command_ports = Some(ports);
                    Ok(())
                },
                RecordingOutOfProcessFarSideLinkDelivery::default(),
            )
            .expect("the new helper's setup command goes out");

        let ports = new_far_sides_setup_command_ports.expect("the setup command carried ports");
        assert_eq!(
            link_ids_the_setup_command_carries(&ports, "inputs"),
            ["L-before-setup"]
        );
        assert_eq!(
            link_ids_the_setup_command_carries(&ports, "outputs"),
            ["L-past-setup"]
        );
    }

    fn an_island_of(input_port: &str, function_node_ids: &[&str]) -> JsonValue {
        serde_json::json!({
            "input_port": input_port,
            "link_id": format!("L-{input_port}"),
            "output": {"node": function_node_ids.last().unwrap(), "port": "result"},
            "nodes": function_node_ids
                .iter()
                .map(|node_id| serde_json::json!({"node": node_id, "links_into": []}))
                .collect::<Vec<_>>(),
        })
    }

    /// The far side's `ready` says how it ran each function node, and only the
    /// nodes its islands call are told; a bool is all it reads.
    #[test]
    fn each_function_node_is_told_how_the_far_side_ran_it() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        let how_lfo_ran = Arc::new(HowItsHelpersRanAFunctionNode::default());
        let how_tally_ran = Arc::new(HowItsHelpersRanAFunctionNode::default());
        envelope.tell_each_function_node_how_its_far_side_ran_it([
            ("lfo-1".to_string(), Arc::clone(&how_lfo_ran)),
            ("tally-1".to_string(), Arc::clone(&how_tally_ran)),
        ]);

        envelope.note_how_its_far_side_ran_its_function_nodes(Some(&serde_json::json!({
            "lfo-1": true,
            "tally-1": false,
            "never-carried": false,
        })));
        assert_eq!(how_lfo_ran.ran_compiled(), Some(true));
        assert_eq!(how_tally_ran.ran_compiled(), Some(false));

        envelope.note_how_its_far_side_ran_its_function_nodes(Some(&serde_json::json!({
            "lfo-1": "yes",
        })));
        envelope.note_how_its_far_side_ran_its_function_nodes(None);
        assert_eq!(how_lfo_ran.ran_compiled(), Some(true));
    }

    /// Each function node an island calls rides the setup command with one
    /// breadcrumb number, the same in every island that calls it, so the
    /// number its far side writes names one node.
    #[test]
    fn each_function_node_rides_the_setup_command_with_one_breadcrumb_number() {
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
        envelope
            .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
                an_island_of("brightness", &["lfo-1", "remap-1"]),
            )
            .unwrap();
        envelope
            .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
                an_island_of("hue", &["lfo-1", "clamp-1"]),
            )
            .unwrap();

        let islands = envelope.as_setup_command_ports()["islands"].clone();
        let breadcrumb_numbers: Vec<Vec<(String, u64)>> = islands
            .as_array()
            .unwrap()
            .iter()
            .map(|island| {
                island["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|node| {
                        (
                            node["node"].as_str().unwrap().to_string(),
                            node["breadcrumb_number"].as_u64().unwrap(),
                        )
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            breadcrumb_numbers,
            [
                vec![("lfo-1".to_string(), 0), ("remap-1".to_string(), 1)],
                vec![("lfo-1".to_string(), 0), ("clamp-1".to_string(), 2)],
            ]
        );
    }
}

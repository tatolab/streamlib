// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::core::Result;
use crate::core::context::RuntimeContextFullAccess;
use crate::core::context::texture_pools_and_rings::node_output_surface_pool::OutputFramesKeptByTheOutputLinksOfOneNode;
use crate::core::execution::ProcessExecution;
use crate::core::graph::{HowItsHelpersRanAFunctionNode, HowOneReadersHelperRanEachFunctionNode};
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::core::nodes::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
use crate::core::runtime::FunctionNodeStatesCarriedForOneStream;
use crate::iceoryx2::{HelperPlacedNodeLossCounts, Iceoryx2Node};
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

    /// Hand the far side the value each of its inputs reads while no link
    /// feeds it, in place of those it reads now, and one fire of each Trigger
    /// input of `fired`, in order.
    fn hand_over_the_unlinked_input_values(
        &self,
        input_values: &serde_json::Map<String, JsonValue>,
        fired: &[String],
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

/// Why fires were not handed to a far side.
#[derive(Debug)]
pub(crate) enum FiresNotHandedToTheFarSide {
    /// The far side has not been sent its setup command, so its node does not run yet.
    NotSetUpYet,
    /// The far side, or the way to it, refused them.
    Refused(crate::core::Error),
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
    /// The value each of its inputs reads while no link feeds it, by input
    /// name.
    unlinked_input_values: serde_json::Map<String, JsonValue>,
    /// The config fields its node takes as inputs, each read from the input
    /// port of its name.
    config_fields_its_node_exposes_as_inputs: Vec<String>,
    /// Each function node its islands call, by the breadcrumb number its far
    /// side writes while calling it.
    function_nodes_by_breadcrumb_number: Vec<FunctionNodeNumberedOnTheBreadcrumb>,
    /// Each stateful function node its far side calls, whose state its board
    /// carries.
    stateful_function_nodes_its_far_side_calls: Vec<StatefulFunctionNodeItsFarSideCalls>,
    runs_a_function_node_as_a_standalone_node: bool,
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
            "input_values": self.unlinked_input_values,
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

pub struct OutOfProcessLinkWiringEnvelope {
    recorded_links_and_how_the_next_reaches_the_far_side:
        Mutex<RecordedLinksAndHowTheNextReachesTheFarSide>,
    loss_counts: Arc<HelperPlacedNodeLossCounts>,
    output_frames_kept_by_its_output_links: Arc<OutputFramesKeptByTheOutputLinksOfOneNode>,
    /// Where each function node its islands call is told how its far side ran
    /// it, by the node's id.
    how_its_far_side_ran_each_function_node: Arc<HowOneReadersHelperRanEachFunctionNode>,
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
    pub fn for_a_far_side_driven_in(far_side_process_execution: ProcessExecution) -> Self {
        Self {
            recorded_links_and_how_the_next_reaches_the_far_side: Mutex::new(
                RecordedLinksAndHowTheNextReachesTheFarSide {
                    input_links: Vec::new(),
                    output_links: Vec::new(),
                    function_node_islands_feeding_its_input_ports: Vec::new(),
                    unlinked_input_values: serde_json::Map::new(),
                    config_fields_its_node_exposes_as_inputs: Vec::new(),
                    function_nodes_by_breadcrumb_number: Vec::new(),
                    stateful_function_nodes_its_far_side_calls: Vec::new(),
                    runs_a_function_node_as_a_standalone_node: false,
                    answer_cells_of_links_handed_over_past_the_setup_command: Vec::new(),
                    how_a_link_reaches_the_far_side:
                        HowALinkReachesTheFarSide::RidesTheSetupCommand,
                },
            ),
            loss_counts: Arc::default(),
            output_frames_kept_by_its_output_links: Arc::default(),
            how_its_far_side_ran_each_function_node: Arc::new(
                HowOneReadersHelperRanEachFunctionNode::for_a_new_helper(),
            ),
            far_side_process_execution,
            function_node_promotion_out_of_its_islands: Default::default(),
            function_node_states_its_far_side_writes: Mutex::new(None),
        }
    }

    pub(crate) fn output_frames_kept_by_its_output_links(
        &self,
    ) -> Arc<OutputFramesKeptByTheOutputLinksOfOneNode> {
        Arc::clone(&self.output_frames_kept_by_its_output_links)
    }

    /// Where each function node its islands call is told how its far side ran
    /// it: whether compiled, and what its calls raised.
    pub(crate) fn how_its_far_side_ran_each_function_node(
        &self,
    ) -> Arc<HowOneReadersHelperRanEachFunctionNode> {
        Arc::clone(&self.how_its_far_side_ran_each_function_node)
    }

    pub fn far_side_process_execution(&self) -> ProcessExecution {
        self.far_side_process_execution
    }

    pub(crate) fn helper_placed_node_loss_counts(&self) -> Arc<HelperPlacedNodeLossCounts> {
        Arc::clone(&self.loss_counts)
    }

    pub fn create_the_loss_count_board_for_this_helper_spawn(
        &self,
        ctx: &RuntimeContextFullAccess<'_>,
        node_run_id: &str,
        output_port_names: Vec<String>,
    ) -> Result<JsonValue> {
        self.create_the_loss_count_board_for_this_helper_spawn_on(
            ctx.host_base().iceoryx2_node(),
            ctx.host_base().this_streams_function_node_states(),
            node_run_id,
            output_port_names,
        )
    }

    pub(crate) fn create_the_loss_count_board_for_this_helper_spawn_on(
        &self,
        iceoryx2_node: &Iceoryx2Node,
        function_node_states: &Arc<FunctionNodeStatesCarriedForOneStream>,
        node_run_id: &str,
        output_port_names: Vec<String>,
    ) -> Result<JsonValue> {
        let service_name = format!(
            "streamlib/{node_run_id}/loss-counts/{}",
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

    /// Record the value each of its inputs reads while no link feeds it, and
    /// hand them to a far side past its setup command when they differ from
    /// those it holds; values a far side refuses are not recorded.
    pub(crate) fn record_the_unlinked_input_values_and_hand_them_to_a_far_side_past_its_setup_command(
        &self,
        input_values: serde_json::Map<String, JsonValue>,
    ) -> Result<()> {
        let mut recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        if recorded.unlinked_input_values == input_values {
            return Ok(());
        }
        if let HowALinkReachesTheFarSide::HandedOverTo(delivery) =
            &recorded.how_a_link_reaches_the_far_side
        {
            delivery.hand_over_the_unlinked_input_values(&input_values, &[])?;
        }
        recorded.unlinked_input_values = input_values;
        Ok(())
    }

    /// Hand a far side past its setup command one fire of each Trigger input of
    /// `fired`, in order, beside the input values recorded, which it holds
    /// already; nothing is recorded, a fire being read once. Refused, as its node
    /// not running yet, while the far side has not been sent its setup command.
    pub(crate) fn hand_a_far_side_past_its_setup_command_the_fires_of(
        &self,
        fired: &[String],
    ) -> std::result::Result<(), FiresNotHandedToTheFarSide> {
        let recorded = self
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock();
        match &recorded.how_a_link_reaches_the_far_side {
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => delivery
                .hand_over_the_unlinked_input_values(&recorded.unlinked_input_values, fired)
                .map_err(FiresNotHandedToTheFarSide::Refused),
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(_) => {
                Err(FiresNotHandedToTheFarSide::NotSetUpYet)
            }
            HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(reason) => {
                Err(FiresNotHandedToTheFarSide::Refused(
                    crate::core::Error::Runtime(format!("its helper is gone: {reason}")),
                ))
            }
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
        self.how_its_far_side_ran_each_function_node
            .report_each_function_node_to(function_nodes);
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
        for (function_node_id, ran_compiled) in ran_compiled_by_function_node_id {
            if let Some(ran_compiled) = ran_compiled.as_bool() {
                self.how_its_far_side_ran_each_function_node
                    .note_it_ran_compiled(function_node_id, ran_compiled);
            }
        }
    }

    pub(crate) fn number_the_function_node_its_far_side_runs_as_a_standalone_node(
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
        recorded.runs_a_function_node_as_a_standalone_node = true;
        debug_assert_eq!(
            breadcrumb_number,
            crate::iceoryx2::BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_A_STANDALONE_NODE
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
        let runs_a_standalone_node = recorded.runs_a_function_node_as_a_standalone_node;
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
                (runs_a_standalone_node
                    && stateful.breadcrumb_number
                        == crate::iceoryx2::BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_A_STANDALONE_NODE)
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

#[cfg(test)]
mod tests;

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Object-safe wrapper for GeneratedProcessor - DO NOT USE DIRECTLY.

use std::sync::Arc;

use parking_lot::Mutex;

use super::GeneratedProcessor;
use crate::core::ProcessorDescriptor;
use crate::core::Result;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
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

    /// Refuse every link the far side still owes an answer for, because its
    /// host gave up on it.
    fn refuse_every_link_still_awaiting_the_far_sides_answer(&self);
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
    how_a_link_reaches_the_far_side: HowALinkReachesTheFarSide,
}

impl RecordedLinksAndHowTheNextReachesTheFarSide {
    fn links_facing(&mut self, port_direction: crate::core::PortDirection) -> &mut Vec<JsonValue> {
        match port_direction {
            crate::core::PortDirection::Input => &mut self.input_links,
            crate::core::PortDirection::Output => &mut self.output_links,
        }
    }

    fn as_setup_command_ports(&self) -> JsonValue {
        serde_json::json!({
            "inputs": self.input_links,
            "outputs": self.output_links,
        })
    }
}

fn refuse_the_link_waiting_for_the_setup_command(
    waiting: LinkAwaitingHandoverBehindTheSetupCommand,
    reason: &str,
) {
    waiting.answer_cell.note_the_far_sides_answer(
        OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason: reason.to_string(),
        },
    );
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
    far_side_process_execution: ProcessExecution,
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
                    how_a_link_reaches_the_far_side:
                        HowALinkReachesTheFarSide::RidesTheSetupCommand,
                },
            ),
            loss_counts: Arc::default(),
            far_side_process_execution,
        }
    }

    /// The mode the far side drives its processor in, which decides whether
    /// it ever drains the listener its sources would notify.
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
    /// command.
    ///
    /// Named per spawn rather than per processor, so a spawn never opens a
    /// board an earlier one's dead writer still holds. The envelope keeps the
    /// board until the processor is removed, whatever becomes of the child.
    pub fn create_the_loss_count_board_for_this_helper_spawn(
        &self,
        ctx: &RuntimeContextFullAccess<'_>,
        processor_id: &str,
        output_port_names: Vec<String>,
    ) -> Result<JsonValue> {
        self.create_the_loss_count_board_for_this_helper_spawn_on(
            ctx.host_base().iceoryx2_node(),
            processor_id,
            output_port_names,
        )
    }

    pub(crate) fn create_the_loss_count_board_for_this_helper_spawn_on(
        &self,
        iceoryx2_node: &Iceoryx2Node,
        processor_id: &str,
        output_port_names: Vec<String>,
    ) -> Result<JsonValue> {
        let service_name = format!(
            "streamlib/{processor_id}/loss-counts/{}",
            mint_machine_global_unique_name_suffix()
        );
        let board = iceoryx2_node
            .create_helper_process_loss_count_board(&service_name, output_port_names)?;
        let setup_command_loss_count_board = serde_json::json!({
            "service_name": board.service_name(),
            "output_ports": board.output_port_names().collect::<Vec<_>>(),
        });
        self.loss_counts.hold_the_board_of_this_spawn(board)?;
        Ok(setup_command_loss_count_board)
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
                recorded.links_facing(port_direction).push(link_wiring);
                Ok(Some(answer_cell))
            }
            HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(reason) => {
                Err(crate::core::error::Error::Runtime(reason.clone()))
            }
        }
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
        self.loss_counts.forget_link(link_id);
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
        let waiting = match std::mem::replace(
            &mut recorded.how_a_link_reaches_the_far_side,
            HowALinkReachesTheFarSide::RidesTheSetupCommand,
        ) {
            HowALinkReachesTheFarSide::RidesTheSetupCommand => Vec::new(),
            HowALinkReachesTheFarSide::WaitsForTheSetupCommandToGoOut(waiting) => waiting,
            already_past_setup @ (HowALinkReachesTheFarSide::HandedOverTo(_)
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_)) => {
                recorded.how_a_link_reaches_the_far_side = already_past_setup;
                return Err(crate::core::error::Error::Runtime(
                    "this far side's setup command was already sent, or the far side is gone"
                        .to_string(),
                ));
            }
        };

        if let Err(send_failure) =
            send_the_setup_command_carrying_these_ports(recorded.as_setup_command_ports())
        {
            let reason = format!("the far side's setup command could not be sent: {send_failure}");
            for waiting_link in waiting {
                refuse_the_link_waiting_for_the_setup_command(waiting_link, &reason);
            }
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
                Ok(()) => recorded
                    .links_facing(waiting_link.port_direction)
                    .push(waiting_link.link_wiring),
                Err(handover_failure) => refuse_the_link_waiting_for_the_setup_command(
                    waiting_link,
                    &handover_failure.to_string(),
                ),
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
                for waiting_link in waiting {
                    refuse_the_link_waiting_for_the_setup_command(waiting_link, &reason);
                }
            }
            HowALinkReachesTheFarSide::HandedOverTo(delivery) => {
                delivery.refuse_every_link_still_awaiting_the_far_sides_answer();
            }
            HowALinkReachesTheFarSide::RidesTheSetupCommand
            | HowALinkReachesTheFarSide::RefusedBecauseTheFarSideIsGone(_) => {}
        }
    }

    /// The `ports` payload a setup command sent now would carry.
    pub fn as_setup_command_ports(&self) -> JsonValue {
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
    /// a read. `docs/plan/ARCHITECTURE.md` §Processor model puts a helper's
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
            "processor 'Blur' (Pblur) has failed, so no link can be wired into it".to_string(),
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
            "processor 'Blur' (Pblur) has failed, so no link can be wired into it".to_string(),
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
}

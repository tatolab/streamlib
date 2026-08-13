// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Object-safe wrapper for GeneratedProcessor - DO NOT USE DIRECTLY.

use super::GeneratedProcessor;
use crate::core::ProcessorDescriptor;
use crate::core::Result;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::ExecutionConfig;
use serde_json::Value as JsonValue;

/// One processor's pending link wiring, for a transport the engine cannot
/// reach into.
///
/// The engine fills this as it opens each channel; whoever owns the far side —
/// a helper process, a Deno subprocess — reads it back as the `ports` payload
/// of the setup command and opens its own publisher, subscriber and notifier
/// from the service names inside.
#[derive(Debug, Default)]
pub struct OutOfProcessLinkWiringEnvelope {
    input_links: Vec<serde_json::Value>,
    output_links: Vec<serde_json::Value>,
}

impl OutOfProcessLinkWiringEnvelope {
    /// Record one link, in the direction its port faces.
    ///
    /// One call per link. Fan-out out of one port records one entry per link:
    /// the far side installs its single publisher once and appends a notifier
    /// per entry, because iceoryx2 admits exactly one publisher per channel.
    pub fn record(&mut self, port_direction: crate::core::PortDirection, link_wiring: JsonValue) {
        match port_direction {
            crate::core::PortDirection::Input => self.input_links.push(link_wiring),
            crate::core::PortDirection::Output => self.output_links.push(link_wiring),
        }
    }

    /// Forget one link, in both directions, on disconnect.
    ///
    /// A reconnect re-records the link from scratch, so an entry left here
    /// would be sent again the next time the far side is set up — a second
    /// subscriber or notifier for one link, which is what exhausts the notify
    /// service's create-time `max_notifiers` cap.
    ///
    /// Crate-internal for the same reason [`record`] is only ever called by the
    /// compiler op: the engine owns both sides of this bookkeeping, so no host
    /// can forget to do it.
    ///
    /// [`record`]: OutOfProcessLinkWiringEnvelope::record
    pub(crate) fn remove_link(&mut self, link_id: &str) {
        let carries_link = |link_wiring: &JsonValue| {
            link_wiring.get("link_id").and_then(JsonValue::as_str) == Some(link_id)
        };
        self.input_links.retain(|link| !carries_link(link));
        self.output_links.retain(|link| !carries_link(link));
    }

    /// The `ports` payload of the setup command, as the far side reads it.
    pub fn as_setup_command_ports(&self) -> JsonValue {
        serde_json::json!({
            "inputs": self.input_links,
            "outputs": self.output_links,
        })
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
    /// Both the record and the erase run through this one accessor, from the
    /// compiler op alone — a host supplies the envelope and never writes to it,
    /// so it cannot forget half of the bookkeeping. Answering `Some` here is
    /// also what commits a host to [`unwire_out_of_process_link`], which is
    /// why that one refuses rather than defaulting quietly.
    ///
    /// [`unwire_out_of_process_link`]: DynGeneratedProcessor::unwire_out_of_process_link
    fn out_of_process_link_wiring(&mut self) -> Option<&mut OutOfProcessLinkWiringEnvelope> {
        None
    }

    /// Ask the far side to drop the iceoryx2 port it opened for one link the
    /// engine is disconnecting.
    ///
    /// The engine cannot drop that port itself — it belongs to the process that
    /// opened it from the envelope — so this is the one part of the reclaim its
    /// owner has to do. The envelope entry is pruned by the compiler op
    /// through [`out_of_process_link_wiring`], not here.
    ///
    /// `local_port_name` is the port on *this* processor: the source output
    /// port for [`PortDirection::Output`], the destination input port for
    /// [`PortDirection::Input`].
    ///
    /// The default refuses rather than succeeding quietly. Only a processor
    /// the compiler op already classified out-of-process ever reaches this, so
    /// arriving at the default means a host takes the wiring and leaves the
    /// reclaim, which is the exact leak this exists to close. A silent `Ok`
    /// would have the engine log a reclaim that never happened.
    ///
    /// [`out_of_process_link_wiring`]: DynGeneratedProcessor::out_of_process_link_wiring
    /// [`PortDirection::Output`]: crate::core::PortDirection::Output
    /// [`PortDirection::Input`]: crate::core::PortDirection::Input
    fn unwire_out_of_process_link(
        &mut self,
        _port_direction: crate::core::PortDirection,
        _local_port_name: &str,
        _link_id: &str,
    ) -> Result<()> {
        Err(crate::core::error::Error::Configuration(format!(
            "processor '{}' records out-of-process link wiring but implements no \
             reclaim for it, so every disconnected link leaks the port its far side \
             opened; implement `unwire_out_of_process_link`",
            self.name()
        )))
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

    fn link_wiring_entry(link_id: &str, port_name: &str) -> JsonValue {
        serde_json::json!({ "name": port_name, "link_id": link_id })
    }

    /// A disconnected link leaves the envelope in both directions, and only
    /// that link does. Leave it behind and the next setup re-sends it beside
    /// the reconnect's own entry — two subscribers, two notifiers, one link.
    #[test]
    fn a_removed_link_leaves_the_envelope_and_its_neighbours_stay() {
        let mut envelope = OutOfProcessLinkWiringEnvelope::default();
        envelope.record(PortDirection::Input, link_wiring_entry("L-gone", "in1"));
        envelope.record(PortDirection::Input, link_wiring_entry("L-stays", "in1"));
        envelope.record(PortDirection::Output, link_wiring_entry("L-gone", "out1"));
        envelope.record(PortDirection::Output, link_wiring_entry("L-stays", "out1"));

        envelope.remove_link("L-gone");

        let ports = envelope.as_setup_command_ports();
        let surviving_link_ids = |direction: &str| -> Vec<String> {
            ports[direction]
                .as_array()
                .expect("the envelope renders both directions as arrays")
                .iter()
                .map(|link| link["link_id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(surviving_link_ids("inputs"), ["L-stays"]);
        assert_eq!(surviving_link_ids("outputs"), ["L-stays"]);
    }

    /// Removing a link the envelope never carried changes nothing — the
    /// compiler reclaims every link it closes, including ones whose far side
    /// was never wired.
    #[test]
    fn removing_an_unknown_link_leaves_the_envelope_alone() {
        let mut envelope = OutOfProcessLinkWiringEnvelope::default();
        envelope.record(PortDirection::Output, link_wiring_entry("L-only", "out1"));

        envelope.remove_link("L-never-recorded");

        assert_eq!(
            envelope.as_setup_command_ports()["outputs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

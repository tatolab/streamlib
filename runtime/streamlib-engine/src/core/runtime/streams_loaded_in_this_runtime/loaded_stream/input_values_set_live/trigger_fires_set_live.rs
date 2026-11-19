// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Fires of unlinked Trigger inputs, each a live set of no value: checked with the other sets of
//! its transaction, never held in the live values or announced, and queued on the node that runs,
//! its helper through `set_input_values` or the node in this process through its input mailboxes,
//! each in the order written.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::input_values::{
    InputValueRefusal, InputValueRefusalCode, InputValueToSetLive, LiveInputValueSetRefusal,
    the_refusal_of_a_set_on_a_trigger_input,
};
use crate::core::graph::{
    Graph, GraphNode, GraphNodeWithComponents, NodeInstanceComponent,
    OutOfProcessLinkWiringComponent, PortInfo, cast_exposed_name_to_url_safe,
};
use crate::core::nodes::{
    FiresNotHandedToTheFarSide, NodeInstance, OutOfProcessLinkWiringEnvelope,
};
use crate::core::runtime::streams_loaded_in_this_runtime::node_of_a_stream_not_running::the_refusal_of_a_reach_for_a_node_not_running;

/// Whether `set` fires a Trigger input of its node in `graph`.
pub(super) fn is_a_fire(graph: &Graph, set: &InputValueToSetLive) -> bool {
    graph
        .node_by_uuid(set.node)
        .and_then(|set_on| set_on.input_port(&set.input))
        .is_some_and(PortInfo::is_a_trigger)
}

/// Refuse the fire `set`, at `set_index`, of a Trigger input of `set_on` in `graph` that no
/// running node reads: one sending a value, one into a function or shader node, whose reader
/// wakes on no message, one a link feeds, whose messages are its only events, and one of a node
/// that does not run, `why_a_node_not_built_does_not_run` saying why when it has not failed.
pub(super) fn refuse_a_fire_no_running_node_reads(
    graph: &Graph,
    set_on: &GraphNode,
    set_index: usize,
    set: &InputValueToSetLive,
    why_a_node_not_built_does_not_run: WhyANodeOfAStreamIsNotRunning,
) -> std::result::Result<(), LiveInputValueSetRefusal> {
    let input = &set.input;
    let node_name = &set_on.display_name;
    let refused = |code, words| LiveInputValueSetRefusal::TheInputDoesNotTakeIt {
        set_index,
        refusal: InputValueRefusal { code, words },
    };
    if set.value.as_ref().is_some_and(|value| !value.is_null()) {
        return Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt {
            set_index,
            refusal: the_refusal_of_a_set_on_a_trigger_input(node_name, input, set.value.as_ref()),
        });
    }
    if matches!(
        set_on.node_kind(),
        Some(NodeKind::Function { .. } | NodeKind::Shader)
    ) {
        return Err(refused(
            InputValueRefusalCode::InputTakesNoValue,
            format!(
                "input `{input}` of `{node_name}` is a Trigger of a function or shader node, \
                 which runs when the node reading it reads and wakes on no message, so a fire \
                 would reach no run. To fix: link it from a Trigger output"
            ),
        ));
    }
    if graph.link_into(&set_on.id, input).is_some() {
        return Err(refused(
            InputValueRefusalCode::InputTakesNoValue,
            format!(
                "input `{input}` of `{node_name}` is a Trigger a link feeds, and the link's \
                 messages are its only events. To fix: unlink it, or fire the node feeding it"
            ),
        ));
    }
    let does_not_run = |why_short_of_its_own_failure| {
        Err(LiveInputValueSetRefusal::TheNodeDoesNotRun {
            set_index,
            refusal: the_refusal_of_a_reach_for_a_node_not_running(
                set_on,
                why_short_of_its_own_failure,
                format!(
                    "input `{input}` of `{node_name}` is a Trigger whose node does not run, so a \
                     fire would reach no read"
                ),
            ),
        })
    };
    if why_a_node_not_built_does_not_run == WhyANodeOfAStreamIsNotRunning::PlaybackStopped {
        return does_not_run(WhyANodeOfAStreamIsNotRunning::PlaybackStopped);
    }
    if set_on.get::<OutOfProcessLinkWiringComponent>().is_none()
        && set_on.get::<NodeInstanceComponent>().is_none()
    {
        return does_not_run(why_a_node_not_built_does_not_run);
    }
    Ok(())
}

/// How the fires of one node reach it: the Trigger inputs fired, in order, an input fired twice
/// named twice, and the index of its first fire among the sets, which a refusal names.
pub(super) struct TriggerFiresDelivery {
    to: TheNodeAFireReaches,
    node_name: String,
    first_set_index: usize,
    fired: Vec<String>,
}

/// Where the fires of one node are queued.
enum TheNodeAFireReaches {
    /// The helper the node runs in, which queues them from `set_input_values`.
    ItsHelper(Arc<OutOfProcessLinkWiringEnvelope>),
    /// The node built in this process, which queues them on its input mailboxes.
    TheNodeInThisProcess(Arc<Mutex<NodeInstance>>),
}

impl TriggerFiresDelivery {
    /// The delivery of the fires `sets` make on each node of `graph`, one per node in the order
    /// first fired; each of `sets` is a fire of a running node, as
    /// [`refuse_a_fire_no_running_node_reads`] checks.
    pub(super) fn of_each_node_fired(
        graph: &Graph,
        sets: &[(usize, &InputValueToSetLive)],
    ) -> Vec<Self> {
        let mut deliveries: Vec<(uuid::Uuid, Self)> = Vec::new();
        for (set_index, set) in sets {
            if let Some((_, delivery)) = deliveries.iter_mut().find(|(node, _)| *node == set.node) {
                delivery.fired.push(set.input.clone());
                continue;
            }
            let Some(set_on) = graph.node_by_uuid(set.node) else {
                continue;
            };
            let to = match (
                set_on.get::<OutOfProcessLinkWiringComponent>(),
                set_on.get::<NodeInstanceComponent>(),
            ) {
                (Some(link_wiring), _) => {
                    TheNodeAFireReaches::ItsHelper(Arc::clone(&link_wiring.0))
                }
                (None, Some(node_instance)) => {
                    TheNodeAFireReaches::TheNodeInThisProcess(Arc::clone(&node_instance.0))
                }
                (None, None) => continue,
            };
            deliveries.push((
                set.node,
                Self {
                    to,
                    node_name: set_on.display_name.clone(),
                    first_set_index: *set_index,
                    fired: vec![set.input.clone()],
                },
            ));
        }
        deliveries
            .into_iter()
            .map(|(_, delivery)| delivery)
            .collect()
    }

    /// Queue each fire on the node, in order; a fire queued stays queued.
    pub(super) fn hand_over(self) -> std::result::Result<(), LiveInputValueSetRefusal> {
        match self.to {
            TheNodeAFireReaches::ItsHelper(link_wiring) => link_wiring
                .hand_a_far_side_past_its_setup_command_the_fires_of(&self.fired)
                .map_err(|not_handed| match not_handed {
                    FiresNotHandedToTheFarSide::NotSetUpYet => {
                        LiveInputValueSetRefusal::TheNodeDoesNotRun {
                            set_index: self.first_set_index,
                            refusal: Error::NodeOfAStreamIsNotRunning {
                                not_running_because: WhyANodeOfAStreamIsNotRunning::Preparing,
                                runtime_words: format!(
                                    "`{}` does not run yet, so a fire of its Trigger `{}` would \
                                     reach no read: its helper is still being set up",
                                    self.node_name, self.fired[0]
                                ),
                            },
                        }
                    }
                    FiresNotHandedToTheFarSide::Refused(refusal) => {
                        LiveInputValueSetRefusal::NotHandedOver(refusal)
                    }
                }),
            TheNodeAFireReaches::TheNodeInThisProcess(node_instance) => {
                let input_mailboxes = node_instance.lock().iceoryx2_input_mailboxes_inner();
                let Some(input_mailboxes) = input_mailboxes else {
                    return Err(LiveInputValueSetRefusal::NotHandedOver(Error::Link(
                        format!(
                            "`{}` holds no input mailboxes, so a fire of its Trigger `{}` has \
                         nowhere to queue",
                            self.node_name, self.fired[0]
                        ),
                    )));
                };
                for input in &self.fired {
                    cast_exposed_name_to_url_safe(input)
                        .map_err(|refusal| Error::Link(refusal.to_string()))
                        .and_then(|port| input_mailboxes.queue_a_trigger_fire_now(&port))
                        .map_err(LiveInputValueSetRefusal::NotHandedOver)?;
                }
                Ok(())
            }
        }
    }
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::path::Path;

use parking_lot::RwLock;
use serde::Serialize;

use super::LoadedStreamInThisRuntime;
use super::input_values_set_live::{
    hand_back_each, hand_each_island_again_or_put_the_values_back, is_held_on_the_graph,
};
use crate::core::compiler::PendingOperation;
use crate::core::compiler::compiler_ops::function_node_island::runs_inside_the_read_of_each_node_it_feeds;
use crate::core::compiler::compiler_ops::input_port_pin_values::refuse_a_pin_value_a_process_node_cannot_read;
use crate::core::compiler::compiler_ops::unlinked_input_values_delivery::UnlinkedInputValuesDelivery;
use crate::core::compiler::compiler_ops::unlinked_input_values_delivery::deliver_the_unlinked_input_values_of_each_node;
use crate::core::descriptors::NodeValueLifetime;
use crate::core::graph::graph_snapshot::{
    GraphSnapshot, LinkSetAsideByTheGraphsValidation, NodeChangeADrawnLinkBlocks,
};
use crate::core::graph::input_values::{the_input_value_change, the_lifetime_of_the_input};
use crate::core::graph::legacy_config_of_a_node::{
    input_values_of_a_config_and_pin_values, node_spec_holding_the_input_values,
};
use crate::core::graph::{Graph, LinkUniqueId, NodeRunId, cast_exposed_name_to_url_safe};
use crate::core::nodes::NodeSpec;
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::RuntimeOperations;
use crate::core::runtime::operations_runtime::ALinkOfASavedGraphMade;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::LoadedStreamHolding;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

impl LoadedStreamInThisRuntime {
    // =========================================================================
    // Graph operations
    // =========================================================================

    pub fn add_node(&self, spec: impl Into<NodeSpec>) -> Result<NodeRunId> {
        <Self as RuntimeOperations>::add_node(self, spec.into())
    }

    pub fn remove_node(&self, node_run_id: &NodeRunId) -> Result<()> {
        <Self as RuntimeOperations>::remove_node(self, node_run_id)
    }

    /// Connect two ports in this stream's graph.
    pub fn connect(
        &self,
        from: impl Into<OutputLinkPortRef>,
        to: impl Into<InputLinkPortRef>,
    ) -> Result<LinkUniqueId> {
        <Self as RuntimeOperations>::connect(self, from.into(), to.into())
    }

    /// Disconnect a link in this stream's graph.
    pub fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        <Self as RuntimeOperations>::disconnect(self, link_id)
    }

    pub(crate) fn refuse_a_node_change_a_drawn_link_names(
        &self,
        node_run_id: &NodeRunId,
        change: NodeChangeADrawnLinkBlocks,
    ) -> Result<()> {
        let Some(node_name) = self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(node_run_id)
                .first()
                .map(|node| node.display_name.clone())
        }) else {
            return Ok(());
        };
        self.graph_only_nodes_as_drawn
            .lock()
            .refuse_a_node_change_a_drawn_link_names(&node_name, change)
    }

    pub(crate) fn rename_node(
        &self,
        node_run_id: &NodeRunId,
        requested_name: &str,
    ) -> Result<String> {
        self.refuse_a_node_change_a_drawn_link_names(
            node_run_id,
            NodeChangeADrawnLinkBlocks::Rename,
        )?;
        let renamed = self
            .compiler
            .scope(|graph, _tx| graph.traversal_mut().rename_v(node_run_id, requested_name))?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(renamed)
    }

    /// Set the saved value `value` on input `input` of the node `node_run_id`, `None` taking it
    /// off, in place of any value set live on the input; hand it to that node alone, wherever it
    /// is hosted, and announce the input's new value. No node, link or port changes, so nothing
    /// compiles; on an input held on the graph, a setting its type holds in its config, the graph
    /// change is announced.
    pub(crate) fn set_pin_value(
        &self,
        node_run_id: &NodeRunId,
        input: &str,
        value: Option<serde_json::Value>,
    ) -> Result<()> {
        self.with_no_other_input_value_change(|| {
            let changes_announced = set_the_saved_value_and_hand_it_over(
                self.compiler.graph_lock(),
                node_run_id,
                input,
                value,
            )?;
            for change in changes_announced {
                self.publish_on_this_streams_topic(change);
            }
            Ok(())
        })
    }

    pub(crate) fn set_exposed_config_fields(
        &self,
        node_run_id: &NodeRunId,
        exposed_config_fields: Vec<String>,
    ) -> Result<()> {
        self.compiler.scope(|graph, _tx| -> Result<()> {
            let node = graph
                .traversal()
                .v(node_run_id)
                .first()
                .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))?;
            let declared_ports: Vec<crate::core::graph::PortInfo> = node
                .ports
                .inputs
                .iter()
                .filter(|port| !node.is_an_exposed_config_field_input(&port.name))
                .chain(&node.ports.outputs)
                .cloned()
                .collect();
            let exposed_config_field_input_ports =
                crate::core::graph::the_input_ports_of_the_exposed_config_fields(
                    &node.display_name,
                    &node.node_type,
                    &self.node_types_this_stream_resolves,
                    &declared_ports,
                    &exposed_config_fields,
                )?;
            let fields_still_linked: Vec<String> = graph
                .traversal()
                .e(())
                .iter()
                .filter(|link| link.to_port().node_run_id() == node_run_id)
                .map(|link| link.to_port().port_name().to_string())
                .filter(|port| {
                    node.is_an_exposed_config_field_input(port)
                        && !exposed_config_fields.contains(port)
                })
                .collect();
            if !fields_still_linked.is_empty() {
                return Err(Error::Configuration(format!(
                    "`{}` cannot stop exposing {} as inputs while a link feeds each. To fix: \
                     disconnect it first",
                    node.display_name,
                    fields_still_linked
                        .iter()
                        .map(|field| format!("`{field}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            graph
                .traversal_mut()
                .v(node_run_id)
                .first_mut()
                .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))?
                .replace_the_exposed_config_fields(
                    exposed_config_fields,
                    exposed_config_field_input_ports,
                );
            crate::core::compiler::compiler_ops::function_node_island::hand_each_reader_the_island_a_compile_changed(
                graph,
                &[],
            )?;
            crate::core::compiler::compiler_ops::input_port_pin_values::hand_each_helper_the_config_fields_its_node_exposes_as_inputs(
                graph,
            )
        })?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    pub(crate) fn set_studio_fields(
        &self,
        node_run_id: &NodeRunId,
        studio: serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        self.compiler.scope(|graph, _tx| -> Result<()> {
            graph
                .traversal_mut()
                .v(node_run_id)
                .first_mut()
                .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))?
                .studio = studio;
            Ok(())
        })?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    pub fn update_node_config<C: Serialize>(
        &self,
        node_run_id: &NodeRunId,
        config: C,
    ) -> Result<()> {
        self.log_a_node_config_update_for_the_next_commit(node_run_id, config)?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    pub(in crate::core::runtime) fn log_a_node_config_update_for_the_next_commit<C: Serialize>(
        &self,
        node_run_id: &NodeRunId,
        config: C,
    ) -> Result<()> {
        let config_json =
            serde_json::to_value(&config).map_err(|e| crate::core::Error::Config(e.to_string()))?;
        self.log_a_node_config_update_for_the_next_commit_built_from(node_run_id, |_| {
            Ok(config_json)
        })
    }

    /// Log, for the next commit, the config `build` makes of the config the node will hold once
    /// the updates already logged, and any the commit in progress took, apply; `None` for a node
    /// holding none. The build and the log are one step, so a config update logged beside it, or
    /// taken by a commit not yet applying it, is never lost.
    pub(in crate::core::runtime) fn log_a_node_config_update_for_the_next_commit_built_from(
        &self,
        node_run_id: &NodeRunId,
        build: impl FnOnce(Option<serde_json::Value>) -> Result<serde_json::Value>,
    ) -> Result<()> {
        self.compiler.scope(|graph, tx| -> Result<()> {
            let config_held = tx.the_config_the_node_will_hold(graph, node_run_id);
            let config_json = build(config_held)?;
            if let Some(node) = graph.traversal().v(node_run_id).first() {
                crate::core::graph::refuse_a_config_its_node_types_config_schema_refuses(
                    &node.display_name,
                    &node.node_type,
                    &self.node_types_this_stream_resolves,
                    &config_json,
                )?;
            }
            crate::core::graph::refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse(
                graph,
                node_run_id,
                &config_json,
            )?;
            crate::core::graph::refuse_a_live_config_naming_a_node_the_graph_does_not_hold(
                graph,
                &self.node_types_this_stream_resolves,
                node_run_id,
                &config_json,
            )?;
            if let Some(node) = graph.traversal().v(node_run_id).first() {
                self.graph_only_nodes_as_drawn
                    .lock()
                    .interface_as_drawn
                    .refuse_a_config_update_that_breaks_a_declared_port(
                        &self.node_types_this_stream_resolves,
                        &crate::core::graph::graph_snapshot::GraphSnapshotNode {
                            id: Some(node.node_uuid),
                            name: node.display_name.clone(),
                            node_type: node.node_type.clone(),
                            input_values: input_values_of_a_config_and_pin_values(
                                node.config.as_ref(),
                                &node.pins,
                            ),
                            exposed_config_fields: node.exposed_config_fields.clone(),
                            studio: node.studio.clone(),
                            fragment: None,
                        },
                        &config_json,
                    )?;
            }
            tx.log(PendingOperation::UpdateNodeConfig {
                node_run_id: node_run_id.clone(),
                config_to_apply: config_json,
            });
            Ok(())
        })
    }

    /// Describe the types its installed modules declare, then every type `graph` names, in this
    /// stream's interpreter, so a type an installed module declares keeps the conversion it
    /// states when the graph names it; why its node code did not describe one, for a stream the
    /// runtime recorded, which holds it as a placeholder.
    fn describe_every_type_a_load_names(
        &self,
        graph: &GraphSnapshot,
        lend_directory: Option<&Path>,
    ) -> Result<Option<String>> {
        self.node_interpreter_launch_record
            .describe_and_register_the_node_types_installed_modules_declare(lend_directory);
        let why_its_node_code_did_not_describe = match self
            .node_interpreter_launch_record
            .describe_and_register_every_type_a_load_names(
                &graph.node_types_its_running_nodes_name_at_every_level(),
                lend_directory,
            ) {
            Ok(()) => None,
            Err(not_described @ Error::NodeTypesNotDescribed { .. })
                if self.holding == LoadedStreamHolding::RecordedByTheRuntime =>
            {
                Some(not_described.to_string())
            }
            Err(refusal) => return Err(refusal),
        };
        Ok(why_its_node_code_did_not_describe)
    }

    /// Why this stream cannot play: a node of its graph is of a type its node code did not
    /// describe, held as a placeholder; `None` when none is. A link of its document set aside,
    /// which the engine does not wire, keeps nothing from playing: the stream plays without it.
    pub fn why_its_graph_as_held_keeps_this_stream_from_playing(&self) -> Option<String> {
        self.why_a_node_of_a_type_not_described_keeps_this_stream_from_playing()
    }

    /// Why a node of a type its node code did not describe, held as a placeholder, keeps
    /// this stream from playing.
    fn why_a_node_of_a_type_not_described_keeps_this_stream_from_playing(&self) -> Option<String> {
        if let Some(why_none_of_its_node_code_is_read) = self
            .node_types_this_stream_resolves
            .why_every_node_type_is_held_as_a_placeholder()
        {
            return Some(format!(
                "it is held as its document is written, none of its node code read: \
                 {why_none_of_its_node_code_is_read}"
            ));
        }
        self.compiler.scope(|graph, _tx| {
            graph.traversal().v(()).iter().find_map(|node| {
                self.node_types_this_stream_resolves
                    .placeholder_of_the_node_type_not_described(&node.node_type)
                    .map(|placeholder| {
                        format!(
                            "its node `{}` is of the type `{}`, which its node code did not \
                             describe: {}",
                            node.display_name,
                            node.node_type,
                            placeholder.why_its_node_code_did_not_describe_it
                        )
                    })
            })
        })
    }

    // =========================================================================
    // Graph load
    // =========================================================================

    /// Load `graph` into this stream, which is not in the stream table yet:
    /// every type it names that no native registration holds described in the
    /// stream's own interpreter, the graph validated against what the stream
    /// resolves, each node added under its name, and each link connected by
    /// name as a saved graph's link. A link the engine cannot wire is set aside
    /// on a stream the runtime records, so it opens and plays without it, and
    /// refused on any other.
    pub(crate) fn load_graph_snapshot_into_this_stream(
        &self,
        graph: &GraphSnapshot,
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        graph
            .clock
            .refusal_unless_this_runtime_runs_it(self.stream_id())?;
        let graph_whose_every_node_holds_a_uuid =
            graph.with_a_uuid_minted_for_each_node_holding_none();
        let why_its_node_code_did_not_describe = match self
            .node_types_this_stream_resolves
            .why_every_node_type_is_held_as_a_placeholder()
        {
            Some(why_none_of_it_is_read) => Some(why_none_of_it_is_read),
            None => self.describe_every_type_a_load_names(
                &graph_whose_every_node_holds_a_uuid,
                lend_directory,
            )?,
        };
        let resolution = graph_whose_every_node_holds_a_uuid
            .with_graph_only_nodes_resolved(&self.node_types_this_stream_resolves)?;
        if let Some(why_its_node_code_did_not_describe) = &why_its_node_code_did_not_describe {
            self.node_types_this_stream_resolves
                .hold_a_placeholder_of_each_node_type_not_described(
                    &resolution.graph_the_runtime_runs,
                    why_its_node_code_did_not_describe,
                );
        }
        let mut resolution = if self.holding == LoadedStreamHolding::RecordedByTheRuntime {
            resolution.validated_setting_aside_each_link_it_refuses(
                &self.node_types_this_stream_resolves,
            )?
        } else {
            resolution.validate(&self.node_types_this_stream_resolves)?;
            resolution
        };
        let graph = &resolution.graph_the_runtime_runs;
        let node_names_the_graph_only_nodes_hold =
            resolution.graph_only_nodes_as_drawn.node_names_cast();
        self.compiler.scope(|this_streams_graph, _tx| {
            this_streams_graph.set_stream_clock_spec(graph.clock);
            this_streams_graph.set_stream_asset(graph.asset);
            this_streams_graph.set_stream_display_name(graph.display_name.clone());
            this_streams_graph
                .set_node_names_the_graph_only_nodes_hold(node_names_the_graph_only_nodes_hold);
        });

        let mut node_run_id_by_node_name: HashMap<String, NodeRunId> = HashMap::new();
        for node in &graph.nodes {
            let mut spec = node_spec_holding_the_input_values(
                &self.node_types_this_stream_resolves,
                node.node_type.clone(),
                &node.input_values,
                node.studio.clone(),
            )
            .with_display_name(node.name.clone())
            .with_exposed_config_fields(node.exposed_config_fields.clone());
            if let Some(node_uuid) = node.id {
                spec = spec.with_node_uuid(node_uuid);
            }
            let added = self.add_node_of_a_validated_graph_load(spec)?;
            node_run_id_by_node_name.insert(added.name, added.node_run_id);
        }
        let node_run_id_of = |node: &str| -> Result<NodeRunId> {
            let cast = cast_exposed_name_to_url_safe(node)?;
            node_run_id_by_node_name
                .get(cast.as_ref())
                .cloned()
                .ok_or_else(|| Error::GraphError(format!("the graph holds no node `{node}`")))
        };

        let mut links_the_engine_cannot_wire = Vec::new();
        for link in &graph.links {
            let refused_as_a_link_of_the_graph = |refusal: Error| match resolution
                .graph_only_nodes_as_drawn
                .variables_as_drawn
                .links_resolved_from_a_variable
                .iter()
                .find(|resolved| resolved.link == *link)
            {
                Some(resolved) => Error::GraphError(format!(
                    "`GetVariable` node `{}` reads the variable `{}`, which holds `{}/{}`, \
                     into `{}/{}`, and that link is refused: {refusal}",
                    resolved.get_variable_node,
                    resolved.variable,
                    link.source.node,
                    link.source.port,
                    link.target.node,
                    link.target.port
                )),
                None => refusal,
            };
            let made = self
                .connect_a_link_of_a_saved_graph(
                    OutputLinkPortRef::new(node_run_id_of(&link.source.node)?, &link.source.port),
                    InputLinkPortRef::new(node_run_id_of(&link.target.node)?, &link.target.port),
                )
                .map_err(refused_as_a_link_of_the_graph)?;
            match made {
                ALinkOfASavedGraphMade::Connected => {}
                ALinkOfASavedGraphMade::SetAside(why_the_engine_cannot_wire_it)
                    if self.holding == LoadedStreamHolding::RecordedByTheRuntime =>
                {
                    links_the_engine_cannot_wire.push(LinkSetAsideByTheGraphsValidation {
                        link: link.clone(),
                        why_its_validation_refused_it: why_the_engine_cannot_wire_it.to_string(),
                    });
                }
                ALinkOfASavedGraphMade::SetAside(why_the_engine_cannot_wire_it) => {
                    return Err(refused_as_a_link_of_the_graph(
                        why_the_engine_cannot_wire_it,
                    ));
                }
            }
        }
        resolution
            .graph_only_nodes_as_drawn
            .links_its_validation_set_aside
            .extend(links_the_engine_cannot_wire);
        *self.graph_only_nodes_as_drawn.lock() = resolution.graph_only_nodes_as_drawn;
        self.hold_the_live_values_the_project_store_keeps();
        Ok(())
    }
}

/// Set the saved value `value` on input `input` of the node `node_run_id` in the graph `graph`
/// locks, under the input's current name, `None` taking it off, dropping any value set live on
/// the input first, then hand that node what the input reads now: its islands under the write
/// lock, its helper or itself with the lock released. Answers the changes to announce: the
/// input's, and the graph's for an input held on the graph. Whole or not at all: a node or island
/// not handed the value puts the saved and live values back.
pub(in crate::core::runtime) fn set_the_saved_value_and_hand_it_over(
    graph: &RwLock<Graph>,
    node_run_id: &NodeRunId,
    input: &str,
    value: Option<serde_json::Value>,
) -> Result<Vec<RuntimeEvent>> {
    let (put_back, delivery, changes_announced) = set_the_saved_value_and_hand_it_to_each_island(
        &mut graph.write(),
        node_run_id,
        input,
        value,
    )?;
    let Err(not_handed_over) = delivery.hand_over() else {
        return Ok(changes_announced);
    };
    let handed_back = put_back.put_back(&mut graph.write());
    hand_back_each(handed_back);
    Err(not_handed_over)
}

/// The saved values and the studio fields a reconcile sets on one node it keeps.
pub(in crate::core::runtime) struct SavedValuesOfAKeptNode {
    pub(in crate::core::runtime) node_run_id: NodeRunId,
    pub(in crate::core::runtime) pins: serde_json::Map<String, serde_json::Value>,
    pub(in crate::core::runtime) studio: serde_json::Map<String, serde_json::Value>,
}

/// Set the saved values and the studio fields of each of `kept_nodes`, nodes a reconcile keeps,
/// in the graph `graph` locks, and hand each node what its inputs read now: each island
/// evaluating it under the write lock, its helper or itself with the lock released. Its caller
/// holds the stream's one input value change at a time. Every node is set and handed its values
/// even when one is refused, and the refusals are answered together after the last, so the
/// graph holds the document's values and the reconcile does not report a refusal as done.
pub(in crate::core::runtime) fn set_the_saved_values_of_each_kept_node_and_hand_them_over(
    graph: &RwLock<Graph>,
    kept_nodes: Vec<SavedValuesOfAKeptNode>,
) -> Result<()> {
    let refusals: Vec<String> = kept_nodes
        .into_iter()
        .filter_map(|kept_node| {
            let node_run_id = kept_node.node_run_id.clone();
            set_the_saved_values_of_a_kept_node_and_hand_them_over(graph, kept_node)
                .err()
                .map(|not_handed_over| format!("`{node_run_id}`: {not_handed_over}"))
        })
        .collect();
    if refusals.is_empty() {
        return Ok(());
    }
    Err(Error::Runtime(format!(
        "the graph document's values are set on each node it keeps, and these nodes were not \
         handed them: {}",
        refusals.join("; ")
    )))
}

/// Set the saved values and the studio fields of `kept_node` and hand them over, as
/// [`set_the_saved_values_of_each_kept_node_and_hand_them_over`] does for each; a node or island
/// not handed them has its former values put back and handed back, so the graph holds what the
/// node reads and a reconcile of the same document hands them again.
fn set_the_saved_values_of_a_kept_node_and_hand_them_over(
    graph: &RwLock<Graph>,
    kept_node: SavedValuesOfAKeptNode,
) -> Result<()> {
    let node_run_id = &kept_node.node_run_id;
    let mut graph_written = graph.write();
    let Some((former_pins, former_studio)) = set_the_pins_and_studio_fields(
        &mut graph_written,
        node_run_id,
        kept_node.pins,
        kept_node.studio,
    ) else {
        return Ok(());
    };
    let put_the_former_values_back = |graph: &mut Graph| {
        set_the_pins_and_studio_fields(
            graph,
            node_run_id,
            former_pins.clone(),
            former_studio.clone(),
        );
    };
    if runs_inside_the_read_of_each_node_it_feeds(&graph_written, node_run_id) {
        return hand_each_island_again_or_put_the_values_back(
            &mut graph_written,
            std::slice::from_ref(node_run_id),
            put_the_former_values_back,
        )
        .map_err(|(not_handed_over, ())| not_handed_over);
    }
    drop(graph_written);
    let Err(not_handed_over) =
        deliver_the_unlinked_input_values_of_each_node(graph, std::slice::from_ref(node_run_id))
    else {
        return Ok(());
    };
    put_the_former_values_back(&mut graph.write());
    if let Err(not_handed_back) =
        deliver_the_unlinked_input_values_of_each_node(graph, std::slice::from_ref(node_run_id))
    {
        tracing::error!(
            %not_handed_back,
            %node_run_id,
            "a node the reconcile kept was not handed back the values it held"
        );
    }
    Err(not_handed_over)
}

/// Set the saved values `pins` and the studio fields `studio` on the node `node_run_id` of
/// `graph`; answers those it held, none when the graph no longer holds the node.
fn set_the_pins_and_studio_fields(
    graph: &mut Graph,
    node_run_id: &NodeRunId,
    pins: serde_json::Map<String, serde_json::Value>,
    studio: serde_json::Map<String, serde_json::Value>,
) -> Option<(
    serde_json::Map<String, serde_json::Value>,
    serde_json::Map<String, serde_json::Value>,
)> {
    let node = graph.traversal_mut().v(node_run_id).first_mut()?;
    Some((
        std::mem::replace(&mut node.pins, pins),
        std::mem::replace(&mut node.studio, studio),
    ))
}

/// Set the saved value `value` on input `input` of the node `node_run_id` in `graph`, as
/// [`set_the_saved_value_and_hand_it_over`] does, and hand each island evaluating it that island
/// again; answers what the set replaced, how the node is handed its values outside an island and
/// the changes to announce.
fn set_the_saved_value_and_hand_it_to_each_island(
    graph: &mut Graph,
    node_run_id: &NodeRunId,
    input: &str,
    value: Option<serde_json::Value>,
) -> Result<(
    SavedSetToPutBack,
    UnlinkedInputValuesDelivery,
    Vec<RuntimeEvent>,
)> {
    let put_back = set_the_saved_value(graph, node_run_id, input, value)?;
    let node = graph
        .traversal()
        .v(node_run_id)
        .first()
        .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))?;
    let delivery = UnlinkedInputValuesDelivery::of_the_node(graph, node);
    let changes_announced: Vec<RuntimeEvent> = the_input_value_change(graph, node, input)
        .into_iter()
        .chain(
            is_held_on_the_graph(graph, node, &put_back.input)
                .then_some(RuntimeEvent::GraphDidChange),
        )
        .collect();
    if let UnlinkedInputValuesDelivery::EachIslandEvaluatingIt(function_node) = &delivery {
        hand_each_island_again_or_put_the_values_back(
            graph,
            std::slice::from_ref(function_node),
            |graph| put_back.put_back(graph),
        )
        .map_err(|(not_handed_over, _)| not_handed_over)?;
    }
    Ok((put_back, delivery, changes_announced))
}

/// The saved and live values a saved set on one input replaced, to put back when the set is
/// refused.
struct SavedSetToPutBack {
    node_run_id: NodeRunId,
    node_uuid: uuid::Uuid,
    input: String,
    lifetime: NodeValueLifetime,
    saved_value_before: Option<serde_json::Value>,
    live_value_before: Option<serde_json::Value>,
}

impl SavedSetToPutBack {
    /// Put the saved and live values back on `graph`; answers how the node is handed back the
    /// values it reads now. A node the graph no longer holds has neither put back, and is handed
    /// nothing.
    fn put_back(&self, graph: &mut Graph) -> Option<UnlinkedInputValuesDelivery> {
        let node = graph.traversal_mut().v(&self.node_run_id).first_mut()?;
        set_the_pin(node, &self.input, self.saved_value_before.clone());
        graph.live_input_values().set(
            self.node_uuid,
            &self.input,
            self.live_value_before.clone(),
            self.lifetime,
        );
        let node = graph.traversal().v(&self.node_run_id).first()?;
        Some(UnlinkedInputValuesDelivery::of_the_node(graph, node))
    }
}

/// Set the saved value `value` on input `input` of the node `node_run_id` in `graph`, `None`
/// taking it off, and drop any value set live on the input; answers what it replaced.
fn set_the_saved_value(
    graph: &mut Graph,
    node_run_id: &NodeRunId,
    input: &str,
    value: Option<serde_json::Value>,
) -> Result<SavedSetToPutBack> {
    let node = graph
        .traversal_mut()
        .v(node_run_id)
        .first_mut()
        .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))?;
    let node_uuid = node.node_uuid;
    let Some(input) = node.input_port(input).map(|port| port.name.clone()) else {
        return Err(Error::NodePortNotFound {
            node_run_id: node.display_name.clone(),
            port_name: input.to_string(),
            direction: crate::core::PortDirection::Input,
        });
    };
    let input = input.as_str();
    let lifetime = the_lifetime_of_the_input(node, input);
    if value.is_some() && node.is_an_exposed_config_field_input(input) {
        return Err(Error::Configuration(format!(
            "input `{input}` of `{}` is a config field it exposes, which reads the \
             node's config while no link feeds it, and takes no pin value. To fix: \
             set `{input}` in its config",
            node.display_name
        )));
    }
    if let Some(value) = &value {
        refuse_a_pin_value_a_process_node_cannot_read(node, input, value)?;
    }
    let saved_value_before = set_the_pin(node, input, value);
    let live_value_before = graph
        .live_input_values()
        .set(node_uuid, input, None, lifetime);
    Ok(SavedSetToPutBack {
        node_run_id: node_run_id.clone(),
        node_uuid,
        input: input.to_owned(),
        lifetime,
        saved_value_before,
        live_value_before,
    })
}

/// Set the pin `input` of `node` to `value`, `None` taking it off; answers the pin it held.
fn set_the_pin(
    node: &mut crate::core::graph::GraphNode,
    input: &str,
    value: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match value {
        Some(value) => node.pins.insert(input.to_string(), value),
        None => node.pins.remove(input),
    }
}

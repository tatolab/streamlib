// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use super::LoadedStreamInThisRuntime;
use crate::core::compiler::PendingOperation;
use crate::core::graph::graph_snapshot::{GraphSnapshot, NodeChangeADrawnLinkBlocks};
use crate::core::graph::legacy_config_of_a_node::{
    input_values_of_a_config_and_pin_values, node_spec_holding_the_input_values,
};
use crate::core::graph::{LinkUniqueId, NodeRunId, cast_exposed_name_to_url_safe};
use crate::core::nodes::NodeSpec;
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::RuntimeOperations;
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

    pub(crate) fn set_pin_value(
        &self,
        node_run_id: &NodeRunId,
        input: &str,
        value: Option<serde_json::Value>,
    ) -> Result<()> {
        self.compiler.scope(|graph, _tx| -> Result<()> {
            let node = graph
                .traversal_mut()
                .v(node_run_id)
                .first_mut()
                .ok_or_else(|| Error::NodeNotFound(node_run_id.to_string()))?;
            if !node.has_input(input) {
                return Err(Error::NodePortNotFound {
                    node_run_id: node.display_name.clone(),
                    port_name: input.to_string(),
                    direction: crate::core::PortDirection::Input,
                });
            }
            if value.is_some() && node.is_an_exposed_config_field_input(input) {
                return Err(Error::Configuration(format!(
                    "input `{input}` of `{}` is a config field it exposes, which reads the node's \
                     config while no link feeds it, and takes no pin value. To fix: set `{input}` \
                     in its config",
                    node.display_name
                )));
            }
            match value {
                Some(value) => {
                    crate::core::compiler::compiler_ops::input_port_pin_values::refuse_a_pin_value_a_process_node_cannot_read(
                        node, input, &value,
                    )?;
                    node.pins.insert(input.to_string(), value);
                }
                None => {
                    node.pins.remove(input);
                }
            }
            crate::core::compiler::compiler_ops::function_node_island::hand_each_reader_the_island_a_compile_changed(
                graph,
                &[],
            )?;
            crate::core::compiler::compiler_ops::input_port_pin_values::hand_each_helper_the_pin_values_its_input_ports_read(
                graph,
            )
        })?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
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
    /// the updates already logged apply, `None` for a node holding none; the build and the log
    /// are one step, so a config update logged beside it is never lost.
    pub(in crate::core::runtime) fn log_a_node_config_update_for_the_next_commit_built_from(
        &self,
        node_run_id: &NodeRunId,
        build: impl FnOnce(Option<serde_json::Value>) -> Result<serde_json::Value>,
    ) -> Result<()> {
        self.compiler.scope(|graph, tx| -> Result<()> {
            let config_held = tx.the_config_last_logged_for(node_run_id).or_else(|| {
                graph
                    .traversal()
                    .v(node_run_id)
                    .first()
                    .and_then(|node| node.config.clone())
            });
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

    /// Describe every type `graph` names, and those its installed modules declare, in this
    /// stream's interpreter; why its node code did not describe one, for a stream the runtime
    /// recorded, which holds it as a placeholder.
    fn describe_every_type_a_load_names(
        &self,
        graph: &GraphSnapshot,
        lend_directory: Option<&Path>,
    ) -> Result<Option<String>> {
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
        self.node_interpreter_launch_record
            .describe_and_register_the_node_types_installed_modules_declare(lend_directory);
        Ok(why_its_node_code_did_not_describe)
    }

    /// Why this stream cannot play: a node of its graph is of a type its node code did not
    /// describe, held as a placeholder, or a link of its document is set aside by its
    /// validation; `None` when neither holds.
    pub fn why_its_graph_as_held_keeps_this_stream_from_playing(&self) -> Option<String> {
        self.why_a_node_of_a_type_not_described_keeps_this_stream_from_playing()
            .or_else(|| {
                self.graph_only_nodes_as_drawn
                    .lock()
                    .links_its_validation_set_aside
                    .first()
                    .map(|set_aside| set_aside.described())
            })
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
    /// name.
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
        let resolution = if self.holding == LoadedStreamHolding::RecordedByTheRuntime {
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

        for link in &graph.links {
            self.connect(
                OutputLinkPortRef::new(node_run_id_of(&link.source.node)?, &link.source.port),
                InputLinkPortRef::new(node_run_id_of(&link.target.node)?, &link.target.port),
            )
            .map_err(|refusal| {
                match resolution
                    .graph_only_nodes_as_drawn
                    .variables_as_drawn
                    .links_resolved_from_a_variable
                    .iter()
                    .find(|resolved| resolved.link == *link)
                {
                    Some(resolved) => Error::GraphError(format!(
                        "`GetVariable` node `{}` reads the variable `{}`, which holds \
                         `{}/{}`, into `{}/{}`, and that link is refused: {refusal}",
                        resolved.get_variable_node,
                        resolved.variable,
                        link.source.node,
                        link.source.port,
                        link.target.node,
                        link.target.port
                    )),
                    None => refusal,
                }
            })?;
        }
        *self.graph_only_nodes_as_drawn.lock() = resolution.graph_only_nodes_as_drawn;
        Ok(())
    }
}

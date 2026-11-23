// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::compiler::compiler_ops::function_node_pool::{
    is_the_function_node_pool_type, the_function_node_pool_type,
};
use crate::core::compiler::compiler_ops::shader_node_program::ShaderNodeProgram;
use crate::core::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeKind, PinConversion, PinConversionSavedValueCast,
};
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, GraphNode, GraphNodePorts,
    GraphNodeWithComponents, PortInfo,
};
use crate::core::nodes::node_instance_factory::{
    NodeTypeAsRegistered, in_node_type_order, the_node_type_as_registered,
};
use crate::core::nodes::project_view_node::{ValueSourceNode, the_node_type_of_a_project_view};
use crate::core::nodes::{
    DynamicNodeConstructorFn, NODE_REGISTRY, NodeInstance, the_node_type_of_a_graph_input,
    the_node_types_of_the_graph_itself,
};
use crate::core::{NodeDescriptor, PortDirection};

struct NodeTypeOneStreamResolvesBeyondTheNativeOnes {
    descriptor: NodeDescriptor,
    input_and_output_port_info: GraphNodePorts,
    how_its_nodes_run: HowANodeOfADescribedTypeRuns,
    /// The conversion it states.
    conversion: Option<PinConversion>,
}

/// The node type every stream's top-level `GraphInput` runs as.
static THE_NODE_TYPE_A_TOP_LEVEL_GRAPH_INPUT_RUNS_AS: std::sync::LazyLock<
    Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>,
> = std::sync::LazyLock::new(|| {
    let descriptor =
        the_node_type_of_a_graph_input().expect("the GraphInput node type's names are valid");
    Arc::new(NodeTypeOneStreamResolvesBeyondTheNativeOnes {
        input_and_output_port_info: GraphNodePorts::of_a_node_just_placed(&descriptor),
        descriptor,
        conversion: None,
        how_its_nodes_run: HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(
            |node| {
                Ok(
                    Box::new(ValueSourceNode::for_a_top_level_graph_input(node)?)
                        as Box<dyn crate::core::nodes::DynGeneratedNode + Send>,
                )
            },
        )),
    })
});

pub(crate) enum HowANodeOfADescribedTypeRuns {
    AsANodeItsConstructorBuilds(DynamicNodeConstructorFn),
    InsideTheReadOfEachNodeItFeeds {
        node_once_promoted_to_its_own_process: DynamicNodeConstructorFn,
    },
    /// On the native evaluator of its shader node island: the constructor
    /// builds the evaluator of the island a node hosts, running each member's
    /// program.
    OnTheNativeShaderEvaluatorOfItsIsland {
        shader_node_program: ShaderNodeProgram,
        evaluator_of_the_island_it_hosts: DynamicNodeConstructorFn,
    },
}

/// The installed distribution that declares a node type in its `tatolab.nodes`
/// entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledModuleOfANodeType {
    /// The distribution's name, as `importlib.metadata` reads it.
    pub distribution: String,
    /// The distribution's installed version.
    pub distribution_version: String,
    /// The entry point's name in `tatolab.nodes`.
    pub entry_point: String,
}

/// One type as a stream resolved it before a change that may be refused: its description and
/// declared execution, each `None` when the stream had not described it.
struct DescribedNodeTypeAsItStood {
    node_type: NodeClassImportPath,
    described: Option<Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>>,
    execution: Option<ProcessExecution>,
}

/// The types a change describes, as the stream resolved them before it, which
/// [`NodeTypesOneStreamResolves::put_back_the_described_node_types_as_they_stood`] restores
/// when the change is refused.
pub(crate) struct DescribedNodeTypesAsTheyStoodBeforeARefusedChange {
    each_node_type: Vec<DescribedNodeTypeAsItStood>,
}

#[derive(Default)]
pub struct NodeTypesOneStreamResolves {
    view_types_this_streams_project_brings:
        RwLock<HashMap<NodeClassImportPath, Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>>>,
    why_no_node_takes_each_view_this_streams_project_brings_that_is_no_type:
        RwLock<HashMap<NodeClassImportPath, String>>,
    described_in_this_streams_node_interpreter:
        RwLock<HashMap<NodeClassImportPath, Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>>>,
    installed_module_declaring_each_described_type:
        RwLock<HashMap<NodeClassImportPath, InstalledModuleOfANodeType>>,
    execution_each_described_class_declares: RwLock<HashMap<NodeClassImportPath, ProcessExecution>>,
    placeholder_of_each_node_type_not_described:
        RwLock<HashMap<NodeClassImportPath, PlaceholderOfANodeTypeNotDescribed>>,
    why_every_node_type_is_held_as_a_placeholder: RwLock<Option<String>>,
    /// What casts a saved value through a function node conversion this stream's interpreter
    /// describes: its body run once out of stream; `None` until the stream has an interpreter
    /// to run it in.
    saved_value_cast_of_a_function_node_conversion:
        RwLock<Option<Arc<SavedValueCastOfAFunctionNodeConversion>>>,
}

/// For a conversion a function node type states, the cast its body makes of a saved value run
/// once out of stream.
pub(crate) type SavedValueCastOfAFunctionNodeConversion =
    dyn Fn(&PinConversion) -> PinConversionSavedValueCast + Send + Sync;

/// A node type a stream's graph document names and its node code did not describe, held as a
/// placeholder so the stream's graph stays readable and editable: ports drawn from the
/// document, no config schema, and nothing a node of it runs.
#[derive(Debug, Clone)]
pub struct PlaceholderOfANodeTypeNotDescribed {
    /// Why its node code did not describe it, in the runtime's words.
    pub why_its_node_code_did_not_describe_it: String,
    input_and_output_port_info: GraphNodePorts,
    config_fields_its_nodes_expose: Vec<String>,
}

impl PlaceholderOfANodeTypeNotDescribed {
    /// Each config field a node of the type exposes as an input, which is config though no
    /// schema lists it.
    pub(crate) fn config_fields_its_nodes_expose(&self) -> impl Iterator<Item = String> + '_ {
        self.config_fields_its_nodes_expose.iter().cloned()
    }
}

/// Whether `node_type` names a node type compiled into the runtime, which every stream resolves
/// alike.
pub(crate) fn is_a_natively_compiled_node_type(node_type: &NodeClassImportPath) -> bool {
    NODE_REGISTRY.descriptor(node_type).is_some()
}

impl NodeTypesOneStreamResolves {
    /// A stream that has described nothing yet: it resolves the native types alone.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `node_type` is natively compiled, which no describe replaces.
    pub fn is_a_natively_compiled_node_type(&self, node_type: &NodeClassImportPath) -> bool {
        is_a_natively_compiled_node_type(node_type)
    }

    pub fn was_described_in_this_streams_node_interpreter(
        &self,
        node_type: &NodeClassImportPath,
    ) -> bool {
        self.described_in_this_streams_node_interpreter
            .read()
            .contains_key(node_type)
    }

    /// Resolve each view in `views`, as its project and their imports bring them, as a node type
    /// of this stream named by its view type, in place of the views it resolved before; a view
    /// that does not open, or that no node takes, is no type, and why is kept for a refusal.
    pub(crate) fn replace_the_view_types_this_streams_project_brings(
        &self,
        views: &[streamlib_project_repository::ProjectViewFolderRead],
    ) {
        let mut view_types = HashMap::new();
        let mut why_no_node_takes_each_view = HashMap::new();
        for view in views {
            let Ok(node_type) = NodeClassImportPath::new(view.view_type.clone()) else {
                continue;
            };
            if view_types.contains_key(&node_type) {
                continue;
            }
            let descriptor = match the_node_type_of_a_project_view(view) {
                Ok(descriptor) => descriptor,
                Err(why) => {
                    why_no_node_takes_each_view.entry(node_type).or_insert(why);
                    continue;
                }
            };
            if self.is_a_natively_compiled_node_type(&node_type) {
                tracing::warn!(
                    view_type = node_type.as_str(),
                    "the view is no node type: a natively compiled node type has its name"
                );
                continue;
            }
            let descriptor = match the_node_type_as_registered(descriptor) {
                Ok(NodeTypeAsRegistered { descriptor, .. }) => descriptor,
                Err(refusal) => {
                    why_no_node_takes_each_view.insert(
                        node_type,
                        format!(
                            "the view `{}` declares a port no node takes: {refusal}",
                            view.view_type
                        ),
                    );
                    continue;
                }
            };
            why_no_node_takes_each_view.remove(&node_type);
            let input_and_output_port_info = GraphNodePorts::of_a_node_just_placed(&descriptor);
            let descriptor_for_constructor = descriptor.clone();
            view_types.insert(
                node_type,
                Arc::new(NodeTypeOneStreamResolvesBeyondTheNativeOnes {
                    descriptor,
                    input_and_output_port_info,
                    conversion: None,
                    how_its_nodes_run: HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                        Box::new(move |node| {
                            Ok(Box::new(ValueSourceNode::for_the_node(
                                node,
                                descriptor_for_constructor.clone(),
                            ))
                                as Box<dyn crate::core::nodes::DynGeneratedNode + Send>)
                        }),
                    ),
                }),
            );
        }
        *self.view_types_this_streams_project_brings.write() = view_types;
        *self
            .why_no_node_takes_each_view_this_streams_project_brings_that_is_no_type
            .write() = why_no_node_takes_each_view;
    }

    /// Whether `node_type` is a view this stream's project or one of its imports brings.
    pub fn is_a_view_type_of_this_streams_project(&self, node_type: &NodeClassImportPath) -> bool {
        self.view_types_this_streams_project_brings
            .read()
            .contains_key(node_type)
    }

    /// Why no node takes `node_type` when it names a view this stream's project brings that is
    /// no node type: it does not open, or no node takes it.
    pub fn why_no_node_takes_the_view(&self, node_type: &NodeClassImportPath) -> Option<String> {
        self.why_no_node_takes_each_view_this_streams_project_brings_that_is_no_type
            .read()
            .get(node_type)
            .cloned()
    }

    /// The descriptor of each view type this stream's project brings, in view-type order.
    pub fn descriptors_of_the_view_types_this_streams_project_brings(&self) -> Vec<NodeDescriptor> {
        let mut descriptors: Vec<NodeDescriptor> = self
            .view_types_this_streams_project_brings
            .read()
            .values()
            .map(|view_type| view_type.descriptor.clone())
            .collect();
        descriptors.sort_by(|left, right| {
            left.node_class_import_path
                .cmp(&right.node_class_import_path)
        });
        descriptors
    }

    /// Register `descriptor` as a type this stream's interpreter described, for a test in
    /// another crate: a function node runs inside the read of each node it feeds, and any other
    /// node, driven as `execution` says, is built by nothing. Neither runs a node.
    #[cfg(any(test, feature = "test-support"))]
    pub fn register_a_described_type_that_runs_no_node_for_a_test(
        &self,
        descriptor: NodeDescriptor,
        execution: Option<ProcessExecution>,
    ) -> Result<()> {
        let node_type = descriptor.node_class_import_path.clone();
        let runs_no_node = || Error::NotSupported("a test's node type runs no node".to_owned());
        let how_its_nodes_run =
            match descriptor.node_kind {
                NodeKind::Function { .. } => {
                    HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                        node_once_promoted_to_its_own_process: Box::new(move |_node| {
                            Err(runs_no_node())
                        }),
                    }
                }
                _ => HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(
                    move |_node| Err(runs_no_node()),
                )),
            };
        self.register_a_type_described_in_this_streams_node_interpreter(
            descriptor,
            how_its_nodes_run,
        )?;
        if let Some(execution) = execution {
            self.record_the_execution_a_described_class_declares(node_type, execution);
        }
        Ok(())
    }

    /// Cast a saved value through each conversion a function node type this stream describes
    /// from now on states with the cast `cast_of` gives for it, standing in for its body's run
    /// out of stream in place of the one the stream's interpreter gives, for a test in another
    /// crate.
    #[cfg(any(test, feature = "test-support"))]
    pub fn cast_a_saved_value_through_each_function_node_conversion_for_a_test(
        &self,
        cast_of: impl Fn(&PinConversion) -> PinConversionSavedValueCast + Send + Sync + 'static,
    ) {
        *self.saved_value_cast_of_a_function_node_conversion.write() = Some(Arc::new(cast_of));
    }

    /// Register `descriptor`, which `installed_module` declares and this stream's interpreter
    /// described, its nodes run as `how_its_nodes_run`, with the conversion it states, and
    /// record the module declaring it.
    pub(crate) fn register_a_type_an_installed_module_declares(
        &self,
        descriptor: NodeDescriptor,
        how_its_nodes_run: HowANodeOfADescribedTypeRuns,
        installed_module: InstalledModuleOfANodeType,
    ) -> Result<()> {
        let node_type = descriptor.node_class_import_path.clone();
        self.register_a_type_described_in_this_streams_node_interpreter(
            descriptor,
            how_its_nodes_run,
        )?;
        self.record_the_installed_module_declaring(node_type, installed_module);
        Ok(())
    }

    /// Cast a saved value through each conversion a function node type this stream describes
    /// from now on states with the cast the factory `cast_of` builds gives for it, built and
    /// installed only the first time: a later call leaves the one installed and builds nothing.
    pub(crate) fn install_once_the_saved_value_cast_of_each_function_node_conversion<CastOf>(
        &self,
        cast_of: impl FnOnce() -> CastOf,
    ) where
        CastOf: Fn(&PinConversion) -> PinConversionSavedValueCast + Send + Sync + 'static,
    {
        let mut installed = self.saved_value_cast_of_a_function_node_conversion.write();
        if installed.is_none() {
            *installed = Some(Arc::new(cast_of()));
        }
    }

    /// The cast of a saved value through `conversion`, which a function node type whose body is
    /// in `language` states: a Python body's run out of stream as
    /// [`Self::install_once_the_saved_value_cast_of_each_function_node_conversion`] gives it;
    /// one that does not run, saying why, for a Rust body, and for any body while the stream
    /// gives no such run.
    fn the_saved_value_cast_of_a_function_node(
        &self,
        conversion: &PinConversion,
        language: FunctionNodeLanguage,
    ) -> PinConversionSavedValueCast {
        match (
            language,
            self.saved_value_cast_of_a_function_node_conversion
                .read()
                .as_ref(),
        ) {
            (FunctionNodeLanguage::Rust, _) => PinConversionSavedValueCast::that_does_not_run(
                "its body is Rust, which runs only through its evaluate entry inside a started \
                 stream",
            ),
            (FunctionNodeLanguage::Python, Some(cast_of)) => cast_of(conversion),
            (FunctionNodeLanguage::Python, None) => PinConversionSavedValueCast::that_does_not_run(
                "this stream has no interpreter to run its body in out of stream",
            ),
        }
    }

    /// Register `descriptor`, which this stream's interpreter described, its nodes run as
    /// `how_its_nodes_run`, with the conversion it states: a function node's casting a saved
    /// value as [`Self::the_saved_value_cast_of_a_function_node`] gives it.
    pub(crate) fn register_a_type_described_in_this_streams_node_interpreter(
        &self,
        descriptor: NodeDescriptor,
        how_its_nodes_run: HowANodeOfADescribedTypeRuns,
    ) -> Result<()> {
        let NodeTypeAsRegistered {
            descriptor,
            mut conversion,
        } = the_node_type_as_registered(descriptor)?;
        if let (Some(conversion), NodeKind::Function { language }) =
            (conversion.as_mut(), descriptor.node_kind)
        {
            conversion.saved_value_cast =
                Some(self.the_saved_value_cast_of_a_function_node(conversion, language));
        }
        let node_type = descriptor.node_class_import_path.clone();
        if self.is_a_natively_compiled_node_type(&node_type) {
            return Err(Error::Configuration(format!(
                "the stream's interpreter described `{node_type}`, and a natively compiled node \
                 type already has that import path; one import path names one class. Rename the \
                 Python class or its module"
            )));
        }
        let input_and_output_port_info = GraphNodePorts::of_a_node_just_placed(&descriptor);
        self.described_in_this_streams_node_interpreter
            .write()
            .insert(
                node_type.clone(),
                Arc::new(NodeTypeOneStreamResolvesBeyondTheNativeOnes {
                    descriptor,
                    input_and_output_port_info,
                    how_its_nodes_run,
                    conversion,
                }),
            );
        if !is_the_function_node_pool_type(&node_type) {
            tracing::info!(
                node_class_import_path = node_type.as_str(),
                "node type described in the stream's node interpreter registered"
            );
        }
        Ok(())
    }

    /// The descriptor `node_type` resolves to, native first.
    pub fn descriptor(&self, node_type: &NodeClassImportPath) -> Option<NodeDescriptor> {
        NODE_REGISTRY.descriptor(node_type).or_else(|| {
            self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                described.descriptor.clone()
            })
        })
    }

    /// The pin groups the node type `node_type` names repeats; none for a type this stream does
    /// not resolve.
    pub fn the_pin_groups_of(
        &self,
        node_type: &str,
    ) -> Vec<crate::core::descriptors::PinGroupDescriptor> {
        NodeClassImportPath::new(node_type)
            .ok()
            .and_then(|node_type| self.descriptor(&node_type))
            .map(|descriptor| descriptor.pin_groups)
            .unwrap_or_default()
    }

    /// The program of shader node type `node_type`; `None` for any other type.
    pub(crate) fn shader_node_program_of(
        &self,
        node_type: &NodeClassImportPath,
    ) -> Option<ShaderNodeProgram> {
        self.project_the_type_this_streams_interpreter_described(node_type, |described| {
            match &described.how_its_nodes_run {
                HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                    shader_node_program,
                    ..
                } => Some(shader_node_program.clone()),
                _ => None,
            }
        })
        .flatten()
    }

    /// The input and output ports `node_type` declares, native first.
    pub fn port_info(&self, node_type: &NodeClassImportPath) -> Option<GraphNodePorts> {
        self.placeholder_of_the_node_type_not_described(node_type)
            .map(|placeholder| placeholder.input_and_output_port_info)
            .or_else(|| NODE_REGISTRY.port_info(node_type))
            .or_else(|| {
                self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                    described.input_and_output_port_info.clone()
                })
            })
    }

    /// Hold every node type a graph names as a placeholder from here on, reading none of its
    /// node code, for `why_none_of_it_is_read`.
    pub(crate) fn hold_every_node_type_as_a_placeholder_reading_no_node_code(
        &self,
        why_none_of_it_is_read: String,
    ) {
        *self.why_every_node_type_is_held_as_a_placeholder.write() = Some(why_none_of_it_is_read);
    }

    /// Why every node type a graph names is held as a placeholder, reading none of its node
    /// code; `None` when node code is read.
    pub(crate) fn why_every_node_type_is_held_as_a_placeholder(&self) -> Option<String> {
        self.why_every_node_type_is_held_as_a_placeholder
            .read()
            .clone()
    }

    /// The placeholder this stream holds for `node_type`, which its node code did not describe.
    pub fn placeholder_of_the_node_type_not_described(
        &self,
        node_type: &NodeClassImportPath,
    ) -> Option<PlaceholderOfANodeTypeNotDescribed> {
        self.placeholder_of_each_node_type_not_described
            .read()
            .get(node_type)
            .cloned()
    }

    /// Hold a placeholder of each node type `graph` names that this stream resolves no other
    /// way, `why_its_node_code_did_not_describe_them` kept for each: each input is a port a
    /// link of the graph reads into, an exposed config field of a node of that type, or an
    /// input value of one that is not known to be config, which no schema says, so the value
    /// stays on an input as it was before a node held one map of them; each output is a port
    /// a link reads from. Each takes the family of the port at its link's other end, else
    /// Records, and an exposed field Channels.
    pub(crate) fn hold_a_placeholder_of_each_node_type_not_described(
        &self,
        graph: &crate::core::graph::graph_snapshot::GraphSnapshot,
        why_its_node_code_did_not_describe_them: &str,
    ) {
        let every_node_type_is_held = self
            .why_every_node_type_is_held_as_a_placeholder()
            .is_some();
        use crate::core::descriptors::{PortFamily, PortFamilyDeclaration};
        let node_type_of_each_node: HashMap<&str, &NodeClassImportPath> = graph
            .nodes
            .iter()
            .map(|node| (node.name.as_str(), &node.node_type))
            .collect();
        let not_described: Vec<&NodeClassImportPath> = graph
            .nodes
            .iter()
            .map(|node| &node.node_type)
            .filter(|node_type| {
                if every_node_type_is_held {
                    self.placeholder_of_the_node_type_not_described(node_type)
                        .is_none()
                } else {
                    self.port_info(node_type).is_none()
                }
            })
            .collect();
        if not_described.is_empty() {
            return;
        }
        let family_of_a_described_port =
            |node_name: &str, port_name: &str, direction: PortDirection| {
                node_type_of_each_node
                    .get(node_name)
                    .and_then(|node_type| self.port_info(node_type))
                    .and_then(|ports| {
                        ports
                            .the_one_a_name_finds(direction, port_name)
                            .map(|port| port.family.clone())
                    })
                    .unwrap_or_else(|| PortFamilyDeclaration::of_family(PortFamily::Records))
            };
        let a_port = |name: &str, family: PortFamilyDeclaration, is_input: bool| PortInfo {
            delivery_profile: is_input.then(|| "newest".to_string()),
            ..PortInfo::declaring_only_a_family(name, String::new(), family)
        };
        let mut placeholders: HashMap<NodeClassImportPath, PlaceholderOfANodeTypeNotDescribed> =
            HashMap::new();
        for node_type in not_described {
            let placeholder = placeholders.entry(node_type.clone()).or_insert_with(|| {
                PlaceholderOfANodeTypeNotDescribed {
                    why_its_node_code_did_not_describe_it: why_its_node_code_did_not_describe_them
                        .to_string(),
                    input_and_output_port_info: every_node_type_is_held
                        .then(|| NODE_REGISTRY.port_info(node_type))
                        .flatten()
                        .unwrap_or_default(),
                    config_fields_its_nodes_expose: Vec::new(),
                }
            });
            let fields_its_config_schema_lists =
                crate::core::graph::legacy_config_of_a_node::fields_the_config_schema_of_a_node_type_lists(
                    self, node_type,
                );
            let ports = &mut placeholder.input_and_output_port_info;
            // A name a held port had finds that port, so it adds no port of its own.
            let add_once =
                |ports: &mut GraphNodePorts, direction: PortDirection, port: PortInfo| {
                    if ports.the_one_a_name_finds(direction, &port.name).is_none() {
                        ports.on_mut(direction).push(port);
                    }
                };
            for link in &graph.links {
                if node_type_of_each_node.get(link.target.node.as_str()) == Some(&node_type) {
                    let family = family_of_a_described_port(
                        &link.source.node,
                        &link.source.port,
                        PortDirection::Output,
                    );
                    add_once(
                        ports,
                        PortDirection::Input,
                        a_port(&link.target.port, family, true),
                    );
                }
                if node_type_of_each_node.get(link.source.node.as_str()) == Some(&node_type) {
                    let family = family_of_a_described_port(
                        &link.target.node,
                        &link.target.port,
                        PortDirection::Input,
                    );
                    add_once(
                        ports,
                        PortDirection::Output,
                        a_port(&link.source.port, family, false),
                    );
                }
            }
            for node in graph
                .nodes
                .iter()
                .filter(|node| &node.node_type == node_type)
            {
                for input in node.input_values.keys().filter(|input| {
                    !fields_its_config_schema_lists.contains(*input)
                        && !node.exposed_config_fields.contains(*input)
                }) {
                    add_once(
                        ports,
                        PortDirection::Input,
                        a_port(
                            input,
                            PortFamilyDeclaration::of_family(PortFamily::Records),
                            true,
                        ),
                    );
                }
                for exposed in &node.exposed_config_fields {
                    add_once(
                        ports,
                        PortDirection::Input,
                        a_port(
                            exposed,
                            PortFamilyDeclaration::of_family(PortFamily::Channels),
                            true,
                        ),
                    );
                    if !placeholder.config_fields_its_nodes_expose.contains(exposed) {
                        placeholder
                            .config_fields_its_nodes_expose
                            .push(exposed.clone());
                    }
                }
            }
        }
        self.placeholder_of_each_node_type_not_described
            .write()
            .extend(placeholders);
    }

    /// How `node_type` runs, native first.
    pub(crate) fn node_kind(&self, node_type: &NodeClassImportPath) -> Option<NodeKind> {
        if self
            .placeholder_of_the_node_type_not_described(node_type)
            .is_some()
        {
            return None;
        }
        NODE_REGISTRY.node_kind(node_type).or_else(|| {
            self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                described.descriptor.node_kind
            })
        })
    }

    /// The class's short name — what an instance's display name defaults to — of a type the
    /// registry builds, this stream's interpreter described, or the graph resolves itself.
    pub(crate) fn default_display_name(&self, node_type: &NodeClassImportPath) -> Option<String> {
        NODE_REGISTRY
            .default_display_name(node_type)
            .or_else(|| {
                self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                    described
                        .descriptor
                        .node_class_short_name
                        .as_str()
                        .to_string()
                })
            })
            .or_else(|| {
                the_node_types_of_the_graph_itself()
                    .into_iter()
                    .find(|descriptor| &descriptor.node_class_import_path == node_type)
                    .map(|descriptor| descriptor.node_class_short_name.as_str().to_string())
            })
    }

    /// `project` of `node_type` as this stream resolves it beyond the native types, `None` when
    /// it resolves no such type.
    fn project_the_type_this_streams_interpreter_described<T>(
        &self,
        node_type: &NodeClassImportPath,
        project: impl FnOnce(&NodeTypeOneStreamResolvesBeyondTheNativeOnes) -> T,
    ) -> Option<T> {
        self.the_type_this_stream_resolves_beyond_the_native_ones(node_type)
            .map(|resolved| project(&resolved))
    }

    /// `node_type` as this stream resolves it beyond the native types: a view its project
    /// brings, else a type its interpreter described.
    fn the_type_this_stream_resolves_beyond_the_native_ones(
        &self,
        node_type: &NodeClassImportPath,
    ) -> Option<Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>> {
        if node_type.is_the_graph_input_type() {
            return Some(Arc::clone(&THE_NODE_TYPE_A_TOP_LEVEL_GRAPH_INPUT_RUNS_AS));
        }
        self.view_types_this_streams_project_brings
            .read()
            .get(node_type)
            .cloned()
            .or_else(|| {
                self.described_in_this_streams_node_interpreter
                    .read()
                    .get(node_type)
                    .cloned()
            })
    }

    /// Refuse a node this stream cannot add before anything is added: a
    /// built-in that moved to a module, or a config a native type would not
    /// take. A described type's config is checked by its own config class,
    /// where the node runs.
    pub fn refuse_a_node_this_stream_cannot_add(
        &self,
        node_name: &str,
        node_type: &NodeClassImportPath,
        config: &serde_json::Value,
    ) -> Result<()> {
        if self
            .placeholder_of_the_node_type_not_described(node_type)
            .is_some()
        {
            return Ok(());
        }
        NODE_REGISTRY.refuse_a_node_this_runtime_cannot_add(node_name, node_type, config)
    }

    pub fn create(&self, node: &GraphNode) -> Result<NodeInstance> {
        if let Some(placeholder) = self.placeholder_of_the_node_type_not_described(&node.node_type)
        {
            return Err(Error::Configuration(format!(
                "node `{}` is of the type `{}`, which its node code did not describe, so it has \
                 nothing to run: {}",
                node.display_name,
                node.node_type,
                placeholder.why_its_node_code_did_not_describe_it
            )));
        }
        if NODE_REGISTRY.can_create(&node.node_type) {
            return NODE_REGISTRY.create(node);
        }
        let Some(described) =
            self.the_type_this_stream_resolves_beyond_the_native_ones(&node.node_type)
        else {
            return NODE_REGISTRY.create(node);
        };
        let constructor = match &described.how_its_nodes_run {
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(constructor)
            | HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                evaluator_of_the_island_it_hosts: constructor,
                ..
            } => constructor,
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process,
            } if node.has::<FunctionNodePromotedToItsOwnProcessComponent>() => {
                node_once_promoted_to_its_own_process
            }
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds { .. } => {
                return Err(Error::Configuration(format!(
                    "`{}` is a function node, which runs inside the read of the node it \
                     feeds and has no node of its own",
                    node.node_type
                )));
            }
        };
        let mut instance = NodeInstance::new(constructor(node)?);
        instance.install_iceoryx2_resources()?;
        instance.reach_the_stream_facts_of(node);
        Ok(instance)
    }

    pub fn descriptors_described_in_this_streams_node_interpreter(&self) -> Vec<NodeDescriptor> {
        self.described_in_this_streams_node_interpreter
            .read()
            .values()
            .map(|described| described.descriptor.clone())
            .filter(|descriptor| {
                !is_the_function_node_pool_type(&descriptor.node_class_import_path)
            })
            .collect()
    }

    /// Record how a described class `node_type` declares it is driven.
    pub(crate) fn record_the_execution_a_described_class_declares(
        &self,
        node_type: NodeClassImportPath,
        execution: ProcessExecution,
    ) {
        self.execution_each_described_class_declares
            .write()
            .insert(node_type, execution);
    }

    /// How a described class `node_type` declares it is driven; `None` for a
    /// native type, a function node or a type never described.
    pub(crate) fn the_execution_a_described_class_declares(
        &self,
        node_type: &NodeClassImportPath,
    ) -> Option<ProcessExecution> {
        self.execution_each_described_class_declares
            .read()
            .get(node_type)
            .copied()
    }

    /// Record that `installed_module` declares the described type `node_type`.
    pub(crate) fn record_the_installed_module_declaring(
        &self,
        node_type: NodeClassImportPath,
        installed_module: InstalledModuleOfANodeType,
    ) {
        self.installed_module_declaring_each_described_type
            .write()
            .insert(node_type, installed_module);
    }

    /// The descriptors of every described type no installed module declares:
    /// the ones a graph or a live add named.
    pub fn descriptors_described_in_this_streams_node_interpreter_no_installed_module_declares(
        &self,
    ) -> Vec<NodeDescriptor> {
        let installed = self.installed_module_declaring_each_described_type.read();
        self.descriptors_described_in_this_streams_node_interpreter()
            .into_iter()
            .filter(|descriptor| !installed.contains_key(&descriptor.node_class_import_path))
            .collect()
    }

    /// Every described type an installed module declares, with that module,
    /// in import-path order.
    pub fn node_types_installed_modules_declare(
        &self,
    ) -> Vec<(NodeDescriptor, InstalledModuleOfANodeType)> {
        let installed = self.installed_module_declaring_each_described_type.read();
        let described = self.described_in_this_streams_node_interpreter.read();
        let mut node_types: Vec<(NodeDescriptor, InstalledModuleOfANodeType)> = installed
            .iter()
            .filter_map(|(node_type, installed_module)| {
                described
                    .get(node_type)
                    .map(|described| (described.descriptor.clone(), installed_module.clone()))
            })
            .collect();
        node_types.sort_by(|(left, _), (right, _)| {
            left.node_class_import_path
                .as_str()
                .cmp(right.node_class_import_path.as_str())
        });
        node_types
    }

    /// What this stream resolves now for each type in `node_types` and for its function node
    /// pool, which a describe of a function node registers, so a refused change can put it back.
    pub(crate) fn the_described_node_types_as_they_stand_now<'a>(
        &self,
        node_types: impl IntoIterator<Item = &'a NodeClassImportPath>,
    ) -> Result<DescribedNodeTypesAsTheyStoodBeforeARefusedChange> {
        let mut node_types_to_hold: Vec<NodeClassImportPath> = vec![the_function_node_pool_type()?];
        for node_type in node_types {
            if !self.is_a_natively_compiled_node_type(node_type)
                && !node_types_to_hold.contains(node_type)
            {
                node_types_to_hold.push(node_type.clone());
            }
        }
        let described = self.described_in_this_streams_node_interpreter.read();
        let executions = self.execution_each_described_class_declares.read();
        Ok(DescribedNodeTypesAsTheyStoodBeforeARefusedChange {
            each_node_type: node_types_to_hold
                .into_iter()
                .map(|node_type| DescribedNodeTypeAsItStood {
                    described: described.get(&node_type).cloned(),
                    execution: executions.get(&node_type).copied(),
                    node_type,
                })
                .collect(),
        })
    }

    /// Put each type `as_they_stood` holds back as it stood: re-registered as it was described
    /// then, or taken out when it was not described then.
    pub(crate) fn put_back_the_described_node_types_as_they_stood(
        &self,
        as_they_stood: DescribedNodeTypesAsTheyStoodBeforeARefusedChange,
    ) {
        let mut described = self.described_in_this_streams_node_interpreter.write();
        let mut executions = self.execution_each_described_class_declares.write();
        for DescribedNodeTypeAsItStood {
            node_type,
            described: described_then,
            execution: execution_then,
        } in as_they_stood.each_node_type
        {
            match described_then {
                Some(described_then) => described.insert(node_type.clone(), described_then),
                None => described.remove(&node_type),
            };
            match execution_then {
                Some(execution_then) => executions.insert(node_type, execution_then),
                None => executions.remove(&node_type),
            };
        }
    }

    /// The conversion each node type this stream resolves states, the native types', the
    /// installed modules' and the project's, sorted by node type: the native list itself when no
    /// described type states one. Each described type's is read from the type its name resolves
    /// to, so one a view of the project shadows, which states none, is left out.
    pub fn pin_conversions(&self) -> Arc<[PinConversion]> {
        let native = NODE_REGISTRY.pin_conversions();
        let described_types: Vec<NodeClassImportPath> = self
            .described_in_this_streams_node_interpreter
            .read()
            .keys()
            .cloned()
            .collect();
        let described: Vec<PinConversion> = described_types
            .iter()
            .filter_map(|node_type| {
                self.the_type_this_stream_resolves_beyond_the_native_ones(node_type)?
                    .conversion
                    .clone()
            })
            .collect();
        if described.is_empty() {
            return native;
        }
        in_node_type_order(native.iter().cloned().chain(described).collect())
    }

    pub fn node_catalog(&self) -> Vec<NodeDescriptor> {
        let mut catalog = NODE_REGISTRY.list_registered();
        catalog.extend(self.descriptors_of_the_view_types_this_streams_project_brings());
        catalog.extend(self.descriptors_described_in_this_streams_node_interpreter());
        catalog
    }
}

#[cfg(test)]
mod tests;

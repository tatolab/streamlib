// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::ProcessorDescriptor;
use crate::core::compiler::compiler_ops::function_node_pool::{
    is_the_function_node_pool_type, the_function_node_pool_type,
};
use crate::core::compiler::compiler_ops::shader_node_program::ShaderNodeProgram;
use crate::core::descriptors::{NodeKind, ProcessorClassImportPath};
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, GraphNodeWithComponents, PortInfo, ProcessorNode,
};
use crate::core::processors::processor_instance_factory::refuse_port_names_not_cast_or_declared_twice;
use crate::core::processors::project_view_node::{
    NodeWhoseOutputsAClientWritesProcessor, the_node_type_of_a_project_view,
    the_node_type_of_a_top_level_graph_input,
};
use crate::core::processors::{
    DynamicProcessorConstructorFn, PROCESSOR_REGISTRY, ProcessorInstance,
};

/// A node type one stream resolves beyond the natively compiled ones — described in its
/// processor interpreter, or a view its project brings: what it declares, and how a node of it
/// runs.
struct NodeTypeOneStreamResolvesBeyondTheNativeOnes {
    descriptor: ProcessorDescriptor,
    input_and_output_port_info: (Vec<PortInfo>, Vec<PortInfo>),
    how_its_nodes_run: HowANodeOfADescribedTypeRuns,
}

/// The node type every stream's top-level `GraphInput` runs as.
static THE_NODE_TYPE_A_TOP_LEVEL_GRAPH_INPUT_RUNS_AS: std::sync::LazyLock<
    Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>,
> = std::sync::LazyLock::new(|| {
    let descriptor = the_node_type_of_a_top_level_graph_input()
        .expect("the GraphInput node type's names are valid");
    Arc::new(NodeTypeOneStreamResolvesBeyondTheNativeOnes {
        input_and_output_port_info: (
            Vec::new(),
            descriptor.outputs.iter().map(PortInfo::from).collect(),
        ),
        descriptor,
        how_its_nodes_run: HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
            Box::new(|node| {
                Ok(Box::new(
                    NodeWhoseOutputsAClientWritesProcessor::for_a_top_level_graph_input(node)?,
                )
                    as Box<
                        dyn crate::core::processors::DynGeneratedProcessor + Send,
                    >)
            }),
        ),
    })
});

/// How a node of a type a stream's processor interpreter described runs.
pub(crate) enum HowANodeOfADescribedTypeRuns {
    /// As a processor the constructor builds, which starts the node in that
    /// stream's interpreter.
    AsAProcessorItsConstructorBuilds(DynamicProcessorConstructorFn),
    /// Inside the read of each node it feeds, with no processor of its own: a
    /// function node, until it is promoted out of its readers' islands and the
    /// constructor builds the processor of its own helper.
    InsideTheReadOfEachNodeItFeeds {
        processor_once_promoted_to_its_own_process: DynamicProcessorConstructorFn,
    },
    /// On the native evaluator of its shader node island: the constructor
    /// builds the evaluator of the island a node hosts, running each member's
    /// program.
    OnTheNativeShaderEvaluatorOfItsIsland {
        shader_node_program: ShaderNodeProgram,
        evaluator_of_the_island_it_hosts: DynamicProcessorConstructorFn,
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
    node_type: ProcessorClassImportPath,
    described: Option<Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>>,
    execution: Option<ProcessExecution>,
}

/// The types a change describes, as the stream resolved them before it, which
/// [`NodeTypesOneStreamResolves::put_back_the_described_node_types_as_they_stood`] restores
/// when the change is refused.
pub(crate) struct DescribedNodeTypesAsTheyStoodBeforeARefusedChange {
    each_node_type: Vec<DescribedNodeTypeAsItStood>,
}

/// The node types one loaded stream resolves: every natively compiled type in
/// [`PROCESSOR_REGISTRY`], then the type a top-level `GraphInput` runs as, then each view its
/// project and their imports bring, then the types described in the stream's own processor
/// interpreter.
#[derive(Default)]
pub struct NodeTypesOneStreamResolves {
    view_types_this_streams_project_brings: RwLock<
        HashMap<ProcessorClassImportPath, Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>>,
    >,
    why_no_node_takes_each_view_this_streams_project_brings_that_is_no_type:
        RwLock<HashMap<ProcessorClassImportPath, String>>,
    described_in_this_streams_processor_interpreter: RwLock<
        HashMap<ProcessorClassImportPath, Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>>,
    >,
    installed_module_declaring_each_described_type:
        RwLock<HashMap<ProcessorClassImportPath, InstalledModuleOfANodeType>>,
    execution_each_described_class_declares:
        RwLock<HashMap<ProcessorClassImportPath, ProcessExecution>>,
    placeholder_of_each_node_type_not_described:
        RwLock<HashMap<ProcessorClassImportPath, PlaceholderOfANodeTypeNotDescribed>>,
    why_every_node_type_is_held_as_a_placeholder: RwLock<Option<String>>,
}

/// A node type a stream's graph document names and its node code did not describe, held as a
/// placeholder so the stream's graph stays readable and editable: ports drawn from the
/// document, no config schema, and nothing a node of it runs.
#[derive(Debug, Clone)]
pub struct PlaceholderOfANodeTypeNotDescribed {
    /// Why its node code did not describe it, in the runtime's words.
    pub why_its_node_code_did_not_describe_it: String,
    input_and_output_port_info: (Vec<PortInfo>, Vec<PortInfo>),
}

impl NodeTypesOneStreamResolves {
    /// A stream that has described nothing yet: it resolves the native types alone.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `node_type` is natively compiled, which no describe replaces.
    pub fn is_a_natively_compiled_node_type(&self, node_type: &ProcessorClassImportPath) -> bool {
        PROCESSOR_REGISTRY.descriptor(node_type).is_some()
    }

    /// Whether `node_type` was described in this stream's processor interpreter.
    pub fn was_described_in_this_streams_processor_interpreter(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> bool {
        self.described_in_this_streams_processor_interpreter
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
            let Ok(node_type) = ProcessorClassImportPath::new(view.view_type.clone()) else {
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
            if let Err(refusal) = refuse_port_names_not_cast_or_declared_twice(&descriptor) {
                why_no_node_takes_each_view.insert(
                    node_type,
                    format!(
                        "the view `{}` declares a port no node takes: {refusal}",
                        view.view_type
                    ),
                );
                continue;
            }
            why_no_node_takes_each_view.remove(&node_type);
            let input_and_output_port_info = (
                descriptor.inputs.iter().map(PortInfo::from).collect(),
                descriptor.outputs.iter().map(PortInfo::from).collect(),
            );
            let descriptor_for_constructor = descriptor.clone();
            view_types.insert(
                node_type,
                Arc::new(NodeTypeOneStreamResolvesBeyondTheNativeOnes {
                    descriptor,
                    input_and_output_port_info,
                    how_its_nodes_run:
                        HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(
                            move |node| {
                                Ok(
                                    Box::new(NodeWhoseOutputsAClientWritesProcessor::for_the_node(
                                        node,
                                        descriptor_for_constructor.clone(),
                                    ))
                                        as Box<
                                            dyn crate::core::processors::DynGeneratedProcessor
                                                + Send,
                                        >,
                                )
                            },
                        )),
                }),
            );
        }
        *self.view_types_this_streams_project_brings.write() = view_types;
        *self
            .why_no_node_takes_each_view_this_streams_project_brings_that_is_no_type
            .write() = why_no_node_takes_each_view;
    }

    /// Whether `node_type` is a view this stream's project or one of its imports brings.
    pub fn is_a_view_type_of_this_streams_project(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> bool {
        self.view_types_this_streams_project_brings
            .read()
            .contains_key(node_type)
    }

    /// Why no node takes `node_type` when it names a view this stream's project brings that is
    /// no node type: it does not open, or no node takes it.
    pub fn why_no_node_takes_the_view(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Option<String> {
        self.why_no_node_takes_each_view_this_streams_project_brings_that_is_no_type
            .read()
            .get(node_type)
            .cloned()
    }

    /// The descriptor of each view type this stream's project brings, in view-type order.
    pub fn descriptors_of_the_view_types_this_streams_project_brings(
        &self,
    ) -> Vec<ProcessorDescriptor> {
        let mut descriptors: Vec<ProcessorDescriptor> = self
            .view_types_this_streams_project_brings
            .read()
            .values()
            .map(|view_type| view_type.descriptor.clone())
            .collect();
        descriptors.sort_by(|left, right| {
            left.processor_class_import_path
                .cmp(&right.processor_class_import_path)
        });
        descriptors
    }

    /// Register a type this stream's processor interpreter described,
    /// replacing what an earlier describe into this stream left. A natively
    /// compiled type of the same path is refused: it is never replaced.
    pub(crate) fn register_a_type_described_in_this_streams_processor_interpreter(
        &self,
        descriptor: ProcessorDescriptor,
        how_its_nodes_run: HowANodeOfADescribedTypeRuns,
    ) -> Result<()> {
        refuse_port_names_not_cast_or_declared_twice(&descriptor)?;
        let node_type = descriptor.processor_class_import_path.clone();
        if self.is_a_natively_compiled_node_type(&node_type) {
            return Err(Error::Configuration(format!(
                "the stream's interpreter described `{node_type}`, and a natively compiled node \
                 type already has that import path; one import path names one class. Rename the \
                 Python class or its module"
            )));
        }
        let input_and_output_port_info = (
            descriptor.inputs.iter().map(PortInfo::from).collect(),
            descriptor.outputs.iter().map(PortInfo::from).collect(),
        );
        self.described_in_this_streams_processor_interpreter
            .write()
            .insert(
                node_type.clone(),
                Arc::new(NodeTypeOneStreamResolvesBeyondTheNativeOnes {
                    descriptor,
                    input_and_output_port_info,
                    how_its_nodes_run,
                }),
            );
        if !is_the_function_node_pool_type(&node_type) {
            tracing::info!(
                processor_class_import_path = node_type.as_str(),
                "node type described in the stream's processor interpreter registered"
            );
        }
        Ok(())
    }

    /// The descriptor `node_type` resolves to, native first.
    pub fn descriptor(&self, node_type: &ProcessorClassImportPath) -> Option<ProcessorDescriptor> {
        PROCESSOR_REGISTRY.descriptor(node_type).or_else(|| {
            self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                described.descriptor.clone()
            })
        })
    }

    /// The program of shader node type `node_type`; `None` for any other type.
    pub(crate) fn shader_node_program_of(
        &self,
        node_type: &ProcessorClassImportPath,
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
    pub fn port_info(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.placeholder_of_the_node_type_not_described(node_type)
            .map(|placeholder| placeholder.input_and_output_port_info)
            .or_else(|| PROCESSOR_REGISTRY.port_info(node_type))
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
        node_type: &ProcessorClassImportPath,
    ) -> Option<PlaceholderOfANodeTypeNotDescribed> {
        self.placeholder_of_each_node_type_not_described
            .read()
            .get(node_type)
            .cloned()
    }

    /// Hold a placeholder of each node type `graph` names that this stream resolves no other
    /// way, `why_its_node_code_did_not_describe_them` kept for each: each input is a port a
    /// link of the graph reads into, a pin or an exposed config field of a node of that type,
    /// and each output a port a link reads from, each taking the family of the port at its
    /// link's other end, else Records.
    pub(crate) fn hold_a_placeholder_of_each_node_type_not_described(
        &self,
        graph: &crate::core::graph_snapshot::GraphSnapshot,
        why_its_node_code_did_not_describe_them: &str,
    ) {
        let every_node_type_is_held = self
            .why_every_node_type_is_held_as_a_placeholder()
            .is_some();
        use crate::core::descriptors::{PortFamily, PortFamilyDeclaration};
        use crate::core::graph::PortKind;
        let node_type_of_each_node: HashMap<&str, &ProcessorClassImportPath> = graph
            .nodes
            .iter()
            .map(|node| (node.name.as_str(), &node.processor_type))
            .collect();
        let not_described: Vec<&ProcessorClassImportPath> = graph
            .nodes
            .iter()
            .map(|node| &node.processor_type)
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
        let family_of_a_described_port = |node_name: &str, port_name: &str, is_input: bool| {
            node_type_of_each_node
                .get(node_name)
                .and_then(|node_type| self.port_info(node_type))
                .and_then(|(inputs, outputs)| {
                    (if is_input { inputs } else { outputs })
                        .into_iter()
                        .find(|port| port.name == port_name)
                        .map(|port| port.family)
                })
                .unwrap_or_else(|| PortFamilyDeclaration::of_family(PortFamily::Records))
        };
        let a_port = |name: &str, family: PortFamilyDeclaration, is_input: bool| PortInfo {
            name: name.to_string(),
            description: String::new(),
            port_kind: PortKind::default(),
            delivery_profile: is_input.then(|| "newest".to_string()),
            borrow_quota: None,
            audio_window: None,
            family,
            pin_default: None,
        };
        let mut placeholders: HashMap<
            ProcessorClassImportPath,
            PlaceholderOfANodeTypeNotDescribed,
        > = HashMap::new();
        for node_type in not_described {
            let placeholder = placeholders.entry(node_type.clone()).or_insert_with(|| {
                PlaceholderOfANodeTypeNotDescribed {
                    why_its_node_code_did_not_describe_it: why_its_node_code_did_not_describe_them
                        .to_string(),
                    input_and_output_port_info: every_node_type_is_held
                        .then(|| PROCESSOR_REGISTRY.port_info(node_type))
                        .flatten()
                        .unwrap_or_default(),
                }
            });
            let (inputs, outputs) = &mut placeholder.input_and_output_port_info;
            let add_once = |ports: &mut Vec<PortInfo>, port: PortInfo| {
                if !ports.iter().any(|held| held.name == port.name) {
                    ports.push(port);
                }
            };
            for link in &graph.links {
                if node_type_of_each_node.get(link.target.node.as_str()) == Some(&node_type) {
                    let family =
                        family_of_a_described_port(&link.source.node, &link.source.port, false);
                    add_once(inputs, a_port(&link.target.port, family, true));
                }
                if node_type_of_each_node.get(link.source.node.as_str()) == Some(&node_type) {
                    let family =
                        family_of_a_described_port(&link.target.node, &link.target.port, true);
                    add_once(outputs, a_port(&link.source.port, family, false));
                }
            }
            for node in graph
                .nodes
                .iter()
                .filter(|node| &node.processor_type == node_type)
            {
                for pinned in node.pins.keys() {
                    add_once(
                        inputs,
                        a_port(
                            pinned,
                            PortFamilyDeclaration::of_family(PortFamily::Records),
                            true,
                        ),
                    );
                }
                for exposed in &node.exposed_config_fields {
                    add_once(
                        inputs,
                        a_port(
                            exposed,
                            PortFamilyDeclaration::of_family(PortFamily::Channels),
                            true,
                        ),
                    );
                }
            }
        }
        self.placeholder_of_each_node_type_not_described
            .write()
            .extend(placeholders);
    }

    /// How `node_type` runs, native first.
    pub(crate) fn node_kind(&self, node_type: &ProcessorClassImportPath) -> Option<NodeKind> {
        if self
            .placeholder_of_the_node_type_not_described(node_type)
            .is_some()
        {
            return None;
        }
        PROCESSOR_REGISTRY.node_kind(node_type).or_else(|| {
            self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                described.descriptor.node_kind
            })
        })
    }

    /// The class's short name — what an instance's display name defaults to.
    pub(crate) fn default_display_name(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Option<String> {
        PROCESSOR_REGISTRY
            .default_display_name(node_type)
            .or_else(|| {
                self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                    described
                        .descriptor
                        .processor_class_short_name
                        .as_str()
                        .to_string()
                })
            })
    }

    /// `project` of `node_type` as this stream resolves it beyond the native types, `None` when
    /// it resolves no such type.
    fn project_the_type_this_streams_interpreter_described<T>(
        &self,
        node_type: &ProcessorClassImportPath,
        project: impl FnOnce(&NodeTypeOneStreamResolvesBeyondTheNativeOnes) -> T,
    ) -> Option<T> {
        self.the_type_this_stream_resolves_beyond_the_native_ones(node_type)
            .map(|resolved| project(&resolved))
    }

    /// `node_type` as this stream resolves it beyond the native types: a view its project
    /// brings, else a type its interpreter described.
    fn the_type_this_stream_resolves_beyond_the_native_ones(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Option<Arc<NodeTypeOneStreamResolvesBeyondTheNativeOnes>> {
        if node_type.as_str() == crate::core::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE {
            return Some(Arc::clone(&THE_NODE_TYPE_A_TOP_LEVEL_GRAPH_INPUT_RUNS_AS));
        }
        self.view_types_this_streams_project_brings
            .read()
            .get(node_type)
            .cloned()
            .or_else(|| {
                self.described_in_this_streams_processor_interpreter
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
        node_type: &ProcessorClassImportPath,
        config: &serde_json::Value,
    ) -> Result<()> {
        if self
            .placeholder_of_the_node_type_not_described(node_type)
            .is_some()
        {
            return Ok(());
        }
        PROCESSOR_REGISTRY.refuse_a_node_this_runtime_cannot_add(node_name, node_type, config)
    }

    /// Build the processor `node` is, native first.
    pub fn create(&self, node: &ProcessorNode) -> Result<ProcessorInstance> {
        if let Some(placeholder) =
            self.placeholder_of_the_node_type_not_described(&node.processor_type)
        {
            return Err(Error::Configuration(format!(
                "node `{}` is of the type `{}`, which its node code did not describe, so it has \
                 nothing to run: {}",
                node.display_name,
                node.processor_type,
                placeholder.why_its_node_code_did_not_describe_it
            )));
        }
        if PROCESSOR_REGISTRY.can_create(&node.processor_type) {
            return PROCESSOR_REGISTRY.create(node);
        }
        let Some(described) =
            self.the_type_this_stream_resolves_beyond_the_native_ones(&node.processor_type)
        else {
            return PROCESSOR_REGISTRY.create(node);
        };
        let constructor = match &described.how_its_nodes_run {
            HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(constructor)
            | HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                evaluator_of_the_island_it_hosts: constructor,
                ..
            } => constructor,
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                processor_once_promoted_to_its_own_process,
            } if node.has::<FunctionNodePromotedToItsOwnProcessComponent>() => {
                processor_once_promoted_to_its_own_process
            }
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds { .. } => {
                return Err(Error::Configuration(format!(
                    "`{}` is a function node, which runs inside the read of the node it \
                     feeds and has no processor of its own",
                    node.processor_type
                )));
            }
        };
        let mut instance = ProcessorInstance::new(constructor(node)?);
        instance.install_iceoryx2_resources()?;
        instance.reach_the_stream_facts_of(node);
        Ok(instance)
    }

    /// The descriptors of every type described in this stream's processor
    /// interpreter, leaving out the stream's function node pool, which no
    /// graph names.
    pub fn descriptors_described_in_this_streams_processor_interpreter(
        &self,
    ) -> Vec<ProcessorDescriptor> {
        self.described_in_this_streams_processor_interpreter
            .read()
            .values()
            .map(|described| described.descriptor.clone())
            .filter(|descriptor| {
                !is_the_function_node_pool_type(&descriptor.processor_class_import_path)
            })
            .collect()
    }

    /// Record how a described class `node_type` declares it is driven.
    pub(crate) fn record_the_execution_a_described_class_declares(
        &self,
        node_type: ProcessorClassImportPath,
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
        node_type: &ProcessorClassImportPath,
    ) -> Option<ProcessExecution> {
        self.execution_each_described_class_declares
            .read()
            .get(node_type)
            .copied()
    }

    /// Record that `installed_module` declares the described type `node_type`.
    pub(crate) fn record_the_installed_module_declaring(
        &self,
        node_type: ProcessorClassImportPath,
        installed_module: InstalledModuleOfANodeType,
    ) {
        self.installed_module_declaring_each_described_type
            .write()
            .insert(node_type, installed_module);
    }

    /// The descriptors of every described type no installed module declares:
    /// the ones a graph or a live add named.
    pub fn descriptors_described_in_this_streams_processor_interpreter_no_installed_module_declares(
        &self,
    ) -> Vec<ProcessorDescriptor> {
        let installed = self.installed_module_declaring_each_described_type.read();
        self.descriptors_described_in_this_streams_processor_interpreter()
            .into_iter()
            .filter(|descriptor| !installed.contains_key(&descriptor.processor_class_import_path))
            .collect()
    }

    /// Every described type an installed module declares, with that module,
    /// in import-path order.
    pub fn node_types_installed_modules_declare(
        &self,
    ) -> Vec<(ProcessorDescriptor, InstalledModuleOfANodeType)> {
        let installed = self.installed_module_declaring_each_described_type.read();
        let described = self.described_in_this_streams_processor_interpreter.read();
        let mut node_types: Vec<(ProcessorDescriptor, InstalledModuleOfANodeType)> = installed
            .iter()
            .filter_map(|(node_type, installed_module)| {
                described
                    .get(node_type)
                    .map(|described| (described.descriptor.clone(), installed_module.clone()))
            })
            .collect();
        node_types.sort_by(|(left, _), (right, _)| {
            left.processor_class_import_path
                .as_str()
                .cmp(right.processor_class_import_path.as_str())
        });
        node_types
    }

    /// What this stream resolves now for each type in `node_types` and for its function node
    /// pool, which a describe of a function node registers, so a refused change can put it back.
    pub(crate) fn the_described_node_types_as_they_stand_now<'a>(
        &self,
        node_types: impl IntoIterator<Item = &'a ProcessorClassImportPath>,
    ) -> Result<DescribedNodeTypesAsTheyStoodBeforeARefusedChange> {
        let mut node_types_to_hold: Vec<ProcessorClassImportPath> =
            vec![the_function_node_pool_type()?];
        for node_type in node_types {
            if !self.is_a_natively_compiled_node_type(node_type)
                && !node_types_to_hold.contains(node_type)
            {
                node_types_to_hold.push(node_type.clone());
            }
        }
        let described = self.described_in_this_streams_processor_interpreter.read();
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
        let mut described = self.described_in_this_streams_processor_interpreter.write();
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

    /// The native types, then the views this stream's project brings, then the types described
    /// in this stream's processor interpreter — the stream's node catalog.
    pub fn node_catalog(&self) -> Vec<ProcessorDescriptor> {
        let mut catalog = PROCESSOR_REGISTRY.list_registered();
        catalog.extend(self.descriptors_of_the_view_types_this_streams_project_brings());
        catalog.extend(self.descriptors_described_in_this_streams_processor_interpreter());
        catalog
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName};

    fn class_import_path(path: &str) -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(path).expect("the fixture path names a class")
    }

    fn descriptor_for(path: &str) -> ProcessorDescriptor {
        ProcessorDescriptor::new(
            ProcessorClassShortName::new("HeldConstant").unwrap(),
            class_import_path(path),
            "test",
        )
    }

    fn port_named(name: &str) -> PortDescriptor {
        PortDescriptor {
            name: name.to_string(),
            description: String::new(),
            required: true,
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            family: crate::core::descriptors::PortFamilyDeclaration::of_family(
                crate::core::descriptors::PortFamily::Image,
            ),
            pin_default: None,
        }
    }

    /// A constructor that refuses with `refusal`, so a test can tell whose
    /// constructor a create reached.
    fn a_constructor_refusing_with(refusal: &'static str) -> DynamicProcessorConstructorFn {
        Box::new(move |_node| Err(Error::NotSupported(refusal.to_string())))
    }

    fn a_node_of(node_type: &ProcessorClassImportPath) -> ProcessorNode {
        ProcessorNode::new(node_type.clone(), None, "a-node", None, vec![], vec![])
    }

    /// A later describe of a type into one stream replaces what the earlier
    /// one registered — the class's ports may have changed since.
    #[test]
    fn a_later_describe_into_a_stream_replaces_the_earlier_ones_registration() {
        let node_types = NodeTypesOneStreamResolves::new();
        let path = "node_types_one_stream_resolves_tests.filters:ReDescribedProcessor";

        node_types
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor_for(path).with_input(port_named("video")),
                HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
                    a_constructor_refusing_with("first describe"),
                ),
            )
            .expect("a first describe registers the type");
        node_types
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor_for(path).with_input(port_named("frames")),
                HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
                    a_constructor_refusing_with("later describe"),
                ),
            )
            .expect("a later describe replaces it");

        let (inputs, _) = node_types
            .port_info(&class_import_path(path))
            .expect("the type is registered");
        let input_names: Vec<_> = inputs.iter().map(|port| port.name.as_str()).collect();
        assert_eq!(input_names, ["frames"]);
        assert!(
            node_types
                .was_described_in_this_streams_processor_interpreter(&class_import_path(path))
        );
        assert!(!node_types.is_a_natively_compiled_node_type(&class_import_path(path)));
    }

    /// A typed Rust registration and a descriptor alone are never replaced by
    /// a describe claiming their path: a Python class cannot quietly displace
    /// a native built-in.
    #[test]
    fn a_describe_never_replaces_a_natively_compiled_node_type() {
        use crate::core::test_support::{MockSourceTakingOneSetting, ensure_test_mocks_registered};
        ensure_test_mocks_registered();
        let typed = MockSourceTakingOneSetting::processor_class_import_path();
        let descriptor_only = class_import_path(
            "node_types_one_stream_resolves_tests.filters:RegisteredAsADescriptor",
        );
        if PROCESSOR_REGISTRY.descriptor(&descriptor_only).is_none() {
            PROCESSOR_REGISTRY
                .register_descriptor_only(descriptor_for(descriptor_only.as_str()))
                .expect("the descriptor registers");
        }
        let node_types = NodeTypesOneStreamResolves::new();

        for registered_natively in [&typed, &descriptor_only] {
            assert!(node_types.is_a_natively_compiled_node_type(registered_natively));
            let refused = node_types
                .register_a_type_described_in_this_streams_processor_interpreter(
                    descriptor_for(registered_natively.as_str()),
                    HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
                        a_constructor_refusing_with("a describe that must be refused"),
                    ),
                )
                .expect_err("a describe never replaces a natively compiled type");
            assert!(
                refused.to_string().contains(registered_natively.as_str()),
                "{refused}"
            );
            assert!(
                !node_types
                    .was_described_in_this_streams_processor_interpreter(registered_natively)
            );
        }
    }

    /// Two streams describing one class each resolve their own descriptor,
    /// ports and constructor: neither starts its nodes from the other's
    /// interpreter, and neither registration reaches the machine's registry.
    #[test]
    fn two_streams_describing_one_class_each_resolve_their_own() {
        let first_stream = NodeTypesOneStreamResolves::new();
        let second_stream = NodeTypesOneStreamResolves::new();
        let path = class_import_path("node_types_one_stream_resolves_tests.filters:SharedClass");

        first_stream
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor_for(path.as_str()).with_input(port_named("video")),
                HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
                    a_constructor_refusing_with("the first stream's interpreter"),
                ),
            )
            .expect("the first stream describes the class");
        second_stream
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor_for(path.as_str()).with_input(port_named("frames")),
                HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
                    a_constructor_refusing_with("the second stream's interpreter"),
                ),
            )
            .expect("the second stream describes the class");

        let input_names_of = |node_types: &NodeTypesOneStreamResolves| -> Vec<String> {
            node_types
                .port_info(&path)
                .expect("the stream resolves the class")
                .0
                .into_iter()
                .map(|port| port.name)
                .collect()
        };
        assert_eq!(input_names_of(&first_stream), ["video"]);
        assert_eq!(input_names_of(&second_stream), ["frames"]);

        let created_by = |node_types: &NodeTypesOneStreamResolves| -> String {
            match node_types.create(&a_node_of(&path)) {
                Ok(_) => panic!("the test constructor never builds a processor"),
                Err(refusal) => refusal.to_string(),
            }
        };
        assert!(created_by(&first_stream).contains("the first stream's interpreter"));
        assert!(created_by(&second_stream).contains("the second stream's interpreter"));

        assert!(PROCESSOR_REGISTRY.descriptor(&path).is_none());
        assert!(
            NodeTypesOneStreamResolves::new()
                .descriptor(&path)
                .is_none(),
            "a stream that described nothing resolved another stream's class"
        );
        assert!(
            first_stream
                .node_catalog()
                .iter()
                .any(|descriptor| descriptor.processor_class_import_path == path),
            "the stream's node catalog leaves out the class it described"
        );
    }
}

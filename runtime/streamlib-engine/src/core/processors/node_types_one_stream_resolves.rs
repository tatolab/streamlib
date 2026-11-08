// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;

use parking_lot::RwLock;

use crate::core::ProcessorDescriptor;
use crate::core::compiler::compiler_ops::function_node_pool::is_the_function_node_pool_type;
use crate::core::descriptors::{NodeKind, ProcessorClassImportPath};
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, GraphNodeWithComponents, PortInfo, ProcessorNode,
};
use crate::core::processors::processor_instance_factory::refuse_port_names_not_cast_or_declared_twice;
use crate::core::processors::{
    DynamicProcessorConstructorFn, PROCESSOR_REGISTRY, ProcessorInstance,
};

/// A node type described in one stream's processor interpreter: what it
/// declares, and how a node of it runs.
struct NodeTypeDescribedInAStreamsProcessorInterpreter {
    descriptor: ProcessorDescriptor,
    input_and_output_port_info: (Vec<PortInfo>, Vec<PortInfo>),
    how_its_nodes_run: HowANodeOfADescribedTypeRuns,
}

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

/// The node types one loaded stream resolves: every natively compiled type in
/// [`PROCESSOR_REGISTRY`], then the types described in the stream's own
/// processor interpreter.
#[derive(Default)]
pub struct NodeTypesOneStreamResolves {
    described_in_this_streams_processor_interpreter:
        RwLock<HashMap<ProcessorClassImportPath, NodeTypeDescribedInAStreamsProcessorInterpreter>>,
    installed_module_declaring_each_described_type:
        RwLock<HashMap<ProcessorClassImportPath, InstalledModuleOfANodeType>>,
    execution_each_described_class_declares:
        RwLock<HashMap<ProcessorClassImportPath, ProcessExecution>>,
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
                NodeTypeDescribedInAStreamsProcessorInterpreter {
                    descriptor,
                    input_and_output_port_info,
                    how_its_nodes_run,
                },
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

    /// The input and output ports `node_type` declares, native first.
    pub fn port_info(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        PROCESSOR_REGISTRY.port_info(node_type).or_else(|| {
            self.project_the_type_this_streams_interpreter_described(node_type, |described| {
                described.input_and_output_port_info.clone()
            })
        })
    }

    /// How `node_type` runs, native first.
    pub(crate) fn node_kind(&self, node_type: &ProcessorClassImportPath) -> Option<NodeKind> {
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

    /// `project` of `node_type` as this stream's processor interpreter
    /// described it, `None` when it described no such type.
    fn project_the_type_this_streams_interpreter_described<T>(
        &self,
        node_type: &ProcessorClassImportPath,
        project: impl FnOnce(&NodeTypeDescribedInAStreamsProcessorInterpreter) -> T,
    ) -> Option<T> {
        self.described_in_this_streams_processor_interpreter
            .read()
            .get(node_type)
            .map(project)
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
        PROCESSOR_REGISTRY.refuse_a_node_this_runtime_cannot_add(node_name, node_type, config)
    }

    /// Build the processor `node` is, native first.
    pub fn create(&self, node: &ProcessorNode) -> Result<ProcessorInstance> {
        if PROCESSOR_REGISTRY.can_create(&node.processor_type) {
            return PROCESSOR_REGISTRY.create(node);
        }
        let described = self.described_in_this_streams_processor_interpreter.read();
        let Some(described) = described.get(&node.processor_type) else {
            return PROCESSOR_REGISTRY.create(node);
        };
        let constructor = match &described.how_its_nodes_run {
            HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(constructor) => {
                constructor
            }
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

    /// The native types, then the types described in this stream's processor
    /// interpreter — the stream's node catalog.
    pub fn node_catalog(&self) -> Vec<ProcessorDescriptor> {
        let mut catalog = PROCESSOR_REGISTRY.list_registered();
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

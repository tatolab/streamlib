// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;

use parking_lot::RwLock;

use crate::core::ProcessorDescriptor;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::processor_instance_factory::refuse_port_names_not_cast_or_declared_twice;
use crate::core::processors::{
    DynamicProcessorConstructorFn, PROCESSOR_REGISTRY, ProcessorInstance,
};

/// A node type described in one stream's processor interpreter: what it
/// declares, and the constructor that starts its processor in that stream's
/// interpreter.
struct NodeTypeDescribedInAStreamsProcessorInterpreter {
    descriptor: ProcessorDescriptor,
    input_and_output_port_info: (Vec<PortInfo>, Vec<PortInfo>),
    constructor: DynamicProcessorConstructorFn,
}

/// The node types one loaded stream resolves: every natively compiled type in
/// [`PROCESSOR_REGISTRY`], then the types described in the stream's own
/// processor interpreter.
#[derive(Default)]
pub struct NodeTypesOneStreamResolves {
    described_in_this_streams_processor_interpreter:
        RwLock<HashMap<ProcessorClassImportPath, NodeTypeDescribedInAStreamsProcessorInterpreter>>,
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
        constructor: DynamicProcessorConstructorFn,
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
                    constructor,
                },
            );
        tracing::info!(
            processor_class_import_path = node_type.as_str(),
            "node type described in the stream's processor interpreter registered"
        );
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
    /// built-in this floor compiles out, or a config a native type would not
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
        let mut instance = ProcessorInstance::new((described.constructor)(node)?);
        instance.install_iceoryx2_resources()?;
        Ok(instance)
    }

    /// The descriptors of every type described in this stream's processor
    /// interpreter.
    pub fn descriptors_described_in_this_streams_processor_interpreter(
        &self,
    ) -> Vec<ProcessorDescriptor> {
        self.described_in_this_streams_processor_interpreter
            .read()
            .values()
            .map(|described| described.descriptor.clone())
            .collect()
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
            audio_window: None,
        }
    }

    /// A constructor that refuses with `refusal`, so a test can tell whose
    /// constructor a create reached.
    fn a_constructor_refusing_with(refusal: &'static str) -> DynamicProcessorConstructorFn {
        Box::new(move |_node| Err(Error::NotSupported(refusal.to_string())))
    }

    fn a_node_of(node_type: &ProcessorClassImportPath) -> ProcessorNode {
        ProcessorNode::new(node_type.clone(), "a-node", None, vec![], vec![])
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
                a_constructor_refusing_with("first describe"),
            )
            .expect("a first describe registers the type");
        node_types
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor_for(path).with_input(port_named("frames")),
                a_constructor_refusing_with("later describe"),
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
                    a_constructor_refusing_with("a describe that must be refused"),
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
                a_constructor_refusing_with("the first stream's interpreter"),
            )
            .expect("the first stream describes the class");
        second_stream
            .register_a_type_described_in_this_streams_processor_interpreter(
                descriptor_for(path.as_str()).with_input(port_named("frames")),
                a_constructor_refusing_with("the second stream's interpreter"),
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

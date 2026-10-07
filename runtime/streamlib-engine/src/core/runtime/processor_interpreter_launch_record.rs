// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use parking_lot::Mutex;

use crate::core::compiler::compiler_ops::processor_interpreter_describe::{
    a_processor_interpreter_describes, describe_and_register_node_types_in_a_processor_interpreter,
};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::processors::PROCESSOR_REGISTRY;

use super::StreamEnvironment;

/// What a runtime starts its processor interpreters with: the lend directory
/// its host handed it, and the stream environment recorded at the last load.
#[derive(Default)]
pub(crate) struct ProcessorInterpreterLaunchRecord {
    processor_interpreter_lend_directory: Mutex<Option<PathBuf>>,
    stream_environment_recorded_at_the_last_load: Mutex<Option<StreamEnvironment>>,
}

impl ProcessorInterpreterLaunchRecord {
    pub(crate) fn set_processor_interpreter_lend_directory(&self, lend_directory: PathBuf) {
        *self.processor_interpreter_lend_directory.lock() = Some(lend_directory);
    }

    pub(crate) fn record_the_stream_environment_of_a_load(
        &self,
        stream_environment: Option<StreamEnvironment>,
    ) {
        *self.stream_environment_recorded_at_the_last_load.lock() = stream_environment;
    }

    pub(crate) fn stream_environment_recorded_at_the_last_load(&self) -> Option<StreamEnvironment> {
        self.stream_environment_recorded_at_the_last_load
            .lock()
            .clone()
    }

    /// Describe, in one interpreter start, every type in `node_types` a
    /// processor interpreter describes and no other registration holds —
    /// re-describing one an earlier describe registered — and register each.
    pub(crate) fn describe_and_register_every_type_a_load_names<'a>(
        &self,
        node_types: impl IntoIterator<Item = &'a ProcessorClassImportPath>,
    ) -> Result<()> {
        let mut node_types_to_describe: Vec<ProcessorClassImportPath> = Vec::new();
        for node_type in node_types {
            if a_processor_interpreter_describes(node_type)
                && !PROCESSOR_REGISTRY.is_registered_other_than_by_a_describe(node_type)
                && !node_types_to_describe.contains(node_type)
            {
                node_types_to_describe.push(node_type.clone());
            }
        }
        self.describe_and_register(&node_types_to_describe)
    }

    /// Whether a live add of `node_type` has to describe it first: a type a
    /// processor interpreter describes that nothing has registered yet.
    pub(crate) fn a_live_add_must_describe(&self, node_type: &ProcessorClassImportPath) -> bool {
        a_processor_interpreter_describes(node_type)
            && PROCESSOR_REGISTRY.descriptor(node_type).is_none()
    }

    /// Describe and register `node_type` for a live add, in the environment
    /// recorded at the last load.
    pub(crate) fn describe_and_register_a_type_a_live_add_names(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Result<()> {
        self.describe_and_register(std::slice::from_ref(node_type))
    }

    fn describe_and_register(&self, node_types: &[ProcessorClassImportPath]) -> Result<()> {
        if node_types.is_empty() {
            return Ok(());
        }
        let Some(stream_environment) = self.stream_environment_recorded_at_the_last_load() else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this runtime was given no stream environment — \
                          load the stream with its project directory and interpreter"
                    .to_string(),
            });
        };
        let Some(lend_directory) = self.processor_interpreter_lend_directory.lock().clone() else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this runtime's host gave it no lend directory to \
                          start one from"
                    .to_string(),
            });
        };
        describe_and_register_node_types_in_a_processor_interpreter(
            node_types,
            &stream_environment,
            &lend_directory,
        )
    }
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::compiler::compiler_ops::processor_interpreter_describe::{
    INSTALLED_MODULES_DESCRIBE_BOUND, a_processor_interpreter_describes,
    describe_and_register_node_types_in_a_processor_interpreter,
    describe_the_node_types_installed_modules_declare_within,
    register_the_node_types_installed_modules_declare,
};
use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::LoadedStreamAHelperProcessBelongsTo;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::processors::NodeTypesOneStreamResolves;
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};

use super::StreamEnvironment;

/// What one loaded stream starts its processor interpreters with: its stream
/// environment, the node types described into it, the tag and shutdown its
/// helper processes belong to, and whether its host interrupted its describes.
pub(crate) struct ProcessorInterpreterLaunchRecordOfOneStream {
    stream_identity: LoadedStreamIdentity,
    project_directory: Arc<Path>,
    stream_environment: Option<StreamEnvironment>,
    node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
    the_stream_its_helpers_belong_to: LoadedStreamAHelperProcessBelongsTo,
    describes_interrupted_by_the_host: AtomicBool,
}

impl ProcessorInterpreterLaunchRecordOfOneStream {
    pub(crate) fn new(
        stream_identity: LoadedStreamIdentity,
        project_directory: Arc<Path>,
        stream_environment: Option<StreamEnvironment>,
        node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
        the_stream_its_helpers_belong_to: LoadedStreamAHelperProcessBelongsTo,
    ) -> Self {
        let launch_record = Self {
            stream_identity,
            project_directory,
            stream_environment,
            node_types_this_stream_resolves,
            the_stream_its_helpers_belong_to,
            describes_interrupted_by_the_host: AtomicBool::new(false),
        };
        launch_record.resolve_the_views_the_streams_project_brings_as_they_are_on_disk();
        launch_record
    }

    /// Resolve each view the stream's project and its imports bring, as their `views/` are on
    /// disk now, as a node type of this stream. A stream whose directory holds no project brings
    /// none; one whose views cannot be listed is warned about and brings none.
    pub(crate) fn resolve_the_views_the_streams_project_brings_as_they_are_on_disk(&self) {
        let views = match streamlib_project_repository::every_view_a_project_and_its_imports_bring(
            &self.project_directory,
        ) {
            Ok(views) => views,
            Err(streamlib_project_repository::ProjectRepositoryRefusal::NotAProject { .. }) => {
                Vec::new()
            }
            Err(refusal) => {
                tracing::warn!(
                    project_directory = %self.project_directory.display(),
                    %refusal,
                    "the stream's project views are no node types: they could not be listed"
                );
                Vec::new()
            }
        };
        self.node_types_this_stream_resolves
            .replace_the_view_types_this_streams_project_brings(&views);
    }

    /// The node types this stream resolves, which a live add checks its
    /// config's references against.
    pub(crate) fn node_types_this_stream_resolves(&self) -> &NodeTypesOneStreamResolves {
        &self.node_types_this_stream_resolves
    }

    pub(crate) fn stream_environment(&self) -> Option<&StreamEnvironment> {
        self.stream_environment.as_ref()
    }

    pub(crate) fn interrupt_every_describe(&self) {
        self.describes_interrupted_by_the_host
            .store(true, Ordering::SeqCst);
    }

    /// Describe, in one interpreter start, every type in `node_types` a
    /// processor interpreter describes and no natively compiled type holds, and
    /// register each into this stream.
    pub(crate) fn describe_and_register_every_type_a_load_names<'a>(
        &self,
        node_types: impl IntoIterator<Item = &'a ProcessorClassImportPath>,
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        self.resolve_the_views_the_streams_project_brings_as_they_are_on_disk();
        let mut node_types_to_describe: Vec<ProcessorClassImportPath> = Vec::new();
        for node_type in node_types {
            if a_processor_interpreter_describes(node_type)
                && !self
                    .node_types_this_stream_resolves
                    .is_a_natively_compiled_node_type(node_type)
                && !self
                    .node_types_this_stream_resolves
                    .is_a_view_type_of_this_streams_project(node_type)
                && !node_types_to_describe.contains(node_type)
            {
                node_types_to_describe.push(node_type.clone());
            }
        }
        self.describe_and_register(&node_types_to_describe, lend_directory)
    }

    /// Whether a live add of `node_type` has to describe it first: a type a
    /// processor interpreter describes that this stream does not resolve yet, even once the
    /// views its project brings are read again.
    pub(crate) fn a_live_add_must_describe(&self, node_type: &ProcessorClassImportPath) -> bool {
        if !a_processor_interpreter_describes(node_type)
            || self
                .node_types_this_stream_resolves
                .descriptor(node_type)
                .is_some()
        {
            return false;
        }
        self.resolve_the_views_the_streams_project_brings_as_they_are_on_disk();
        self.node_types_this_stream_resolves
            .descriptor(node_type)
            .is_none()
    }

    /// Describe and register `node_type` into this stream for a live add.
    pub(crate) fn describe_and_register_a_type_a_live_add_names(
        &self,
        node_type: &ProcessorClassImportPath,
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        self.describe_and_register(std::slice::from_ref(node_type), lend_directory)
    }

    /// Describe every class installed distributions declare in `tatolab.nodes`
    /// and register each into this stream, so its catalog lists them before a
    /// graph names one. A stream with no environment or no lend directory has
    /// no interpreter to read them in; a describe that fails is warned about
    /// and leaves the stream as it was, since no graph depends on it.
    pub(crate) fn describe_and_register_the_node_types_installed_modules_declare(
        &self,
        lend_directory: Option<&Path>,
    ) {
        let (Some(stream_environment), Some(lend_directory)) =
            (self.stream_environment.as_ref(), lend_directory)
        else {
            return;
        };
        let described = describe_the_node_types_installed_modules_declare_within(
            stream_environment,
            lend_directory,
            INSTALLED_MODULES_DESCRIBE_BOUND,
            &self.the_stream_its_helpers_belong_to,
            &|| {
                self.describes_interrupted_by_the_host
                    .load(Ordering::SeqCst)
            },
        );
        let described = match described {
            Ok(described) => described,
            Err(refusal) => {
                tracing::warn!(
                    %refusal,
                    "the stream's installed modules were not described, so its node catalog \
                     lists none of their node types"
                );
                return;
            }
        };
        let registered = register_the_node_types_installed_modules_declare(
            &self.node_types_this_stream_resolves,
            described,
            stream_environment,
            lend_directory,
            &self.the_stream_its_helpers_belong_to,
        );
        for node_type in registered {
            self.stream_identity.publish_on_this_streams_topic(
                RuntimeEvent::RuntimeDidRegisterProcessorType {
                    processor_type: node_type,
                },
            );
        }
    }

    fn describe_and_register(
        &self,
        node_types: &[ProcessorClassImportPath],
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        if node_types.is_empty() {
            return Ok(());
        }
        let views_no_node_takes: Vec<String> = node_types
            .iter()
            .filter_map(|node_type| {
                self.node_types_this_stream_resolves
                    .why_no_node_takes_the_view(node_type)
            })
            .collect();
        if !views_no_node_takes.is_empty() {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: views_no_node_takes.join(" "),
            });
        }
        let Some(stream_environment) = self.stream_environment.as_ref() else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this stream was loaded with no stream environment — \
                          load the stream with its project directory and interpreter"
                    .to_string(),
            });
        };
        let Some(lend_directory) = lend_directory else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this runtime's host gave it no lend directory to \
                          start one from"
                    .to_string(),
            });
        };
        for node_types_of_one_import_folder in
            node_types_grouped_by_the_import_folder_holding_their_code(node_types)
        {
            describe_and_register_node_types_in_a_processor_interpreter(
                &self.node_types_this_stream_resolves,
                &node_types_of_one_import_folder,
                stream_environment,
                lend_directory,
                &self.the_stream_its_helpers_belong_to,
                &|| {
                    self.describes_interrupted_by_the_host
                        .load(Ordering::SeqCst)
                },
            )?;
        }
        for node_type in node_types {
            self.stream_identity.publish_on_this_streams_topic(
                RuntimeEvent::RuntimeDidRegisterProcessorType {
                    processor_type: node_type.clone(),
                },
            );
        }
        Ok(())
    }
}

/// `node_types` split by the import folder whose node code defines each, the project's own and
/// its venv's first, in their order: one interpreter imports one module per top-level name, so
/// each folder's types are described in an interpreter of their own.
fn node_types_grouped_by_the_import_folder_holding_their_code(
    node_types: &[ProcessorClassImportPath],
) -> Vec<Vec<ProcessorClassImportPath>> {
    let mut node_types_by_import_folder: std::collections::BTreeMap<
        Option<&str>,
        Vec<ProcessorClassImportPath>,
    > = std::collections::BTreeMap::new();
    for node_type in node_types {
        node_types_by_import_folder
            .entry(
                node_type
                    .import_folder_and_class_import_path()
                    .map(|(import_folder, _)| import_folder),
            )
            .or_default()
            .push(node_type.clone());
    }
    node_types_by_import_folder.into_values().collect()
}

/// A path in the runtime unit that the runtime's host hands it once, before any
/// load: the lend directory every stream's describes and processor interpreters
/// borrow, or the uv every project's `.venv` is made with.
pub(crate) struct RuntimeUnitPathTheHostHandsTheEngineOnce {
    what_the_path_is: &'static str,
    held: std::sync::OnceLock<PathBuf>,
}

impl RuntimeUnitPathTheHostHandsTheEngineOnce {
    /// None held yet, refusals naming the path as `what_the_path_is`.
    pub(crate) const fn naming(what_the_path_is: &'static str) -> Self {
        Self {
            what_the_path_is,
            held: std::sync::OnceLock::new(),
        }
    }

    /// Hold `path`, refusing a second one: what the engine already started
    /// uses the first.
    pub(crate) fn set(&self, path: PathBuf) -> Result<()> {
        self.held.set(path).map_err(|refused_path| {
            Error::Runtime(format!(
                "this runtime was already handed {} {}, so {} was refused",
                self.what_the_path_is,
                self.held
                    .get()
                    .map(|held| held.display().to_string())
                    .unwrap_or_default(),
                refused_path.display()
            ))
        })
    }

    pub(crate) fn get(&self) -> Option<&Path> {
        self.held.get().map(PathBuf::as_path)
    }
}

#[cfg(test)]
mod tests;

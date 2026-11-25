// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The fragments a refresh re-imports when a project's Python modules change.

use std::path::PathBuf;

use super::*;

/// The module a project's Python file at `changed_file` is imported as: its path from the
/// project directory, `/` read as `.`, a package's `__init__.py` as the package; `None` for a
/// file that is no `.py` or lies outside the project.
pub(super) fn the_module_a_project_file_is_imported_as(
    project_directory: &std::path::Path,
    changed_file: &std::path::Path,
) -> Option<String> {
    let relative_file = if changed_file.is_absolute() {
        changed_file.strip_prefix(project_directory).ok()?
    } else {
        changed_file
    };
    if relative_file.extension()? != "py" {
        return None;
    }
    let mut module_parts: Vec<String> = relative_file
        .with_extension("")
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part) => part.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if module_parts.last().is_some_and(|last| last == "__init__") {
        module_parts.pop();
    }
    (!module_parts.is_empty()).then(|| module_parts.join("."))
}

/// An `import_graph_generator` for each top-level fragment of each loaded stream of the project
/// at `project_directory` whose generator, or one nested in it, a module of `changed_files`
/// defines, run with the promoted settings the fragment keeps.
pub(crate) fn the_refresh_of_each_fragment_a_changed_module_defines(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project_repositories: &ProjectRepositoriesOpenInThisRuntime,
    project_directory: &std::path::Path,
    changed_files: &[PathBuf],
) -> Result<Vec<StreamGraphAction>, ProjectRepositoryRefusal> {
    let changed_modules: HashSet<String> = changed_files
        .iter()
        .filter_map(|changed_file| {
            the_module_a_project_file_is_imported_as(project_directory, changed_file)
        })
        .collect();
    if changed_modules.is_empty() {
        return Ok(Vec::new());
    }
    let project_repository = project_repositories.the_project_repository_at(project_directory)?;
    let project_repository = project_repository.lock();
    let project = project_repository.project_file().project;
    let mut refreshes = Vec::new();
    for listing in operations_on_the_loaded_streams.list_streams() {
        let Some(stream_location) = listing.stream_location.filter(|stream_location| {
            stream_location.project == project && listing.node_count.is_some()
        }) else {
            continue;
        };
        let staged_graph =
            project_repository.stream_graph_document(stream_location.stream_asset)?;
        for fragment_node in staged_graph.fragment_nodes() {
            if fragment_node.node_type == STREAM_FRAGMENT_NODE_TYPE {
                continue;
            }
            let defined_in_a_changed_module = fragment_node
                .graph_generators_a_refresh_runs()
                .iter()
                .any(|generator| {
                    generator
                        .split_once(':')
                        .is_some_and(|(module, _)| changed_modules.contains(module))
                });
            if !defined_in_a_changed_module {
                continue;
            }
            refreshes.push(StreamGraphAction::ImportGraphGenerator {
                stream: stream_location.stream_asset,
                generator: fragment_node.node_type,
                name: Some(fragment_node.name),
                label: None,
            });
        }
    }
    Ok(refreshes)
}

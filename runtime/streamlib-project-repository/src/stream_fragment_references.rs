// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use streamlib_processor_schema::{
    IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR, ProcessorClassImportPath,
};
use uuid::Uuid;

use crate::project_file::TatolabProjectFile;
use crate::project_import_folders::the_folder_of_an_import;
use crate::project_stream_document::project_stream_documents;
use crate::stream_graph_document::{
    STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE,
    asset_references_made_references_into_the_import,
};

/// The keys of a held stream's graph document a stream fragment places: its graph.
const KEYS_OF_A_HELD_STREAM_A_FRAGMENT_PLACES: [&str; 3] = ["nodes", "links", "groups"];

/// `graph_document`, a stream of the project at `project_directory`, with each stream fragment
/// it holds, `{"stream": {"asset", "from"?}}`, given the graph of the stream it names as its
/// `fragment`: read from this project, or from the import `from` names, as its document is on
/// disk now, and resolved in turn. Each node and group the fragment places takes the UUID
/// UUIDv5(fragment node's UUID, its own UUID), each asset reference into an import names
/// that import with `from`, and each node type an import's own node code defines is led by the
/// import's folder, `imports/<folder>/<module>:<qualname>`, so it is imported from there. Refused, naming each stream, when a stream holds itself, directly
/// or through another stream, and when a held stream does not read.
pub fn graph_document_with_each_stream_fragment_reference_resolved(
    project_directory: &Path,
    graph_document: &Value,
) -> Result<Value, String> {
    if !graph_document_holds_a_stream_fragment(graph_document) {
        return Ok(graph_document.clone());
    }
    let project = TatolabProjectFile::read_from_project_directory(project_directory)
        .map_err(|refusal| refusal.to_string())?
        .project;
    let mut resolved = graph_document.clone();
    let stream_asset = graph_document
        .get("asset")
        .and_then(Value::as_str)
        .and_then(|asset| Uuid::parse_str(asset).ok());
    let mut streams_being_resolved = Vec::new();
    if let Some(stream_asset) = stream_asset {
        streams_being_resolved.push(HeldStreamBeingResolved {
            project,
            stream_asset,
            named_as: format!("stream {stream_asset}"),
        });
    }
    let context = ProjectHoldingTheReferences {
        project_directory: project_directory.to_path_buf(),
        project,
        root_project: project,
        root_project_directory: project_directory.to_path_buf(),
    };
    if let Value::Object(level) = &mut resolved {
        resolve_each_stream_fragment_of_the_level(&context, level, &mut streams_being_resolved)?;
    }
    Ok(resolved)
}

/// The fragment the stream fragment node `fragment_node` places for `held_stream`, its config's
/// `stream`, in a stream of the project at `project_directory`.
pub fn the_fragment_a_stream_fragment_places(
    project_directory: &Path,
    fragment_node: Uuid,
    held_stream: &Value,
) -> Result<Value, String> {
    let project = TatolabProjectFile::read_from_project_directory(project_directory)
        .map_err(|refusal| refusal.to_string())?
        .project;
    let context = ProjectHoldingTheReferences {
        project_directory: project_directory.to_path_buf(),
        project,
        root_project: project,
        root_project_directory: project_directory.to_path_buf(),
    };
    the_fragment_held_stream_places(&context, fragment_node, held_stream, &mut Vec::new())
}

/// The graph document and file name of the stream `stream_asset` of the import `from`, as the
/// project at `project_directory` resolves it.
pub fn the_stream_document_of_an_import(
    project_directory: &Path,
    from: Uuid,
    stream_asset: Uuid,
) -> Result<(Value, String), String> {
    let import_folder =
        the_folder_of_an_import(project_directory, from).map_err(|refusal| refusal.to_string())?;
    read_the_held_stream(&import_folder, from, stream_asset)
}

/// Whether `level`, a graph document or a fragment, holds a stream fragment at any depth.
pub fn graph_document_holds_a_stream_fragment(level: &Value) -> bool {
    level
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|node| {
            node.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE)
                || node
                    .get("fragment")
                    .is_some_and(graph_document_holds_a_stream_fragment)
        })
}

struct ProjectHoldingTheReferences {
    project_directory: PathBuf,
    project: Uuid,
    root_project: Uuid,
    root_project_directory: PathBuf,
}

struct HeldStreamBeingResolved {
    project: Uuid,
    stream_asset: Uuid,
    named_as: String,
}

fn resolve_each_stream_fragment_of_the_level(
    context: &ProjectHoldingTheReferences,
    level: &mut Map<String, Value>,
    streams_being_resolved: &mut Vec<HeldStreamBeingResolved>,
) -> Result<(), String> {
    let Some(Value::Array(nodes)) = level.get_mut("nodes") else {
        return Ok(());
    };
    for node in nodes.iter_mut().filter_map(Value::as_object_mut) {
        if node.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE) {
            let fragment_node = node
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or("a stream fragment holds no UUID as its `id`")?;
            let held_stream = node
                .get("config")
                .and_then(|config| config.get(STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY))
                .cloned()
                .unwrap_or(Value::Null);
            let fragment = the_fragment_held_stream_places(
                context,
                fragment_node,
                &held_stream,
                streams_being_resolved,
            )?;
            node.insert("fragment".to_owned(), fragment);
        } else if let Some(Value::Object(fragment)) = node.get_mut("fragment") {
            resolve_each_stream_fragment_of_the_level(context, fragment, streams_being_resolved)?;
        }
    }
    Ok(())
}

fn the_fragment_held_stream_places(
    context: &ProjectHoldingTheReferences,
    fragment_node: Uuid,
    held_stream: &Value,
    streams_being_resolved: &mut Vec<HeldStreamBeingResolved>,
) -> Result<Value, String> {
    let stream_asset = held_stream
        .get("asset")
        .and_then(Value::as_str)
        .and_then(|asset| Uuid::parse_str(asset).ok())
        .ok_or_else(|| {
            format!(
                "stream fragment {fragment_node} names no stream: its config's \
                 `{STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY}` holds no `asset` UUID"
            )
        })?;
    let from = match held_stream.get("from") {
        None => None,
        Some(from) => Some(
            from.as_str()
                .and_then(|from| Uuid::parse_str(from).ok())
                .ok_or_else(|| {
                    format!("stream fragment {fragment_node}'s `from` is no project's UUID")
                })?,
        ),
    };
    let held_context = match from {
        None => ProjectHoldingTheReferences {
            project_directory: context.project_directory.clone(),
            project: context.project,
            root_project: context.root_project,
            root_project_directory: context.root_project_directory.clone(),
        },
        Some(from) => ProjectHoldingTheReferences {
            project_directory: the_folder_of_an_import(&context.project_directory, from)
                .map_err(|refusal| format!("stream fragment {fragment_node}: {refusal}"))?,
            project: from,
            root_project: context.root_project,
            root_project_directory: context.root_project_directory.clone(),
        },
    };
    let (held_document, stream_name) = read_the_held_stream(
        &held_context.project_directory,
        held_context.project,
        stream_asset,
    )?;
    let named_as = format!("`{stream_name}` ({stream_asset})");
    if streams_being_resolved
        .iter()
        .any(|held| held.project == held_context.project && held.stream_asset == stream_asset)
    {
        let cycle: Vec<&str> = streams_being_resolved
            .iter()
            .map(|held| held.named_as.as_str())
            .chain(std::iter::once(named_as.as_str()))
            .collect();
        return Err(format!(
            "the streams {} hold one another as fragments in a cycle, so none of them loads",
            cycle.join(" -> ")
        ));
    }
    streams_being_resolved.push(HeldStreamBeingResolved {
        project: held_context.project,
        stream_asset,
        named_as,
    });
    let mut fragment = Map::new();
    if let Value::Object(held_level) = &held_document {
        for key in KEYS_OF_A_HELD_STREAM_A_FRAGMENT_PLACES {
            if let Some(value) = held_level.get(key) {
                fragment.insert(key.to_owned(), value.clone());
            }
        }
    }
    resolve_each_stream_fragment_of_the_level(
        &held_context,
        &mut fragment,
        streams_being_resolved,
    )?;
    streams_being_resolved.pop();
    if let Some(import_folder) = held_context.import_folder_relative_to_the_root_project() {
        each_node_type_the_imports_own_code_defines_led_by_its_folder(
            &mut fragment,
            &held_context.project_directory,
            &import_folder,
        )?;
    }
    each_node_and_group_given_its_uuid_under(&mut fragment, fragment_node);
    let mut fragment = Value::Object(fragment);
    if held_context.project != held_context.root_project {
        asset_references_made_references_into_the_import(&mut fragment, held_context.project);
    }
    Ok(fragment)
}

impl ProjectHoldingTheReferences {
    /// The folder of the import holding the references, relative to the root project, as a
    /// node type names it; `None` for the root project itself.
    fn import_folder_relative_to_the_root_project(&self) -> Option<String> {
        let relative = self
            .project_directory
            .strip_prefix(&self.root_project_directory)
            .ok()?;
        let components: Vec<String> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        (!components.is_empty())
            .then(|| components.join(&IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR.to_string()))
    }
}

/// Lead each node type of `level`, at every level of a graph generator's stored output it holds,
/// with `import_folder` where the import at `import_directory` holds its module's top-level
/// package or file. A stream fragment's own nodes are left alone: they were named when it was
/// placed, from the project holding its stream.
fn each_node_type_the_imports_own_code_defines_led_by_its_folder(
    level: &mut Map<String, Value>,
    import_directory: &Path,
    import_folder: &str,
) -> Result<(), String> {
    let Some(Value::Array(nodes)) = level.get_mut("nodes") else {
        return Ok(());
    };
    for node in nodes.iter_mut().filter_map(Value::as_object_mut) {
        if node.get("type").and_then(Value::as_str) == Some(STREAM_FRAGMENT_NODE_TYPE) {
            continue;
        }
        if let Some(Value::Object(stored_output)) = node.get_mut("fragment") {
            each_node_type_the_imports_own_code_defines_led_by_its_folder(
                stored_output,
                import_directory,
                import_folder,
            )?;
            continue;
        }
        let Some(Value::String(node_type)) = node.get_mut("type") else {
            continue;
        };
        if the_imports_own_code_defines(import_directory, node_type) {
            *node_type =
                ProcessorClassImportPath::of_a_class_in_an_import_folder(import_folder, node_type)
                    .map_err(|refusal| refusal.to_string())?
                    .as_str()
                    .to_owned();
        }
    }
    Ok(())
}

/// Whether `node_type`, a Python `<module>:<qualname>`, names a module whose top-level package
/// or file the import at `import_directory` holds.
fn the_imports_own_code_defines(import_directory: &Path, node_type: &str) -> bool {
    let Some((module_name, qualname)) = node_type.split_once(':') else {
        return false;
    };
    if module_name.is_empty()
        || qualname.is_empty()
        || node_type.contains("::")
        || node_type.contains(IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR)
        || ProcessorClassImportPath::new(node_type)
            .map_or(true, |path| path.names_a_built_in_node())
    {
        return false;
    }
    let top_level_module = module_name.split('.').next().unwrap_or(module_name);
    import_directory.join(top_level_module).is_dir()
        || import_directory
            .join(format!("{top_level_module}.py"))
            .is_file()
}

fn read_the_held_stream(
    project_directory: &Path,
    project: Uuid,
    stream_asset: Uuid,
) -> Result<(Value, String), String> {
    let stream_document = project_stream_documents(project_directory)
        .map_err(|refusal| refusal.to_string())?
        .into_iter()
        .find(|stream_document| stream_document.stream_asset(project) == stream_asset)
        .ok_or_else(|| {
            format!(
                "{} holds no stream {stream_asset}",
                project_directory.display()
            )
        })?;
    let bytes = stream_document
        .bytes
        .as_ref()
        .map_err(|not_read| format!("`{}` does not read: {not_read}", stream_document.path))?;
    let document = serde_json::from_slice(bytes)
        .map_err(|not_parsed| format!("`{}` does not parse: {not_parsed}", stream_document.path))?;
    Ok((document, stream_document.stream_name().to_owned()))
}

/// Give each node and group of `level`, at every level of a fragment it holds, the UUID
/// UUIDv5(`namespace`, its own), and each link end and group membership the node's.
fn each_node_and_group_given_its_uuid_under(level: &mut Map<String, Value>, namespace: Uuid) {
    let placed_uuid_of = |held: &str| -> Option<String> {
        Uuid::parse_str(held)
            .ok()
            .map(|held| Uuid::new_v5(&namespace, held.as_bytes()).to_string())
    };
    let mut placed_uuid_of_each_node: BTreeMap<String, String> = BTreeMap::new();
    if let Some(Value::Array(nodes)) = level.get_mut("nodes") {
        for node in nodes.iter_mut().filter_map(Value::as_object_mut) {
            if let Some(Value::String(id)) = node.get_mut("id")
                && let Some(placed) = placed_uuid_of(id)
            {
                placed_uuid_of_each_node.insert(id.clone(), placed.clone());
                *id = placed;
            }
            if let Some(Value::Object(studio)) = node.get_mut("studio")
                && let Some(Value::String(group)) = studio.get_mut("group")
                && let Some(placed) = placed_uuid_of(group)
            {
                *group = placed;
            }
            if let Some(Value::Object(fragment)) = node.get_mut("fragment") {
                each_node_and_group_given_its_uuid_under(fragment, namespace);
            }
        }
    }
    if let Some(Value::Array(links)) = level.get_mut("links") {
        for link in links.iter_mut().filter_map(Value::as_object_mut) {
            for end_key in ["source", "target"] {
                if let Some(Value::Object(end)) = link.get_mut(end_key)
                    && let Some(Value::String(node)) = end.get_mut("node")
                    && let Some(placed) = placed_uuid_of_each_node.get(node.as_str())
                {
                    *node = placed.clone();
                }
            }
        }
    }
    if let Some(Value::Array(groups)) = level.get_mut("groups") {
        for group in groups.iter_mut().filter_map(Value::as_object_mut) {
            if let Some(Value::String(id)) = group.get_mut("id")
                && let Some(placed) = placed_uuid_of(id)
            {
                *id = placed;
            }
        }
    }
}

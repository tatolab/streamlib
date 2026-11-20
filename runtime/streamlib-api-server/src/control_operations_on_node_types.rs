// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The three node type reads: `list_pin_types`, `list_node_types` and `describe_node_types`.
//! Each answers the best description the runtime has of a node type and says where it came
//! from: the runtime's own for a built-in, a loaded stream's interpreter for a project node type
//! one has described, else its declaration read from source.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use streamlib::sdk::descriptors::{
    FunctionNodeLanguage, NodeDescriptor, NodeKind, PinConversion, PinType,
};
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeTypePortsDescribedOnTheWire};
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_control_contract::{
    ControlOperation, DescribeNodeTypes, DescribeNodeTypesRequest, ListNodeTypes,
    ListNodeTypesRequest, ListPinTypes, ListPinTypesRequest, the_suggested_call,
};
use streamlib_node_schema::pin_type::built_in_pin_types;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, DescribeNodeTypesToolResult, GraphGeneratorListed,
    GraphGeneratorReadFromSource, ListNodeTypesToolResult, ListPinTypesToolResult,
    ListProjectNodeTypesToolResult, NamedPinTypeListed, NodeTypeCategoryFrom,
    NodeTypeDescribedFrom, NodeTypeDescribedOrWhyNot, NodeTypeDescription, NodeTypeInputDescribed,
    NodeTypeKind, NodeTypeListed, NodeTypeNotDescribed, NodeTypePortDescribed,
    NodeTypePortReadFromSource, NodeTypeReadFromSource, NodeTypeSourceListed,
    NodeTypeSourceListedOrigin, NodeTypeSourceOrigin, PinTypeConversionListed, PinTypeListed,
    ProjectActionRefusalOnTheWire, ProjectFilePlaceOnTheWire,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::node_type_groups_of_a_project::NodeTypeGroupsOfAProject;

/// The operation that checks a project's code, which a node type that cannot be described
/// suggests once the runtime serves it.
const CHECK_PROJECT_CODE_OPERATION: &str = "check_project_code";

/// The node types a project's loaded streams described in their own interpreter, its installed
/// modules' among them, by type; a type two streams describe is the first's.
type NodeTypesDescribedInTheProjectsInterpreter = BTreeMap<String, NodeDescriptor>;

impl ControlOperationDispatcher {
    pub(crate) async fn list_pin_types(
        &self,
        ListPinTypesRequest { project_directory }: ListPinTypesRequest,
    ) -> Result<ListPinTypesToolResult, ControlOperationRefusal> {
        let project = self
            .the_project_in(ListPinTypes::METHOD_NAME, project_directory.clone())
            .await?;
        let conversions: Vec<PinTypeConversionListed> = self
            .the_conversions_the_projects_loaded_streams_resolve(project)
            .await?
            .into_values()
            .map(|conversion| PinTypeConversionListed {
                from: conversion.input_pin_type,
                to: conversion.output_pin_type,
                node_type: conversion.node_type.to_string(),
            })
            .collect();
        let (named_pin_types, named_pin_types_not_listed_because) = match self
            .the_node_types_the_project_offers_and_its_groups(
                ListPinTypes::METHOD_NAME,
                project_directory,
            )
            .await
        {
            Ok((listing, _)) => (the_named_pin_types_listed_in(listing), None),
            Err(not_listed) => (Vec::new(), Some(not_listed.to_string())),
        };
        Ok(ListPinTypesToolResult {
            pin_types: built_in_pin_types()
                .into_iter()
                .map(|row| PinTypeListed {
                    name: row.name,
                    label: row.label.to_owned(),
                    documentation: row.documentation.to_owned(),
                    flows: row.flows.to_vec(),
                    annotations: row.annotations.to_vec(),
                    python_forms: row
                        .python_forms
                        .iter()
                        .map(|&form| form.to_owned())
                        .collect(),
                    typescript_form: row.typescript_form.to_owned(),
                    how_to_declare: row.name.how_to_declare().map(str::to_owned),
                    value_json_schema: row.name.value_json_schema_on_its_own(),
                    converts_to: conversions
                        .iter()
                        .filter(|conversion| conversion.from.built_in_name() == row.name)
                        .cloned()
                        .collect(),
                })
                .collect(),
            named_pin_types,
            named_pin_types_not_listed_because,
        })
    }

    pub(crate) async fn list_node_types(
        &self,
        ListNodeTypesRequest { project_directory }: ListNodeTypesRequest,
    ) -> Result<ListNodeTypesToolResult, ControlOperationRefusal> {
        let (listing, node_type_groups) = self
            .the_node_types_the_project_offers_and_its_groups(
                ListNodeTypes::METHOD_NAME,
                project_directory,
            )
            .await?;
        let described = self
            .the_node_types_the_projects_loaded_streams_described(the_project_of(&listing)?)
            .await?;
        let ListProjectNodeTypesToolResult {
            project_directory,
            project,
            project_name,
            sources,
        } = listing;
        let mut sources: Vec<NodeTypeSourceListed> = sources
            .into_iter()
            .map(|source| NodeTypeSourceListed {
                origin: the_listed_origin_of(source.origin),
                node_types: source
                    .node_types
                    .iter()
                    .map(|read| NodeTypeListed {
                        node_type: read.node_type.clone(),
                        documentation: the_first_line_of(&read.description),
                        category: read.category.clone(),
                        category_from: read.category_from,
                        kind: read.kind.as_deref().and_then(the_kind_named),
                        described_from: if described.contains_key(&read.node_type) {
                            NodeTypeDescribedFrom::Interpreter
                        } else {
                            NodeTypeDescribedFrom::Source
                        },
                    })
                    .collect(),
                graph_generators: source
                    .graph_generators
                    .into_iter()
                    .map(a_graph_generator_listed)
                    .collect(),
                unreadable_modules: source.unreadable_modules,
            })
            .collect();
        sources.push(the_built_in_source(&node_type_groups));
        Ok(ListNodeTypesToolResult {
            project_directory,
            project,
            project_name,
            sources,
        })
    }

    pub(crate) async fn describe_node_types(
        &self,
        DescribeNodeTypesRequest {
            project_directory,
            node_types,
        }: DescribeNodeTypesRequest,
    ) -> Result<DescribeNodeTypesToolResult, ControlOperationRefusal> {
        let (listing, node_type_groups) = self
            .the_node_types_the_project_offers_and_its_groups(
                DescribeNodeTypes::METHOD_NAME,
                project_directory,
            )
            .await?;
        let described = self
            .the_node_types_the_projects_loaded_streams_described(the_project_of(&listing)?)
            .await?;
        let read_from_source: BTreeMap<&str, (&NodeTypeSourceOrigin, &NodeTypeReadFromSource)> =
            listing
                .sources
                .iter()
                .flat_map(|source| {
                    source
                        .node_types
                        .iter()
                        .map(move |read| (read.node_type.as_str(), (&source.origin, read)))
                })
                // Reversed, so a type two sources declare is the first's, as the listing orders
                // its sources.
                .rev()
                .collect();
        let node_types = node_types
            .iter()
            .map(|node_type| {
                if let Some((origin, read)) = read_from_source.get(node_type.as_str()) {
                    return Ok(the_project_node_type_described(
                        &listing,
                        origin,
                        read,
                        described.get(node_type),
                    ));
                }
                the_built_in_node_type(node_type)
                    .map(|descriptor| {
                        let mut description = a_registered_node_type_described(
                            &descriptor,
                            NodeTypeSourceListedOrigin::BuiltIn,
                            NodeTypeDescribedFrom::BuiltIn,
                            None,
                        );
                        node_type_groups.file(
                            &description.node_type,
                            &mut description.category,
                            &mut description.category_from,
                        );
                        NodeTypeDescribedOrWhyNot::Described(Box::new(description))
                    })
                    .ok_or_else(|| {
                        the_refusal_of_a_node_type_not_offered(
                            DescribeNodeTypes::METHOD_NAME,
                            &listing,
                            node_type,
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DescribeNodeTypesToolResult { node_types })
    }

    /// The UUID of the project in `project_directory`, read for the operation `method_name`.
    async fn the_project_in(
        &self,
        method_name: &'static str,
        project_directory: PathBuf,
    ) -> Result<Uuid, ControlOperationRefusal> {
        self.a_project_repository_call_off_the_async_runtime(
            method_name,
            project_directory,
            |project_repository| Ok(project_repository.project_file().project),
        )
        .await
    }

    /// The conversion each node type the project's loaded streams resolve states, as each
    /// stream's link verdict reads them, by type: the native ones when no stream is loaded, and
    /// a type two streams resolve the first's.
    async fn the_conversions_the_projects_loaded_streams_resolve(
        &self,
        project: Uuid,
    ) -> Result<BTreeMap<String, PinConversion>, ControlOperationRefusal> {
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || {
            let mut conversions: BTreeMap<String, PinConversion> = NODE_REGISTRY
                .pin_conversions()
                .iter()
                .map(|conversion| (conversion.node_type.to_string(), conversion.clone()))
                .collect();
            for stream in the_loaded_streams_of(operations_on_the_loaded_streams.as_ref(), project)
            {
                for conversion in operations_on_the_loaded_streams
                    .pin_conversions_of_the_stream_a_call_names(&stream)
                    .unwrap_or_else(|_| Arc::from([]))
                    .iter()
                {
                    conversions
                        .entry(conversion.node_type.to_string())
                        .or_insert_with(|| conversion.clone());
                }
            }
            conversions
        })
        .await
        .map_err(ControlOperationRefusal::the_task_did_not_finish(
            "the read of the conversions the project's streams resolve",
        ))
    }

    /// Every node type the loaded streams of `project` described in their own interpreter,
    /// those its installed modules declare among them; none while no stream of it is loaded.
    async fn the_node_types_the_projects_loaded_streams_described(
        &self,
        project: Uuid,
    ) -> Result<NodeTypesDescribedInTheProjectsInterpreter, ControlOperationRefusal> {
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || {
            the_node_types_described_in_the_interpreter_of_each_stream_of(
                operations_on_the_loaded_streams.as_ref(),
                project,
            )
        })
        .await
        .map_err(ControlOperationRefusal::the_task_did_not_finish(
            "the read of the node types the project's streams described",
        ))
    }
}

/// The node types each loaded stream of `project` described in its own interpreter, a stream
/// unloaded while they are read left out.
fn the_node_types_described_in_the_interpreter_of_each_stream_of(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project: Uuid,
) -> NodeTypesDescribedInTheProjectsInterpreter {
    let mut described = NodeTypesDescribedInTheProjectsInterpreter::new();
    for stream in the_loaded_streams_of(operations_on_the_loaded_streams, project) {
        let stream = stream.as_str();
        let in_its_interpreter = operations_on_the_loaded_streams
            .node_types_described_in_the_interpreter_of_the_stream_a_call_names(stream)
            .unwrap_or_default();
        let from_its_installed_modules = operations_on_the_loaded_streams
            .node_types_installed_modules_declare_in_the_stream_a_call_names(stream)
            .unwrap_or_default()
            .into_iter()
            .map(|(descriptor, _)| descriptor);
        for descriptor in in_its_interpreter
            .into_iter()
            .chain(from_its_installed_modules)
        {
            described
                .entry(descriptor.node_class_import_path.to_string())
                .or_insert(descriptor);
        }
    }
    described
}

/// The id of each stream of `project` this runtime lists.
fn the_loaded_streams_of(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project: Uuid,
) -> Vec<String> {
    operations_on_the_loaded_streams
        .list_streams()
        .into_iter()
        .filter(|listing| {
            listing
                .stream_location
                .as_ref()
                .is_some_and(|location| location.project == project)
        })
        .map(|listing| listing.stream_id)
        .collect()
}

/// The UUID a listing names its project by.
fn the_project_of(
    listing: &ListProjectNodeTypesToolResult,
) -> Result<Uuid, ControlOperationRefusal> {
    Uuid::parse_str(&listing.project).map_err(|not_a_uuid| {
        ControlOperationRefusal::runtime_failed(format!(
            "the project's UUID `{}` does not read: {not_a_uuid}",
            listing.project
        ))
    })
}

/// A source as the old listing names it, as `list_node_types` names it.
/// Each named type a source of `listing` declares, with the source it comes from, in the order
/// the sources list.
fn the_named_pin_types_listed_in(
    listing: ListProjectNodeTypesToolResult,
) -> Vec<NamedPinTypeListed> {
    listing
        .sources
        .into_iter()
        .flat_map(|source| {
            let origin = the_listed_origin_of(source.origin);
            source
                .named_pin_types
                .into_iter()
                .map(move |read| NamedPinTypeListed {
                    origin: origin.clone(),
                    pin_type: PinType::Named {
                        named_pin_type: read.named_pin_type,
                    },
                    documentation: read.description,
                    path: read.path,
                    line: read.line,
                })
        })
        .collect()
}

fn the_listed_origin_of(origin: NodeTypeSourceOrigin) -> NodeTypeSourceListedOrigin {
    match origin {
        NodeTypeSourceOrigin::Project => NodeTypeSourceListedOrigin::Project,
        NodeTypeSourceOrigin::Dependency {
            project,
            folder,
            version,
            project_name,
        } => NodeTypeSourceListedOrigin::Dependency {
            project,
            folder,
            version,
            project_name,
        },
        NodeTypeSourceOrigin::InstalledDistribution {
            distribution,
            distribution_version,
        } => NodeTypeSourceListedOrigin::InstalledDistribution {
            distribution,
            distribution_version,
        },
    }
}

/// The node types the runtime brings itself, by import path, each filed under the group the
/// project's and its imports' `node_type_groups` put it in, else the one it declares.
fn the_built_in_source(node_type_groups: &NodeTypeGroupsOfAProject) -> NodeTypeSourceListed {
    let mut registered = NODE_REGISTRY.list_registered();
    registered.sort_by(|one, other| {
        one.node_class_import_path
            .as_str()
            .cmp(other.node_class_import_path.as_str())
    });
    NodeTypeSourceListed {
        origin: NodeTypeSourceListedOrigin::BuiltIn,
        node_types: registered
            .iter()
            .map(|descriptor| {
                let mut listed = NodeTypeListed {
                    node_type: descriptor.node_class_import_path.to_string(),
                    documentation: the_first_line_of(&descriptor.description),
                    category: descriptor.category.clone(),
                    category_from: None,
                    kind: Some(the_kind_of(descriptor.node_kind)),
                    described_from: NodeTypeDescribedFrom::BuiltIn,
                };
                node_type_groups.file(
                    &listed.node_type,
                    &mut listed.category,
                    &mut listed.category_from,
                );
                listed
            })
            .collect(),
        graph_generators: Vec::new(),
        unreadable_modules: Vec::new(),
    }
}

/// A graph generator as the old listing reads it, its promoted settings so named.
fn a_graph_generator_listed(generator: GraphGeneratorReadFromSource) -> GraphGeneratorListed {
    GraphGeneratorListed {
        generator: generator.generator,
        description: generator.description,
        promoted_settings: generator.promoted_config,
        path: generator.path,
        line: generator.line,
    }
}

/// A project node type `read` from source in the source `origin`: as a loaded stream's
/// interpreter `described` it where one did, else as its declaration states it, else why it
/// cannot be described.
pub(crate) fn the_project_node_type_described(
    listing: &ListProjectNodeTypesToolResult,
    origin: &NodeTypeSourceOrigin,
    read: &NodeTypeReadFromSource,
    described: Option<&NodeDescriptor>,
) -> NodeTypeDescribedOrWhyNot {
    let origin = the_listed_origin_of(origin.clone());
    let declared_at = read.path.as_ref().map(|path| ProjectFilePlaceOnTheWire {
        path: path.clone(),
        line: read.line,
        column: None,
    });
    if let Some(descriptor) = described {
        let mut description = a_registered_node_type_described(
            descriptor,
            origin,
            NodeTypeDescribedFrom::Interpreter,
            declared_at,
        );
        // Filed as the listing filed it where the project or an import files it, else under the
        // group the interpreter reads its author declaring.
        match read.category_from {
            Some(NodeTypeCategoryFrom::Project | NodeTypeCategoryFrom::Import) => {
                description.category = read.category.clone();
                description.category_from = read.category_from;
            }
            Some(NodeTypeCategoryFrom::Declaration) | None => {
                description.category_from = description
                    .category
                    .as_ref()
                    .map(|_| NodeTypeCategoryFrom::Declaration);
            }
        }
        return NodeTypeDescribedOrWhyNot::Described(Box::new(description));
    }
    let (Some(inputs), Some(outputs)) = (&read.inputs, &read.outputs) else {
        return NodeTypeDescribedOrWhyNot::NotDescribed(Box::new(NodeTypeNotDescribed {
            node_type: read.node_type.clone(),
            not_described_because: the_refusal_of_a_node_type_source_does_not_describe(
                listing, read,
            ),
            origin,
        }));
    };
    NodeTypeDescribedOrWhyNot::Described(Box::new(NodeTypeDescription {
        node_type: read.node_type.clone(),
        origin,
        described_from: NodeTypeDescribedFrom::Source,
        documentation: read.description.clone(),
        category: read.category.clone(),
        category_from: read.category_from,
        kind: read.kind.as_deref().and_then(the_kind_named),
        declared_at,
        inputs: inputs.iter().map(an_input_read_from_source).collect(),
        outputs: outputs.iter().map(a_port_read_from_source).collect(),
        exposable_config_fields: Vec::new(),
    }))
}

/// A registered node type, from the source `origin`, described from `described_from`, under the
/// group it declares, which no one has filed yet.
fn a_registered_node_type_described(
    descriptor: &NodeDescriptor,
    origin: NodeTypeSourceListedOrigin,
    described_from: NodeTypeDescribedFrom,
    declared_at: Option<ProjectFilePlaceOnTheWire>,
) -> NodeTypeDescription {
    let NodeTypePortsDescribedOnTheWire {
        inputs,
        outputs,
        exposable_config_fields,
    } = NodeTypePortsDescribedOnTheWire::of(descriptor);
    NodeTypeDescription {
        node_type: descriptor.node_class_import_path.to_string(),
        origin,
        described_from,
        documentation: descriptor.description.clone(),
        category: descriptor.category.clone(),
        category_from: None,
        kind: Some(the_kind_of(descriptor.node_kind)),
        declared_at,
        inputs,
        outputs,
        exposable_config_fields,
    }
}

/// The node type the runtime brings itself by the import path `node_type`.
pub(crate) fn the_built_in_node_type(node_type: &str) -> Option<NodeDescriptor> {
    NODE_REGISTRY
        .list_registered()
        .into_iter()
        .find(|descriptor| descriptor.node_class_import_path.as_str() == node_type)
}

/// A port as its declaration states it in source: by its family alone, a pin type not being
/// read there.
fn a_port_read_from_source(port: &NodeTypePortReadFromSource) -> NodeTypePortDescribed {
    NodeTypePortDescribed {
        name: port.name.clone(),
        documentation: port.description.clone(),
        pin: None,
        annotations: Default::default(),
        family: port
            .family
            .as_ref()
            .map(|family| json!({ "family": family })),
        annotation: port.annotation.clone(),
    }
}

/// An input as its declaration states it in source, with the default it writes where that is
/// a literal.
fn an_input_read_from_source(port: &NodeTypePortReadFromSource) -> NodeTypeInputDescribed {
    NodeTypeInputDescribed {
        port: a_port_read_from_source(port),
        mode: None,
        takes_a_wire: true,
        read: None,
        default_value: port.pin_default.clone(),
        lifetime: None,
        value_json_schema: None,
    }
}

/// Why `read`, which its source states no ports of and no loaded stream described, cannot be
/// described, suggesting the check of its file where the runtime serves one.
fn the_refusal_of_a_node_type_source_does_not_describe(
    listing: &ListProjectNodeTypesToolResult,
    read: &NodeTypeReadFromSource,
) -> ProjectActionRefusalOnTheWire {
    let mut params = Map::from_iter([(
        "project_directory".to_owned(),
        json!(listing.project_directory),
    )]);
    if let Some(path) = &read.path {
        params.insert("paths".to_owned(), json!([path]));
    }
    ProjectActionRefusalOnTheWire {
        refused_file: read.path.as_ref().map(|path| ProjectFilePlaceOnTheWire {
            path: path.clone(),
            line: read.line,
            column: None,
        }),
        suggested_call: the_suggested_call(CHECK_PROJECT_CODE_OPERATION, params),
        ..ProjectActionRefusalOnTheWire::of_the_action(
            DescribeNodeTypes::METHOD_NAME,
            ControlOperationRefusalCode::Refused,
            format!(
                "{} failed: `{}` states its ports only in the code that registers it, which its \
                 source does not show and no loaded stream of the project has described; add it \
                 to a stream of the project, which describes it in that stream's interpreter",
                DescribeNodeTypes::METHOD_NAME,
                read.node_type
            ),
        )
    }
}

/// The refusal of a `method_name` call naming `node_type`, which neither the project nor the
/// runtime offers, suggesting the call that lists what they do.
pub(crate) fn the_refusal_of_a_node_type_not_offered(
    method_name: &'static str,
    listing: &ListProjectNodeTypesToolResult,
    node_type: &str,
) -> ControlOperationRefusal {
    ControlOperationRefusal::RefusedAction(Box::new(ProjectActionRefusalOnTheWire {
        suggested_call: the_suggested_call(
            ListNodeTypes::METHOD_NAME,
            Map::from_iter([(
                "project_directory".to_owned(),
                json!(listing.project_directory),
            )]),
        ),
        ..ProjectActionRefusalOnTheWire::of_the_action(
            method_name,
            ControlOperationRefusalCode::NodeTypeNotFound,
            format!(
                "{method_name} failed: neither the project `{}` nor the runtime offers a node \
                 type `{node_type}`; `{}` lists the ones they do",
                listing.project_name,
                ListNodeTypes::METHOD_NAME
            ),
        )
    }))
}

/// The first line of `description`, trimmed.
fn the_first_line_of(description: &str) -> String {
    description
        .trim()
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// How a node of `kind` runs.
fn the_kind_of(kind: NodeKind) -> NodeTypeKind {
    match kind {
        NodeKind::Process => NodeTypeKind::Process,
        NodeKind::Shader => NodeTypeKind::Shader,
        NodeKind::Function {
            language: FunctionNodeLanguage::Python | FunctionNodeLanguage::Rust,
        } => NodeTypeKind::Function,
    }
}

/// The kind the source reader names `kind`; `None` for a name it never writes.
fn the_kind_named(kind: &str) -> Option<NodeTypeKind> {
    serde_json::from_value(Value::String(kind.to_owned())).ok()
}

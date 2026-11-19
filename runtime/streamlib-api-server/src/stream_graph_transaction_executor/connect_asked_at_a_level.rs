// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operation a `connect` asks of one level of a running stream, read as a transaction's
//! `connect` reads it and made nowhere: the stream's own level as its graph renders it, or a
//! level inside a placed stream as the project's documents hold it.

use super::*;

/// The document of the level `level` of placed streams reaches inside the stream asset
/// `stream`, each document as `project_repository` holds it, and the operation a `connect` from
/// `source` to `target` at that level asks of it, as
/// [`super::the_operation_a_connect_asks_of_the_running_stream`] makes it, `rendered_graph` the
/// running stream's `graph` render and `stream_operations` its operations; a connect is made an
/// operation on the level's own document, refused as that document refuses it.
pub(crate) fn the_operation_a_connect_asks_inside_a_placed_stream(
    source: StreamGraphActionLinkEnd,
    target: StreamGraphActionLinkEnd,
    stream: Uuid,
    level: &[Uuid],
    rendered_graph: &Value,
    stream_operations: &dyn RuntimeOperations,
    project_repository: &ProjectRepository,
) -> Result<(StreamGraphDocument, StreamGraphOperation), GraphRequestRefusal> {
    let kept = std::cell::RefCell::new(None);
    let reached = project_repository
        .stream_graph_document(stream)?
        .the_level_a_path_of_placed_streams_reaches(stream, level, &mut |placed_stream| {
            project_repository
                .stream_graph_document(placed_stream)
                .map_err(|refusal| GraphRequestRefusal::from(refusal).words_kept_in(&kept))
        })
        .map_err(|why| GraphRequestRefusal::kept_in_or(&kept, why))?;
    let operation = the_operation_a_connect_asks_at_the_level(
        source,
        target,
        &reached.the_document_as_drawn()?,
        rendered_graph,
        stream_operations,
    )?;
    let operation = match operation {
        StreamGraphOperation::Connect { .. } => {
            let connect = reached.the_operation_on_the_document(operation)?;
            reached.document.clone().apply(&connect)?;
            connect
        }
        other => other,
    };
    Ok((reached.document, operation))
}

/// The operation a `connect` from `source` to `target` asks of `level_as_drawn`, a level of the
/// running stream as drawn, made as [`super::the_operation_a_connect_asks_of_the_running_stream`]
/// makes it.
pub(super) fn the_operation_a_connect_asks_at_the_level(
    source: StreamGraphActionLinkEnd,
    target: StreamGraphActionLinkEnd,
    level_as_drawn: &StreamGraphDocument,
    rendered_graph: &Value,
    stream_operations: &dyn RuntimeOperations,
) -> Result<StreamGraphOperation, GraphRequestRefusal> {
    let no_node_by_label = HashMap::new();
    let (source, target) = the_link_ends_under_their_current_port_names(
        source,
        target,
        level_as_drawn,
        &no_node_by_label,
        stream_operations,
    );
    let operation =
        the_operation_a_connect_asks(&source, &target, level_as_drawn, &no_node_by_label, &|| {
            Ok(the_function_nodes_of_the_render(rendered_graph))
        })?;
    if let StreamGraphOperation::Connect { link } = &operation {
        refuse_a_link_the_graph_already_holds(level_as_drawn, link)?;
    }
    Ok(operation)
}

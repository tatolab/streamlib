// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `connect` inside a placed stream, judged among the nodes of the level it is made at as the
//! project's documents hold that level, and made nowhere.

use super::request_application::the_level_reached;
use super::*;
use crate::link_checked_before_it_is_made::the_verdict_a_link_made_is_judged_by;

/// A `connect` inside a placed stream, judged and made nowhere: whether the level it is made at
/// holds the node at each of its ends by UUID, and what it would make of its link.
pub(crate) struct AConnectJudgedInsideAPlacedStream {
    /// Whether the level, as drawn, holds the node at each end by its UUID.
    pub(crate) level_holds_both_link_ends: bool,
    /// What the `connect` would make of its link, refused as it is refused; `None` when it
    /// closes a loop of function nodes through a `PreviousValue` node it adds.
    pub(crate) made: Result<Option<WhatALiveConnectMakesOfALink>, GraphRequestRefusal>,
}

/// A `connect` from `source` to `target` at the level `level` of placed streams reaches inside
/// the stream asset `stream`, each document as `project_repository` holds it, read as a
/// transaction's `connect` reads it, `rendered_graph` the running stream's `graph` render and
/// `stream_operations` its operations; the link is made on a copy of the level's own document,
/// refused as that document refuses it, and judged there as
/// [`the_judgement_of_a_link_at_a_placed_level`] judges it.
pub(crate) fn a_connect_judged_inside_a_placed_stream(
    source: StreamGraphActionLinkEnd,
    target: StreamGraphActionLinkEnd,
    stream: Uuid,
    level: &[Uuid],
    rendered_graph: &Value,
    stream_operations: &dyn RuntimeOperations,
    project_repository: &ProjectRepository,
) -> AConnectJudgedInsideAPlacedStream {
    let mut level_holds_both_link_ends = false;
    let mut judged = || {
        let reached = the_level_reached(
            project_repository.stream_graph_document(stream)?,
            stream,
            level,
            &|placed_stream| Ok(project_repository.stream_graph_document(placed_stream)?),
        )?;
        let level_as_drawn = reached.the_document_as_drawn()?;
        level_holds_both_link_ends =
            the_level_holds_both_ends_by_uuid(&level_as_drawn, &source, &target, &HashMap::new());
        let asked = what_a_connect_asks_at_a_level(
            source.clone(),
            target.clone(),
            rendered_graph,
            &level_as_drawn,
            stream_operations,
            &|_| Ok(None),
        )?;
        let WhatAConnectAsks::Link(link) = asked else {
            return Ok(None);
        };
        let connect =
            reached.the_operation_on_the_document(StreamGraphOperation::Connect { link })?;
        reached.document.clone().apply(&connect)?;
        let StreamGraphOperation::Connect { link } = connect else {
            return Err("a connect was made no link on the level's document".into());
        };
        the_judgement_of_a_link_at_a_placed_level(stream_operations, &reached.document, &link)
            .map(Some)
    };
    let made = judged();
    AConnectJudgedInsideAPlacedStream {
        level_holds_both_link_ends,
        made,
    }
}

/// What a `connect` would make of `link` at `level`, a level of a placed stream as its document
/// holds it, by the engine's judgement of a link there; a link only a convert node bridges is
/// refused, since `connect` places that node itself only at a stream's own level.
fn the_judgement_of_a_link_at_a_placed_level(
    stream_operations: &dyn RuntimeOperations,
    level: &StreamGraphDocument,
    link: &StreamGraphDocumentLink,
) -> Result<WhatALiveConnectMakesOfALink, GraphRequestRefusal> {
    let made = stream_operations.what_a_connect_makes_of_a_link_at_a_placed_level(level, link)?;
    if let WhatALiveConnectMakesOfALink::Makes(Some(verdict)) = &made
        && let PinLinkOutcome::ConnectsThroughConvertNode(convert_node) = &verdict.outcome
    {
        return Err(the_refusal_of_a_link_only_a_convert_node_bridges(
            convert_node,
            WhyConnectPlacesNoConvertNode::TheLinkIsAtAPlacedLevel,
        ));
    }
    Ok(made)
}

/// Why each link `edit` makes at `level`, a level of a placed stream as its document held it
/// before the edit, is made without being judged, in the order the edit makes them, `None` for
/// one judged: each judged as [`the_judgement_of_a_link_at_a_placed_level`] judges it, a node
/// `edit` adds placed first and each link judged among the ones made before it, as a running
/// stream makes them. Refused as a connect of a refused link is.
pub(super) fn why_each_link_an_edit_makes_at_a_placed_level_goes_unjudged(
    stream_operations: &dyn RuntimeOperations,
    level: &StreamGraphDocument,
    edit: &StreamGraphOperation,
) -> Result<Vec<Option<String>>, GraphRequestRefusal> {
    let why_unjudged = |level: &StreamGraphDocument, link: &StreamGraphDocumentLink| {
        the_judgement_of_a_link_at_a_placed_level(stream_operations, level, link).map(|made| {
            the_verdict_a_link_made_is_judged_by(made)
                .err()
                .map(|why| why.to_string())
        })
    };
    match edit {
        StreamGraphOperation::Connect { link } => Ok(vec![why_unjudged(level, link)?]),
        StreamGraphOperation::AddNode { node, links } if !links.is_empty() => {
            let mut level = level.clone();
            level.apply(&StreamGraphOperation::AddNode {
                node: node.clone(),
                links: Vec::new(),
            })?;
            let mut why_each_link_goes_unjudged = Vec::with_capacity(links.len());
            for link in links {
                why_each_link_goes_unjudged.push(why_unjudged(&level, link)?);
                level.apply(&StreamGraphOperation::Connect { link: link.clone() })?;
            }
            Ok(why_each_link_goes_unjudged)
        }
        _ => Ok(Vec::new()),
    }
}

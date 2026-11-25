// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;

use crate::core::graph::graph_fragment_flattening::{
    GRAPH_FRAGMENT_NODE_PATH_SEPARATOR, GraphFragmentsAsDrawn,
};

/// The levels a flattened graph's nodes run on: the stream's own, named `""`, and each
/// fragment's, named by its path of names.
pub(super) struct GraphLevelsOfFlattenedFragments {
    fragment_paths_of_names: HashSet<String>,
}

impl GraphLevelsOfFlattenedFragments {
    pub(super) fn of(fragments_flattened: &GraphFragmentsAsDrawn) -> Self {
        let mut fragment_paths_of_names = HashSet::new();
        let mut fragments_to_visit: Vec<_> = fragments_flattened.fragments.iter().collect();
        while let Some(fragment) = fragments_to_visit.pop() {
            fragment_paths_of_names.insert(fragment.path_of_names.clone());
            fragments_to_visit.extend(&fragment.fragments_it_holds);
        }
        Self {
            fragment_paths_of_names,
        }
    }

    /// The level `node_name` runs on: the innermost fragment whose path of names prefixes
    /// it, or the stream's own. Asked of a level, it answers the level enclosing it.
    pub(super) fn level_of<'name>(&self, node_name: &'name str) -> &'name str {
        let mut prefix_end = node_name.len();
        while let Some(separator) =
            node_name[..prefix_end].rfind(GRAPH_FRAGMENT_NODE_PATH_SEPARATOR)
        {
            if self
                .fragment_paths_of_names
                .contains(&node_name[..separator])
            {
                return &node_name[..separator];
            }
            prefix_end = separator;
        }
        ""
    }
}

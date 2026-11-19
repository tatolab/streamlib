// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The group each node type a project offers is filed under: the project's own map first, then
//! the map of the import that holds it, then the group its author declared. An import's map
//! names the import's own node types as the import names them, so a node type listed as
//! `imports/<folder>/<module>:<class>` is looked up in the map of the import in `<folder>` by
//! `<module>:<class>`; it files nothing else the project offers.

use std::collections::BTreeMap;

use streamlib_project_repository::PROJECT_IMPORTS_DIRECTORY;
use streamlib_runtime_client_contract::local_api_wire_contract::NodeTypeCategoryFrom;

/// The node type to group maps a project and its imports hold.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct NodeTypeGroupsOfAProject {
    /// The project's own map, by node type.
    pub(crate) own: BTreeMap<String, String>,
    /// Each import's map that names any node type.
    pub(crate) of_each_import: Vec<NodeTypeGroupsOfAnImport>,
}

/// The node type to group map one import holds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NodeTypeGroupsOfAnImport {
    /// The folder under `imports/` that holds the import.
    pub(crate) folder: String,
    /// The import's map, by node type as the import names it.
    pub(crate) groups: BTreeMap<String, String>,
}

impl NodeTypeGroupsOfAProject {
    /// `category`, the group the author of `node_type` declared, replaced by the group the
    /// project or one of its imports files it under, and `category_from` set to who filed it.
    pub(crate) fn file(
        &self,
        node_type: &str,
        category: &mut Option<String>,
        category_from: &mut Option<NodeTypeCategoryFrom>,
    ) {
        match self.the_group_filed_under(node_type) {
            Some((group, filed_by)) => {
                *category = Some(group.to_owned());
                *category_from = Some(filed_by);
            }
            None => *category_from = category.as_ref().map(|_| NodeTypeCategoryFrom::Declaration),
        }
    }

    /// The group the project's own map, else the map of the import holding `node_type`, files
    /// it under, and which.
    fn the_group_filed_under(&self, node_type: &str) -> Option<(&str, NodeTypeCategoryFrom)> {
        if let Some(group) = self.own.get(node_type) {
            return Some((group, NodeTypeCategoryFrom::Project));
        }
        let (folder, as_the_import_names_it) = node_type
            .strip_prefix(PROJECT_IMPORTS_DIRECTORY)?
            .strip_prefix('/')?
            .split_once('/')?;
        self.of_each_import
            .iter()
            .find(|import| import.folder == folder)?
            .groups
            .get(as_the_import_names_it)
            .map(|group| (group.as_str(), NodeTypeCategoryFrom::Import))
    }
}

#[cfg(test)]
mod tests;

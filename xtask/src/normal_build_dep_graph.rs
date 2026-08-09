// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `cargo metadata` resolve graph reduced to link-closure edges.
//!
//! Relocated from `streamlib-pack` when the packaging tools were deleted:
//! `check_boundaries`' trunk-set → engine boundary walk is the only surviving
//! consumer, and the type carries no packaging semantics of its own.

use anyhow::Result;

/// True iff a `deps[]` entry from `cargo metadata`'s resolve graph is a normal
/// or build edge (`kind` `null` or `"build"`) — i.e. it participates in the
/// publish / link closure. A dev-only edge returns `false`.
///
/// Dropping dev-only edges is load-bearing: a crate's conformance test may pull
/// a heavy dependency through `[dev-dependencies]`, and counting that edge as a
/// real link would false-report the dependency as linked into the crate.
fn resolve_dep_is_normal_or_build(dep: &serde_json::Value) -> bool {
    dep.get("dep_kinds")
        .and_then(|k| k.as_array())
        .map(|kinds| {
            kinds
                .iter()
                .any(|k| matches!(k.get("kind").and_then(|v| v.as_str()), None | Some("build")))
        })
        .unwrap_or(true)
}


/// A `cargo metadata` dependency graph reduced to the edges that participate in
/// the publish / link closure: normal + build edges only, keyed by package id.
///
#[derive(Debug, Clone)]
pub struct NormalBuildDepGraph {
    deps_by_id: std::collections::HashMap<String, Vec<String>>,
    name_by_id: std::collections::HashMap<String, String>,
    workspace_member_ids: std::collections::HashSet<String>,
}

impl NormalBuildDepGraph {
    /// Build the graph from a parsed `cargo metadata --format-version 1`
    /// document. Errors if the document carries no `resolve` graph (produced
    /// with `--no-deps`), since the dependency edges are then absent.
    pub fn from_metadata(metadata: &serde_json::Value) -> Result<Self> {
        let workspace_member_ids: std::collections::HashSet<String> = metadata
            .get("workspace_members")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let empty = Vec::new();
        let packages = metadata
            .get("packages")
            .and_then(|p| p.as_array())
            .unwrap_or(&empty);
        let mut name_by_id = std::collections::HashMap::new();
        for pkg in packages {
            if let (Some(id), Some(name)) = (
                pkg.get("id").and_then(|v| v.as_str()),
                pkg.get("name").and_then(|v| v.as_str()),
            ) {
                name_by_id.insert(id.to_string(), name.to_string());
            }
        }

        let resolve_nodes = metadata
            .get("resolve")
            .and_then(|r| r.get("nodes"))
            .and_then(|n| n.as_array())
            .ok_or_else(|| {
                anyhow::anyhow!("cargo metadata has no resolve graph (ran with --no-deps?)")
            })?;
        let mut deps_by_id = std::collections::HashMap::new();
        for node in resolve_nodes {
            let Some(id) = node.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let mut deps = Vec::new();
            if let Some(dep_arr) = node.get("deps").and_then(|d| d.as_array()) {
                for dep in dep_arr {
                    if resolve_dep_is_normal_or_build(dep)
                        && let Some(pkg) = dep.get("pkg").and_then(|v| v.as_str())
                    {
                        deps.push(pkg.to_string());
                    }
                }
            }
            deps_by_id.insert(id.to_string(), deps);
        }

        Ok(Self {
            deps_by_id,
            name_by_id,
            workspace_member_ids,
        })
    }

    /// The package name for `id`, if the metadata carried it.
    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.name_by_id.get(id).map(|s| s.as_str())
    }

    /// True iff `id` is a workspace member (from `workspace_members`).
    pub fn is_workspace_member(&self, id: &str) -> bool {
        self.workspace_member_ids.contains(id)
    }

    /// The normal + build dependency ids of `id` (dev-only edges already
    /// dropped). Empty for an unknown id.
    pub fn normal_build_deps(&self, id: &str) -> &[String] {
        self.deps_by_id.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Every workspace-member package id.
    pub fn workspace_member_ids(&self) -> impl Iterator<Item = &str> {
        self.workspace_member_ids.iter().map(|s| s.as_str())
    }

    /// Every package id whose name equals `name`.
    pub fn ids_named<'graph>(&'graph self, name: &str) -> Vec<&'graph str> {
        self.name_by_id
            .iter()
            .filter(|(_, n)| n.as_str() == name)
            .map(|(id, _)| id.as_str())
            .collect()
    }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The core project the runtime unit carries: `packages/tatolab-core-nodes`, copied beside the
//! lend into `lib/tatolab/core-projects/tatolab-core-nodes/`, from where the runtime links it
//! into every project as an import.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    core_nodes_project_directory_beside_the_lend, runtime_unit_lend_directory_in_the_workspace,
};

/// The Core Nodes project the unit's copy is staged from, relative to the workspace.
pub const CORE_NODES_PROJECT_RELATIVE_TO_WORKSPACE: &str = "packages/tatolab-core-nodes";

/// The file that makes the folder a project.
const PROJECT_FILE_NAME: &str = "tatolab-project.json";

/// Folders the unit's copy never carries: caches, and the project's own tests.
const FOLDERS_THE_CORE_PROJECT_COPY_NEVER_CARRIES: [&str; 3] =
    ["__pycache__", "node_modules", "tests"];

/// Files the unit's copy never carries: a lock written for the project opened on its own.
const FILES_THE_CORE_PROJECT_COPY_NEVER_CARRIES: [&str; 1] = ["uv.lock"];

/// Where a project holds what a runtime wrote for it, which a `pyproject.toml` the runtime
/// rewrote names its sources and find-links under.
const PROJECT_RUNTIME_DIRECTORY_PREFIX: &str = ".tatolab/";

/// The Core Nodes project directory of the runtime unit in `workspace_root`.
pub fn runtime_unit_core_nodes_project_directory_in_the_workspace(
    workspace_root: &Path,
) -> Result<PathBuf> {
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    core_nodes_project_directory_beside_the_lend(&lend_directory).with_context(|| {
        format!(
            "the lend at {} has no directory beside it",
            lend_directory.display()
        )
    })
}

/// Replace the runtime unit's Core Nodes project with a fresh copy of the workspace's.
pub fn stage_the_core_nodes_project(workspace_root: &Path) -> Result<()> {
    let staged_directory =
        runtime_unit_core_nodes_project_directory_in_the_workspace(workspace_root)?;
    if staged_directory.exists() {
        std::fs::remove_dir_all(&staged_directory)
            .with_context(|| format!("removing {}", staged_directory.display()))?;
    }
    stage_the_core_project_from(
        &workspace_root.join(CORE_NODES_PROJECT_RELATIVE_TO_WORKSPACE),
        &staged_directory,
    )?;
    tracing::info!(
        "build-runtime: the Core Nodes project staged in {}",
        staged_directory.display()
    );
    Ok(())
}

/// Copy the project at `core_project` into `staged_directory`, its `pyproject.toml` as its
/// source declares it and not as a runtime that opened it rewrote it.
pub fn stage_the_core_project_from(core_project: &Path, staged_directory: &Path) -> Result<()> {
    anyhow::ensure!(
        core_project.join(PROJECT_FILE_NAME).is_file(),
        "{} holds no {PROJECT_FILE_NAME}: the runtime unit carries a project",
        core_project.display()
    );
    for relative in every_file_the_core_project_copy_carries_from(core_project)? {
        let staged = staged_directory.join(&relative);
        if let Some(parent) = staged.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(core_project.join(&relative), &staged).with_context(|| {
            format!(
                "copying {} into {}",
                core_project.join(&relative).display(),
                staged.display()
            )
        })?;
    }
    let pyproject_path = staged_directory.join("pyproject.toml");
    let pyproject_text = std::fs::read_to_string(&pyproject_path)
        .with_context(|| format!("reading {}", pyproject_path.display()))?;
    std::fs::write(
        &pyproject_path,
        pyproject_without_what_a_runtime_wrote_into_it(&pyproject_text)
            .with_context(|| format!("rewriting {}", pyproject_path.display()))?,
    )
    .with_context(|| format!("writing {}", pyproject_path.display()))?;
    Ok(())
}

/// Every regular file under `core_project` the unit's copy carries, relative and sorted: none
/// in a hidden folder, a cache or the project's tests, and no lock.
fn every_file_the_core_project_copy_carries_from(core_project: &Path) -> Result<Vec<PathBuf>> {
    let mut carried = Vec::new();
    let walk = walkdir::WalkDir::new(core_project)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !(entry.file_type().is_dir()
                && (name.starts_with('.')
                    || FOLDERS_THE_CORE_PROJECT_COPY_NEVER_CARRIES.contains(&&*name)))
        });
    for entry in walk {
        let entry = entry.with_context(|| format!("walking {}", core_project.display()))?;
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_file()
            && !FILES_THE_CORE_PROJECT_COPY_NEVER_CARRIES.contains(&&*name)
            && !name.starts_with('.')
            && !name.ends_with(".pyc")
        {
            carried.push(entry.path().strip_prefix(core_project)?.to_path_buf());
        }
    }
    carried.sort();
    Ok(carried)
}

/// `pyproject_text` without what a runtime that opened the project wrote into it: each uv path
/// source into the project's `.tatolab/` with the requirement naming it, and each find-links
/// entry there. A runtime writes those for the project opened on its own, and they name links
/// the unit's copy does not hold.
pub fn pyproject_without_what_a_runtime_wrote_into_it(pyproject_text: &str) -> Result<String> {
    let mut pyproject: toml_edit::DocumentMut =
        pyproject_text.parse().context("parsing pyproject.toml")?;
    let mut distributions_sourced_from_the_runtime_directory = Vec::new();
    let Some(uv) = pyproject
        .get_mut("tool")
        .and_then(|tool| tool.get_mut("uv"))
        .and_then(toml_edit::Item::as_table_like_mut)
    else {
        return Ok(pyproject_text.to_owned());
    };
    if let Some(sources) = uv
        .get_mut("sources")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        distributions_sourced_from_the_runtime_directory = sources
            .iter()
            .filter(|(_, source)| {
                source
                    .get("path")
                    .and_then(toml_edit::Item::as_str)
                    .is_some_and(|path| path.starts_with(PROJECT_RUNTIME_DIRECTORY_PREFIX))
            })
            .map(|(distribution_name, _)| distribution_name.to_owned())
            .collect();
        for distribution_name in &distributions_sourced_from_the_runtime_directory {
            sources.remove(distribution_name);
        }
        if sources.is_empty() {
            uv.remove("sources");
        }
    }
    if let Some(find_links) = uv
        .get_mut("find-links")
        .and_then(toml_edit::Item::as_array_mut)
    {
        find_links.retain(|entry| {
            !entry
                .as_str()
                .is_some_and(|entry| entry.starts_with(PROJECT_RUNTIME_DIRECTORY_PREFIX))
        });
        if find_links.is_empty() {
            uv.remove("find-links");
        }
    }
    if uv.is_empty()
        && let Some(tool) = pyproject
            .get_mut("tool")
            .and_then(toml_edit::Item::as_table_like_mut)
    {
        tool.remove("uv");
        if tool.is_empty() {
            pyproject.remove("tool");
        }
    }
    if let Some(dependencies) = pyproject
        .get_mut("project")
        .and_then(|project| project.get_mut("dependencies"))
        .and_then(toml_edit::Item::as_array_mut)
    {
        dependencies.retain(|requirement| {
            !requirement.as_str().is_some_and(|requirement| {
                distributions_sourced_from_the_runtime_directory
                    .iter()
                    .any(|distribution_name| distribution_name == requirement.trim())
            })
        });
    }
    Ok(pyproject.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn the_workspace_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
    }

    #[test]
    fn the_unit_carries_the_core_nodes_project_without_its_tests_or_what_a_run_wrote() {
        let staged = tempfile::tempdir().unwrap();
        let staged_directory = staged.path().join("tatolab-core-nodes");

        stage_the_core_project_from(
            &the_workspace_root().join(CORE_NODES_PROJECT_RELATIVE_TO_WORKSPACE),
            &staged_directory,
        )
        .unwrap();

        for carried in [
            "tatolab-project.json",
            "pyproject.toml",
            "core_nodes/color.py",
            "views/program/tatolab_view.json",
            "streams/main.graph.json",
        ] {
            assert!(staged_directory.join(carried).is_file(), "{carried}");
        }
        for never_carried in ["tests", ".tatolab", ".venv", "uv.lock", ".gitignore"] {
            assert!(
                !staged_directory.join(never_carried).exists(),
                "{never_carried}"
            );
        }
    }

    #[test]
    fn what_a_runtime_wrote_into_the_pyproject_is_left_out_of_the_units_copy() {
        let rewritten_by_a_runtime = "[project]\nname = \"tatolab-core-nodes\"\ndependencies = [\
             \"tatolab-stream>=0.54.3\", \"numba>=0.61\", \"tatolab-testpattern\", \"tatolab-stream\"]\n\n\
             [tool.pytest.ini_options]\ntestpaths = [\"tests\"]\n\n\
             [tool.uv]\nfind-links = [\".tatolab/carried-wheels\"]\n\n\
             [tool.uv.sources]\n\
             tatolab-testpattern = { path = \".tatolab/first-party-imports/tatolab_testpattern-0.68.87-py3-none-any.whl\" }\n\
             tatolab-stream = { path = \".tatolab/first-party-imports/tatolab_stream-0.68.87-py3-none-any.whl\" }\n";

        let staged: toml::Table =
            pyproject_without_what_a_runtime_wrote_into_it(rewritten_by_a_runtime)
                .unwrap()
                .parse()
                .unwrap();

        assert_eq!(
            staged["project"]["dependencies"],
            toml::Value::try_from(["tatolab-stream>=0.54.3", "numba>=0.61"]).unwrap()
        );
        assert!(staged["tool"].get("uv").is_none(), "{staged}");
        assert!(staged["tool"].get("pytest").is_some());
    }

    #[test]
    fn a_pyproject_no_runtime_wrote_into_is_carried_as_written() {
        let as_written = "[project]\nname = \"tatolab-core-nodes\"\n# Numba compiles.\n\
                          dependencies = [\"numba>=0.61\"]\n";

        assert_eq!(
            pyproject_without_what_a_runtime_wrote_into_it(as_written).unwrap(),
            as_written
        );
    }
}

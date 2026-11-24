// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The sample projects the runtime unit carries: every converted example in `examples/`,
//! copied beside the lend into `lib/tatolab/samples/<name>/` as a project that stands on its
//! own — its `tatolab-*` requirements floored at the runtime's version, resolved from Tatolab's
//! index rather than the workspace's `packages/`. A unit built to be proven under a test root is
//! the exception: its samples keep their sources into this checkout's `packages/`, so they run on
//! the modules of the commit being proven.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    RuntimeUnitMachineDirectories, runtime_unit_lend_directory_in_the_workspace,
    sample_projects_directory_beside_the_lend,
};
use crate::check_workspace_version_pins::read_workspace_package_version;

/// The examples every sample is staged from, relative to the workspace.
const EXAMPLES_RELATIVE_TO_WORKSPACE: &str = "examples";

/// The file that makes an example a project, and so a sample.
const PROJECT_FILE_NAME: &str = "tatolab-project.json";

/// The name every project gives Tatolab's package index.
const TATOLAB_PACKAGE_INDEX_NAME: &str = "tatolab";

/// Tatolab's package index, which serves every first-party module.
const TATOLAB_PACKAGE_INDEX_URL: &str = "https://get.tatolab.com/simple/";

/// The prefix every first-party distribution's name starts with.
const FIRST_PARTY_DISTRIBUTION_NAME_PREFIX: &str = "tatolab-";

/// Folders a sample never carries from its example.
const FOLDERS_A_SAMPLE_NEVER_CARRIES: [&str; 2] = ["__pycache__", "node_modules"];

/// Files a sample never carries from its example: a lock written against the workspace.
const FILES_A_SAMPLE_NEVER_CARRIES: [&str; 1] = ["uv.lock"];

/// The sample projects directory of the runtime unit in `workspace_root`.
pub fn runtime_unit_sample_projects_directory_in_the_workspace(
    workspace_root: &Path,
) -> Result<PathBuf> {
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    sample_projects_directory_beside_the_lend(&lend_directory).with_context(|| {
        format!(
            "the lend at {} has no directory beside it",
            lend_directory.display()
        )
    })
}

/// Every converted example in `workspace_root`'s `examples/`, in name order: each folder holding
/// a project file.
pub fn every_converted_example_in_the_workspace(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let examples_directory = workspace_root.join(EXAMPLES_RELATIVE_TO_WORKSPACE);
    let mut converted_examples: Vec<PathBuf> = std::fs::read_dir(&examples_directory)
        .with_context(|| format!("reading {}", examples_directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|example| example.join(PROJECT_FILE_NAME).is_file())
        .collect();
    converted_examples.sort();
    Ok(converted_examples)
}

/// Where a sample's first-party modules come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhereASamplesFirstPartyModulesComeFrom {
    /// Tatolab's index, at the runtime's version or later: what a released unit's samples use.
    TatolabsIndex,
    /// The workspace's own `packages/`, each named by its absolute path: what a unit proven under
    /// a test root uses. A build from main carries the version main's manifests hold, which only a
    /// release's tag moves, so the index would serve it modules built before that commit.
    TheWorkspacesPackages,
}

impl WhereASamplesFirstPartyModulesComeFrom {
    /// Where the samples of a unit keeping `machine_directories` take their modules from.
    pub fn of_a_unit_keeping(machine_directories: RuntimeUnitMachineDirectories) -> Self {
        match machine_directories {
            RuntimeUnitMachineDirectories::TheMachines => Self::TatolabsIndex,
            RuntimeUnitMachineDirectories::UnderATestRoot => Self::TheWorkspacesPackages,
        }
    }
}

/// Replace the runtime unit's sample projects directory with a fresh copy of every converted
/// example, each standing on its own at the workspace's version, its modules from `modules_from`.
pub fn stage_the_sample_projects(
    workspace_root: &Path,
    modules_from: WhereASamplesFirstPartyModulesComeFrom,
) -> Result<()> {
    let samples_directory =
        runtime_unit_sample_projects_directory_in_the_workspace(workspace_root)?;
    if samples_directory.exists() {
        std::fs::remove_dir_all(&samples_directory)
            .with_context(|| format!("removing {}", samples_directory.display()))?;
    }
    let runtime_version = read_workspace_package_version(workspace_root)?;
    let converted_examples = every_converted_example_in_the_workspace(workspace_root)?;
    for example in &converted_examples {
        let sample_name = example
            .file_name()
            .with_context(|| format!("{} names no folder", example.display()))?;
        stage_one_sample_project(
            example,
            &samples_directory.join(sample_name),
            &runtime_version,
            modules_from,
        )?;
    }
    tracing::info!(
        "build-runtime: {} sample projects staged in {}",
        converted_examples.len(),
        samples_directory.display()
    );
    Ok(())
}

/// Copy `example` into `sample_directory` with its `pyproject.toml` made to stand on its own at
/// `runtime_version`, its modules from `modules_from`.
pub fn stage_one_sample_project(
    example: &Path,
    sample_directory: &Path,
    runtime_version: &str,
    modules_from: WhereASamplesFirstPartyModulesComeFrom,
) -> Result<()> {
    for relative in every_file_a_sample_carries_from(example)? {
        let staged = sample_directory.join(&relative);
        if let Some(parent) = staged.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(example.join(&relative), &staged).with_context(|| {
            format!(
                "copying {} into {}",
                example.join(&relative).display(),
                staged.display()
            )
        })?;
    }
    let pyproject_path = sample_directory.join("pyproject.toml");
    let pyproject_text = std::fs::read_to_string(&pyproject_path)
        .with_context(|| format!("reading {}", pyproject_path.display()))?;
    std::fs::write(
        &pyproject_path,
        pyproject_of_a_sample_standing_on_its_own(
            &pyproject_text,
            runtime_version,
            modules_from,
            example,
        )
        .with_context(|| format!("rewriting {}", pyproject_path.display()))?,
    )
    .with_context(|| format!("writing {}", pyproject_path.display()))?;
    Ok(())
}

/// Every regular file under `example` a sample carries, relative and sorted: none in a hidden
/// folder, a cache or a Python virtual environment, and no lock.
fn every_file_a_sample_carries_from(example: &Path) -> Result<Vec<PathBuf>> {
    let mut carried = Vec::new();
    let walk = walkdir::WalkDir::new(example)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !(entry.file_type().is_dir()
                && (name.starts_with('.') || FOLDERS_A_SAMPLE_NEVER_CARRIES.contains(&&*name)))
        });
    for entry in walk {
        let entry = entry.with_context(|| format!("walking {}", example.display()))?;
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_file()
            && !FILES_A_SAMPLE_NEVER_CARRIES.contains(&&*name)
            && !name.ends_with(".pyc")
        {
            carried.push(entry.path().strip_prefix(example)?.to_path_buf());
        }
    }
    carried.sort();
    Ok(carried)
}

/// `pyproject_text`, read from the folder `example`, as a sample carries it: every `tatolab-*`
/// requirement floored at `runtime_version`, and Tatolab's index named, so the sample resolves
/// anywhere as a project the runtime created would. Every uv path source into the workspace is
/// taken out when `modules_from` is Tatolab's index, and otherwise kept with its path made
/// absolute against `example`, so the sample's copy, wherever it stands, reaches the same package.
pub fn pyproject_of_a_sample_standing_on_its_own(
    pyproject_text: &str,
    runtime_version: &str,
    modules_from: WhereASamplesFirstPartyModulesComeFrom,
    example: &Path,
) -> Result<String> {
    let mut pyproject: toml_edit::DocumentMut =
        pyproject_text.parse().context("parsing pyproject.toml")?;
    let dependencies = pyproject
        .get_mut("project")
        .and_then(|project| project.get_mut("dependencies"))
        .and_then(toml_edit::Item::as_array_mut)
        .context("pyproject.toml names no [project].dependencies array")?;
    for requirement in dependencies.iter_mut() {
        let Some(requirement_text) = requirement.as_str() else {
            continue;
        };
        let distribution_name = the_distribution_name_a_requirement_names(requirement_text);
        if !distribution_name.starts_with(FIRST_PARTY_DISTRIBUTION_NAME_PREFIX) {
            continue;
        }
        let decor = requirement.decor().clone();
        *requirement = toml_edit::Value::from(format!("{distribution_name}>={runtime_version}"));
        *requirement.decor_mut() = decor;
    }

    let uv = pyproject
        .entry("tool")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .context("pyproject.toml's [tool] is not a table")?
        .entry("uv")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .context("pyproject.toml's [tool.uv] is not a table")?;
    uv.set_implicit(true);
    let mut kept_workspace_packages = Vec::new();
    if let Some(sources) = uv
        .get_mut("sources")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        let workspace_path_sources: Vec<String> = sources
            .iter()
            .filter(|(_, source)| source.get("path").is_some())
            .map(|(distribution_name, _)| distribution_name.to_owned())
            .collect();
        let mut workspace_path_sources_kept = Vec::new();
        for distribution_name in workspace_path_sources {
            match modules_from {
                WhereASamplesFirstPartyModulesComeFrom::TatolabsIndex => {
                    sources.remove(&distribution_name);
                }
                WhereASamplesFirstPartyModulesComeFrom::TheWorkspacesPackages => {
                    let path_item = sources
                        .get_mut(&distribution_name)
                        .and_then(|source| source.get_mut("path"))
                        .with_context(|| {
                            format!("the source of {distribution_name} lost its path")
                        })?;
                    let path = path_item.as_str().with_context(|| {
                        format!("the path source of {distribution_name} is not a string")
                    })?;
                    let absolute = the_path_with_each_parent_step_taken(
                        &std::path::absolute(example.join(path)).with_context(|| {
                            format!("making the path source of {distribution_name} absolute")
                        })?,
                    );
                    let absolute = absolute.to_str().with_context(|| {
                        format!("the path source of {distribution_name} is not UTF-8")
                    })?;
                    let decor = path_item.as_value().map(|value| value.decor().clone());
                    *path_item = toml_edit::value(absolute);
                    if let (Some(decor), Some(value)) = (decor, path_item.as_value_mut()) {
                        *value.decor_mut() = decor;
                    }
                    workspace_path_sources_kept.push(distribution_name);
                }
            }
        }
        if sources.is_empty() {
            uv.remove("sources");
        }
        kept_workspace_packages = workspace_path_sources_kept;
    }
    // uv rebuilds a directory source only when its `pyproject.toml` changes, so a native
    // module's Rust changed since the last sync would run as it was built then: each kept
    // package is reinstalled, and so rebuilt, at every sync.
    if !kept_workspace_packages.is_empty() {
        uv.insert(
            "reinstall-package",
            toml_edit::value(toml_edit::Array::from_iter(kept_workspace_packages)),
        );
    }
    let indexes = uv
        .entry("index")
        .or_insert(toml_edit::Item::ArrayOfTables(
            toml_edit::ArrayOfTables::new(),
        ))
        .as_array_of_tables_mut()
        .context("pyproject.toml's [[tool.uv.index]] is not an array of tables")?;
    let names_the_tatolab_index = indexes.iter().any(|index| {
        index.get("name").and_then(toml_edit::Item::as_str) == Some(TATOLAB_PACKAGE_INDEX_NAME)
    });
    if !names_the_tatolab_index {
        let mut tatolab_index = toml_edit::Table::new();
        tatolab_index.insert("name", toml_edit::value(TATOLAB_PACKAGE_INDEX_NAME));
        tatolab_index.insert("url", toml_edit::value(TATOLAB_PACKAGE_INDEX_URL));
        indexes.push(tatolab_index);
    }
    Ok(pyproject.to_string())
}

/// `path` with each `..` taken as a step up from the folder before it, and each `.` dropped.
fn the_path_with_each_parent_step_taken(path: &Path) -> PathBuf {
    let mut taken = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                taken.pop();
            }
            std::path::Component::CurDir => {}
            other => taken.push(other),
        }
    }
    taken
}

/// The distribution name a PEP 508 requirement names, lowercased.
fn the_distribution_name_a_requirement_names(requirement_text: &str) -> String {
    requirement_text
        .trim()
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        })
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn the_workspace_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
    }

    #[test]
    fn a_samples_first_party_requirements_are_floored_and_resolved_from_the_tatolab_index() {
        let staged = pyproject_of_a_sample_standing_on_its_own(
            "[project]\nname = \"lens\"\ndependencies = [\n    \"tatolab-stream>=0.54.3\",\n    \
             # The camera.\n    \"tatolab-camera\",\n    \"numpy>=2.1\",\n]\n\n\
             [tool.uv.sources]\ntatolab-camera = { path = \"../../packages/tatolab-camera\", \
             editable = true }\n",
            "0.70.1",
            WhereASamplesFirstPartyModulesComeFrom::TatolabsIndex,
            Path::new("/examples/lens"),
        )
        .unwrap();

        let staged_pyproject: toml::Table = staged.parse().unwrap();
        assert_eq!(
            staged_pyproject["project"]["dependencies"],
            toml::Value::try_from([
                "tatolab-stream>=0.70.1",
                "tatolab-camera>=0.70.1",
                "numpy>=2.1"
            ])
            .unwrap()
        );
        assert!(staged.contains("# The camera."), "{staged}");
        let uv = &staged_pyproject["tool"]["uv"];
        assert!(uv.get("sources").is_none(), "{staged}");
        assert!(uv.get("reinstall-package").is_none(), "{staged}");
        assert_eq!(
            uv["index"],
            toml::Value::try_from([toml::Table::from_iter([
                ("name".to_owned(), toml::Value::from("tatolab")),
                (
                    "url".to_owned(),
                    toml::Value::from("https://get.tatolab.com/simple/")
                ),
            ])])
            .unwrap()
        );
    }

    #[test]
    fn a_sample_of_a_unit_proven_under_a_test_root_keeps_its_workspace_path_sources_absolute() {
        let staged = pyproject_of_a_sample_standing_on_its_own(
            "[project]\nname = \"lens\"\ndependencies = [\"tatolab-stream>=0.54.3\", \
             \"tatolab-camera\"]\n\n\
             [tool.uv.sources]\ntatolab-camera = { path = \"../../packages/tatolab-camera\", \
             editable = true }\ntorch = { index = \"pytorch\" }\n",
            "0.70.1",
            WhereASamplesFirstPartyModulesComeFrom::TheWorkspacesPackages,
            Path::new("/checkout/examples/lens"),
        )
        .unwrap();

        let staged_pyproject: toml::Table = staged.parse().unwrap();
        assert_eq!(
            staged_pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-stream>=0.70.1", "tatolab-camera>=0.70.1"]).unwrap()
        );
        let sources = &staged_pyproject["tool"]["uv"]["sources"];
        assert_eq!(
            sources["tatolab-camera"]["path"].as_str(),
            Some("/checkout/packages/tatolab-camera"),
            "{staged}"
        );
        assert_eq!(sources["tatolab-camera"]["editable"].as_bool(), Some(true));
        assert_eq!(sources["torch"]["index"].as_str(), Some("pytorch"));
        assert_eq!(
            staged_pyproject["tool"]["uv"]["reinstall-package"],
            toml::Value::try_from(["tatolab-camera"]).unwrap(),
            "a kept package is rebuilt at every sync, its Rust read afresh: {staged}"
        );
    }

    #[test]
    fn a_sample_keeps_a_source_that_is_not_a_workspace_path_and_an_index_it_already_names() {
        let staged = pyproject_of_a_sample_standing_on_its_own(
            "[project]\nname = \"lens\"\ndependencies = [\"torch>=2.10\"]\n\n\
             [tool.uv.sources]\ntorch = { index = \"pytorch\" }\n\n\
             [[tool.uv.index]]\nname = \"tatolab\"\nurl = \"https://get.tatolab.com/simple/\"\n",
            "0.70.1",
            WhereASamplesFirstPartyModulesComeFrom::TatolabsIndex,
            Path::new("/examples/lens"),
        )
        .unwrap();

        let staged_pyproject: toml::Table = staged.parse().unwrap();
        let uv = &staged_pyproject["tool"]["uv"];
        assert_eq!(uv["sources"]["torch"]["index"].as_str(), Some("pytorch"));
        assert_eq!(uv["index"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn every_converted_example_is_a_sample() {
        let converted_examples: Vec<String> =
            every_converted_example_in_the_workspace(the_workspace_root())
                .unwrap()
                .iter()
                .map(|example| example.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
        assert!(
            converted_examples.contains(&"test-pattern-halftone-lens".to_owned()),
            "the sample that keeps the old starter's halftone lens and brightness meter ships: \
             {converted_examples:?}"
        );
        assert!(
            converted_examples
                .iter()
                .any(|name| name == "every-pin-kind"),
            "the sample the Studio's recordings are made from, declaring each kind of pin, \
             ships: {converted_examples:?}"
        );
    }

    #[test]
    fn every_converted_example_stages_as_a_sample_that_stands_on_its_own() {
        let samples_directory = tempfile::tempdir().unwrap();
        for example in every_converted_example_in_the_workspace(the_workspace_root()).unwrap() {
            let sample_name = example.file_name().unwrap().to_string_lossy().into_owned();
            let sample_directory = samples_directory.path().join(&sample_name);

            stage_one_sample_project(
                &example,
                &sample_directory,
                "0.70.1",
                WhereASamplesFirstPartyModulesComeFrom::TatolabsIndex,
            )
            .unwrap();

            for file_a_sample_needs in [
                PROJECT_FILE_NAME,
                "streams/main.graph.json",
                "pyproject.toml",
                ".python-version",
            ] {
                assert!(
                    sample_directory.join(file_a_sample_needs).is_file(),
                    "{sample_name} carries no {file_a_sample_needs}"
                );
            }
            let project_file: serde_json::Value = serde_json::from_slice(
                &std::fs::read(sample_directory.join(PROJECT_FILE_NAME)).unwrap(),
            )
            .unwrap();
            assert!(
                project_file["description"]
                    .as_str()
                    .is_some_and(|description| !description.is_empty()),
                "{sample_name}'s project file gives no description to list it by"
            );
            let staged_pyproject: toml::Table =
                std::fs::read_to_string(sample_directory.join("pyproject.toml"))
                    .unwrap()
                    .parse()
                    .unwrap();
            let sources = staged_pyproject["tool"]["uv"].get("sources");
            assert!(
                sources
                    .and_then(toml::Value::as_table)
                    .is_none_or(|sources| sources
                        .values()
                        .all(|source| source.get("path").is_none())),
                "{sample_name} keeps a path source into the workspace"
            );
            for requirement in staged_pyproject["project"]["dependencies"]
                .as_array()
                .unwrap()
            {
                let requirement = requirement.as_str().unwrap();
                if requirement.starts_with(FIRST_PARTY_DISTRIBUTION_NAME_PREFIX) {
                    assert!(
                        requirement.ends_with(">=0.70.1"),
                        "{sample_name} requires {requirement}"
                    );
                }
            }
        }
    }
}

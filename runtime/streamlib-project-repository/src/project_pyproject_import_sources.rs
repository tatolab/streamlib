// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::Path;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_file::ProjectImport;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The Python project file uv reads beside `tatolab-project.json`.
pub const PYPROJECT_FILE_NAME: &str = "pyproject.toml";

/// `pyproject_text` with each import holding a `pyproject.toml` named as a requirement of the
/// project and as a uv path source into its folder, and every source into `imports/` that no
/// import holds now taken out with its requirement. Each source is written
/// `{ path = "imports/<folder>", package = false }`: uv installs the module's requirements and
/// never builds the module, so no import's code reaches the project's shared import path.
pub(crate) fn pyproject_with_each_import_as_a_uv_path_source(
    pyproject_text: &str,
    project_directory: &Path,
    imports: &[ProjectImport],
) -> Result<String, ProjectRepositoryRefusal> {
    let mut pyproject: DocumentMut =
        pyproject_text
            .parse()
            .map_err(|not_toml: toml_edit::TomlError| {
                ProjectRepositoryRefusal::PyprojectUnreadable {
                    path: project_directory.join(PYPROJECT_FILE_NAME),
                    why: not_toml.to_string(),
                }
            })?;
    let mut source_path_of_each_requirement = BTreeMap::new();
    for import in imports {
        let import_folder = project_directory
            .join(PROJECT_IMPORTS_DIRECTORY)
            .join(&import.folder);
        if let Some(distribution_name) = the_distribution_name_the_folder_declares(&import_folder)?
        {
            let source_path = format!("{PROJECT_IMPORTS_DIRECTORY}/{}", import.folder);
            if let Some(first_source_path) =
                source_path_of_each_requirement.insert(distribution_name.clone(), source_path)
            {
                return Err(ProjectRepositoryRefusal::ImportsDeclareOneDistribution {
                    distribution_name,
                    first_folder: first_source_path,
                    second_folder: format!("{PROJECT_IMPORTS_DIRECTORY}/{}", import.folder),
                });
            }
        }
    }

    let uv_sources = the_table_at(
        &mut pyproject,
        &["tool", "uv", "sources"],
        project_directory,
    )?;
    let requirements_no_import_holds: Vec<String> = uv_sources
        .iter()
        .filter(|(name, source)| {
            is_a_source_into_the_imports_folder(source)
                && !source_path_of_each_requirement
                    .contains_key(&normalized_distribution_name(name))
        })
        .map(|(name, _)| name.to_owned())
        .collect();
    let sources_an_import_replaces: Vec<String> = uv_sources
        .iter()
        .map(|(name, _)| name.to_owned())
        .filter(|name| {
            source_path_of_each_requirement.contains_key(&normalized_distribution_name(name))
        })
        .collect();
    for name in requirements_no_import_holds
        .iter()
        .chain(&sources_an_import_replaces)
    {
        uv_sources.remove(name);
    }
    for (name, path) in &source_path_of_each_requirement {
        let mut source = InlineTable::new();
        source.insert("path", Value::from(path.as_str()));
        source.insert("package", Value::from(false));
        uv_sources.insert(name, Item::Value(Value::InlineTable(source)));
    }
    if uv_sources.is_empty() {
        remove_the_empty_tables_down_to(&mut pyproject, &["tool", "uv", "sources"]);
    }

    let requirements = the_project_requirements(&mut pyproject, project_directory)?;
    let names_taken_out: Vec<String> = requirements_no_import_holds
        .iter()
        .map(|name| normalized_distribution_name(name))
        .collect();
    requirements.retain(|requirement| {
        requirement
            .as_str()
            .map(|requirement| {
                let name = normalized_distribution_name(the_name_a_requirement_names(requirement));
                !names_taken_out.contains(&name)
                    && !source_path_of_each_requirement.contains_key(&name)
            })
            .unwrap_or(true)
    });
    for name in source_path_of_each_requirement.keys() {
        requirements.push(name.as_str());
    }
    Ok(pyproject.to_string())
}

/// The distribution name `folder`'s own `pyproject.toml` declares, normalized; `None` when the
/// folder holds no `pyproject.toml` or it names no `[project].name`.
fn the_distribution_name_the_folder_declares(
    folder: &Path,
) -> Result<Option<String>, ProjectRepositoryRefusal> {
    let path = folder.join(PYPROJECT_FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(not_read) => return Err(not_read.into()),
    };
    let pyproject: DocumentMut = text.parse().map_err(|not_toml: toml_edit::TomlError| {
        ProjectRepositoryRefusal::PyprojectUnreadable {
            path: path.clone(),
            why: not_toml.to_string(),
        }
    })?;
    Ok(pyproject
        .get("project")
        .and_then(|project| project.get("name"))
        .and_then(Item::as_str)
        .map(normalized_distribution_name))
}

fn is_a_source_into_the_imports_folder(source: &Item) -> bool {
    source
        .get("path")
        .and_then(Item::as_str)
        .is_some_and(|path| path.starts_with(&format!("{PROJECT_IMPORTS_DIRECTORY}/")))
}

/// The table at `keys`, made where it is missing, each one written as its own `[header]`
/// section, an inline one included. Refused when a key on the way holds something other than a table.
fn the_table_at<'document>(
    pyproject: &'document mut DocumentMut,
    keys: &[&str],
    project_directory: &Path,
) -> Result<&'document mut Table, ProjectRepositoryRefusal> {
    let mut table = pyproject.as_table_mut();
    for (depth, key) in keys.iter().enumerate() {
        let is_the_last_key = depth + 1 == keys.len();
        let entry = table.entry(key).or_insert_with(|| {
            let mut new_table = Table::new();
            new_table.set_implicit(!is_the_last_key);
            Item::Table(new_table)
        });
        if let Some(inline_table) = entry.as_inline_table().cloned() {
            *entry = Item::Table(inline_table.into_table());
        }
        table =
            entry
                .as_table_mut()
                .ok_or_else(|| ProjectRepositoryRefusal::PyprojectUnreadable {
                    path: project_directory.join(PYPROJECT_FILE_NAME),
                    why: format!("its `{}` is not a table", keys[..=depth].join(".")),
                })?;
    }
    Ok(table)
}

fn remove_the_empty_tables_down_to(pyproject: &mut DocumentMut, keys: &[&str]) {
    for depth in (1..=keys.len()).rev() {
        let Some((last, parents)) = keys[..depth].split_last() else {
            return;
        };
        let mut table = pyproject.as_table_mut();
        for key in parents {
            match table.get_mut(key).and_then(Item::as_table_mut) {
                Some(child) => table = child,
                None => return,
            }
        }
        if table
            .get(last)
            .and_then(Item::as_table)
            .is_some_and(Table::is_empty)
        {
            table.remove(last);
        } else {
            return;
        }
    }
}

fn the_project_requirements<'document>(
    pyproject: &'document mut DocumentMut,
    project_directory: &Path,
) -> Result<&'document mut Array, ProjectRepositoryRefusal> {
    let project = the_table_at(pyproject, &["project"], project_directory)?;
    project
        .entry("dependencies")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())))
        .as_array_mut()
        .ok_or_else(|| ProjectRepositoryRefusal::PyprojectUnreadable {
            path: project_directory.join(PYPROJECT_FILE_NAME),
            why: "its `[project].dependencies` is not an array".to_owned(),
        })
}

/// The distribution name at the start of a PEP 508 requirement.
fn the_name_a_requirement_names(requirement: &str) -> &str {
    let requirement = requirement.trim_start();
    let end = requirement
        .find(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        })
        .unwrap_or(requirement.len());
    &requirement[..end]
}

/// `name` as PEP 503 compares distribution names: lowercase, each run of `-`, `_` and `.` one `-`.
fn normalized_distribution_name(name: &str) -> String {
    let mut normalized = String::new();
    for character in name.trim().chars() {
        if matches!(character, '-' | '_' | '.') {
            if !normalized.ends_with('-') {
                normalized.push('-');
            }
        } else {
            normalized.push(character.to_ascii_lowercase());
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_import_named(project_directory: &Path, folder: &str, name: &str) -> ProjectImport {
        let import_folder = project_directory.join("imports").join(folder);
        std::fs::create_dir_all(&import_folder).unwrap();
        std::fs::write(
            import_folder.join(PYPROJECT_FILE_NAME),
            format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        ProjectImport {
            project: uuid::Uuid::new_v4(),
            folder: folder.to_owned(),
            source: "/elsewhere".to_owned(),
            version: "0".repeat(64),
            copied_files: BTreeMap::new(),
        }
    }

    const SCAFFOLDED_PYPROJECT: &str = "[project]\nname = \"cell\"\nversion = \"0.1.0\"\n\
        dependencies = [\"tatolab-stream>=0.68\", \"numpy>=2.1\"]\n\n\
        [[tool.uv.index]]\nname = \"tatolab\"\nurl = \"https://get.tatolab.com/simple/\"\n";

    #[test]
    fn an_import_becomes_a_requirement_and_a_path_source_that_never_builds_it() {
        let project = tempfile::tempdir().unwrap();
        let arm = an_import_named(project.path(), "robot-arm", "Robot_Arm");

        let written = pyproject_with_each_import_as_a_uv_path_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            &[arm],
        )
        .unwrap();

        let pyproject: toml::Value = toml::from_str(&written).unwrap();
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-stream>=0.68", "numpy>=2.1", "robot-arm"]).unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["sources"]["robot-arm"],
            toml::from_str::<toml::Value>("path = \"imports/robot-arm\"\npackage = false").unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["index"][0]["name"].as_str(),
            Some("tatolab")
        );
        assert!(written.contains("[tool.uv.sources]"), "{written}");
    }

    #[test]
    fn a_removed_import_takes_its_requirement_and_source_out_and_leaves_the_rest() {
        let project = tempfile::tempdir().unwrap();
        let arm = an_import_named(project.path(), "robot-arm", "robot-arm");
        let with_the_arm = pyproject_with_each_import_as_a_uv_path_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            &[arm],
        )
        .unwrap();

        let without =
            pyproject_with_each_import_as_a_uv_path_source(&with_the_arm, project.path(), &[])
                .unwrap();

        let pyproject: toml::Value = toml::from_str(&without).unwrap();
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-stream>=0.68", "numpy>=2.1"]).unwrap()
        );
        assert!(
            pyproject["tool"]["uv"].get("sources").is_none(),
            "{without}"
        );
        assert_eq!(
            pyproject["tool"]["uv"]["index"][0]["name"].as_str(),
            Some("tatolab")
        );
    }

    #[test]
    fn an_import_holding_no_pyproject_is_no_requirement() {
        let project = tempfile::tempdir().unwrap();
        let import = ProjectImport {
            project: uuid::Uuid::new_v4(),
            folder: "media-only".to_owned(),
            source: "/elsewhere".to_owned(),
            version: "0".repeat(64),
            copied_files: BTreeMap::new(),
        };

        let written = pyproject_with_each_import_as_a_uv_path_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            &[import],
        )
        .unwrap();

        assert_eq!(written, SCAFFOLDED_PYPROJECT);
    }

    #[test]
    fn two_imports_declaring_one_distribution_are_refused() {
        let project = tempfile::tempdir().unwrap();
        let arm = an_import_named(project.path(), "robot-arm", "robot-arm");
        let other_arm = an_import_named(project.path(), "other-arm", "Robot_Arm");

        let refusal = pyproject_with_each_import_as_a_uv_path_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            &[arm, other_arm],
        )
        .unwrap_err();

        assert!(
            matches!(
                refusal,
                ProjectRepositoryRefusal::ImportsDeclareOneDistribution { .. }
            ),
            "{refusal}"
        );
    }

    #[test]
    fn an_inline_sources_table_and_a_source_spelled_another_way_are_rewritten_once() {
        let project = tempfile::tempdir().unwrap();
        let arm = an_import_named(project.path(), "robot-arm", "robot-arm");
        let pyproject = "[project]\nname = \"cell\"\ndependencies = [\"Robot_Arm\"]\n\n\
            [tool.uv]\nsources = { Robot_Arm = { path = \"elsewhere\" }, torch = { index = \"pytorch\" } }\n";

        let written =
            pyproject_with_each_import_as_a_uv_path_source(pyproject, project.path(), &[arm])
                .unwrap();

        let pyproject: toml::Value = toml::from_str(&written).unwrap();
        let sources = pyproject["tool"]["uv"]["sources"].as_table().unwrap();
        assert_eq!(
            sources.keys().collect::<Vec<_>>(),
            ["robot-arm", "torch"],
            "{written}"
        );
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["robot-arm"]).unwrap()
        );
    }
}

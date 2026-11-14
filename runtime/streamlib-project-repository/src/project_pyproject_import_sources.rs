// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::path::Path;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_file::ProjectImport;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The Python project file uv reads beside `tatolab-project.json`.
pub const PYPROJECT_FILE_NAME: &str = "pyproject.toml";

/// `pyproject_text` with each import holding a `pyproject.toml` named as a requirement of the
/// project, unless one already names it, and as a uv path source into its folder, and every
/// source into `imports/` that no import holds now taken out with its requirement. Each source
/// is written `{ path = "imports/<folder>", package = false }`: uv installs the module's
/// requirements and never builds the module, so no import's code reaches the project's shared
/// import path. A manifest with no `[project]` table has no requirements to name an import in,
/// so it is left as it is.
pub fn pyproject_with_each_import_as_a_uv_path_source(
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
    if !pyproject.contains_table("project") {
        return Ok(pyproject_text.to_owned());
    }
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
                !names_taken_out.contains(&normalized_distribution_name(
                    the_name_a_requirement_names(requirement),
                ))
            })
            .unwrap_or(true)
    });
    let names_already_required: Vec<String> = requirements
        .iter()
        .filter_map(|requirement| requirement.as_str())
        .map(|requirement| normalized_distribution_name(the_name_a_requirement_names(requirement)))
        .collect();
    for name in source_path_of_each_requirement.keys() {
        if !names_already_required.contains(name) {
            requirements.push(name.as_str());
        }
    }
    Ok(pyproject.to_string())
}

/// Where a project links each first-party module the runtime carries, relative to the project.
pub const PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY: &str = ".tatolab/first-party-imports";

/// A first-party module the runtime carries, as a project declares it: the distribution its
/// wheel installs and the wheel file name its link has under
/// [`PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstPartyModuleDeclaredByAProject {
    /// The distribution name the module's wheel installs.
    pub distribution_name: String,
    /// The module's wheel file name, which its link inside
    /// [`PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY`] keeps, since uv reads a wheel's tags from it.
    pub link_file_name: String,
}

/// `pyproject_text` with each of `first_party_modules` named as a requirement and as a uv path
/// source through its link, `{ path = ".tatolab/first-party-imports/<wheel file name>" }`,
/// which uv installs so `uv sync` keeps the module and its requirements. A requirement, or a
/// source outside the links folder, the manifest already holds for one is kept as written; every
/// source into the links folder is written again to the module's link now, since a wheel's file
/// name changes with its version, and one no module of `first_party_modules` holds now is taken
/// out with its requirement. A manifest with no `[project]` table is left as it is.
pub fn pyproject_with_each_first_party_module_as_a_uv_path_source(
    pyproject_text: &str,
    project_directory: &Path,
    first_party_modules: &[FirstPartyModuleDeclaredByAProject],
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
    if !pyproject.contains_table("project") {
        return Ok(pyproject_text.to_owned());
    }
    let declared_names: Vec<String> = first_party_modules
        .iter()
        .map(|module| normalized_distribution_name(&module.distribution_name))
        .collect();

    let uv_sources = the_table_at(
        &mut pyproject,
        &["tool", "uv", "sources"],
        project_directory,
    )?;
    let requirements_no_module_holds: Vec<String> = uv_sources
        .iter()
        .filter(|(name, source)| {
            is_a_source_into_the_first_party_module_links_folder(source)
                && !declared_names.contains(&normalized_distribution_name(name))
        })
        .map(|(name, _)| name.to_owned())
        .collect();
    let sources_into_the_links_folder: Vec<String> = uv_sources
        .iter()
        .filter(|(_, source)| is_a_source_into_the_first_party_module_links_folder(source))
        .map(|(name, _)| name.to_owned())
        .collect();
    for name in &sources_into_the_links_folder {
        uv_sources.remove(name);
    }
    let names_with_a_source: Vec<String> = uv_sources
        .iter()
        .map(|(name, _)| normalized_distribution_name(name))
        .collect();
    for module in first_party_modules {
        if names_with_a_source.contains(&normalized_distribution_name(&module.distribution_name)) {
            continue;
        }
        let mut source = InlineTable::new();
        source.insert(
            "path",
            Value::from(format!(
                "{PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY}/{}",
                module.link_file_name
            )),
        );
        uv_sources.insert(
            &module.distribution_name,
            Item::Value(Value::InlineTable(source)),
        );
    }
    if uv_sources.is_empty() {
        remove_the_empty_tables_down_to(&mut pyproject, &["tool", "uv", "sources"]);
    }

    let requirements = the_project_requirements(&mut pyproject, project_directory)?;
    let names_taken_out: Vec<String> = requirements_no_module_holds
        .iter()
        .map(|name| normalized_distribution_name(name))
        .collect();
    requirements.retain(|requirement| {
        requirement
            .as_str()
            .map(|requirement| {
                !names_taken_out.contains(&normalized_distribution_name(
                    the_name_a_requirement_names(requirement),
                ))
            })
            .unwrap_or(true)
    });
    let names_already_required: Vec<String> = requirements
        .iter()
        .filter_map(|requirement| requirement.as_str())
        .map(|requirement| normalized_distribution_name(the_name_a_requirement_names(requirement)))
        .collect();
    for module in first_party_modules {
        if !names_already_required
            .contains(&normalized_distribution_name(&module.distribution_name))
        {
            requirements.push(module.distribution_name.as_str());
        }
    }
    Ok(pyproject.to_string())
}

/// Each first-party module link the `pyproject.toml` in `pyproject_text` names as a uv path source,
/// `{ path = ".tatolab/first-party-imports/<wheel file name>" }`: the source's distribution and the
/// wheel file name its link has. A source that names a path below the links folder rather than one
/// file in it is left out, as is every source when the manifest names none.
pub fn first_party_module_links_a_pyproject_names(
    pyproject_text: &str,
    project_directory: &Path,
) -> Result<Vec<FirstPartyModuleDeclaredByAProject>, ProjectRepositoryRefusal> {
    let pyproject: DocumentMut =
        pyproject_text
            .parse()
            .map_err(|not_toml: toml_edit::TomlError| {
                ProjectRepositoryRefusal::PyprojectUnreadable {
                    path: project_directory.join(PYPROJECT_FILE_NAME),
                    why: not_toml.to_string(),
                }
            })?;
    let Some(uv_sources) = pyproject
        .get("tool")
        .and_then(|tool| tool.get("uv"))
        .and_then(|uv| uv.get("sources"))
        .and_then(Item::as_table_like)
    else {
        return Ok(Vec::new());
    };
    let links_folder_prefix = format!("{PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY}/");
    Ok(uv_sources
        .iter()
        .filter_map(|(distribution_name, source)| {
            let link_file_name = source
                .get("path")
                .and_then(Item::as_str)?
                .strip_prefix(&links_folder_prefix)?;
            let is_one_file_in_the_links_folder = !link_file_name.is_empty()
                && !link_file_name.contains(['/', '\\'])
                && link_file_name != ".."
                && link_file_name != ".";
            is_one_file_in_the_links_folder.then(|| FirstPartyModuleDeclaredByAProject {
                distribution_name: distribution_name.to_owned(),
                link_file_name: link_file_name.to_owned(),
            })
        })
        .collect())
}

/// Whether `first` and `second` name one distribution, as PEP 503 compares distribution names.
pub fn distribution_names_are_one(first: &str, second: &str) -> bool {
    normalized_distribution_name(first) == normalized_distribution_name(second)
}

/// Where a project links the third-party wheels the runtime carries, relative to the project.
pub const PROJECT_CARRIED_WHEELS_LINK: &str = ".tatolab/carried-wheels";

/// `pyproject_text` with [`PROJECT_CARRIED_WHEELS_LINK`] named once in `[tool.uv].find-links`
/// when `the_runtime_carries_wheels`, so any `uv sync` of the project, the runtime's or a
/// person's, finds the wheels the runtime carries beside its indexes, and with it taken out
/// otherwise. Every other find-links entry is kept as written, and a manifest with no
/// `[project]` table is left as it is.
pub fn pyproject_with_the_carried_wheels_as_a_uv_find_links_source(
    pyproject_text: &str,
    project_directory: &Path,
    the_runtime_carries_wheels: bool,
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
    if !pyproject.contains_table("project") {
        return Ok(pyproject_text.to_owned());
    }
    let names_the_link = |find_links: &Array| {
        find_links
            .iter()
            .any(|entry| entry.as_str() == Some(PROJECT_CARRIED_WHEELS_LINK))
    };
    let already_named = pyproject
        .get("tool")
        .and_then(|tool| tool.get("uv"))
        .and_then(|uv| uv.get("find-links"))
        .and_then(Item::as_array)
        .is_some_and(names_the_link);
    if already_named == the_runtime_carries_wheels {
        return Ok(pyproject_text.to_owned());
    }
    let uv = the_table_at(&mut pyproject, &["tool", "uv"], project_directory)?;
    let find_links = uv
        .entry("find-links")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())))
        .as_array_mut()
        .ok_or_else(|| ProjectRepositoryRefusal::PyprojectUnreadable {
            path: project_directory.join(PYPROJECT_FILE_NAME),
            why: "its `[tool.uv].find-links` is not an array".to_owned(),
        })?;
    if the_runtime_carries_wheels {
        find_links.push(PROJECT_CARRIED_WHEELS_LINK);
    } else {
        find_links.retain(|entry| entry.as_str() != Some(PROJECT_CARRIED_WHEELS_LINK));
        if find_links.is_empty() {
            uv.remove("find-links");
        }
        remove_the_empty_tables_down_to(&mut pyproject, &["tool", "uv"]);
    }
    Ok(pyproject.to_string())
}

fn is_a_source_into_the_first_party_module_links_folder(source: &Item) -> bool {
    source
        .get("path")
        .and_then(Item::as_str)
        .is_some_and(|path| {
            path.starts_with(&format!("{PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY}/"))
        })
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

#[cfg(all(test, feature = "history"))]
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
            toml::Value::try_from(["Robot_Arm"]).unwrap()
        );
    }

    #[test]
    fn a_requirement_already_naming_an_import_is_kept_as_written() {
        let project = tempfile::tempdir().unwrap();
        let arm = an_import_named(project.path(), "robot-arm", "robot-arm");
        let pyproject = "[project]\nname = \"cell\"\ndependencies = [\"robot-arm[vision]>=2\"]\n";

        let written =
            pyproject_with_each_import_as_a_uv_path_source(pyproject, project.path(), &[arm])
                .unwrap();

        let pyproject: toml::Value = toml::from_str(&written).unwrap();
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["robot-arm[vision]>=2"]).unwrap()
        );
    }

    #[test]
    fn a_manifest_with_no_project_table_is_left_as_it_is() {
        let project = tempfile::tempdir().unwrap();
        let arm = an_import_named(project.path(), "robot-arm", "robot-arm");
        let pyproject = "[dependency-groups]\ndev = [\"pytest\"]\n";

        let written =
            pyproject_with_each_import_as_a_uv_path_source(pyproject, project.path(), &[arm])
                .unwrap();

        assert_eq!(written, pyproject);
    }

    fn tatolab_compositor_as_a_first_party_module() -> FirstPartyModuleDeclaredByAProject {
        FirstPartyModuleDeclaredByAProject {
            distribution_name: "tatolab-compositor".to_owned(),
            link_file_name: "tatolab_compositor-0.68.51-py3-none-any.whl".to_owned(),
        }
    }

    #[test]
    fn a_first_party_module_becomes_a_requirement_and_a_path_source_through_its_link() {
        let project = tempfile::tempdir().unwrap();

        let written = pyproject_with_each_first_party_module_as_a_uv_path_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            &[tatolab_compositor_as_a_first_party_module()],
        )
        .unwrap();

        let pyproject: toml::Value = toml::from_str(&written).unwrap();
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-stream>=0.68", "numpy>=2.1", "tatolab-compositor"])
                .unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["sources"]["tatolab-compositor"],
            toml::from_str::<toml::Value>(
                "path = \".tatolab/first-party-imports/tatolab_compositor-0.68.51-py3-none-any.whl\""
            )
            .unwrap()
        );
        assert_eq!(
            pyproject_with_each_first_party_module_as_a_uv_path_source(
                &written,
                project.path(),
                &[tatolab_compositor_as_a_first_party_module()],
            )
            .unwrap(),
            written,
            "a project that declares it already is left as it is"
        );
    }

    #[test]
    fn the_stream_package_the_runtime_carries_keeps_the_scaffolds_floor_and_gains_a_link_source() {
        let project = tempfile::tempdir().unwrap();
        let tatolab_stream_as_a_first_party_package = FirstPartyModuleDeclaredByAProject {
            distribution_name: "tatolab-stream".to_owned(),
            link_file_name: "tatolab_stream-0.68.56-py3-none-any.whl".to_owned(),
        };

        let written = pyproject_with_each_first_party_module_as_a_uv_path_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            &[
                tatolab_stream_as_a_first_party_package,
                tatolab_compositor_as_a_first_party_module(),
            ],
        )
        .unwrap();

        let pyproject: toml::Value = toml::from_str(&written).unwrap();
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-stream>=0.68", "numpy>=2.1", "tatolab-compositor"])
                .unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["sources"]["tatolab-stream"]["path"].as_str(),
            Some(".tatolab/first-party-imports/tatolab_stream-0.68.56-py3-none-any.whl")
        );
    }

    #[test]
    fn the_carried_wheels_link_is_named_once_as_find_links_and_taken_out_with_the_wheels() {
        let project = tempfile::tempdir().unwrap();
        let with_a_find_links_of_its_own =
            format!("{SCAFFOLDED_PYPROJECT}\n[tool.uv]\nfind-links = [\"../wheels\"]\n");

        let declared = pyproject_with_the_carried_wheels_as_a_uv_find_links_source(
            &with_a_find_links_of_its_own,
            project.path(),
            true,
        )
        .unwrap();
        let pyproject: toml::Value = toml::from_str(&declared).unwrap();
        assert_eq!(
            pyproject["tool"]["uv"]["find-links"],
            toml::Value::try_from(["../wheels", ".tatolab/carried-wheels"]).unwrap()
        );
        assert_eq!(
            pyproject_with_the_carried_wheels_as_a_uv_find_links_source(
                &declared,
                project.path(),
                true
            )
            .unwrap(),
            declared,
            "a project that names it already is left as it is"
        );

        let taken_out = pyproject_with_the_carried_wheels_as_a_uv_find_links_source(
            &declared,
            project.path(),
            false,
        )
        .unwrap();
        let pyproject: toml::Value = toml::from_str(&taken_out).unwrap();
        assert_eq!(
            pyproject["tool"]["uv"]["find-links"],
            toml::Value::try_from(["../wheels"]).unwrap()
        );
    }

    #[test]
    fn a_scaffolded_project_gains_a_find_links_table_and_loses_it_with_the_wheels() {
        let project = tempfile::tempdir().unwrap();
        let declared = pyproject_with_the_carried_wheels_as_a_uv_find_links_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            true,
        )
        .unwrap();
        let pyproject: toml::Value = toml::from_str(&declared).unwrap();
        assert_eq!(
            pyproject["tool"]["uv"]["find-links"],
            toml::Value::try_from([".tatolab/carried-wheels"]).unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["index"][0]["name"].as_str(),
            Some("tatolab")
        );

        let taken_out = pyproject_with_the_carried_wheels_as_a_uv_find_links_source(
            &declared,
            project.path(),
            false,
        )
        .unwrap();
        let pyproject: toml::Value = toml::from_str(&taken_out).unwrap();
        assert!(
            pyproject["tool"]["uv"].get("find-links").is_none(),
            "{taken_out}"
        );
        assert_eq!(
            pyproject["tool"]["uv"]["index"][0]["name"].as_str(),
            Some("tatolab")
        );
    }

    #[test]
    fn a_link_source_to_an_earlier_wheel_is_written_again_to_the_wheel_carried_now() {
        let project = tempfile::tempdir().unwrap();
        let pyproject = "[project]\nname = \"suite\"\ndependencies = [\"tatolab-compositor\"]\n\n\
            [tool.uv.sources]\n\
            tatolab-compositor = { path = \".tatolab/first-party-imports/tatolab-compositor\" }\n";

        let written = pyproject_with_each_first_party_module_as_a_uv_path_source(
            pyproject,
            project.path(),
            &[tatolab_compositor_as_a_first_party_module()],
        )
        .unwrap();

        let pyproject: toml::Value = toml::from_str(&written).unwrap();
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-compositor"]).unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["sources"]["tatolab-compositor"]["path"].as_str(),
            Some(".tatolab/first-party-imports/tatolab_compositor-0.68.51-py3-none-any.whl")
        );
    }

    #[test]
    fn a_source_the_project_holds_for_a_first_party_module_is_kept_as_written() {
        let project = tempfile::tempdir().unwrap();
        let pyproject = "[project]\nname = \"suite\"\ndependencies = [\"tatolab-compositor\"]\n\n\
            [tool.uv.sources]\ntatolab-compositor = { path = \"../../packages/tatolab-compositor\" }\n";

        let written = pyproject_with_each_first_party_module_as_a_uv_path_source(
            pyproject,
            project.path(),
            &[tatolab_compositor_as_a_first_party_module()],
        )
        .unwrap();

        assert_eq!(written, pyproject);
    }

    #[test]
    fn a_first_party_module_the_runtime_no_longer_carries_is_taken_out_and_imports_are_left() {
        let project = tempfile::tempdir().unwrap();
        let arm = an_import_named(project.path(), "robot-arm", "robot-arm");
        let with_the_arm = pyproject_with_each_import_as_a_uv_path_source(
            SCAFFOLDED_PYPROJECT,
            project.path(),
            &[arm],
        )
        .unwrap();
        let with_both = pyproject_with_each_first_party_module_as_a_uv_path_source(
            &with_the_arm,
            project.path(),
            &[tatolab_compositor_as_a_first_party_module()],
        )
        .unwrap();

        let without_ops = pyproject_with_each_first_party_module_as_a_uv_path_source(
            &with_both,
            project.path(),
            &[],
        )
        .unwrap();

        let pyproject: toml::Value = toml::from_str(&without_ops).unwrap();
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-stream>=0.68", "numpy>=2.1", "robot-arm"]).unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["sources"]
                .as_table()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["robot-arm"]
        );
    }

    #[test]
    fn the_first_party_module_links_a_pyproject_names_are_each_one_file_in_the_links_folder() {
        let named = first_party_module_links_a_pyproject_names(
            "[project]\nname = \"lib\"\n\n[tool.uv.sources]\n\
             tatolab-stream = { path = \".tatolab/first-party-imports/tatolab_stream-0.68.59-py3-none-any.whl\" }\n\
             tatolab-compositor = { path = \".tatolab/first-party-imports/tatolab-compositor/inner\" }\n\
             robot-arm = { path = \"imports/robot-arm\", package = false }\n",
            Path::new("/lib"),
        )
        .unwrap();

        assert_eq!(
            named,
            [FirstPartyModuleDeclaredByAProject {
                distribution_name: "tatolab-stream".to_owned(),
                link_file_name: "tatolab_stream-0.68.59-py3-none-any.whl".to_owned(),
            }]
        );
        assert!(
            first_party_module_links_a_pyproject_names(
                "[project]\nname = \"lib\"\n",
                Path::new("/lib")
            )
            .unwrap()
            .is_empty()
        );
        assert!(distribution_names_are_one(
            "tatolab_stream",
            "Tatolab-Stream"
        ));
    }
}

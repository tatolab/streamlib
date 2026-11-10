// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The files a new stream project starts from, rendered from the templates the
//! SDK carries — its project file and one stream asset's graph document among them — and
//! their write into the project's directory.

use std::fs;
use std::io::Write;
use std::path::Path;

use streamlib_project_repository::{PROJECT_FILE_NAME, THIS_RUNTIME_VERSION, TatolabProjectFile};
use uuid::Uuid;

use super::StreamLocation;
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::{Error, Result};

/// The name of the one stream asset a new project starts with.
pub(crate) const NEW_PROJECT_SCAFFOLD_STREAM_NAME: &str = "main";

/// Where a new project's one stream asset's graph document sits in it.
const NEW_PROJECT_SCAFFOLD_STREAM_DOCUMENT_PATH: &str = "streams/main.graph.json";

/// The graph a new project's stream asset starts as, its nodes and links named by node name.
const MAIN_GRAPH_DOCUMENT_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/streams/main.graph.json");
const PYPROJECT_TOML_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/pyproject.toml");
const PYTHON_VERSION_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/python-version");
const GITIGNORE_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/gitignore");

const PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER: &str = "name = \"tatolab-stream-project\"";

/// The template's `tatolab-stream` requirement, raised on render to this runtime's own version:
/// a project depending on an older `tatolab-stream` is refused by the runtime that created it.
const PYPROJECT_TOML_TATOLAB_STREAM_REQUIREMENT_PLACEHOLDER: &str = "\"tatolab-stream>=0.54.2\"";

/// The template's `tatolab-testpattern` requirement, raised on render to this runtime's own
/// version, which the module is released at.
const PYPROJECT_TOML_TATOLAB_TESTPATTERN_REQUIREMENT_PLACEHOLDER: &str =
    "\"tatolab-testpattern>=0.68.18\"";

/// The distribution name a directory whose name casts to nothing is given.
const FALLBACK_DISTRIBUTION_NAME: &str = "tatolab-stream-project";

/// One file a new project starts with: its path inside the project and its rendered contents.
pub(crate) struct ScaffoldedProjectFile {
    /// The path relative to the project directory.
    pub(crate) path_in_project: &'static str,
    /// The file's contents.
    pub(crate) rendered_contents: String,
}

/// A new project as the scaffold starts it: its name, its project's UUID and its one stream
/// asset's, each drawn fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewProjectScaffold {
    /// The project's name: the folder it is created in.
    pub(crate) project_name: String,
    /// The project's UUID.
    pub(crate) project: Uuid,
    /// Its one stream asset's UUID.
    pub(crate) stream_asset: Uuid,
}

impl NewProjectScaffold {
    /// A new project in `project_directory`, named after its folder, with fresh UUIDs.
    pub(crate) fn of_a_new_project_in(project_directory: &Path) -> Self {
        Self {
            project_name: project_directory
                .file_name()
                .map(|folder_name| folder_name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            project: Uuid::new_v4(),
            stream_asset: Uuid::new_v4(),
        }
    }

    /// Where its one stream asset lives once it is written into `project_directory`.
    pub(crate) fn stream_location_in(&self, project_directory: &Path) -> StreamLocation {
        StreamLocation {
            project_directory: project_directory.to_path_buf(),
            project: self.project,
            stream_asset: self.stream_asset,
        }
    }

    /// The id its one stream asset is first recorded under.
    pub(crate) fn stream_id_in(&self, project_directory: &Path) -> String {
        self.stream_location_in(project_directory)
            .stream_id_named(&self.project_name, NEW_PROJECT_SCAFFOLD_STREAM_NAME)
    }

    /// Its `tatolab-project.json`, canonical.
    fn project_file_contents(&self) -> Result<String> {
        let project_file = TatolabProjectFile {
            project: self.project,
            name: self.project_name.clone(),
            description: None,
            cover: None,
            created_with_runtime: Some(THIS_RUNTIME_VERSION.to_owned()),
            dependencies: Vec::new(),
        };
        let bytes = project_file.canonical_bytes().map_err(|refusal| {
            Error::Runtime(format!("the project file was not written: {refusal}"))
        })?;
        String::from_utf8(bytes).map_err(|not_utf8| {
            Error::Runtime(format!("the project file is not UTF-8: {not_utf8}"))
        })
    }

    /// Its stream asset's graph document: the template's graph, holding the stream asset's
    /// UUID, the project's name to show it under, and a fresh UUID on each node.
    fn main_graph_document_contents(&self) -> Result<String> {
        let mut graph = GraphSnapshot::from_json_str(MAIN_GRAPH_DOCUMENT_TEMPLATE)?
            .with_a_uuid_minted_for_each_node_holding_none();
        graph.asset = Some(self.stream_asset);
        graph.display_name = Some(self.project_name.clone());
        String::from_utf8(graph.graph_document_bytes()?).map_err(|not_utf8| {
            Error::Runtime(format!("the graph document is not UTF-8: {not_utf8}"))
        })
    }
}

/// Every file `scaffold` starts with, in write order, rendered from the embedded templates.
pub(crate) fn render_scaffold_template_files(
    distribution_name: &str,
    scaffold: &NewProjectScaffold,
) -> Result<Vec<ScaffoldedProjectFile>> {
    let rendered_pyproject_toml = PYPROJECT_TOML_TEMPLATE
        .replace(
            PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER,
            &format!("name = \"{distribution_name}\""),
        )
        .replace(
            PYPROJECT_TOML_TATOLAB_STREAM_REQUIREMENT_PLACEHOLDER,
            &format!("\"tatolab-stream>={}\"", env!("CARGO_PKG_VERSION")),
        )
        .replace(
            PYPROJECT_TOML_TATOLAB_TESTPATTERN_REQUIREMENT_PLACEHOLDER,
            &format!("\"tatolab-testpattern>={}\"", env!("CARGO_PKG_VERSION")),
        );

    Ok(vec![
        ScaffoldedProjectFile {
            path_in_project: PROJECT_FILE_NAME,
            rendered_contents: scaffold.project_file_contents()?,
        },
        ScaffoldedProjectFile {
            path_in_project: NEW_PROJECT_SCAFFOLD_STREAM_DOCUMENT_PATH,
            rendered_contents: scaffold.main_graph_document_contents()?,
        },
        ScaffoldedProjectFile {
            path_in_project: "pyproject.toml",
            rendered_contents: rendered_pyproject_toml,
        },
        ScaffoldedProjectFile {
            path_in_project: ".python-version",
            rendered_contents: PYTHON_VERSION_TEMPLATE.to_owned(),
        },
        ScaffoldedProjectFile {
            path_in_project: ".gitignore",
            rendered_contents: GITIGNORE_TEMPLATE.to_owned(),
        },
    ])
}

/// A PEP 503 name for the scaffolded project, from its directory name.
pub(crate) fn python_distribution_name_for(directory_name: &str) -> String {
    let mut normalized = String::with_capacity(directory_name.len());
    let mut previous_character_was_replaced = false;
    for character in directory_name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            normalized.push(character);
            previous_character_was_replaced = false;
        } else if !previous_character_was_replaced {
            normalized.push('-');
            previous_character_was_replaced = true;
        }
    }
    let trimmed = normalized.trim_matches(|character| matches!(character, '-' | '.'));
    if trimmed.is_empty() {
        FALLBACK_DISTRIBUTION_NAME.to_owned()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Write the files `scaffold` starts from into `project_directory`, created when absent, its
/// distribution named after the folder. Refused, with nothing written, when any of those files
/// is already there.
pub(crate) fn write_the_new_project_scaffold_into(
    project_directory: &Path,
    scaffold: &NewProjectScaffold,
) -> Result<()> {
    let scaffolded_files = render_scaffold_template_files(
        &python_distribution_name_for(&scaffold.project_name),
        scaffold,
    )?;

    // Checked before anything is written: a half-scaffolded directory is worse than a refusal.
    let mut already_present: Vec<&str> = scaffolded_files
        .iter()
        .map(|scaffolded_file| scaffolded_file.path_in_project)
        .filter(|path_in_project| project_directory.join(path_in_project).exists())
        .collect();
    already_present.sort_unstable();
    if !already_present.is_empty() {
        return Err(Error::Configuration(format!(
            "`{}` already has {}, and a new project's files would overwrite it; create the \
             project in an empty directory",
            project_directory.display(),
            already_present.join(", ")
        )));
    }

    for scaffolded_file in &scaffolded_files {
        let scaffolded_file_path = project_directory.join(scaffolded_file.path_in_project);
        if let Some(parent_directory) = scaffolded_file_path.parent() {
            fs::create_dir_all(parent_directory).map_err(|io_failure| {
                Error::Runtime(format!(
                    "cannot create `{}`: {io_failure}",
                    parent_directory.display()
                ))
            })?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&scaffolded_file_path)
            .and_then(|mut scaffolded_file_written| {
                scaffolded_file_written.write_all(scaffolded_file.rendered_contents.as_bytes())
            })
            .map_err(|io_failure| {
                Error::Runtime(format!(
                    "cannot write `{}`, so the project is left partly written: {io_failure}",
                    scaffolded_file_path.display()
                ))
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_name_matches_the_pep_503_normalization_new_has_always_used() {
        assert_eq!(python_distribution_name_for("My Cool App"), "my-cool-app");
        assert_eq!(
            python_distribution_name_for("..weird__Name!!"),
            "weird__name"
        );
        assert_eq!(python_distribution_name_for("a  b/c"), "a-b-c");
        assert_eq!(
            python_distribution_name_for("-.-"),
            FALLBACK_DISTRIBUTION_NAME
        );
        assert_eq!(python_distribution_name_for(""), FALLBACK_DISTRIBUTION_NAME);
        assert_eq!(python_distribution_name_for("café"), "caf");
    }

    #[test]
    fn the_scaffold_template_directory_holds_no_python() {
        let scaffold_template_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sdk/tatolab-stream/scaffold_template");
        let mut pending_directories = vec![scaffold_template_directory];
        while let Some(walked_directory) = pending_directories.pop() {
            for directory_entry in fs::read_dir(&walked_directory).unwrap() {
                let entry_path = directory_entry.unwrap().path();
                if entry_path.is_dir() {
                    pending_directories.push(entry_path);
                } else {
                    assert!(
                        entry_path
                            .extension()
                            .is_none_or(|extension| extension != "py"),
                        "{} is node code, and a new project holds none",
                        entry_path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn the_distribution_name_placeholder_is_present_in_its_template() {
        assert!(PYPROJECT_TOML_TEMPLATE.contains(PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER));
    }

    #[test]
    fn the_tatolab_stream_requirement_placeholder_is_present_in_its_template() {
        assert!(
            PYPROJECT_TOML_TEMPLATE.contains(PYPROJECT_TOML_TATOLAB_STREAM_REQUIREMENT_PLACEHOLDER)
        );
    }

    fn a_scaffold_named(project_name: &str) -> NewProjectScaffold {
        NewProjectScaffold {
            project_name: project_name.to_string(),
            project: Uuid::new_v4(),
            stream_asset: Uuid::new_v4(),
        }
    }

    fn the_rendered_file_at<'rendered>(
        rendered_files: &'rendered [ScaffoldedProjectFile],
        path_in_project: &str,
    ) -> &'rendered str {
        &rendered_files
            .iter()
            .find(|rendered_file| rendered_file.path_in_project == path_in_project)
            .unwrap_or_else(|| panic!("the scaffold writes no {path_in_project}"))
            .rendered_contents
    }

    #[test]
    fn every_file_is_rendered_without_the_licence_header_and_names_its_distribution() {
        let rendered_files =
            render_scaffold_template_files("probe-app", &a_scaffold_named("probe-app")).unwrap();
        for rendered_file in &rendered_files {
            assert!(
                !rendered_file
                    .rendered_contents
                    .contains("SPDX-License-Identifier"),
                "{} kept the licence header",
                rendered_file.path_in_project
            );
            assert!(
                !rendered_file.rendered_contents.contains("StreamLib")
                    && !rendered_file.rendered_contents.contains("streamlib"),
                "{} names the product's old name",
                rendered_file.path_in_project
            );
        }
        let rendered_pyproject_toml = the_rendered_file_at(&rendered_files, "pyproject.toml");
        assert!(
            rendered_pyproject_toml.contains(&format!(
                "dependencies = [\"tatolab-stream>={runtime_version}\", \
                 \"tatolab-testpattern>={runtime_version}\"]",
                runtime_version = env!("CARGO_PKG_VERSION")
            )),
            "a new project floors tatolab-stream and tatolab-testpattern at the version of \
             the runtime that created it; tatolab-ops is declared by the runtime when it makes \
             the project's .venv, through a link to the copy it carries"
        );
        assert!(rendered_pyproject_toml.contains("name = \"probe-app\""));
        let rendered_pyproject: toml::Table = toml::from_str(rendered_pyproject_toml).unwrap();
        let named_indexes = rendered_pyproject["tool"]["uv"]["index"]
            .as_array()
            .unwrap();
        assert_eq!(named_indexes.len(), 1);
        assert_eq!(named_indexes[0]["name"].as_str(), Some("tatolab"));
        assert_eq!(
            named_indexes[0]["url"].as_str(),
            Some("https://get.tatolab.com/simple/")
        );
        assert_eq!(
            named_indexes[0]
                .get("explicit")
                .and_then(toml::Value::as_bool),
            None,
            "a non-explicit index is what lets a transitive first-party module resolve from it"
        );
    }

    #[test]
    fn the_starter_stream_is_a_graph_document_holding_one_test_pattern_source() {
        let scaffold = a_scaffold_named("probe-app");
        let rendered_files = render_scaffold_template_files("probe-app", &scaffold).unwrap();

        let project_file: TatolabProjectFile =
            serde_json::from_str(the_rendered_file_at(&rendered_files, PROJECT_FILE_NAME)).unwrap();
        assert_eq!(project_file.project, scaffold.project);
        assert_eq!(project_file.name, "probe-app");
        let graph_document =
            the_rendered_file_at(&rendered_files, NEW_PROJECT_SCAFFOLD_STREAM_DOCUMENT_PATH);
        let graph = GraphSnapshot::from_json_str(graph_document).unwrap();
        assert_eq!(graph.asset, Some(scaffold.stream_asset));
        let node_types: Vec<&str> = graph
            .nodes
            .iter()
            .map(|node| node.processor_type.as_str())
            .collect();
        assert_eq!(node_types, ["tatolab.testpattern:TestPatternSource"]);
        assert!(graph.nodes.iter().all(|node| node.id.is_some()));
        assert!(graph.links.is_empty());
        assert_eq!(
            graph.graph_document_bytes().unwrap(),
            graph_document.as_bytes(),
            "the scaffold writes the document canonically"
        );
        assert!(
            !rendered_files
                .iter()
                .any(|rendered_file| rendered_file.path_in_project.ends_with(".py")),
            "a new project holds no node code of its own"
        );
    }

    #[test]
    fn two_new_projects_never_share_a_node_uuid() {
        let node_uuids_of = |scaffold: &NewProjectScaffold| -> Vec<Uuid> {
            let rendered_files = render_scaffold_template_files("probe-app", scaffold).unwrap();
            GraphSnapshot::from_json_str(the_rendered_file_at(
                &rendered_files,
                NEW_PROJECT_SCAFFOLD_STREAM_DOCUMENT_PATH,
            ))
            .unwrap()
            .nodes
            .iter()
            .filter_map(|node| node.id)
            .collect()
        };
        let first = node_uuids_of(&a_scaffold_named("probe-app"));
        let second = node_uuids_of(&a_scaffold_named("probe-app"));
        assert!(first.iter().all(|node_uuid| !second.contains(node_uuid)));
    }

    #[test]
    fn the_scaffold_is_written_into_a_missing_directory_naming_its_distribution_after_the_folder() {
        let parent_directory = tempfile::tempdir().unwrap();
        let project_directory = parent_directory.path().join("Bright Lens");
        let scaffold = NewProjectScaffold::of_a_new_project_in(&project_directory);

        write_the_new_project_scaffold_into(&project_directory, &scaffold).unwrap();

        assert_eq!(scaffold.project_name, "Bright Lens");
        for scaffolded_file in render_scaffold_template_files("bright-lens", &scaffold).unwrap() {
            if scaffolded_file.path_in_project == NEW_PROJECT_SCAFFOLD_STREAM_DOCUMENT_PATH {
                assert_eq!(
                    GraphSnapshot::from_json_str(
                        &fs::read_to_string(
                            project_directory.join(scaffolded_file.path_in_project)
                        )
                        .unwrap()
                    )
                    .unwrap()
                    .asset,
                    Some(scaffold.stream_asset)
                );
                continue;
            }
            assert_eq!(
                fs::read_to_string(project_directory.join(scaffolded_file.path_in_project))
                    .unwrap(),
                scaffolded_file.rendered_contents,
                "{}",
                scaffolded_file.path_in_project
            );
        }
    }

    #[test]
    fn a_directory_already_holding_a_scaffold_file_is_refused_with_nothing_written() {
        let project_directory = tempfile::tempdir().unwrap();
        fs::write(
            project_directory.path().join(PROJECT_FILE_NAME),
            "{\"the user's own\": true}\n",
        )
        .unwrap();

        let refusal = write_the_new_project_scaffold_into(
            project_directory.path(),
            &NewProjectScaffold::of_a_new_project_in(project_directory.path()),
        )
        .unwrap_err()
        .to_string();

        assert!(
            refusal.contains(&format!("already has {PROJECT_FILE_NAME}")),
            "{refusal}"
        );
        assert!(!project_directory.path().join("pyproject.toml").exists());
        assert_eq!(
            fs::read_to_string(project_directory.path().join(PROJECT_FILE_NAME)).unwrap(),
            "{\"the user's own\": true}\n"
        );
    }
}

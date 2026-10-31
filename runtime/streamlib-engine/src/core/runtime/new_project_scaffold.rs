// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The files a new stream project starts from, rendered from the templates the
//! SDK carries, and their write into the project's directory.

use std::fs;
use std::path::Path;

use crate::core::{Error, Result};

/// The licence header each `.py` template opens with; the project it starts is the user's own.
const SCAFFOLD_TEMPLATE_LICENSE_HEADER: &str =
    "# Copyright (c) 2025 Jonathan Fontanez\n# SPDX-License-Identifier: BUSL-1.1\n\n";

/// The `module:qualname` of the one `@stream` the scaffold's `stream.py` defines.
pub(crate) const NEW_PROJECT_SCAFFOLD_STREAM_FUNCTION: &str = "stream:main";

const STREAM_PY_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/stream.py");
const NODES_INIT_PY_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/nodes/__init__.py");
const HALFTONE_LENS_EFFECT_PY_TEMPLATE: &str = include_str!(
    "../../../../../sdk/tatolab-stream/scaffold_template/nodes/halftone_lens_effect.py"
);
const BRIGHTNESS_METER_PY_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/nodes/brightness_meter.py");
const PYPROJECT_TOML_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/pyproject.toml");
const PYTHON_VERSION_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/python-version");
const GITIGNORE_TEMPLATE: &str =
    include_str!("../../../../../sdk/tatolab-stream/scaffold_template/gitignore");

const PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER: &str = "name = \"tatolab-stream-project\"";

/// The distribution name a directory whose name casts to nothing is given.
const FALLBACK_DISTRIBUTION_NAME: &str = "tatolab-stream-project";

/// One file a new project starts with: its path inside the project and its rendered contents.
pub(crate) struct ScaffoldedProjectFile {
    /// The path relative to the project directory.
    pub(crate) path_in_project: &'static str,
    /// The file's contents.
    pub(crate) rendered_contents: String,
}

fn without_license_header(python_template: &'static str) -> &'static str {
    python_template
        .strip_prefix(SCAFFOLD_TEMPLATE_LICENSE_HEADER)
        .unwrap_or(python_template)
}

/// Every file a new project starts with, in write order, rendered from the embedded templates.
pub(crate) fn render_scaffold_template_files(
    distribution_name: &str,
) -> Vec<ScaffoldedProjectFile> {
    let rendered_pyproject_toml = PYPROJECT_TOML_TEMPLATE.replace(
        PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER,
        &format!("name = \"{distribution_name}\""),
    );

    vec![
        ScaffoldedProjectFile {
            path_in_project: "stream.py",
            rendered_contents: without_license_header(STREAM_PY_TEMPLATE).to_owned(),
        },
        ScaffoldedProjectFile {
            path_in_project: "nodes/__init__.py",
            rendered_contents: without_license_header(NODES_INIT_PY_TEMPLATE).to_owned(),
        },
        ScaffoldedProjectFile {
            path_in_project: "nodes/halftone_lens_effect.py",
            rendered_contents: without_license_header(HALFTONE_LENS_EFFECT_PY_TEMPLATE).to_owned(),
        },
        ScaffoldedProjectFile {
            path_in_project: "nodes/brightness_meter.py",
            rendered_contents: without_license_header(BRIGHTNESS_METER_PY_TEMPLATE).to_owned(),
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
    ]
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

/// Write the files a new stream project starts from into `project_directory`,
/// created when absent, its distribution named after the folder. Refused,
/// with nothing written, when any of those files is already there.
pub(crate) fn write_the_new_project_scaffold_into(project_directory: &Path) -> Result<()> {
    let directory_name = project_directory
        .file_name()
        .map(|file_name| file_name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let scaffolded_files =
        render_scaffold_template_files(&python_distribution_name_for(&directory_name));

    // Checked before anything is written: a half-scaffolded directory is worse than a
    // refusal, and the user's own `stream.py` is the file most likely to already be there.
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
        fs::write(&scaffolded_file_path, &scaffolded_file.rendered_contents).map_err(
            |io_failure| {
                Error::Runtime(format!(
                    "cannot write `{}`: {io_failure}",
                    scaffolded_file_path.display()
                ))
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `.py` template, each opening with [`SCAFFOLD_TEMPLATE_LICENSE_HEADER`].
    const EMBEDDED_PYTHON_SCAFFOLD_TEMPLATES: [&str; 4] = [
        STREAM_PY_TEMPLATE,
        NODES_INIT_PY_TEMPLATE,
        HALFTONE_LENS_EFFECT_PY_TEMPLATE,
        BRIGHTNESS_METER_PY_TEMPLATE,
    ];

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
    fn every_embedded_python_template_opens_with_the_license_header() {
        for python_template in EMBEDDED_PYTHON_SCAFFOLD_TEMPLATES {
            assert!(
                python_template.starts_with(SCAFFOLD_TEMPLATE_LICENSE_HEADER),
                "a scaffold template lost its licence header:\n{python_template}"
            );
        }
    }

    #[test]
    fn every_python_file_in_the_scaffold_template_directory_is_an_embedded_template() {
        let scaffold_template_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sdk/tatolab-stream/scaffold_template");
        let mut pending_directories = vec![scaffold_template_directory];
        let mut walked_python_template_count = 0;
        while let Some(walked_directory) = pending_directories.pop() {
            for directory_entry in fs::read_dir(&walked_directory).unwrap() {
                let entry_path = directory_entry.unwrap().path();
                if entry_path.is_dir() {
                    pending_directories.push(entry_path);
                } else if entry_path
                    .extension()
                    .is_some_and(|extension| extension == "py")
                {
                    let python_template_on_disk = fs::read_to_string(&entry_path).unwrap();
                    assert!(
                        EMBEDDED_PYTHON_SCAFFOLD_TEMPLATES
                            .contains(&python_template_on_disk.as_str()),
                        "{} is not in EMBEDDED_PYTHON_SCAFFOLD_TEMPLATES",
                        entry_path.display()
                    );
                    walked_python_template_count += 1;
                }
            }
        }
        assert_eq!(
            walked_python_template_count,
            EMBEDDED_PYTHON_SCAFFOLD_TEMPLATES.len()
        );
    }

    #[test]
    fn the_distribution_name_placeholder_is_present_in_its_template() {
        assert!(PYPROJECT_TOML_TEMPLATE.contains(PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER));
    }

    #[test]
    fn every_file_is_rendered_without_the_licence_header_and_names_its_distribution() {
        let rendered_files = render_scaffold_template_files("probe-app");
        for rendered_file in &rendered_files {
            assert!(
                !rendered_file
                    .rendered_contents
                    .contains("SPDX-License-Identifier"),
                "{} kept the licence header",
                rendered_file.path_in_project
            );
        }
        assert!(
            rendered_files[4]
                .rendered_contents
                .contains("name = \"probe-app\"")
        );
    }

    #[test]
    fn the_sample_stream_encodes_the_test_pattern_publicly_and_opens_no_window() {
        let rendered_stream_py = &render_scaffold_template_files("probe-app")[0].rendered_contents;
        for present in [
            "stream_builder.add(TestPatternSource)",
            "stream_builder.add(H264Encoder)",
            "stream_builder.expose(encoder.output(\"encoded_video\"), Exposure.PUBLIC)",
        ] {
            assert!(
                rendered_stream_py.contains(present),
                "the sample stream lacks {present}:\n{rendered_stream_py}"
            );
        }
        assert!(
            !rendered_stream_py.contains("DisplayWindow"),
            "{rendered_stream_py}"
        );
    }

    #[test]
    fn the_scaffold_is_written_into_a_missing_directory_naming_its_distribution_after_the_folder()
    {
        let parent_directory = tempfile::tempdir().unwrap();
        let project_directory = parent_directory.path().join("Bright Lens");

        write_the_new_project_scaffold_into(&project_directory).unwrap();

        for scaffolded_file in render_scaffold_template_files("bright-lens") {
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
        fs::write(project_directory.path().join("stream.py"), "# the user's own\n").unwrap();

        let refusal = write_the_new_project_scaffold_into(project_directory.path())
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("already has stream.py"), "{refusal}");
        assert!(!project_directory.path().join("pyproject.toml").exists());
        assert_eq!(
            fs::read_to_string(project_directory.path().join("stream.py")).unwrap(),
            "# the user's own\n"
        );
    }
}

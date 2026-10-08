// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::TatolabCommandFailure;
use crate::verb_standard_output::write_verb_standard_output;

/// The licence header each `.py` template opens with; the project `new` writes is the user's own.
const SCAFFOLD_TEMPLATE_LICENSE_HEADER: &str =
    "# Copyright (c) 2025 Jonathan Fontanez\n# SPDX-License-Identifier: BUSL-1.1\n\n";

const STREAM_PY_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/scaffold_template/stream.py");
const NODES_INIT_PY_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/scaffold_template/nodes/__init__.py");
const INVERTING_EFFECT_PY_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/scaffold_template/nodes/inverting_effect.py");
const BRIGHTNESS_METER_PY_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/scaffold_template/nodes/brightness_meter.py");
const PYPROJECT_TOML_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/scaffold_template/pyproject.toml");
const PYTHON_VERSION_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/scaffold_template/python-version");
const GITIGNORE_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/scaffold_template/gitignore");

const STREAM_PY_DOCSTRING_OPENER_PLACEHOLDER: &str = "A Tatolab stream: camera →";
const STREAM_PY_IMPORT_LINE_PLACEHOLDER: &str =
    "from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream";
const STREAM_PY_FUNCTION_DOCSTRING_PLACEHOLDER: &str = "\"\"\"Camera, inverted,";
const STREAM_PY_SOURCE_ADD_PLACEHOLDER: &str = "stream_builder.add(CameraSource)";
const PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER: &str = "name = \"tatolab-stream-project\"";

/// The distribution name a directory whose name casts to nothing is given.
const FALLBACK_DISTRIBUTION_NAME: &str = "tatolab-stream-project";

/// The source the scaffolded stream's pipeline starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScaffoldedStreamSource {
    /// The built-in camera source.
    Camera,
    /// The built-in test pattern, for a machine with no capture device.
    TestPattern,
}

impl ScaffoldedStreamSource {
    fn source_class_name(self) -> &'static str {
        match self {
            ScaffoldedStreamSource::Camera => "CameraSource",
            ScaffoldedStreamSource::TestPattern => "TestPatternSource",
        }
    }

    fn source_description(self) -> &'static str {
        match self {
            ScaffoldedStreamSource::Camera => "camera",
            ScaffoldedStreamSource::TestPattern => "test pattern",
        }
    }

    fn source_description_capitalized(self) -> &'static str {
        match self {
            ScaffoldedStreamSource::Camera => "Camera",
            ScaffoldedStreamSource::TestPattern => "Test pattern",
        }
    }
}

/// One file `new` writes: its path inside the project and its rendered contents.
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

/// Every file `new` writes, in write order, rendered from the embedded templates.
pub(crate) fn render_scaffold_template_files(
    distribution_name: &str,
    scaffolded_stream_source: ScaffoldedStreamSource,
) -> Vec<ScaffoldedProjectFile> {
    let source_class_name = scaffolded_stream_source.source_class_name();
    let source_description = scaffolded_stream_source.source_description();
    let source_description_capitalized = scaffolded_stream_source.source_description_capitalized();
    let mut tatolab_stream_import_names = [
        source_class_name,
        "DisplayWindow",
        "StreamBuilder",
        "stream",
    ];
    tatolab_stream_import_names.sort_unstable();

    let rendered_stream_py = without_license_header(STREAM_PY_TEMPLATE)
        .replace(
            STREAM_PY_DOCSTRING_OPENER_PLACEHOLDER,
            &format!("A Tatolab stream: {source_description} →"),
        )
        .replace(
            STREAM_PY_IMPORT_LINE_PLACEHOLDER,
            &format!(
                "from tatolab.stream import {}",
                tatolab_stream_import_names.join(", ")
            ),
        )
        .replace(
            STREAM_PY_FUNCTION_DOCSTRING_PLACEHOLDER,
            &format!("\"\"\"{source_description_capitalized}, inverted,"),
        )
        .replace(
            STREAM_PY_SOURCE_ADD_PLACEHOLDER,
            &format!("stream_builder.add({source_class_name})"),
        );
    let rendered_pyproject_toml = PYPROJECT_TOML_TEMPLATE.replace(
        PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER,
        &format!("name = \"{distribution_name}\""),
    );

    vec![
        ScaffoldedProjectFile {
            path_in_project: "stream.py",
            rendered_contents: rendered_stream_py,
        },
        ScaffoldedProjectFile {
            path_in_project: "nodes/__init__.py",
            rendered_contents: without_license_header(NODES_INIT_PY_TEMPLATE).to_owned(),
        },
        ScaffoldedProjectFile {
            path_in_project: "nodes/inverting_effect.py",
            rendered_contents: without_license_header(INVERTING_EFFECT_PY_TEMPLATE).to_owned(),
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

fn resolved_directory_name(target_directory: &Path) -> std::io::Result<String> {
    let resolved_target_directory = match target_directory.canonicalize() {
        Ok(canonical_target_directory) => canonical_target_directory,
        Err(_) => {
            let mut lexically_resolved = PathBuf::new();
            for component in std::path::absolute(target_directory)?.components() {
                match component {
                    Component::ParentDir => {
                        lexically_resolved.pop();
                    }
                    Component::CurDir => {}
                    other_component => lexically_resolved.push(other_component),
                }
            }
            lexically_resolved
        }
    };
    Ok(resolved_target_directory
        .file_name()
        .map(|file_name| file_name.to_string_lossy().into_owned())
        .unwrap_or_default())
}

/// `tatolab new`: write a working stream project into `target_directory`.
pub(crate) fn scaffold_new_stream_project(
    target_directory: &Path,
    scaffolded_stream_source: ScaffoldedStreamSource,
) -> Result<(), TatolabCommandFailure> {
    let directory_name = resolved_directory_name(target_directory).map_err(|io_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot resolve `{}`: {io_failure}",
            target_directory.display()
        ))
    })?;
    let scaffolded_files = render_scaffold_template_files(
        &python_distribution_name_for(&directory_name),
        scaffolded_stream_source,
    );

    // Checked before anything is written: a half-scaffolded directory is worse than a
    // refusal, and the user's own `stream.py` is the file most likely to already be there.
    let mut already_present: Vec<&str> = scaffolded_files
        .iter()
        .map(|scaffolded_file| scaffolded_file.path_in_project)
        .filter(|path_in_project| target_directory.join(path_in_project).exists())
        .collect();
    already_present.sort_unstable();
    if !already_present.is_empty() {
        return Err(TatolabCommandFailure::refused(format!(
            "`{}` already has {} — scaffolding would overwrite it. Pick an empty directory.",
            target_directory.display(),
            already_present.join(", ")
        )));
    }

    for scaffolded_file in &scaffolded_files {
        let scaffolded_file_path = target_directory.join(scaffolded_file.path_in_project);
        if let Some(parent_directory) = scaffolded_file_path.parent() {
            fs::create_dir_all(parent_directory).map_err(|io_failure| {
                TatolabCommandFailure::refused(format!(
                    "cannot create `{}`: {io_failure}",
                    parent_directory.display()
                ))
            })?;
        }
        fs::write(&scaffolded_file_path, &scaffolded_file.rendered_contents).map_err(
            |io_failure| {
                TatolabCommandFailure::refused(format!(
                    "cannot write `{}`: {io_failure}",
                    scaffolded_file_path.display()
                ))
            },
        )?;
    }

    write_verb_standard_output(&format!(
        "Created a Tatolab stream project in `{target_directory}`.\n\nNext:\n    cd \
         {target_directory}\n    uv sync\n    tatolab dev\n",
        target_directory = target_directory.display()
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `.py` template, each opening with [`SCAFFOLD_TEMPLATE_LICENSE_HEADER`].
    const EMBEDDED_PYTHON_SCAFFOLD_TEMPLATES: [&str; 4] = [
        STREAM_PY_TEMPLATE,
        NODES_INIT_PY_TEMPLATE,
        INVERTING_EFFECT_PY_TEMPLATE,
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
    fn every_placeholder_is_present_in_its_template() {
        for stream_py_placeholder in [
            STREAM_PY_DOCSTRING_OPENER_PLACEHOLDER,
            STREAM_PY_IMPORT_LINE_PLACEHOLDER,
            STREAM_PY_FUNCTION_DOCSTRING_PLACEHOLDER,
            STREAM_PY_SOURCE_ADD_PLACEHOLDER,
        ] {
            assert!(
                STREAM_PY_TEMPLATE.contains(stream_py_placeholder),
                "stream.py lost the placeholder {stream_py_placeholder:?}"
            );
        }
        assert!(PYPROJECT_TOML_TEMPLATE.contains(PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER));
    }

    #[test]
    fn every_placeholder_is_substituted_for_both_sources() {
        for scaffolded_stream_source in [
            ScaffoldedStreamSource::Camera,
            ScaffoldedStreamSource::TestPattern,
        ] {
            let rendered_files =
                render_scaffold_template_files("probe-app", scaffolded_stream_source);
            for rendered_file in &rendered_files {
                assert!(
                    !rendered_file
                        .rendered_contents
                        .contains("SPDX-License-Identifier"),
                    "{} kept the licence header",
                    rendered_file.path_in_project
                );
            }
            let rendered_stream_py = &rendered_files[0].rendered_contents;
            let uses_test_pattern = scaffolded_stream_source == ScaffoldedStreamSource::TestPattern;
            assert_eq!(
                rendered_stream_py.contains("TestPatternSource"),
                uses_test_pattern
            );
            assert_eq!(
                rendered_stream_py.contains("CameraSource"),
                !uses_test_pattern
            );
            assert!(
                rendered_files[4]
                    .rendered_contents
                    .contains("name = \"probe-app\"")
            );
        }
    }
}

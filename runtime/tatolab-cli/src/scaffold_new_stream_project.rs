// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::TatolabCommandFailure;

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

const STREAM_PY_DOCSTRING_OPENER_PLACEHOLDER: &str = "A StreamLib stream: camera →";
const STREAM_PY_IMPORT_LINE_PLACEHOLDER: &str =
    "from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream";
const STREAM_PY_FUNCTION_DOCSTRING_PLACEHOLDER: &str = "\"\"\"Camera, inverted,";
const STREAM_PY_SOURCE_ADD_PLACEHOLDER: &str = "stream_builder.add(CameraSource)";
const PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER: &str = "name = \"streamlib-app\"";

/// The distribution name a directory whose name casts to nothing is given.
const FALLBACK_DISTRIBUTION_NAME: &str = "streamlib-app";

const fn text_starts_with(text: &str, prefix: &str) -> bool {
    let text_bytes = text.as_bytes();
    let prefix_bytes = prefix.as_bytes();
    if prefix_bytes.len() > text_bytes.len() {
        return false;
    }
    let mut byte_index = 0;
    while byte_index < prefix_bytes.len() {
        if text_bytes[byte_index] != prefix_bytes[byte_index] {
            return false;
        }
        byte_index += 1;
    }
    true
}

const fn text_contains(text: &str, needle: &str) -> bool {
    let text_bytes = text.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.len() > text_bytes.len() {
        return false;
    }
    let mut start_index = 0;
    while start_index + needle_bytes.len() <= text_bytes.len() {
        let mut matched_length = 0;
        while matched_length < needle_bytes.len()
            && text_bytes[start_index + matched_length] == needle_bytes[matched_length]
        {
            matched_length += 1;
        }
        if matched_length == needle_bytes.len() {
            return true;
        }
        start_index += 1;
    }
    false
}

// A template that loses its header or a placeholder fails the build, never a user's `new`.
const _: () = {
    assert!(text_starts_with(
        STREAM_PY_TEMPLATE,
        SCAFFOLD_TEMPLATE_LICENSE_HEADER
    ));
    assert!(text_starts_with(
        NODES_INIT_PY_TEMPLATE,
        SCAFFOLD_TEMPLATE_LICENSE_HEADER
    ));
    assert!(text_starts_with(
        INVERTING_EFFECT_PY_TEMPLATE,
        SCAFFOLD_TEMPLATE_LICENSE_HEADER
    ));
    assert!(text_starts_with(
        BRIGHTNESS_METER_PY_TEMPLATE,
        SCAFFOLD_TEMPLATE_LICENSE_HEADER
    ));
    assert!(text_contains(
        STREAM_PY_TEMPLATE,
        STREAM_PY_DOCSTRING_OPENER_PLACEHOLDER
    ));
    assert!(text_contains(
        STREAM_PY_TEMPLATE,
        STREAM_PY_IMPORT_LINE_PLACEHOLDER
    ));
    assert!(text_contains(
        STREAM_PY_TEMPLATE,
        STREAM_PY_FUNCTION_DOCSTRING_PLACEHOLDER
    ));
    assert!(text_contains(
        STREAM_PY_TEMPLATE,
        STREAM_PY_SOURCE_ADD_PLACEHOLDER
    ));
    assert!(text_contains(
        PYPROJECT_TOML_TEMPLATE,
        PYPROJECT_TOML_DISTRIBUTION_NAME_PLACEHOLDER
    ));
};

/// One file `new` writes: its path inside the project and its rendered contents.
pub struct ScaffoldedProjectFile {
    /// The path relative to the project directory.
    pub path_in_project: &'static str,
    /// The file's contents.
    pub rendered_contents: String,
}

fn without_license_header(python_template: &'static str) -> &'static str {
    &python_template[SCAFFOLD_TEMPLATE_LICENSE_HEADER.len()..]
}

/// Every file `new` writes, in write order, rendered from the embedded templates.
pub fn render_scaffold_template_files(
    distribution_name: &str,
    use_test_pattern_source: bool,
) -> Vec<ScaffoldedProjectFile> {
    let (source_class_name, source_description, source_description_capitalized) =
        if use_test_pattern_source {
            ("TestPatternSource", "test pattern", "Test pattern")
        } else {
            ("CameraSource", "camera", "Camera")
        };
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
            &format!("A StreamLib stream: {source_description} →"),
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
pub fn python_distribution_name_for(directory_name: &str) -> String {
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
pub fn scaffold_new_stream_project(
    target_directory: &Path,
    use_test_pattern_source: bool,
) -> Result<(), TatolabCommandFailure> {
    let directory_name = resolved_directory_name(target_directory).map_err(|io_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot resolve `{}`: {io_failure}",
            target_directory.display()
        ))
    })?;
    let scaffolded_files = render_scaffold_template_files(
        &python_distribution_name_for(&directory_name),
        use_test_pattern_source,
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

    println!(
        "Created a StreamLib app in `{}`.\n",
        target_directory.display()
    );
    println!("Next:");
    println!("    cd {}", target_directory.display());
    println!("    uv sync");
    println!("    tatolab dev");
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
    fn every_placeholder_is_substituted_in_both_variants() {
        for use_test_pattern_source in [false, true] {
            let rendered_files =
                render_scaffold_template_files("probe-app", use_test_pattern_source);
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
            assert_eq!(
                rendered_stream_py.contains("TestPatternSource"),
                use_test_pattern_source
            );
            assert_eq!(
                rendered_stream_py.contains("CameraSource"),
                !use_test_pattern_source
            );
            assert!(
                rendered_files[4]
                    .rendered_contents
                    .contains("name = \"probe-app\"")
            );
        }
    }
}

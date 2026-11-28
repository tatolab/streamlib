// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A new node file's starting content, rendered from the template the SDK carries beside the
//! new project scaffold.

/// The node file template, a node written as one function in the current `tatolab.stream` idiom.
const NEW_NODE_FILE_TEMPLATE: &str =
    include_str!("../../../sdk/tatolab-stream/node_file_template/new_node.py");

/// The template's function statement, its node renamed on render after the file's name.
const NEW_NODE_FILE_TEMPLATE_FUNCTION_STATEMENT: &str = "def NewNode[Frame: Texture](";

/// The template's type parameter, which the node cannot be named either.
const NEW_NODE_FILE_TEMPLATE_TYPE_PARAMETER: &str = "Frame";

/// The repository's licence header the template carries, which a person's file never does.
const NEW_NODE_FILE_TEMPLATE_LICENCE_HEADER: &str =
    "# Copyright (c) 2025 Tatolab LLC\n# SPDX-License-Identifier: GPL-3.0-only\n\n";

/// The names the template imports, in the order it imports them, which the node
/// cannot take.
const NAMES_THE_NEW_NODE_FILE_TEMPLATE_IMPORTS: &[&str] = &[
    "Annotated",
    "Callable",
    "Description",
    "Input",
    "InputPin",
    "NumberRange",
    "OutputPin",
    "RuntimeContextLimitedAccess",
    "StreamDelivery",
    "Texture",
    "node",
];

const PYTHON_SOURCE_FILE_SUFFIX: &str = ".py";

/// Python's hard keywords, none of which names a module or a function.
const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// A new node file rendered for one path of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewNodeFileRenderedFromTheTemplate {
    /// The node type the file declares, `<module>:<function>`, as `add_node` takes it.
    pub(crate) node_type: String,
    /// The file's content.
    pub(crate) content: String,
}

fn is_a_python_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|rest| rest.is_ascii_alphanumeric() || rest == '_')
        && !PYTHON_KEYWORDS.contains(&name)
}

/// `invert` → `Invert`, `edge_glow` → `EdgeGlow`.
fn node_name_for_a_module_named(module_file_stem: &str) -> String {
    module_file_stem
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            characters
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// The template rendered for a new Python file at `path_in_project`, its node function named
/// after the file: `nodes/edge_glow.py` declares `nodes.edge_glow:EdgeGlow`. Refused, with why,
/// for a path that is not an importable Python module whose name gives a node name.
pub(crate) fn render_the_new_node_file_template_for(
    path_in_project: &str,
) -> Result<NewNodeFileRenderedFromTheTemplate, String> {
    let module_path = path_in_project
        .strip_suffix(PYTHON_SOURCE_FILE_SUFFIX)
        .ok_or_else(|| format!("`{path_in_project}` is not a Python file; name it `<name>.py`"))?;
    let module_path_segments: Vec<&str> = module_path.split('/').collect();
    if let Some(not_importable) = module_path_segments
        .iter()
        .find(|segment| !is_a_python_identifier(segment))
    {
        return Err(format!(
            "`{path_in_project}` is not importable as a Python module: `{not_importable}` is \
             not a Python identifier; use letters, digits and `_`, not starting with a digit"
        ));
    }
    let module_file_stem = module_path_segments[module_path_segments.len() - 1];
    if module_file_stem.starts_with("__") && module_file_stem.ends_with("__") {
        return Err(format!(
            "`{path_in_project}` is a module Python reserves, never a node file"
        ));
    }
    let node_name = node_name_for_a_module_named(module_file_stem);
    if !is_a_python_identifier(&node_name)
        || NAMES_THE_NEW_NODE_FILE_TEMPLATE_IMPORTS.contains(&node_name.as_str())
        || node_name == NEW_NODE_FILE_TEMPLATE_TYPE_PARAMETER
    {
        return Err(format!(
            "`{path_in_project}` would name its node `{node_name}`, which the file cannot \
             declare; choose another file name"
        ));
    }
    let content = NEW_NODE_FILE_TEMPLATE
        .strip_prefix(NEW_NODE_FILE_TEMPLATE_LICENCE_HEADER)
        .unwrap_or(NEW_NODE_FILE_TEMPLATE)
        .replace(
            NEW_NODE_FILE_TEMPLATE_FUNCTION_STATEMENT,
            &format!("def {node_name}[{NEW_NODE_FILE_TEMPLATE_TYPE_PARAMETER}: Texture]("),
        );
    Ok(NewNodeFileRenderedFromTheTemplate {
        node_type: format!("{}:{node_name}", module_path_segments.join(".")),
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_carries_the_header_and_the_function_statement_its_render_replaces() {
        assert!(NEW_NODE_FILE_TEMPLATE.starts_with(NEW_NODE_FILE_TEMPLATE_LICENCE_HEADER));
        assert_eq!(
            NEW_NODE_FILE_TEMPLATE
                .matches(NEW_NODE_FILE_TEMPLATE_FUNCTION_STATEMENT)
                .count(),
            1
        );
    }

    #[test]
    fn the_template_imports_exactly_the_names_its_node_cannot_take() {
        let (imports, _) = NEW_NODE_FILE_TEMPLATE
            .strip_prefix(NEW_NODE_FILE_TEMPLATE_LICENCE_HEADER)
            .and_then(|template| template.split_once("\n\n\n"))
            .expect("the template's imports are one block after its licence header");
        let imported: Vec<&str> = imports
            .lines()
            .flat_map(|line| {
                line.split_once(" import ")
                    .map_or(line, |(_, names)| names)
                    .split(',')
            })
            .map(|name| name.trim().trim_matches(['(', ')']))
            .filter(|name| !name.is_empty())
            .collect();
        assert_eq!(imported, NAMES_THE_NEW_NODE_FILE_TEMPLATE_IMPORTS);
    }

    #[test]
    fn a_node_file_is_rendered_with_its_node_named_after_the_file_and_no_licence_header() {
        let rendered = render_the_new_node_file_template_for("nodes/edge_glow.py").unwrap();

        assert_eq!(rendered.node_type, "nodes.edge_glow:EdgeGlow");
        assert!(rendered.content.contains("def EdgeGlow[Frame: Texture](\n"));
        assert!(!rendered.content.contains("NewNode"));
        assert!(!rendered.content.contains("SPDX-License-Identifier"));
        assert!(
            rendered
                .content
                .starts_with("from typing import Annotated, Callable\n")
        );
    }

    #[test]
    fn a_file_at_the_project_root_is_its_own_module() {
        assert_eq!(
            render_the_new_node_file_template_for("invert.py")
                .unwrap()
                .node_type,
            "invert:Invert"
        );
    }

    #[test]
    fn a_path_that_is_no_importable_module_or_names_no_usable_node_is_refused() {
        for refused_path in [
            "nodes/invert.txt",
            "nodes/invert",
            "nodes/edge-glow.py",
            "nodes/2x.py",
            "my nodes/invert.py",
            "nodes/class.py",
            "nodes/__init__.py",
            "nodes/texture.py",
            "nodes/frame.py",
            "nodes/none.py",
            "nodes/___.py",
        ] {
            assert!(
                render_the_new_node_file_template_for(refused_path).is_err(),
                "{refused_path} was rendered"
            );
        }
    }
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Renders `tatolab.stream`'s built-in node classes from the runtime's own
//! descriptors: one class per built-in, whose `type` is the path the runtime
//! registers it under, declaring its fields and pins as a class node does. A built-in that
//! takes a config, or has a port stating no one pin type, is refused by name. The Rust
//! declaration is the only one; `--check` is the
//! CI gate that fails when the checked-in module drifts from it.
//!
//! The descriptors come from `streamlib-media-builtins`' example
//! `print_built_in_node_descriptors`, so xtask itself links no engine.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

mod typed_built_in_node;

use typed_built_in_node::{
    ImportedName, refuse_what_a_class_node_cannot_declare, render_fields_and_pins,
};

/// One built-in's catalog entry, as the descriptor example prints it. The
/// config schema stays a JSON value, read only to refuse a built-in that takes one.
#[derive(Debug, Deserialize)]
pub struct BuiltInNodeDescriptor {
    #[serde(rename = "type")]
    built_in_node_type: String,
    description: String,
    #[serde(default)]
    config_schema: Option<Value>,
    inputs: Vec<BuiltInNodePortDescriptor>,
    outputs: Vec<BuiltInNodePortDescriptor>,
}

/// One port of a [`BuiltInNodeDescriptor`].
#[derive(Debug, Deserialize)]
struct BuiltInNodePortDescriptor {
    name: String,
    description: String,
    #[serde(default)]
    delivery_profile: Option<String>,
    /// How the input takes its value, tagged by `mode`; present on a setting, which a field
    /// renders, so the docstring's inputs leave it out.
    #[serde(default)]
    input_mode: Option<BuiltInNodeInputMode>,
    /// The pin type the port declares, or how it is found, tagged by `kind`; absent on a port
    /// that declares none.
    #[serde(default)]
    pin_type: Option<Value>,
    /// How its messages are read, beside its pin type.
    #[serde(default)]
    flow: Option<String>,
    /// The value a setting holds until it is set.
    #[serde(default)]
    default_value: Option<Value>,
    /// What the port states besides its type.
    #[serde(default)]
    annotations: Option<Map<String, Value>>,
}

/// The part of an input's mode the generator reads: whether it is a setting, and when it is
/// read.
#[derive(Debug, Deserialize)]
struct BuiltInNodeInputMode {
    mode: String,
    #[serde(default)]
    read: Option<String>,
}

impl BuiltInNodePortDescriptor {
    /// Whether the catalog lists this input as a setting, which a field renders.
    fn is_a_setting(&self) -> bool {
        self.input_mode
            .as_ref()
            .is_some_and(|input_mode| input_mode.mode == "setting_only")
    }

    /// The one pin type the port states, in the contract's form; `None` when it states none, or
    /// its type is found another way.
    fn exact_pin_type(&self) -> Option<&Value> {
        let declared = self.pin_type.as_ref()?;
        (declared.get("kind").and_then(Value::as_str) == Some("is"))
            .then(|| declared.get("pin_type"))
            .flatten()
    }

    /// The frame rate a Texture output states among its annotations; `None` when it states none.
    fn stated_frame_rate(&self) -> Result<Option<BuiltInNodeImageOutputFrameRate>> {
        self.annotations
            .as_ref()
            .and_then(|annotations| {
                annotations.get(typed_built_in_node::THE_FRAME_RATE_A_DOCSTRING_RENDERS)
            })
            .map(|frame_rate| {
                BuiltInNodeImageOutputFrameRate::deserialize(frame_rate).with_context(|| {
                    format!(
                        "port `{}` states a frame rate {frame_rate} that does not read",
                        self.name
                    )
                })
            })
            .transpose()
    }

    /// The setting whose chosen type the port carries; `None` when it is typed another way.
    fn the_setting_choosing_its_type(&self) -> Option<&str> {
        let declared = self.pin_type.as_ref()?;
        (declared.get("kind").and_then(Value::as_str) == Some("set_by_input"))
            .then(|| declared.get("input_name").and_then(Value::as_str))
            .flatten()
    }
}

/// An Image output's declared frame rate, in the descriptor's wire form.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BuiltInNodeImageOutputFrameRate {
    Stated {
        numerator: u32,
        denominator: u32,
    },
    #[serde(alias = "from_config")]
    FromSetting {
        #[serde(alias = "config_key")]
        setting: String,
    },
}

/// How an Image output's declared frame rate reads: `rate=30`,
/// `rate=ImageStreamFrameRate(30000, 1001)`, or the setting it is read from.
fn python_spelling_of_a_frame_rate(frame_rate: &BuiltInNodeImageOutputFrameRate) -> String {
    match frame_rate {
        BuiltInNodeImageOutputFrameRate::Stated {
            numerator,
            denominator: 1,
        } => format!("`rate={numerator}`"),
        BuiltInNodeImageOutputFrameRate::Stated {
            numerator,
            denominator,
        } => format!("`rate=ImageStreamFrameRate({numerator}, {denominator})`"),
        BuiltInNodeImageOutputFrameRate::FromSetting { setting } => {
            format!("its `{setting}` setting")
        }
    }
}

/// The generated module, relative to the workspace root.
pub const GENERATED_BUILT_IN_NODE_CLASSES_PATH: &str =
    "sdk/tatolab-stream/tatolab/stream/_built_in_nodes.py";

/// What every built-in's `type` starts with; the rest is its class name.
const BUILT_IN_NODE_TYPE_PREFIX: &str = "tatolab.stream:";

/// The column a docstring line wraps before.
const DOCSTRING_WRAP_COLUMN: usize = 88;

const GENERATED_MODULE_HEADER: &str = r#"# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

# Generated by `cargo xtask generate-built-in-node-classes` from the runtime's
# built-in descriptors. Do not edit: CI fails when this file differs from a
# regeneration.

"""The runtime's built-in nodes, each with its fields and pins."""

from __future__ import annotations
"#;

/// Regenerate the module, or with `check_only` fail when the checked-in one
/// differs from a regeneration.
pub fn run(workspace_root: &Path, check_only: bool) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(
            "generate-built-in-node-classes runs on Linux only: Linux is the floor that \
             compiles in every built-in, and a module rendered elsewhere would drop the ones \
             that floor lacks"
        );
    }
    let built_in_node_descriptors = built_in_node_descriptors_this_floor_registers(workspace_root)?;
    let rendered_module = render_built_in_node_classes_module(&built_in_node_descriptors)?;
    let generated_module_path = workspace_root.join(GENERATED_BUILT_IN_NODE_CLASSES_PATH);
    if check_only {
        let checked_in_module = std::fs::read_to_string(&generated_module_path)
            .with_context(|| format!("reading {}", generated_module_path.display()))?;
        if checked_in_module != rendered_module {
            bail!(
                "{GENERATED_BUILT_IN_NODE_CLASSES_PATH} differs from what the runtime's built-in \
                 descriptors render to. A built-in's declaration changed without the module being \
                 regenerated: run `cargo xtask generate-built-in-node-classes` and commit the result."
            );
        }
        tracing::info!("{GENERATED_BUILT_IN_NODE_CLASSES_PATH} matches the runtime's built-ins");
        return Ok(());
    }
    std::fs::write(&generated_module_path, rendered_module)
        .with_context(|| format!("writing {}", generated_module_path.display()))?;
    tracing::info!(
        "wrote {GENERATED_BUILT_IN_NODE_CLASSES_PATH} ({} built-ins)",
        built_in_node_descriptors.len()
    );
    Ok(())
}

fn built_in_node_descriptors_this_floor_registers(
    workspace_root: &Path,
) -> Result<Vec<BuiltInNodeDescriptor>> {
    let output = std::process::Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "run",
            "--locked",
            "-q",
            "-p",
            "streamlib-media-builtins",
            "--example",
            "print_built_in_node_descriptors",
        ])
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("running the print_built_in_node_descriptors example")?;
    if !output.status.success() {
        bail!(
            "the print_built_in_node_descriptors example exited with {}",
            output.status
        );
    }
    let descriptors: Vec<BuiltInNodeDescriptor> = serde_json::from_slice(&output.stdout)
        .context("parsing the built-in descriptors the example printed")?;
    if descriptors.is_empty() {
        bail!("the print_built_in_node_descriptors example printed no built-ins");
    }
    Ok(descriptors)
}

/// The generated module's text for `descriptors`, each a catalog entry as the
/// local API serves it.
pub fn render_built_in_node_classes_module(
    descriptors: &[BuiltInNodeDescriptor],
) -> Result<String> {
    let mut rendered_classes = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        rendered_classes.push(render_one_built_in_node(descriptor)?);
    }
    rendered_classes.sort_by(|left, right| left.class_name.cmp(&right.class_name));

    let exported_names: Vec<&str> = rendered_classes
        .iter()
        .map(|rendered| rendered.class_name.as_str())
        .collect();
    let imports: BTreeSet<ImportedName> = rendered_classes
        .iter()
        .flat_map(|rendered| rendered.imports.iter().copied())
        .collect();

    let mut module = String::from(GENERATED_MODULE_HEADER);
    module.push_str(&render_imports(&imports)?);
    module.push_str("\n__all__ = [\n");
    for exported_name in &exported_names {
        writeln!(module, "    \"{exported_name}\",")?;
    }
    module.push_str("]\n");
    for rendered in &rendered_classes {
        module.push_str(&rendered.text);
    }
    Ok(module)
}

/// The import lines of `imports`: the standard library's first, then the package's, each
/// module's names on one line.
fn render_imports(imports: &BTreeSet<ImportedName>) -> Result<String> {
    let mut text = String::new();
    for package_modules in [false, true] {
        let mut modules: Vec<&str> = imports
            .iter()
            .map(|(module, _)| *module)
            .filter(|module| module.starts_with('.') == package_modules)
            .collect();
        modules.dedup();
        if modules.is_empty() {
            continue;
        }
        text.push('\n');
        for module in modules {
            let names: Vec<&str> = imports
                .iter()
                .filter(|(of_module, _)| *of_module == module)
                .map(|(_, name)| *name)
                .collect();
            writeln!(text, "from {module} import {}", names.join(", "))?;
        }
    }
    Ok(text)
}

struct RenderedBuiltInNode {
    class_name: String,
    /// Each name its text imports.
    imports: BTreeSet<ImportedName>,
    text: String,
}

fn render_one_built_in_node(descriptor: &BuiltInNodeDescriptor) -> Result<RenderedBuiltInNode> {
    let built_in_node_type = descriptor.built_in_node_type.as_str();
    let Some(class_name) = built_in_node_type.strip_prefix(BUILT_IN_NODE_TYPE_PREFIX) else {
        bail!("built-in `{built_in_node_type}` does not start with `{BUILT_IN_NODE_TYPE_PREFIX}`");
    };
    if class_name.is_empty() || !class_name.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("built-in `{built_in_node_type}` does not name a Python class");
    }
    let refused_item_label = format!("built-in `{class_name}`");
    refuse_what_a_class_node_cannot_declare(descriptor, &refused_item_label)?;
    let mut imports = BTreeSet::from([("typing", "ClassVar"), ("._built_in_node", "BuiltInNode")]);

    let mut text = String::new();
    write!(text, "\n\nclass {class_name}(BuiltInNode):\n")?;
    let mut class_doc_paragraphs = vec![python_prose(
        &sentence(&descriptor.description),
        &refused_item_label,
    )?];
    let input_ports: Vec<&BuiltInNodePortDescriptor> = descriptor
        .inputs
        .iter()
        .filter(|input| !input.is_a_setting())
        .collect();
    let output_ports: Vec<&BuiltInNodePortDescriptor> = descriptor.outputs.iter().collect();
    for (heading, ports) in [("Inputs", &input_ports), ("Outputs", &output_ports)] {
        if !ports.is_empty() {
            class_doc_paragraphs.push(render_port_paragraph(heading, ports, &refused_item_label)?);
        }
    }
    text.push_str(&render_docstring(&class_doc_paragraphs, 4));
    writeln!(
        text,
        "\n    type: ClassVar[str] = {}",
        python_string_literal(built_in_node_type, &refused_item_label)?
    )?;
    if class_name.starts_with("Test") {
        text.push_str(
            "\n    # Keeps pytest from collecting the `Test*`-named class in user suites.\n    \
             __test__: ClassVar[bool] = False\n",
        );
    }
    text.push_str(&render_fields_and_pins(
        descriptor,
        &refused_item_label,
        &mut imports,
    )?);
    Ok(RenderedBuiltInNode {
        class_name: class_name.to_owned(),
        imports,
        text,
    })
}

fn render_port_paragraph(
    heading: &str,
    ports: &[&BuiltInNodePortDescriptor],
    refused_item_label: &str,
) -> Result<String> {
    let mut paragraph = format!("{heading}:");
    for port in ports {
        let port_name = &port.name;
        let delivery_profile = match &port.delivery_profile {
            Some(delivery_profile) => format!(" ({delivery_profile})"),
            None => String::new(),
        };
        let description = python_prose(&sentence(&port.description), refused_item_label)?;
        write!(
            paragraph,
            "\n    `{port_name}`{delivery_profile}: {description}"
        )?;
        if let Some(frame_rate) = port.stated_frame_rate()? {
            write!(
                paragraph,
                "\n        Frame rate: {}.",
                python_spelling_of_a_frame_rate(&frame_rate)
            )?;
        }
    }
    Ok(paragraph)
}

pub(crate) fn push_unique(alternatives: &mut Vec<String>, alternative: String) {
    if !alternatives.contains(&alternative) {
        alternatives.push(alternative);
    }
}

/// `alternatives` joined into one union, `None` last.
pub(crate) fn union_of(mut alternatives: Vec<String>) -> String {
    if let Some(none_position) = alternatives
        .iter()
        .position(|alternative| alternative == "None")
    {
        let none = alternatives.remove(none_position);
        alternatives.push(none);
    }
    alternatives.join(" | ")
}

fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// `rustdoc` as a Python author reads it: an intra-doc link becomes its
/// backticked name. A link through a path other than `Self::` names a Rust item
/// the author never sees, so it is refused rather than rendered.
pub(crate) fn python_prose(rustdoc: &str, refused_item_label: &str) -> Result<String> {
    let mut prose = String::with_capacity(rustdoc.len());
    let mut rest = rustdoc;
    while let Some(link_start) = rest.find("[`") {
        prose.push_str(&rest[..link_start]);
        let after_link_start = &rest[link_start + 2..];
        let Some(link_end) = after_link_start.find("`]") else {
            bail!("{refused_item_label}'s description opens an intra-doc link it never closes");
        };
        let linked = &after_link_start[..link_end];
        let linked = linked.strip_prefix("Self::").unwrap_or(linked);
        if linked.contains("::") {
            bail!(
                "{refused_item_label}'s description links `{linked}`, a Rust path a Python author cannot \
                 follow; say what it means in the description instead"
            );
        }
        write!(prose, "`{linked}`")?;
        rest = &after_link_start[link_end + 2..];
    }
    prose.push_str(rest);
    if prose.contains("\"\"\"") {
        bail!("{refused_item_label}'s description holds `\"\"\"`, which would end its docstring");
    }
    Ok(prose.replace('\\', "\\\\"))
}

pub(crate) fn python_string_literal(value: &str, refused_item_label: &str) -> Result<String> {
    if value.contains(['"', '\\']) || value.chars().any(char::is_control) {
        bail!("{refused_item_label} has the string {value:?}, which this generator does not quote");
    }
    Ok(format!("\"{value}\""))
}

/// `value` as Python: a scalar, or a list of scalars.
/// A docstring of `paragraphs` at `indent`, each wrapped to
/// [`DOCSTRING_WRAP_COLUMN`]. Within a paragraph a line indented past the
/// paragraph's first is a list item: it starts its own line, and its wrapped
/// lines hang under it.
pub(crate) fn render_docstring(paragraphs: &[String], indent: usize) -> String {
    let margin = " ".repeat(indent);
    let mut lines: Vec<String> = Vec::new();
    for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
        if paragraph_index > 0 {
            lines.push(String::new());
        }
        for block in docstring_blocks(paragraph) {
            if block.starts_a_prose_paragraph {
                lines.push(String::new());
            }
            let first_prefix = " ".repeat(block.list_item_indent);
            let hanging_prefix = if block.list_item_indent > 0 {
                " ".repeat(block.list_item_indent + 4)
            } else {
                String::new()
            };
            let width = DOCSTRING_WRAP_COLUMN - indent;
            // The docstring's first line also carries its opening quotes.
            let first_line_width = if lines.is_empty() { width - 3 } else { width };
            lines.extend(wrap_words(
                &block.text,
                first_line_width,
                width,
                &first_prefix,
                &hanging_prefix,
            ));
        }
    }
    let Some((first_line, remaining_lines)) = lines.split_first() else {
        return String::new();
    };
    if remaining_lines.is_empty()
        && first_line.chars().count() + indent + 6 <= DOCSTRING_WRAP_COLUMN
    {
        return format!("{margin}\"\"\"{first_line}\"\"\"\n");
    }
    let mut docstring = format!("{margin}\"\"\"{first_line}\n");
    for line in remaining_lines {
        if line.is_empty() {
            docstring.push('\n');
        } else {
            docstring.push_str(&format!("{margin}{line}\n"));
        }
    }
    docstring.push_str(&format!("{margin}\"\"\"\n"));
    docstring
}

/// A run of one paragraph's text that wraps as a unit.
struct DocstringBlock {
    /// How far a list item is indented; zero for prose.
    list_item_indent: usize,
    text: String,
    /// Whether a blank line in the source came before this block.
    starts_a_prose_paragraph: bool,
}

/// The blocks of one paragraph. A blank line in the source separates prose
/// paragraphs; an indented line is a list item; any other line break is a soft
/// wrap.
fn docstring_blocks(paragraph: &str) -> Vec<DocstringBlock> {
    let mut blocks: Vec<DocstringBlock> = Vec::new();
    let mut starts_a_prose_paragraph = false;
    for source_line in paragraph.split('\n') {
        if source_line.trim().is_empty() {
            starts_a_prose_paragraph = true;
            continue;
        }
        let list_item_indent = source_line.len() - source_line.trim_start().len();
        let continues_the_previous_block = list_item_indent == 0
            && !starts_a_prose_paragraph
            && blocks
                .last()
                .is_some_and(|block| block.list_item_indent == 0);
        if continues_the_previous_block {
            if let Some(block) = blocks.last_mut() {
                block.text.push(' ');
                block.text.push_str(source_line.trim());
            }
            continue;
        }
        blocks.push(DocstringBlock {
            list_item_indent,
            text: source_line.trim().to_owned(),
            starts_a_prose_paragraph,
        });
        starts_a_prose_paragraph = false;
    }
    blocks
}

fn wrap_words(
    text: &str,
    first_line_width: usize,
    width: usize,
    first_prefix: &str,
    hanging_prefix: &str,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::from(first_prefix);
    let mut line_has_a_word = false;
    for word in text.split_whitespace() {
        let line_width = if lines.is_empty() {
            first_line_width
        } else {
            width
        };
        let fits = line.chars().count() + usize::from(line_has_a_word) + word.chars().count()
            <= line_width;
        if line_has_a_word && !fits {
            lines.push(std::mem::replace(&mut line, String::from(hanging_prefix)));
            line_has_a_word = false;
        }
        if line_has_a_word {
            line.push(' ');
        }
        line.push_str(word);
        line_has_a_word = true;
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests;

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The description lint: which of a node type's descriptions are missing, and
//! which write raw HTML, which a description's CommonMark does not allow.
//!
//! Off unless a project runs it. The runtime refuses no node type for what it
//! finds; a missing description is shown as "No description".

use std::fmt;

use crate::descriptors::{PortDescriptor, ProcessorDescriptor};

/// One thing of a node type a description is declared on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescribedDeclaration {
    /// The node type itself.
    NodeType,
    /// One input port, by name.
    InputPort(String),
    /// One output port, by name.
    OutputPort(String),
    /// One top-level key of the config, by name.
    ConfigKey(String),
}

impl fmt::Display for DescribedDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DescribedDeclaration::NodeType => formatter.write_str("the node type"),
            DescribedDeclaration::InputPort(name) => write!(formatter, "input port `{name}`"),
            DescribedDeclaration::OutputPort(name) => write!(formatter, "output port `{name}`"),
            DescribedDeclaration::ConfigKey(name) => write!(formatter, "config key `{name}`"),
        }
    }
}

/// What the lint found wrong with one description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptionLintProblem {
    /// No description is declared, or it is blank.
    Missing,
    /// The description writes raw HTML, outside any code span or code block.
    RawHtml,
}

/// One finding of the description lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptionLintFinding {
    /// What the description is declared on.
    pub declaration: DescribedDeclaration,
    /// What is wrong with it.
    pub problem: DescriptionLintProblem,
}

impl fmt::Display for DescriptionLintFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.problem {
            DescriptionLintProblem::Missing => {
                write!(formatter, "{} has no description", self.declaration)
            }
            DescriptionLintProblem::RawHtml => write!(
                formatter,
                "{}'s description writes raw HTML, which a description's CommonMark does not allow",
                self.declaration
            ),
        }
    }
}

/// Every finding on `descriptor`: its own description, each port's, and each
/// top-level config key's.
pub fn lint_the_descriptions_of_a_node_type(
    descriptor: &ProcessorDescriptor,
) -> Vec<DescriptionLintFinding> {
    let mut findings = Vec::new();
    let mut lint = |declaration: DescribedDeclaration, description: Option<&str>| {
        if let Some(problem) = description_lint_problem(description) {
            findings.push(DescriptionLintFinding {
                declaration,
                problem,
            });
        }
    };
    lint(
        DescribedDeclaration::NodeType,
        Some(descriptor.description.as_str()),
    );
    let described_ports = |ports: &[PortDescriptor]| {
        ports
            .iter()
            .map(|port| (port.name.clone(), port.description.clone()))
            .collect::<Vec<_>>()
    };
    for (name, description) in described_ports(&descriptor.inputs) {
        lint(DescribedDeclaration::InputPort(name), Some(&description));
    }
    for (name, description) in described_ports(&descriptor.outputs) {
        lint(DescribedDeclaration::OutputPort(name), Some(&description));
    }
    let config_properties = descriptor
        .config_schema
        .as_ref()
        .and_then(|document| document.get("properties"))
        .and_then(serde_json::Value::as_object);
    for (config_key, key_schema) in config_properties.into_iter().flatten() {
        lint(
            DescribedDeclaration::ConfigKey(config_key.clone()),
            key_schema
                .get("description")
                .and_then(serde_json::Value::as_str),
        );
    }
    findings
}

fn description_lint_problem(description: Option<&str>) -> Option<DescriptionLintProblem> {
    match description {
        None => Some(DescriptionLintProblem::Missing),
        Some(text) if text.trim().is_empty() => Some(DescriptionLintProblem::Missing),
        Some(text) if writes_raw_html(text) => Some(DescriptionLintProblem::RawHtml),
        Some(_) => None,
    }
}

/// Whether `markdown` holds a CommonMark raw HTML construct outside code: an
/// open or closing tag, a comment, a processing instruction or a declaration.
/// An autolink, `<https://…>`, is not one.
pub fn writes_raw_html(markdown: &str) -> bool {
    let prose = markdown_without_code(markdown);
    let characters: Vec<char> = prose.chars().collect();
    characters.iter().enumerate().any(|(index, character)| {
        *character == '<' && raw_html_starts_after_the_angle_bracket(&characters[index + 1..])
    })
}

fn raw_html_starts_after_the_angle_bracket(rest: &[char]) -> bool {
    match rest.first() {
        Some('!') | Some('?') => true,
        Some('/') => tag_name_then_tag_boundary(&rest[1..]),
        Some(_) => tag_name_then_tag_boundary(rest),
        None => false,
    }
}

fn tag_name_then_tag_boundary(rest: &[char]) -> bool {
    if !rest.first().is_some_and(char::is_ascii_alphabetic) {
        return false;
    }
    let tag_name_length = rest
        .iter()
        .take_while(|character| character.is_ascii_alphanumeric() || **character == '-')
        .count();
    matches!(
        rest.get(tag_name_length),
        Some(character) if character.is_whitespace() || *character == '>' || *character == '/'
    )
}

/// `markdown` with its fenced code blocks and code spans taken out.
fn markdown_without_code(markdown: &str) -> String {
    let mut prose = String::with_capacity(markdown.len());
    let mut inside_a_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            inside_a_fence = !inside_a_fence;
            continue;
        }
        if inside_a_fence {
            continue;
        }
        let mut inside_a_code_span = false;
        for character in line.chars() {
            if character == '`' {
                inside_a_code_span = !inside_a_code_span;
            } else if !inside_a_code_span {
                prose.push(character);
            }
        }
        prose.push('\n');
    }
    prose
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptors::PortDescriptor;
    use crate::{
        PortFamily, PortFamilyDeclaration, ProcessorClassImportPath, ProcessorClassShortName,
    };

    fn port(name: &str, description: &str) -> PortDescriptor {
        PortDescriptor {
            name: name.to_string(),
            description: description.to_string(),
            required: true,
            delivery_profile: Some("newest".to_string()),
            borrow_quota: None,
            audio_window: None,
            family: PortFamilyDeclaration::of_family(PortFamily::Image),
            pin_default: None,
        }
    }

    fn descriptor(description: &str) -> ProcessorDescriptor {
        ProcessorDescriptor::new(
            ProcessorClassShortName::new("Blur").expect("a name"),
            ProcessorClassImportPath::new("my_app::Blur").expect("a path"),
            description,
        )
        .with_input(port("video_in", "Frames to blur."))
        .with_input(port("mask", ""))
        .with_output(port("video", "<b>Blurred</b> frames."))
        .with_config_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "radius": {"type": "integer", "description": "In pixels."},
                "sigma": {"type": "number"},
            }
        }))
    }

    #[test]
    fn every_missing_description_and_every_raw_html_one_is_found() {
        let findings = lint_the_descriptions_of_a_node_type(&descriptor("   "));
        let rendered: Vec<String> = findings.iter().map(ToString::to_string).collect();
        assert_eq!(
            rendered,
            [
                "the node type has no description",
                "input port `mask` has no description",
                "output port `video`'s description writes raw HTML, which a description's \
                 CommonMark does not allow",
                "config key `sigma` has no description",
            ]
        );
    }

    #[test]
    fn a_fully_described_node_type_has_no_finding() {
        let described = ProcessorDescriptor::new(
            ProcessorClassShortName::new("Blur").expect("a name"),
            ProcessorClassImportPath::new("my_app::Blur").expect("a path"),
            "Blurs `video_in`.",
        )
        .with_input(port("video_in", "Frames to blur."));
        assert!(lint_the_descriptions_of_a_node_type(&described).is_empty());
    }

    #[test]
    fn raw_html_is_told_apart_from_an_autolink_a_comparison_and_code() {
        for raw_html in [
            "<b>bold</b>",
            "line<br/>break",
            "a <img src=x> image",
            "<!-- a comment -->",
            "</div>",
            "<?php ?>",
        ] {
            assert!(writes_raw_html(raw_html), "{raw_html:?} is raw HTML");
        }
        for not_raw_html in [
            "see <https://tatolab.com>",
            "mail <someone@example.com>",
            "when `a <b>` holds",
            "x < 3 and y > 2",
            "```\n<div>in a fence</div>\n```",
            "a <- b",
        ] {
            assert!(
                !writes_raw_html(not_raw_html),
                "{not_raw_html:?} is not raw HTML"
            );
        }
    }
}

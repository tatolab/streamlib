// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the compiler and the linter printed over one extension folder's check folder, read as
//! findings at each file's place in the project.

use serde::Deserialize;

use super::{ExtensionCodeCheckKind, ExtensionCodeFinding, ExtensionCodeFindingSeverity};

/// The file the check writes from a node UI manifest, tying each control's default export to
/// the class type its entry declares.
pub(super) const TIE_FILE: &str = "tatolab_node_ui_tie.ts";

/// Where the check folder resolves the kit, which a finding never names as the author's file.
const CHECK_FOLDER_NODE_MODULES: &str = "node_modules/";

/// How a file of the check folder is named in the project: the folder's path in the project, and
/// the manifest entries the tie file was written from.
pub(super) struct CheckFolderPlaces<'folder> {
    /// The extension folder's path in the project, as `views/meter` or `node_ui`.
    pub(super) folder_path: &'folder str,
    /// The manifest the tie file was written from, by its path in the project.
    pub(super) manifest_path: &'folder str,
    /// The lines of the tie file before the first control's.
    pub(super) tie_header_line_count: usize,
    /// The lines the tie file holds for each control.
    pub(super) tie_lines_per_control: usize,
    /// Each control the tie file ties, as its manifest entry names it in words.
    pub(super) tied_controls: &'folder [String],
}

impl CheckFolderPlaces<'_> {
    /// The place in the project of `line` of the check folder's `file`, and what the words are
    /// prefixed with there: a tie line is its manifest entry's, and a file of the kit is the
    /// folder's.
    fn place_of(&self, file: &str, line: Option<u32>) -> (String, Option<u32>, String) {
        if file == TIE_FILE {
            let control_index = line.and_then(|line| {
                (line as usize)
                    .checked_sub(1 + self.tie_header_line_count)
                    .map(|after_the_header| after_the_header / self.tie_lines_per_control.max(1))
            });
            let entry = control_index
                .and_then(|index| self.tied_controls.get(index).map(|words| (index, words)))
                .map(|(index, words)| format!("`controls[{index}]` ({words}): "))
                .unwrap_or_default();
            return (self.manifest_path.to_owned(), None, entry);
        }
        if file.starts_with(CHECK_FOLDER_NODE_MODULES) {
            return (self.folder_path.to_owned(), None, format!("{file}: "));
        }
        (format!("{}/{file}", self.folder_path), line, String::new())
    }
}

/// One line the compiler prints for a diagnostic: `file(line,column): error TS2322: words`, or
/// `error TS5083: words` for one at no file.
struct CompilerDiagnosticLine<'printed> {
    file_and_place: Option<(&'printed str, u32, u32)>,
    severity: ExtensionCodeFindingSeverity,
    code: &'printed str,
    words: &'printed str,
}

/// `line` read as the first line of a compiler diagnostic, or `None` when it is not one.
fn compiler_diagnostic_line(line: &str) -> Option<CompilerDiagnosticLine<'_>> {
    let (file_and_place, rest) = match line.split_once("): ") {
        Some((file_and_place, rest)) if !line.starts_with("error ") => {
            let (file, place) = file_and_place.rsplit_once('(')?;
            let (line_number, column) = place.split_once(',')?;
            (
                Some((file, line_number.parse().ok()?, column.parse().ok()?)),
                rest,
            )
        }
        _ => (None, line),
    };
    let (severity, rest) = if let Some(rest) = rest.strip_prefix("error ") {
        (ExtensionCodeFindingSeverity::Error, rest)
    } else {
        (
            ExtensionCodeFindingSeverity::Warning,
            rest.strip_prefix("warning ")?,
        )
    };
    let (code, words) = rest.split_once(": ")?;
    code.starts_with("TS").then_some(CompilerDiagnosticLine {
        file_and_place,
        severity,
        code,
        words,
    })
}

/// Each diagnostic the compiler printed with `--pretty false`, at its place in the project; a
/// line that follows one indented is part of its words.
pub(super) fn compiler_findings(
    printed: &str,
    places: &CheckFolderPlaces<'_>,
) -> Vec<ExtensionCodeFinding> {
    let mut findings: Vec<ExtensionCodeFinding> = Vec::new();
    for line in printed.lines() {
        if let Some(diagnostic) = compiler_diagnostic_line(line) {
            let (path, line, column, prefix) = match diagnostic.file_and_place {
                Some((file, line, column)) => {
                    let (path, line, prefix) = places.place_of(file, Some(line));
                    (path, line, line.map(|_| column), prefix)
                }
                None => (places.folder_path.to_owned(), None, None, String::new()),
            };
            findings.push(ExtensionCodeFinding {
                kind: ExtensionCodeCheckKind::Types,
                finding_code: diagnostic.code.to_owned(),
                severity: diagnostic.severity,
                words: format!("{prefix}{}", diagnostic.words),
                path,
                line,
                column,
            });
        } else if line.starts_with(char::is_whitespace)
            && let Some(last) = findings.last_mut()
        {
            last.words.push('\n');
            last.words.push_str(line.trim());
        }
    }
    findings
}

#[derive(Deserialize)]
struct LinterReport {
    diagnostics: Vec<LinterDiagnostic>,
}

#[derive(Deserialize)]
struct LinterDiagnostic {
    message: String,
    #[serde(default)]
    code: Option<String>,
    severity: String,
    #[serde(default)]
    help: Option<String>,
    filename: String,
    #[serde(default)]
    labels: Vec<LinterLabel>,
}

#[derive(Deserialize)]
struct LinterLabel {
    span: LinterSpan,
}

#[derive(Deserialize)]
struct LinterSpan {
    line: u32,
    column: u32,
}

/// Each diagnostic the linter printed with `--format json`, at its place in the project; or why
/// what it printed is no report.
pub(super) fn linter_findings(
    printed: &str,
    places: &CheckFolderPlaces<'_>,
) -> Result<Vec<ExtensionCodeFinding>, String> {
    let report: LinterReport = serde_json::from_str(printed)
        .map_err(|not_a_report| format!("the linter printed no JSON report: {not_a_report}"))?;
    Ok(report
        .diagnostics
        .into_iter()
        .map(|diagnostic| {
            let span = diagnostic.labels.first().map(|label| &label.span);
            let file = diagnostic
                .filename
                .strip_prefix("./")
                .unwrap_or(&diagnostic.filename);
            let (path, line, prefix) = places.place_of(file, span.map(|span| span.line));
            let words = match diagnostic.help {
                Some(help) => format!("{prefix}{} {help}", diagnostic.message),
                None => format!("{prefix}{}", diagnostic.message),
            };
            ExtensionCodeFinding {
                kind: ExtensionCodeCheckKind::Lint,
                finding_code: diagnostic.code.unwrap_or_else(|| "lint".to_owned()),
                severity: if diagnostic.severity == "error" {
                    ExtensionCodeFindingSeverity::Error
                } else {
                    ExtensionCodeFindingSeverity::Warning
                },
                words,
                path,
                column: line.and(span.map(|span| span.column)),
                line,
            }
        })
        .collect())
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The answer `check_project_code` gives: what each check found in a project's code on disk.

use serde::{Deserialize, Serialize};

use super::ProjectActionRefusalOnTheWire;

/// `check_project_code`'s answer, for the bytes on disk when the checks ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectCodeCheckResult {
    /// `failed` when any finding is an `error`; else `not_fully_checked` when any check did not
    /// run to its end; else `passed`. Warnings alone pass.
    pub verdict: ProjectCodeCheckVerdict,
    /// Each check run, and how it ended.
    pub checks: Vec<ProjectCodeCheckRun>,
    /// Every finding of every check, in the order each check found them.
    pub findings: Vec<ProjectCodeCheckFinding>,
    /// Each file a check finished, with the hash of the bytes it checked. A file a check did
    /// not finish is not listed.
    pub files_checked: Vec<ProjectFileChecked>,
}

impl ProjectCodeCheckResult {
    /// The answer of `checks` and the `findings` and `files_checked` they gave, its verdict
    /// derived from them.
    pub fn of(
        checks: Vec<ProjectCodeCheckRun>,
        findings: Vec<ProjectCodeCheckFinding>,
        files_checked: Vec<ProjectFileChecked>,
    ) -> Self {
        let verdict = if findings
            .iter()
            .any(|finding| finding.severity == ProjectCodeCheckSeverity::Error)
        {
            ProjectCodeCheckVerdict::Failed
        } else if checks
            .iter()
            .any(|check| check.state == ProjectCodeCheckState::NotChecked)
        {
            ProjectCodeCheckVerdict::NotFullyChecked
        } else {
            ProjectCodeCheckVerdict::Passed
        };
        Self {
            verdict,
            checks,
            findings,
            files_checked,
        }
    }
}

/// What `check_project_code` decided of the code it was asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProjectCodeCheckVerdict {
    /// Every check ran to its end and found no error.
    Passed,
    /// A check found an error.
    Failed,
    /// No check found an error, and a check did not run to its end; `checks` says why.
    NotFullyChecked,
}

/// One check of a project's code. The set grows: a check a client does not know is still
/// answered with its state and its findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum ProjectCodeCheckKind {
    /// Each Python module of the project that declares a node type or a graph generator, or
    /// that does not parse, imported in the project's interpreter by the reader that loads the
    /// node: each declaration it refuses, each module that does not import, and each node
    /// declared in a retired form.
    PythonNodeDeclarations,
    /// Each `tatolab_view.json` of the project's own views, read as the runtime reads it when
    /// the view opens: each one that does not open, and each port still declared by the older
    /// `family` field, with the `pin_type` to write.
    ExtensionManifest,
}

/// How one check ended. The set grows: a state a client does not know reads as `not_checked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum ProjectCodeCheckState {
    /// It ran to its end and found no error.
    Passed,
    /// It ran to its end and found an error.
    Failed,
    /// It did not run, or did not run to its end; `not_checked_because` says why, and the
    /// findings of what it finished are kept.
    // `#[serde(other)]` holds only on the last variant, so a new value goes above this one.
    #[serde(other)]
    NotChecked,
}

/// One check run and how it ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectCodeCheckRun {
    /// The check.
    pub check: ProjectCodeCheckKind,
    /// How it ended.
    pub state: ProjectCodeCheckState,
    /// Why it did not run to its end, with its code, such as `check_ran_out_of_time` with the
    /// file it was checking as `refused_file`; present only when `state` is `not_checked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_checked_because: Option<ProjectActionRefusalOnTheWire>,
    /// Each program or package the check ran with, and its version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checked_with: Vec<String>,
}

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProjectCodeCheckSeverity {
    /// The code is refused as it is: the verdict is `failed`.
    Error,
    /// The code works and should change, such as a node declared in a retired form.
    Warning,
}

/// One thing a check found, at its place in a file of the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectCodeCheckFinding {
    /// The check that found it.
    pub check: ProjectCodeCheckKind,
    /// What was found, for a caller to branch on: the code the declaration reader refused with,
    /// such as `value_input_has_no_default`, `module_does_not_import` for a module that does not
    /// import, `declaration_retired` for a node declared in a retired form, or
    /// `port_declared_by_family` for a view's port its manifest declares by `family` alone.
    pub finding_code: String,
    /// How much it matters.
    pub severity: ProjectCodeCheckSeverity,
    /// What was found, and how to fix it when there is a fix.
    pub words: String,
    /// The file, by its path in the project, `/`-separated.
    pub path: String,
    /// The line, from 1, when the finding is narrower than the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The column, from 1, when the finding is narrower than the line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// The node type the finding is about, `<module>:<name>`, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    /// The node type's port the finding is about, as its declaration names it, when it is about
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

/// A file a check finished, and the hash of the bytes it checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectFileChecked {
    /// The file, by its path in the project, `/`-separated.
    pub path: String,
    /// The SHA-256 of its bytes as the check read them, lowercase hex, as `read_project_file`
    /// answers it.
    pub content_sha256: String,
}

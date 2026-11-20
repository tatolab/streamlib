// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's own views and `node_ui` folder checked as the Studio will run them: each folder
//! copied into a scratch check folder beside the extension SDK kit's two files, with one tie file
//! written from its node UI manifest, then type-checked by the runtime unit's TypeScript compiler
//! and read by its linter with the kit's strict rules. Options, rules and tie lines are read from
//! the kit, never written here. A folder's outcome is kept by the hash of its bytes and the kit.

mod checker_output_read;
mod extension_folder_check;
mod extension_sdk_kit_found;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use streamlib_project_repository::{
    NODE_UI_DIRECTORY, NODE_UI_MANIFEST_FILE_NAME, PROJECT_VIEWS_DIRECTORY,
    TATOLAB_VIEW_MANIFEST_FILE_NAME,
};

use extension_folder_check::{
    ExtensionCodeCheckersFound, ExtensionFolderChecked, ExtensionFolderToCheck,
};
pub use extension_sdk_kit_found::EXTENSION_SDK_KIT_ENVIRONMENT_VARIABLE;
use extension_sdk_kit_found::{KIT_TEMPLATES_FOLDER, the_extension_sdk_kit_found};

/// The programs a project's views and controls are checked with, as the host found them beside
/// itself, and the kit its starter names, which the host hands the engine once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCodeCheckers {
    /// The TypeScript compiler, or why the runtime unit carries none.
    pub typescript_compiler: Result<PathBuf, String>,
    /// The linter, or why the runtime unit carries none.
    pub linter: Result<PathBuf, String>,
    /// The kit named in [`EXTENSION_SDK_KIT_ENVIRONMENT_VARIABLE`] as the runtime started.
    pub kit_its_starter_names: Option<PathBuf>,
}

/// A template read from the extension SDK kit, and the kit it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSdkKitTemplate {
    /// The kit, with its version and where it was found, as a check's `checked_with` names it.
    pub kit: String,
    /// The template's text.
    pub template: String,
}

/// The two checks of a project's views and controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCodeCheckKind {
    /// The compiler's, with the kit's strict options.
    Types,
    /// The linter's, with the kit's strict rules.
    Lint,
}

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCodeFindingSeverity {
    /// The code is refused as it is.
    Error,
    /// The code works and should change.
    Warning,
}

/// One thing a check found, at its place in a file of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCodeFinding {
    /// The check that found it.
    pub kind: ExtensionCodeCheckKind,
    /// The compiler's code, as `TS2322`, the rule, as `typescript(no-explicit-any)`, or the
    /// runtime's own for a file of a kind that is not checked.
    pub finding_code: String,
    /// How much it matters.
    pub severity: ExtensionCodeFindingSeverity,
    /// What was found, in the checker's words.
    pub words: String,
    /// The file, by its path in the project.
    pub path: String,
    /// The line, from 1.
    pub line: Option<u32>,
    /// The column, from 1.
    pub column: Option<u32>,
}

/// Why a check did not run to its end, by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCodeNotCheckedCode {
    /// No kit was found to check against.
    ExtensionSdkNotFound,
    /// The runtime unit carries no program for the check.
    ProgramNotCarried,
    /// The program was still running at its bound and was killed.
    RanOutOfTime,
    /// The program ran and its answer could not be read.
    Refused,
}

/// Why a check did not run to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCodeNotChecked {
    /// Of what kind.
    pub code: ExtensionCodeNotCheckedCode,
    /// Why, in words.
    pub words: String,
    /// The extension folder it was checking, by its path in the project.
    pub folder_being_checked: Option<String>,
}

/// One check run over a project's views and controls, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCodeCheckRun {
    /// The check.
    pub kind: ExtensionCodeCheckKind,
    /// Why it did not run to its end; `None` when it did.
    pub not_checked: Option<ExtensionCodeNotChecked>,
    /// Each program and the kit it ran with, with versions.
    pub checked_with: Vec<String>,
}

/// A file both checks finished, with the hash of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFileChecked {
    /// The file, by its path in the project.
    pub path: String,
    /// The SHA-256 of the bytes checked, lowercase hex.
    pub content_sha256: String,
}

/// What both checks of a project's views and controls found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCodeCheck {
    /// The compiler's run.
    pub types: ExtensionCodeCheckRun,
    /// The linter's run.
    pub lint: ExtensionCodeCheckRun,
    /// Every finding, folder by folder, the compiler's before the linter's.
    pub findings: Vec<ExtensionCodeFinding>,
    /// Every file of each folder both checks finished, vendored code included, since a file is
    /// served only when a passing answer lists it.
    pub files_checked: Vec<ExtensionFileChecked>,
}

/// The checkers the host handed the engine, the version each program reports, and each
/// folder's outcome by the hash of what it was checked as.
#[derive(Default)]
pub struct ExtensionCodeCheckersHeld {
    checkers: OnceLock<ExtensionCodeCheckers>,
    program_versions: Mutex<BTreeMap<PathBuf, String>>,
    folder_outcomes_by_content: Mutex<BTreeMap<String, ExtensionFolderChecked>>,
}

impl ExtensionCodeCheckersHeld {
    /// Hold `checkers`, refusing a second hand-over.
    pub fn set(&self, checkers: ExtensionCodeCheckers) -> Result<(), String> {
        self.checkers.set(checkers).map_err(|_| {
            "this runtime was already handed what checks a project's views and controls; it is \
             handed once"
                .to_owned()
        })
    }

    /// Check the own views and `node_ui` folder of the project at `project_directory` that
    /// `paths` reach, each a path in the project, or every one when `paths` is `None`, against
    /// the first kit found, looking beside `lend_directory` first.
    pub fn check_a_projects_extension_code(
        &self,
        project_directory: &Path,
        paths: Option<&[String]>,
        lend_directory: Option<&Path>,
    ) -> ExtensionCodeCheck {
        let folders = the_extension_folders_to_check(project_directory, paths);
        let mut check = ExtensionCodeCheck::of_no_folder();
        let mut to_check = Vec::new();
        for folder in folders {
            match ExtensionFolderToCheck::read(project_directory, folder) {
                Ok(folder_to_check) => to_check.push(folder_to_check),
                Err(not_read) => check.not_checked_in(not_read),
            }
        }
        if !to_check
            .iter()
            .any(ExtensionFolderToCheck::holds_code_to_check)
        {
            for folder_to_check in to_check {
                check.take_in(folder_to_check.checked_with(None));
            }
            return check;
        }
        let checkers = self.checkers.get();
        let kit = the_extension_sdk_kit_found(
            lend_directory,
            checkers.and_then(|checkers| checkers.kit_its_starter_names.as_deref()),
            project_directory,
        );
        let program_of = |program: Option<&Result<PathBuf, String>>, what: &str| match program {
            Some(found) => found.clone(),
            None => Err(format!("this runtime was handed no {what}")),
        };
        let compiler = program_of(
            checkers.map(|checkers| &checkers.typescript_compiler),
            "TypeScript compiler",
        );
        let linter = program_of(checkers.map(|checkers| &checkers.linter), "linter");
        let (kit, compiler, linter) = match (kit, compiler, linter) {
            (Ok(kit), Ok(compiler), Ok(linter)) => (kit, compiler, linter),
            (kit, compiler, linter) => {
                return ExtensionCodeCheck::not_checked(
                    kit.err()
                        .map(|why| (ExtensionCodeNotCheckedCode::ExtensionSdkNotFound, why)),
                    compiler.err(),
                    linter.err(),
                );
            }
        };
        let checkers_found = ExtensionCodeCheckersFound {
            kit: &kit,
            compiler: &compiler,
            linter: &linter,
        };
        for (run, program) in [(&mut check.types, &compiler), (&mut check.lint, &linter)] {
            run.checked_with = vec![self.version_of(program), kit.as_checked_with()];
        }
        for folder_to_check in to_check {
            let content_key = folder_to_check.content_key(&checkers_found);
            let kept = self
                .folder_outcomes_by_content
                .lock()
                .ok()
                .and_then(|kept| kept.get(&content_key).cloned());
            let checked = kept.unwrap_or_else(|| {
                let checked = folder_to_check.checked_with(Some(&checkers_found));
                if checked.ran_to_its_end()
                    && let Ok(mut kept) = self.folder_outcomes_by_content.lock()
                {
                    kept.insert(content_key, checked.clone());
                }
                checked
            });
            check.take_in(checked);
        }
        check
    }

    /// The template `template_file` in the first kit found's `templates/` for the project at
    /// `project_directory`, looking beside `lend_directory` first, or why there is none: no kit
    /// is found, or the kit holds no such template.
    pub fn a_template_from_the_kit(
        &self,
        project_directory: &Path,
        template_file: &str,
        lend_directory: Option<&Path>,
    ) -> Result<ExtensionSdkKitTemplate, String> {
        let kit = the_extension_sdk_kit_found(
            lend_directory,
            self.checkers
                .get()
                .and_then(|checkers| checkers.kit_its_starter_names.as_deref()),
            project_directory,
        )?;
        let template = kit.directory.join(KIT_TEMPLATES_FOLDER).join(template_file);
        let read = std::fs::read_to_string(&template).map_err(|unreadable| {
            format!(
                "the {} holds no template `{KIT_TEMPLATES_FOLDER}/{template_file}`: {} cannot \
                 be read: {unreadable}",
                kit.as_checked_with(),
                template.display()
            )
        })?;
        Ok(ExtensionSdkKitTemplate {
            kit: kit.as_checked_with(),
            template: read,
        })
    }

    /// The version `program` reports, read once, as a `checked_with` names it.
    fn version_of(&self, program: &Path) -> String {
        if let Some(version) = self
            .program_versions
            .lock()
            .ok()
            .and_then(|versions| versions.get(program).cloned())
        {
            return version;
        }
        match extension_folder_check::the_version_a_program_reports(program) {
            Ok(version) => {
                if let Ok(mut versions) = self.program_versions.lock() {
                    versions.insert(program.to_path_buf(), version.clone());
                }
                version
            }
            Err(why) => format!("{} ({why})", program.display()),
        }
    }
}

impl ExtensionCodeCheck {
    /// Both checks run over no code, as for a project with no views or controls of its own.
    fn of_no_folder() -> Self {
        let run = |kind| ExtensionCodeCheckRun {
            kind,
            not_checked: None,
            checked_with: Vec::new(),
        };
        Self {
            types: run(ExtensionCodeCheckKind::Types),
            lint: run(ExtensionCodeCheckKind::Lint),
            findings: Vec::new(),
            files_checked: Vec::new(),
        }
    }

    /// Both checks not run: each for the kit's absence, else for its own program's.
    fn not_checked(
        kit_not_found: Option<(ExtensionCodeNotCheckedCode, String)>,
        compiler_not_carried: Option<String>,
        linter_not_carried: Option<String>,
    ) -> Self {
        let run = |kind, program_not_carried: Option<String>| ExtensionCodeCheckRun {
            kind,
            not_checked: kit_not_found
                .clone()
                .or(program_not_carried
                    .map(|why| (ExtensionCodeNotCheckedCode::ProgramNotCarried, why)))
                .map(|(code, words)| ExtensionCodeNotChecked {
                    code,
                    words,
                    folder_being_checked: None,
                }),
            checked_with: Vec::new(),
        };
        Self {
            types: run(ExtensionCodeCheckKind::Types, compiler_not_carried),
            lint: run(ExtensionCodeCheckKind::Lint, linter_not_carried),
            findings: Vec::new(),
            files_checked: Vec::new(),
        }
    }

    /// Both checks marked not run to their end for `not_run`, the first reason kept.
    fn not_checked_in(&mut self, not_run: ExtensionCodeNotChecked) {
        for run in [&mut self.types, &mut self.lint] {
            run.not_checked.get_or_insert_with(|| not_run.clone());
        }
    }

    /// `checked`'s findings and files taken in, and each check it did not finish marked.
    fn take_in(&mut self, checked: ExtensionFolderChecked) {
        let ran_to_its_end = checked.ran_to_its_end();
        for (run, not_checked) in [
            (&mut self.types, checked.types_not_checked),
            (&mut self.lint, checked.lint_not_checked),
        ] {
            if let Some(not_checked) = not_checked {
                run.not_checked.get_or_insert(not_checked);
            }
        }
        self.findings.extend(checked.findings);
        if ran_to_its_end {
            self.files_checked.extend(checked.files);
        }
    }
}

/// The project's own extension folders `paths` reach, each by its path in the project: its
/// `node_ui` folder when it holds a manifest, then each view folder that does, by name.
fn the_extension_folders_to_check(
    project_directory: &Path,
    paths: Option<&[String]>,
) -> Vec<String> {
    let mut folders = Vec::new();
    if project_directory
        .join(NODE_UI_DIRECTORY)
        .join(NODE_UI_MANIFEST_FILE_NAME)
        .is_file()
    {
        folders.push(NODE_UI_DIRECTORY.to_owned());
    }
    let mut view_folders: Vec<String> =
        std::fs::read_dir(project_directory.join(PROJECT_VIEWS_DIRECTORY))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().join(TATOLAB_VIEW_MANIFEST_FILE_NAME).is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .map(|name| format!("{PROJECT_VIEWS_DIRECTORY}/{name}"))
            .collect();
    view_folders.sort();
    folders.extend(view_folders);
    folders.retain(|folder| {
        paths.is_none_or(|paths| {
            paths.iter().any(|path| {
                path == folder
                    || path
                        .strip_prefix(folder.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
    });
    folders
}

#[cfg(test)]
mod tests;

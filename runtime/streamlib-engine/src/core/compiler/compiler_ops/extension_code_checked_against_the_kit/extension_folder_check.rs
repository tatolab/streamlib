// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One extension folder of a project checked: its files read and hashed, copied into a scratch
//! check folder beside the kit, tied to its node UI manifest, and run through the compiler and
//! the linter, each within its bound.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use streamlib_project_repository::{
    NODE_UI_MANIFEST_FILE_NAME, content_sha256_of, the_node_ui_manifest_file_read,
};
use streamlib_runtime_client_contract::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};

use super::checker_output_read::{CheckFolderPlaces, TIE_FILE, compiler_findings, linter_findings};
use super::extension_sdk_kit_found::{
    ExtensionSdkKitFound, KIT_CHECK_FOLDER, KIT_LINT_RULES_FILE, KIT_TIE_LINES_FILE,
};
use super::{
    ExtensionCodeCheckKind, ExtensionCodeFinding, ExtensionCodeFindingSeverity,
    ExtensionCodeNotChecked, ExtensionCodeNotCheckedCode, ExtensionFileChecked,
};
use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::compiler::compiler_ops::entry_run_in_the_projects_interpreter::{
    EntryRunBounds, EntryRunInTheProjectsInterpreter, run_an_entry_in_the_projects_interpreter,
};

/// How long the compiler or the linter may run over one folder before it is killed.
const CHECKER_RUN_BOUND: Duration = Duration::from_secs(120);

/// How long a program may take to say its version.
const VERSION_RUN_BOUND: Duration = Duration::from_secs(30);

/// The most of what a checker prints that is read.
const CHECKER_OUTPUT_KEPT_BYTES: usize = 16 * 1024 * 1024;

/// A folder an extension's own installed packages sit in, which is neither checked nor copied.
const INSTALLED_PACKAGES_FOLDER: &str = "node_modules";

/// A folder of vendored code an extension carries as it was published, which the linter leaves
/// out.
const VENDORED_FOLDER: &str = "vendor";

/// Where the check folder resolves the kit.
const KIT_LINK_IN_THE_CHECK_FOLDER: &str = "node_modules/@tatolab/tato-app-view-sdk";

/// The kit's files whose bytes, beside its version, say what a check against it reads.
const KIT_FILES_A_CHECK_IS_KEPT_BY: [&str; 4] = [
    "package.json",
    "tsconfig.preset.json",
    KIT_TIE_LINES_FILE,
    KIT_LINT_RULES_FILE,
];

/// The kit's tie lines: the tie file's header, and the lines written for each control, with
/// `{index}`, `{script}` and `{type}` filled in.
#[derive(Deserialize)]
struct KitTieLines {
    header: String,
    controls: String,
}

/// One extension folder of a project, read and ready to check.
pub(super) struct ExtensionFolderToCheck {
    folder: String,
    folder_directory: PathBuf,
    /// Every file of the folder but its installed packages, by its path in the project.
    files: Vec<ExtensionFileChecked>,
}

/// The kit and the two programs a folder's code is checked with.
pub(super) struct ExtensionCodeCheckersFound<'found> {
    pub(super) kit: &'found ExtensionSdkKitFound,
    pub(super) compiler: &'found Path,
    pub(super) linter: &'found Path,
}

/// What one folder's check found, and which of its two checks did not run to their end.
#[derive(Debug, Clone)]
pub(super) struct ExtensionFolderChecked {
    pub(super) types_not_checked: Option<ExtensionCodeNotChecked>,
    pub(super) lint_not_checked: Option<ExtensionCodeNotChecked>,
    pub(super) findings: Vec<ExtensionCodeFinding>,
    pub(super) files: Vec<ExtensionFileChecked>,
}

impl ExtensionFolderChecked {
    /// Whether both checks ran to their end.
    pub(super) fn ran_to_its_end(&self) -> bool {
        self.types_not_checked.is_none() && self.lint_not_checked.is_none()
    }
}

impl ExtensionFolderToCheck {
    /// The extension folder at `folder`, a path in the project at `project_directory`, with
    /// every file of it read and hashed; or why it cannot be.
    pub(super) fn read(
        project_directory: &Path,
        folder: String,
    ) -> Result<Self, ExtensionCodeNotChecked> {
        let folder_directory = project_directory.join(&folder);
        let not_read = |words: String| ExtensionCodeNotChecked {
            code: ExtensionCodeNotCheckedCode::Refused,
            words,
            folder_being_checked: Some(folder.clone()),
        };
        let mut relative_files = Vec::new();
        every_file_under(&folder_directory, "", &mut relative_files).map_err(not_read)?;
        relative_files.sort();
        let files = relative_files
            .into_iter()
            .map(|relative_file| {
                let bytes =
                    std::fs::read(folder_directory.join(&relative_file)).map_err(|unreadable| {
                        not_read(format!(
                            "`{folder}/{relative_file}` cannot be read: {unreadable}"
                        ))
                    })?;
                Ok(ExtensionFileChecked {
                    path: format!("{folder}/{relative_file}"),
                    content_sha256: content_sha256_of(&bytes),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            folder,
            folder_directory,
            files,
        })
    }

    /// Whether the folder holds a `.ts` file, which only the programs check.
    pub(super) fn holds_code_to_check(&self) -> bool {
        self.relative_files()
            .any(|relative_file| relative_file.ends_with(".ts"))
    }

    /// The key a check of this folder with `checkers` is kept by: the hash of the kit, the
    /// programs, the folder's path and each of its files' bytes.
    pub(super) fn content_key(&self, checkers: &ExtensionCodeCheckersFound<'_>) -> String {
        let ExtensionCodeCheckersFound {
            kit,
            compiler,
            linter,
        } = checkers;
        let mut keyed = Vec::new();
        let mut key_part = |part: &[u8]| {
            keyed.extend_from_slice(&(part.len() as u64).to_le_bytes());
            keyed.extend_from_slice(part);
        };
        key_part(kit.directory.as_os_str().as_encoded_bytes());
        key_part(kit.sdk_version.as_bytes());
        for kit_file in KIT_FILES_A_CHECK_IS_KEPT_BY {
            key_part(&std::fs::read(kit.directory.join(kit_file)).unwrap_or_default());
        }
        key_part(compiler.as_os_str().as_encoded_bytes());
        key_part(linter.as_os_str().as_encoded_bytes());
        key_part(self.folder.as_bytes());
        for file in &self.files {
            key_part(file.path.as_bytes());
            key_part(file.content_sha256.as_bytes());
        }
        content_sha256_of(&keyed)
    }

    /// This folder checked with `checkers` in a scratch check folder, or by its files' kinds
    /// alone when it holds no code the programs check, which is when `checkers` may be `None`.
    pub(super) fn checked_with(
        self,
        checkers: Option<&ExtensionCodeCheckersFound<'_>>,
    ) -> ExtensionFolderChecked {
        let mut checked = ExtensionFolderChecked {
            types_not_checked: None,
            lint_not_checked: None,
            findings: self.findings_of_files_not_checked(),
            files: Vec::new(),
        };
        if let Some(checkers) = checkers.filter(|_| self.holds_code_to_check()) {
            match self.checker_findings(checkers) {
                Ok(CheckerFindings {
                    types,
                    lint,
                    types_not_checked,
                    lint_not_checked,
                }) => {
                    let static_lint_findings = std::mem::take(&mut checked.findings)
                        .into_iter()
                        .partition::<Vec<_>, _>(|finding| {
                            finding.kind == ExtensionCodeCheckKind::Types
                        });
                    checked.findings = static_lint_findings.0;
                    checked.findings.extend(types);
                    checked.findings.extend(static_lint_findings.1);
                    checked.findings.extend(lint);
                    checked.types_not_checked = types_not_checked;
                    checked.lint_not_checked = lint_not_checked;
                }
                Err(not_checked) => {
                    checked.types_not_checked = Some(not_checked.clone());
                    checked.lint_not_checked = Some(not_checked);
                }
            }
        }
        checked.files = self.files;
        checked
    }

    /// Each file of the folder, relative to it.
    fn relative_files(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|file| {
            file.path
                .strip_prefix(self.folder.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(&file.path)
        })
    }

    /// A finding for each file of a kind neither checker reads: a `.tsx` file is refused, and
    /// a `.js`, `.mjs` or `.map` file outside vendored code is told to be `.ts`.
    fn findings_of_files_not_checked(&self) -> Vec<ExtensionCodeFinding> {
        let finding = |kind, finding_code: &str, severity, words: &str, relative_file: &str| {
            ExtensionCodeFinding {
                kind,
                finding_code: finding_code.to_owned(),
                severity,
                words: words.to_owned(),
                path: format!("{}/{relative_file}", self.folder),
                line: None,
                column: None,
            }
        };
        let mut not_checked: Vec<ExtensionCodeFinding> = self
            .relative_files()
            .filter(|relative_file| relative_file.ends_with(".tsx"))
            .map(|relative_file| {
                finding(
                    ExtensionCodeCheckKind::Types,
                    "tsx_not_checked",
                    ExtensionCodeFindingSeverity::Error,
                    "an extension's code is written in .ts; a .tsx file is not checked, and the \
                     Studio does not load one",
                    relative_file,
                )
            })
            .collect();
        not_checked.extend(
            self.relative_files()
                .filter(|relative_file| {
                    !relative_file.starts_with(&format!("{VENDORED_FOLDER}/"))
                        && [".js", ".mjs", ".map"]
                            .iter()
                            .any(|extension| relative_file.ends_with(extension))
                })
                .map(|relative_file| {
                    finding(
                        ExtensionCodeCheckKind::Lint,
                        "not_written_in_ts",
                        ExtensionCodeFindingSeverity::Warning,
                        "an extension's code is written in .ts, and only .ts is checked; \
                         vendored code goes under vendor/",
                        relative_file,
                    )
                }),
        );
        not_checked
    }

    /// What the compiler and the linter found over a scratch check folder of this folder.
    fn checker_findings(
        &self,
        &ExtensionCodeCheckersFound {
            kit,
            compiler,
            linter,
        }: &ExtensionCodeCheckersFound<'_>,
    ) -> Result<CheckerFindings, ExtensionCodeNotChecked> {
        let not_checked = |code, words: String| ExtensionCodeNotChecked {
            code,
            words,
            folder_being_checked: Some(self.folder.clone()),
        };
        let refused = |words: String| not_checked(ExtensionCodeNotCheckedCode::Refused, words);
        let check_folder = tempfile::Builder::new()
            .prefix("tatolab_extension_check-")
            .tempdir()
            .map_err(|unmade| unmade.to_string())
            .and_then(|check_folder| {
                std::fs::set_permissions(
                    check_folder.path(),
                    std::fs::Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE),
                )
                .map(|()| check_folder)
                .map_err(|unset| unset.to_string())
            })
            .map_err(|unmade| {
                refused(format!("no scratch check folder could be made: {unmade}"))
            })?;
        for relative_file in self.relative_files() {
            copy_a_file(
                &self.folder_directory.join(relative_file),
                &check_folder.path().join(relative_file),
            )
            .map_err(refused)?;
        }
        copy_a_folder(&kit.directory.join(KIT_CHECK_FOLDER), check_folder.path())
            .map_err(refused)?;
        link_the_kit(
            &kit.directory,
            &check_folder.path().join(KIT_LINK_IN_THE_CHECK_FOLDER),
        )
        .map_err(refused)?;
        let tie = self
            .write_the_tie_file(kit, check_folder.path())
            .map_err(refused)?;
        let manifest_path = format!("{}/{NODE_UI_MANIFEST_FILE_NAME}", self.folder);
        let places = CheckFolderPlaces {
            folder_path: &self.folder,
            manifest_path: &manifest_path,
            tie_header_line_count: tie.header_line_count,
            tie_lines_per_control: tie.lines_per_control,
            tied_controls: &tie.tied_controls,
        };

        let compiler_run = run_a_checker(
            compiler,
            check_folder.path(),
            &["-p", ".", "--pretty", "false"],
            "a TypeScript type check of a project's extension code",
        );
        let (types, types_not_checked) = match compiler_run {
            Err(why) => (Vec::new(), Some(refused(why))),
            Ok(run) if run.ran_past_its_bound => (
                Vec::new(),
                Some(not_checked(
                    ExtensionCodeNotCheckedCode::RanOutOfTime,
                    format!(
                        "the TypeScript compiler was still checking `{}` after {}s and was \
                         stopped",
                        self.folder,
                        CHECKER_RUN_BOUND.as_secs()
                    ),
                )),
            ),
            Ok(run) => {
                let findings =
                    compiler_findings(&String::from_utf8_lossy(&run.standard_output), &places);
                let reported_an_error = findings
                    .iter()
                    .any(|finding| finding.severity == ExtensionCodeFindingSeverity::Error);
                if run.exited_successfully || reported_an_error {
                    (findings, None)
                } else {
                    (
                        Vec::new(),
                        Some(refused(format!(
                            "the TypeScript compiler exited with {} over `{}` and reported no \
                             error. {}",
                            run.exit_status_rendered, self.folder, run.quoted_standard_error
                        ))),
                    )
                }
            }
        };

        let lint_rules = kit.directory.join(KIT_LINT_RULES_FILE);
        let linter_run = run_a_checker(
            linter,
            check_folder.path(),
            &[
                "-c".as_ref(),
                lint_rules.as_os_str(),
                "--format".as_ref(),
                "json".as_ref(),
                "--ignore-pattern".as_ref(),
                format!("{INSTALLED_PACKAGES_FOLDER}/**").as_ref(),
                "--ignore-pattern".as_ref(),
                format!("{VENDORED_FOLDER}/**").as_ref(),
                ".".as_ref(),
            ],
            "a lint of a project's extension code",
        );
        let (lint, lint_not_checked) = match linter_run {
            Err(why) => (Vec::new(), Some(refused(why))),
            Ok(run) if run.ran_past_its_bound => (
                Vec::new(),
                Some(not_checked(
                    ExtensionCodeNotCheckedCode::RanOutOfTime,
                    format!(
                        "the linter was still reading `{}` after {}s and was stopped",
                        self.folder,
                        CHECKER_RUN_BOUND.as_secs()
                    ),
                )),
            ),
            Ok(run) => {
                match linter_findings(&String::from_utf8_lossy(&run.standard_output), &places) {
                    Ok(findings) => (findings, None),
                    Err(why) => (
                        Vec::new(),
                        Some(refused(format!(
                            "{why}; it exited with {} over `{}`. {}",
                            run.exit_status_rendered, self.folder, run.quoted_standard_error
                        ))),
                    ),
                }
            }
        };
        Ok(CheckerFindings {
            types,
            lint,
            types_not_checked,
            lint_not_checked,
        })
    }

    /// The tie file written into `check_folder` from the folder's node UI manifest with the
    /// kit's tie lines, when the manifest reads and registers a control.
    fn write_the_tie_file(
        &self,
        kit: &ExtensionSdkKitFound,
        check_folder: &Path,
    ) -> Result<TieFileWritten, String> {
        let tie_lines_file = kit.directory.join(KIT_TIE_LINES_FILE);
        let tie_lines: KitTieLines = std::fs::read(&tie_lines_file)
            .map_err(|unreadable| unreadable.to_string())
            .and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|not_tie_lines| not_tie_lines.to_string())
            })
            .map_err(|why| {
                format!(
                    "the kit's {} cannot be read: {why}",
                    tie_lines_file.display()
                )
            })?;
        let controls =
            the_node_ui_manifest_file_read(&self.folder_directory.join(NODE_UI_MANIFEST_FILE_NAME))
                .and_then(|read| read.manifest.ok())
                .map(|manifest| manifest.controls)
                .unwrap_or_default();
        let written = TieFileWritten {
            header_line_count: tie_lines.header.lines().count(),
            lines_per_control: tie_lines.controls.lines().count(),
            tied_controls: controls
                .iter()
                .map(|control| serde_json::to_string(control).unwrap_or_default())
                .collect(),
        };
        if controls.is_empty() {
            return Ok(written);
        }
        let mut tie_file = vec![tie_lines.header];
        for (index, control) in controls.iter().enumerate() {
            let pin_type = control.pin_type.as_written();
            tie_file.push(
                tie_lines
                    .controls
                    .replace("{index}", &index.to_string())
                    .replace("{script}", &control.script)
                    .replace("{type}", &pin_type),
            );
        }
        tie_file.push(String::new());
        std::fs::write(check_folder.join(TIE_FILE), tie_file.join("\n"))
            .map_err(|unwritten| format!("the tie file could not be written: {unwritten}"))?;
        Ok(written)
    }
}

/// What the tie file was written from, as a finding at one of its lines is placed.
struct TieFileWritten {
    header_line_count: usize,
    lines_per_control: usize,
    tied_controls: Vec<String>,
}

/// What both checkers found over one folder, and which did not run to its end.
struct CheckerFindings {
    types: Vec<ExtensionCodeFinding>,
    lint: Vec<ExtensionCodeFinding>,
    types_not_checked: Option<ExtensionCodeNotChecked>,
    lint_not_checked: Option<ExtensionCodeNotChecked>,
}

/// Each regular file under `directory`, relative to the folder being walked as `relative`, its
/// installed packages and every link left out.
fn every_file_under(
    directory: &Path,
    relative: &str,
    files: &mut Vec<String>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|unreadable| format!("{} cannot be listed: {unreadable}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|unreadable| {
            format!("{} cannot be listed: {unreadable}", directory.display())
        })?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let relative_entry = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        if file_type.is_dir() && name != INSTALLED_PACKAGES_FOLDER {
            every_file_under(&entry.path(), &relative_entry, files)?;
        } else if file_type.is_file() {
            files.push(relative_entry);
        }
    }
    Ok(())
}

/// `folder` and each folder it lies in made where missing, each reached by us alone, whatever
/// an iceoryx2 listener bind on another thread does to the umask meanwhile.
fn a_folder_only_we_reach(folder: &Path) -> Result<(), String> {
    create_directory_and_its_missing_parents_at_mode(folder, OWNER_ONLY_DIRECTORY_MODE)
        .map_err(|unmade| format!("{} could not be made: {unmade}", folder.display()))
}

/// `from` copied to `to`, its folders made.
fn copy_a_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        a_folder_only_we_reach(parent)?;
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|uncopied| format!("{} could not be copied: {uncopied}", from.display()))
}

/// Every file under the kit's folder `from` copied under `to`, over what is there.
fn copy_a_folder(from: &Path, to: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    every_file_under(from, "", &mut files)?;
    files
        .iter()
        .try_for_each(|file| copy_a_file(&from.join(file), &to.join(file)))
}

/// `link` made a link to the kit at `kit_directory`.
fn link_the_kit(kit_directory: &Path, link: &Path) -> Result<(), String> {
    if let Some(parent) = link.parent() {
        a_folder_only_we_reach(parent)?;
    }
    std::os::unix::fs::symlink(kit_directory, link).map_err(|unlinked| {
        format!("the kit could not be linked into the check folder: {unlinked}")
    })
}

/// The command that runs `program` with `arguments` in `check_folder`, with none of our
/// variables, every standard stream piped and its lifetime bound to ours.
fn checker_command(
    program: &Path,
    check_folder: &Path,
    arguments: &[impl AsRef<std::ffi::OsStr>],
) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(check_folder)
        .env_clear()
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    command
}

/// `program` run over `check_folder` within its bound, or why it could not be started.
fn run_a_checker(
    program: &Path,
    check_folder: &Path,
    arguments: &[impl AsRef<std::ffi::OsStr>],
    what_runs: &'static str,
) -> Result<EntryRunInTheProjectsInterpreter, String> {
    run_an_entry_in_the_projects_interpreter(
        &mut checker_command(program, check_folder, arguments),
        Vec::new(),
        &EntryRunBounds {
            bound: CHECKER_RUN_BOUND,
            standard_output_kept_bytes: CHECKER_OUTPUT_KEPT_BYTES,
            standard_error_log_label: "extension code check",
            what_runs,
        },
    )
    .map_err(|unstarted| format!("{} could not be started: {unstarted}", program.display()))
}

/// The version `program` reports to `--version`, as `tsc 7.0.2`; or why it reports none.
pub(super) fn the_version_a_program_reports(program: &Path) -> Result<String, String> {
    let program_name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.display().to_string());
    let run = run_an_entry_in_the_projects_interpreter(
        &mut checker_command(program, &std::env::temp_dir(), &["--version"]),
        Vec::new(),
        &EntryRunBounds {
            bound: VERSION_RUN_BOUND,
            standard_output_kept_bytes: 4096,
            standard_error_log_label: "extension code checker version",
            what_runs: "a code checker's version",
        },
    )
    .map_err(|unstarted| format!("{program_name} could not be started: {unstarted}"))?;
    let printed = String::from_utf8_lossy(&run.standard_output);
    let version = printed
        .lines()
        .next()
        .map(|line| {
            line.trim()
                .trim_start_matches("Version")
                .trim_start_matches(':')
                .trim()
        })
        .filter(|version| run.exited_successfully && !version.is_empty())
        .ok_or_else(|| {
            format!(
                "{program_name} --version exited with {} and printed no version",
                run.exit_status_rendered
            )
        })?;
    Ok(format!("{program_name} {version}"))
}

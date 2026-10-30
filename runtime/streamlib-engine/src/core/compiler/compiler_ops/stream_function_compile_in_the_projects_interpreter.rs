// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Compiling a project's stream function in the project's own interpreter.
//!
//! The runtime process never imports a project's code. It starts the
//! project's venv interpreter on `tatolab.stream`'s compile entry, in the
//! project directory, under `-I`: the compile ignores the environment's
//! `PYTHON*` variables and takes its import root from the compile entry. It
//! reads the one JSON document the entry prints on its standard output: the
//! compiled graph, or why the project's code gave none.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::processor_interpreter_describe::PROCESSOR_INTERPRETER_DESCRIBE_BOUND;
use super::processor_interpreter_shutdown_ladder::kill_the_process_group_and_reap_its_leader;
use super::processor_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE, StreamInterpreterExitAwaited,
    spawn_standard_error_reader_keeping_its_tail, spawn_standard_output_reader_keeping_its_head,
    standard_error_tail_as_a_refusal_quotes_it, standard_output_head_as_a_refusal_quotes_it,
    stream_interpreter_command_in_its_project_directory, wait_for_a_stream_interpreter_to_exit,
};
use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::error::{Error, Result};
use crate::core::runtime::{StreamEnvironment, StreamLocation};
use crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind;

/// The module `tatolab.stream` compiles a project's stream function with.
pub(crate) const PROJECT_STREAM_COMPILE_ENTRY_MODULE: &str =
    "tatolab.stream._project_stream_compile_entry";

/// How long one compile may run before its process group is killed and the
/// compile refused: the describe's bound, since both run a project's
/// import-time work.
pub(crate) const STREAM_FUNCTION_COMPILE_BOUND: Duration = PROCESSOR_INTERPRETER_DESCRIBE_BOUND;

/// The verb the compile entry is told it serves, as its refusals spell it.
const COMPILE_ENTRY_VERB: &str = "run";

/// What a compile's standard-error lines are logged under.
const COMPILE_STANDARD_ERROR_LOG_LABEL: &str = "stream-function-compile";

/// How much of a compile's standard output is kept; the rest is read and
/// dropped, so the compile never blocks on a full pipe.
const COMPILE_STANDARD_OUTPUT_KEPT_BYTES: usize = 64 * 1024 * 1024;

/// A compile of a project's stream function in the project's own interpreter:
/// what the project's code gave it, the environment its processor interpreters
/// start in, and what it wrote to its standard error.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StreamFunctionCompileInTheProjectsInterpreterReport {
    /// The graph, or why the project's code gave none.
    pub(crate) what_the_projects_code_gave: WhatTheProjectsCodeGaveTheCompile,
    /// The project directory the compile entry reported, and the project's
    /// venv interpreter.
    pub(crate) stream_environment: StreamEnvironment,
    /// What the compile wrote to its standard error, line by line — the
    /// cross-floor check's warnings among it — for the caller to show its
    /// user; the runtime's log carries the same lines.
    pub(crate) compile_warnings: Vec<String>,
}

/// What a project's code gave a compile of one stream function.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WhatTheProjectsCodeGaveTheCompile {
    /// The stream function compiled to `graph_json`, as the entry printed it.
    Compiled {
        /// The project directory and the stream function's `module:qualname`.
        stream_location: StreamLocation,
        /// The stream graph.
        graph_json: serde_json::Value,
    },
    /// The stream function raised while it compiled.
    StreamFunctionRaised {
        /// The project directory and the stream function's `module:qualname`.
        stream_location: StreamLocation,
        /// Its traceback, the compile entry's own frames left out.
        failed_because: String,
    },
    /// The target named a function the entry does not define as a `@stream`.
    StreamFunctionNotDefined {
        /// The project directory and the `module:name` the function would have.
        stream_location: StreamLocation,
        /// What the entry defines instead, and how to name one.
        not_defined_because: String,
    },
    /// The entry raised before any stream function could be chosen.
    StreamEntryDidNotImport {
        /// Its traceback, the compile entry's own frames left out.
        import_failed_because: String,
    },
}

/// The document the compile entry prints on its standard output: the project
/// directory, then the graph or why the project's code gave none.
#[derive(serde::Deserialize)]
struct ProjectStreamCompileDocument {
    project_directory: PathBuf,
    stream_function: Option<String>,
    stream_graph: Option<serde_json::Value>,
    failed_because: Option<String>,
    not_defined_because: Option<String>,
    import_failed_because: Option<String>,
}

impl ProjectStreamCompileDocument {
    /// What the document says the project's code gave; `None` for a document
    /// holding no one answer.
    fn what_the_projects_code_gave(self) -> Option<WhatTheProjectsCodeGaveTheCompile> {
        let stream_location = |stream_function: String| StreamLocation {
            project_directory: self.project_directory.clone(),
            stream_function,
        };
        match (
            self.stream_function,
            self.stream_graph,
            self.failed_because,
            self.not_defined_because,
            self.import_failed_because,
        ) {
            (Some(stream_function), Some(graph_json), None, None, None) => {
                Some(WhatTheProjectsCodeGaveTheCompile::Compiled {
                    stream_location: stream_location(stream_function),
                    graph_json,
                })
            }
            (Some(stream_function), None, Some(failed_because), None, None) => {
                Some(WhatTheProjectsCodeGaveTheCompile::StreamFunctionRaised {
                    stream_location: stream_location(stream_function),
                    failed_because,
                })
            }
            (Some(stream_function), None, None, Some(not_defined_because), None) => Some(
                WhatTheProjectsCodeGaveTheCompile::StreamFunctionNotDefined {
                    stream_location: stream_location(stream_function),
                    not_defined_because,
                },
            ),
            (None, None, None, None, Some(import_failed_because)) => {
                Some(WhatTheProjectsCodeGaveTheCompile::StreamEntryDidNotImport {
                    import_failed_because,
                })
            }
            _ => None,
        }
    }
}

/// The interpreter a project's streams run in: `<project>/.venv/bin/python`,
/// taken as it is spelled, never resolved through its symlink.
pub(crate) fn the_projects_venv_interpreter(project_directory: &Path) -> PathBuf {
    project_directory.join(".venv").join("bin").join("python")
}

/// Refuse `project_directory` unless it is an absolute path: a project is
/// named by its absolute path.
pub(crate) fn refuse_a_project_directory_not_named_by_its_absolute_path(
    project_directory: &Path,
) -> Result<()> {
    if !project_directory.is_absolute() {
        return Err(Error::Configuration(format!(
            "the project directory `{}` is not an absolute path; a stream is compiled in a \
             project named by its absolute path",
            project_directory.display()
        )));
    }
    Ok(())
}

/// Compile `stream_function` of the project in `project_directory` — or the
/// sole `@stream` in its `stream.py` when none is named — in the project's
/// venv interpreter, answering with the graph or why the project's code gave
/// none. Refused for the caller's own target the entry refuses, and for a
/// compile that never reached the project's answer.
///
/// A machine shutdown requested while it runs kills the compile and refuses it.
pub(crate) fn compile_the_stream_function_in_the_projects_interpreter(
    project_directory: &Path,
    stream_function: Option<&str>,
    lend_directory: &Path,
) -> Result<StreamFunctionCompileInTheProjectsInterpreterReport> {
    compile_the_stream_function_in_the_projects_interpreter_within(
        project_directory,
        stream_function,
        lend_directory,
        STREAM_FUNCTION_COMPILE_BOUND,
        &crate::core::runtime::is_the_machines_shutdown_requested,
    )
}

/// The command that compiles `stream_function` in the project's interpreter.
///
/// Separate from the run so what a compile carries is assertable without
/// starting one.
pub(crate) fn stream_function_compile_command(
    project_directory: &Path,
    stream_function: Option<&str>,
    lend_directory: &Path,
) -> Result<Command> {
    let mut command = stream_interpreter_command_in_its_project_directory(
        &the_projects_venv_interpreter(project_directory),
        project_directory,
        lend_directory,
    )?;
    // `-I` keeps the project directory off `sys.path` until the compile entry
    // has imported what it needs, so a project module named like a
    // standard-library one cannot replace it. It also ignores the `PYTHONPATH`
    // the command builder sets.
    command.args([
        "-I",
        "-m",
        PROJECT_STREAM_COMPILE_ENTRY_MODULE,
        "--verb",
        COMPILE_ENTRY_VERB,
    ]);
    if let Some(stream_function) = stream_function {
        command.arg(stream_function);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    Ok(command)
}

/// [`compile_the_stream_function_in_the_projects_interpreter`], bounded by
/// `compile_bound` and cut short whenever `is_interrupted_by_the_host` reports
/// true.
pub(crate) fn compile_the_stream_function_in_the_projects_interpreter_within(
    project_directory: &Path,
    stream_function: Option<&str>,
    lend_directory: &Path,
    compile_bound: Duration,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<StreamFunctionCompileInTheProjectsInterpreterReport> {
    let project = project_directory.display();
    refuse_a_project_directory_not_named_by_its_absolute_path(project_directory)?;
    let interpreter = the_projects_venv_interpreter(project_directory);
    let interpreter_shown = interpreter.display();
    if is_interrupted_by_the_host() {
        return Err(Error::Runtime(format!(
            "the stream function in {project} was not compiled: the runtime is shutting down"
        )));
    }

    let mut command =
        stream_function_compile_command(project_directory, stream_function, lend_directory)?;
    let mut child =
        spawn_outside_every_iceoryx2_listener_bind(&mut command).map_err(|spawn_failure| {
            Error::Runtime(format!(
                "the project's interpreter `{interpreter_shown}` could not be started in \
                 {project}: {spawn_failure}"
            ))
        })?;
    let standard_output_head = child.stdout.take().map(|standard_output| {
        spawn_standard_output_reader_keeping_its_head(
            standard_output,
            COMPILE_STANDARD_OUTPUT_KEPT_BYTES,
        )
    });
    let standard_error_tail = child.stderr.take().map(|standard_error| {
        spawn_standard_error_reader_keeping_its_tail(
            standard_error,
            COMPILE_STANDARD_ERROR_LOG_LABEL,
        )
    });

    // A compile works for no loaded stream yet, so only the host cuts it short.
    let compile_exit = wait_for_a_stream_interpreter_to_exit(
        &child,
        compile_bound,
        &|| false,
        is_interrupted_by_the_host,
    );
    // Killed whether or not the leader already exited: nothing a compile
    // starts outlives it.
    let exit_status = kill_the_process_group_and_reap_its_leader(&mut child, "the compile");
    let standard_error_text = standard_error_tail
        .map(|tail| tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE))
        .unwrap_or_default();
    let quoted_standard_error =
        standard_error_tail_as_a_refusal_quotes_it(standard_error_text.trim());

    match compile_exit {
        StreamInterpreterExitAwaited::Exited => {}
        StreamInterpreterExitAwaited::BoundElapsed => {
            return Err(Error::Runtime(format!(
                "the stream function in {project} did not finish compiling within {}s, and \
                 `{interpreter_shown}` was killed. Work a module does at import time runs in \
                 the compile — a module that blocks at import blocks it. \
                 {quoted_standard_error}",
                compile_bound.as_secs_f64()
            )));
        }
        StreamInterpreterExitAwaited::ItsStreamsShutdownRequested
        | StreamInterpreterExitAwaited::InterruptedByTheHost => {
            return Err(Error::Runtime(format!(
                "the stream function in {project} was not compiled: the runtime began shutting \
                 down while `{interpreter_shown}` compiled it, and it was killed. \
                 {quoted_standard_error}"
            )));
        }
    }

    let exited_successfully = exit_status.is_some_and(|exit_status| exit_status.success());
    if !exited_successfully {
        let exit_status_rendered = exit_status
            .map(|exit_status| exit_status.to_string())
            .unwrap_or_else(|| "an exit status that could not be collected".to_string());
        if the_compile_entry_could_not_be_imported(&standard_error_text) {
            return Err(Error::Configuration(format!(
                "`{interpreter_shown}` cannot import `tatolab.stream`: add `tatolab-stream` to \
                 the project's dependencies and run `uv sync` in {project}. \
                 {quoted_standard_error}"
            )));
        }
        return Err(Error::Runtime(format!(
            "the stream function in {project} failed to compile: `{interpreter_shown}` exited \
             with {exit_status_rendered}. {quoted_standard_error}"
        )));
    }

    let standard_output_bytes = standard_output_head
        .map(|head| head.bytes_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE))
        .unwrap_or_default();
    if standard_output_bytes.trim_ascii().is_empty() {
        return Err(Error::Runtime(format!(
            "the stream function in {project} exited without compiling: `{interpreter_shown}` \
             exited successfully and printed no stream graph, as an app that exits on purpose \
             before its stream compiles does. {quoted_standard_error}"
        )));
    }
    let standard_output_text = String::from_utf8_lossy(&standard_output_bytes);
    crate::core::graph_snapshot::refuse_an_integer_literal_wider_than_64_bits(
        &standard_output_text,
    )?;
    let not_the_compile_document = |why: &dyn std::fmt::Display| {
        Error::Runtime(format!(
            "the stream function in {project} compiled, and `{interpreter_shown}`'s standard \
             output is not the compile document ({why}); it began {}. What a `sitecustomize`, \
             `usercustomize` or `.pth` hook in the project's environment prints lands ahead of \
             the document.",
            standard_output_head_as_a_refusal_quotes_it(&standard_output_bytes)
        ))
    };
    let compile_document =
        serde_json::from_slice::<ProjectStreamCompileDocument>(&standard_output_bytes)
            .map_err(|not_a_compile_document| not_the_compile_document(&not_a_compile_document))?;
    let names_a_graph_and_no_stream_function =
        compile_document.stream_graph.is_some() && compile_document.stream_function.is_none();
    let project_directory = compile_document.project_directory.clone();
    let Some(what_the_projects_code_gave) = compile_document.what_the_projects_code_gave() else {
        if names_a_graph_and_no_stream_function {
            return Err(Error::Configuration(format!(
                "the stream function in {project} compiled, and the project's `tatolab-stream` \
                 is older than this runtime: its compile document names no `stream_function`, \
                 which the stream's id is computed from. Run `uv sync` in {project} to take a \
                 `tatolab-stream` as new as the runtime"
            )));
        }
        return Err(not_the_compile_document(
            &"it holds no one of a graph, `failed_because`, `not_defined_because` or \
              `import_failed_because`",
        ));
    };
    Ok(StreamFunctionCompileInTheProjectsInterpreterReport {
        what_the_projects_code_gave,
        stream_environment: StreamEnvironment {
            project_directory,
            interpreter,
        },
        compile_warnings: standard_error_text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

/// Whether `standard_error` is the interpreter's own report that it could not
/// find the compile entry — `tatolab.stream` missing from the venv, or one too
/// old to carry the entry.
fn the_compile_entry_could_not_be_imported(standard_error: &str) -> bool {
    standard_error.contains(&format!(
        "Error while finding module specification for '{PROJECT_STREAM_COMPILE_ENTRY_MODULE}'"
    )) || standard_error.contains(&format!(
        "No module named {PROJECT_STREAM_COMPILE_ENTRY_MODULE}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsStr;
    use std::time::Instant;
    use streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode;

    const LEND_DIRECTORY_FOR_TEST: &str = "/opt/tatolab/lib/tatolab/lend";

    /// A project whose `.venv/bin/python` is a shell script that runs `body`
    /// and exits however `body` does.
    struct ProjectWithAStubVenvInterpreter {
        project_directory: tempfile::TempDir,
    }

    impl ProjectWithAStubVenvInterpreter {
        fn running(body: &str) -> Self {
            let project_directory =
                crate::core::test_support::a_temporary_directory_at_owner_only_mode()
                    .expect("a project directory");
            let venv_bin = project_directory.path().join(".venv").join("bin");
            create_directory_and_its_missing_parents_at_mode(&venv_bin, 0o755)
                .expect("the venv's bin directory");
            crate::core::test_support::write_an_executable_script_from_a_child_process(
                &venv_bin.join("python"),
                &format!("#!/bin/sh\n{body}\n"),
            );
            Self { project_directory }
        }

        fn path(&self) -> &Path {
            self.project_directory.path()
        }

        fn compile(
            &self,
            stream_function: Option<&str>,
        ) -> Result<StreamFunctionCompileInTheProjectsInterpreterReport> {
            self.compile_within(stream_function, STREAM_FUNCTION_COMPILE_BOUND)
        }

        fn compile_within(
            &self,
            stream_function: Option<&str>,
            compile_bound: Duration,
        ) -> Result<StreamFunctionCompileInTheProjectsInterpreterReport> {
            compile_the_stream_function_in_the_projects_interpreter_within(
                self.path(),
                stream_function,
                Path::new(LEND_DIRECTORY_FOR_TEST),
                compile_bound,
                &|| false,
            )
        }

        fn refusal_of(&self, stream_function: Option<&str>) -> String {
            match self.compile(stream_function) {
                Ok(compiled) => panic!("the compile was refused, and it returned {compiled:?}"),
                Err(refusal) => refusal.to_string(),
            }
        }
    }

    fn a_compiled_graph() -> serde_json::Value {
        serde_json::json!({
            "stream": "camera-preview",
            "nodes": [{"name": "camera", "type": "tatolab.stream:CameraSource", "config": {}}],
            "links": [],
            "exposed": [{"node": "camera", "port": "video", "level": "private"}],
        })
    }

    /// A shell body that prints the compile document naming `project_directory`.
    fn print_the_compile_document(project_directory: &str) -> String {
        format!(
            "cat <<COMPILED\n{}\nCOMPILED",
            serde_json::json!({
                "stream_graph": a_compiled_graph(),
                "project_directory": project_directory,
                "stream_function": "stream:camera_preview",
            })
        )
    }

    #[test]
    #[serial]
    fn a_compile_returns_the_graph_the_location_and_the_projects_venv_interpreter() {
        // A literal path rather than one built from the shell's `$PWD`: a shell
        // started outside its parent's `PWD` reads `getcwd()`, which on macOS
        // resolves the temporary directory's `/var` symlink to `/private/var`.
        let reported_project_directory = "/srv/the-project-the-compile-entry-reported/src-anchor";
        let project = ProjectWithAStubVenvInterpreter::running(&print_the_compile_document(
            reported_project_directory,
        ));

        let compiled = project
            .compile(None)
            .unwrap_or_else(|refusal| panic!("the stream function compiles: {refusal}"));

        assert_eq!(
            compiled.what_the_projects_code_gave,
            WhatTheProjectsCodeGaveTheCompile::Compiled {
                stream_location: StreamLocation {
                    project_directory: PathBuf::from(reported_project_directory),
                    stream_function: "stream:camera_preview".to_string(),
                },
                graph_json: a_compiled_graph(),
            }
        );
        assert_eq!(
            compiled.stream_environment,
            StreamEnvironment {
                project_directory: PathBuf::from(reported_project_directory),
                interpreter: project.path().join(".venv").join("bin").join("python"),
            },
            "the project directory is the one the compile entry reported, and the \
             interpreter the venv's as spelled"
        );
    }

    /// A shell body that prints `document`, then exits 0 as the compile entry does.
    fn print_the_document(document: serde_json::Value) -> String {
        format!("cat <<'COMPILED'\n{document}\nCOMPILED")
    }

    #[test]
    #[serial]
    fn a_stream_function_that_raised_is_answered_under_its_location_with_its_traceback() {
        let project =
            ProjectWithAStubVenvInterpreter::running(&print_the_document(serde_json::json!({
                "project_directory": "/srv/rig",
                "stream_function": "stream:main",
                "failed_because": "Traceback (most recent call last):\nValueError: bad wiring",
            })));

        let compiled = project
            .compile(None)
            .unwrap_or_else(|refusal| panic!("a raising function is answered: {refusal}"));

        assert_eq!(
            compiled.what_the_projects_code_gave,
            WhatTheProjectsCodeGaveTheCompile::StreamFunctionRaised {
                stream_location: StreamLocation {
                    project_directory: PathBuf::from("/srv/rig"),
                    stream_function: "stream:main".to_string(),
                },
                failed_because: "Traceback (most recent call last):\nValueError: bad wiring"
                    .to_string(),
            }
        );
        assert_eq!(
            compiled.stream_environment.project_directory,
            PathBuf::from("/srv/rig")
        );
    }

    #[test]
    #[serial]
    fn a_named_function_the_entry_does_not_define_is_answered_under_its_location() {
        let project =
            ProjectWithAStubVenvInterpreter::running(&print_the_document(serde_json::json!({
                "project_directory": "/srv/rig",
                "stream_function": "stream:side",
                "not_defined_because": "`stream.py` defines no @stream function named `side`",
            })));

        let compiled = project
            .compile(Some("stream.py:side"))
            .unwrap_or_else(|refusal| panic!("a function not defined is answered: {refusal}"));

        assert_eq!(
            compiled.what_the_projects_code_gave,
            WhatTheProjectsCodeGaveTheCompile::StreamFunctionNotDefined {
                stream_location: StreamLocation {
                    project_directory: PathBuf::from("/srv/rig"),
                    stream_function: "stream:side".to_string(),
                },
                not_defined_because: "`stream.py` defines no @stream function named `side`"
                    .to_string(),
            }
        );
    }

    #[test]
    #[serial]
    fn an_entry_that_did_not_import_is_answered_with_its_traceback() {
        let project =
            ProjectWithAStubVenvInterpreter::running(&print_the_document(serde_json::json!({
                "project_directory": "/srv/rig",
                "import_failed_because": "  File \"stream.py\", line 5\nSyntaxError: expected ':'",
            })));

        let compiled = project
            .compile(None)
            .unwrap_or_else(|refusal| panic!("an import failure is answered: {refusal}"));

        assert_eq!(
            compiled.what_the_projects_code_gave,
            WhatTheProjectsCodeGaveTheCompile::StreamEntryDidNotImport {
                import_failed_because: "  File \"stream.py\", line 5\nSyntaxError: expected ':'"
                    .to_string(),
            }
        );
    }

    #[test]
    #[serial]
    fn a_document_holding_both_a_graph_and_a_failure_is_refused_as_no_compile_document() {
        let project =
            ProjectWithAStubVenvInterpreter::running(&print_the_document(serde_json::json!({
                "project_directory": "/srv/rig",
                "stream_function": "stream:main",
                "stream_graph": a_compiled_graph(),
                "failed_because": "Traceback",
            })));

        let refusal = project.refusal_of(None);

        assert!(refusal.contains("is not the compile document"), "{refusal}");
        assert!(refusal.contains("no one of a graph"), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_compile_document_naming_no_stream_function_is_refused_pointing_at_uv_sync() {
        let project = ProjectWithAStubVenvInterpreter::running(&format!(
            "cat <<COMPILED\n{}\nCOMPILED",
            serde_json::json!({
                "stream_graph": a_compiled_graph(),
                "project_directory": "/srv/an-older-tatolab-stream",
            })
        ));

        let refusal = project
            .compile(None)
            .expect_err("a document naming no stream function is refused")
            .to_string();

        assert!(refusal.contains("older than this runtime"), "{refusal}");
        assert!(refusal.contains("`stream_function`"), "{refusal}");
        assert!(refusal.contains("uv sync"), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_compile_returns_every_line_it_wrote_to_its_standard_error_as_its_warnings() {
        let project = ProjectWithAStubVenvInterpreter::running(&format!(
            "echo '' >&2\n\
             echo 'tatolab: the cross-floor check found 1 thing binding this app to one floor.' >&2\n\
             echo '  processors/effect.py:4: imports `cupy`' >&2\n\
             echo '' >&2\n{}",
            print_the_compile_document("$PWD")
        ));

        let compiled = project
            .compile(None)
            .unwrap_or_else(|refusal| panic!("the stream function compiles: {refusal}"));

        assert_eq!(
            compiled.compile_warnings,
            [
                "tatolab: the cross-floor check found 1 thing binding this app to one floor.",
                "  processors/effect.py:4: imports `cupy`",
            ]
        );
    }

    #[test]
    #[serial]
    fn a_compile_that_writes_nothing_to_its_standard_error_returns_no_warnings() {
        let project = ProjectWithAStubVenvInterpreter::running(&print_the_compile_document("$PWD"));

        let compiled = project
            .compile(None)
            .unwrap_or_else(|refusal| panic!("the stream function compiles: {refusal}"));

        assert_eq!(compiled.compile_warnings, Vec::<String>::new());
    }

    #[test]
    #[serial]
    fn the_compile_runs_the_entry_module_in_the_project_with_the_target_it_was_given() {
        let project = ProjectWithAStubVenvInterpreter::running(&format!(
            "printf '%s\\n' \"$@\" > \"$PWD/arguments\"\nprintf '%s' \"$PYTHONPATH\" > \
             \"$PWD/python-path\"\n{}",
            print_the_compile_document("$PWD")
        ));

        project
            .compile(Some("stream.py:main"))
            .unwrap_or_else(|refusal| panic!("the stream function compiles: {refusal}"));
        assert_eq!(
            std::fs::read_to_string(project.path().join("arguments")).unwrap(),
            "-I\n-m\ntatolab.stream._project_stream_compile_entry\n--verb\nrun\nstream.py:main\n",
            "the arguments land in the project directory, so the script ran there"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("python-path")).unwrap(),
            format!("{LEND_DIRECTORY_FOR_TEST}:{}", project.path().display())
        );

        project
            .compile(None)
            .unwrap_or_else(|refusal| panic!("the stream function compiles: {refusal}"));
        assert_eq!(
            std::fs::read_to_string(project.path().join("arguments")).unwrap(),
            "-I\n-m\ntatolab.stream._project_stream_compile_entry\n--verb\nrun\n",
            "no target leaves the entry to find the sole `@stream`"
        );
    }

    #[test]
    fn the_compile_command_is_the_projects_venv_interpreter_in_the_project_with_the_lend_then_the_project_on_its_python_path()
     {
        let command = stream_function_compile_command(
            Path::new("/home/someone/my_app"),
            Some("mod.sub:fn"),
            Path::new(LEND_DIRECTORY_FOR_TEST),
        )
        .expect("the test directories hold no path-list separator");

        assert_eq!(
            command.get_program(),
            OsStr::new("/home/someone/my_app/.venv/bin/python")
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-I",
                "-m",
                PROJECT_STREAM_COMPILE_ENTRY_MODULE,
                "--verb",
                "run",
                "mod.sub:fn"
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new("/home/someone/my_app"))
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new("PYTHONPATH"))
                .and_then(|(_, value)| value),
            Some(OsStr::new(
                "/opt/tatolab/lib/tatolab/lend:/home/someone/my_app"
            ))
        );
    }

    #[test]
    #[serial]
    fn a_compile_that_exits_non_zero_is_refused_quoting_its_traceback() {
        let project = ProjectWithAStubVenvInterpreter::running(
            "echo 'error: stream.py failed' >&2\necho 'Traceback (most recent call last):' >&2\n\
             echo '  File \"stream.py\", line 3, in main' >&2\n\
             echo \"NameError: name 'cmaera' is not defined\" >&2\nexit 1",
        );

        let refusal = project.refusal_of(None);

        assert!(refusal.contains("failed to compile"), "{refusal}");
        assert!(
            refusal.contains("Traceback (most recent call last):\n  File \"stream.py\", line 3"),
            "{refusal}"
        );
        assert!(
            refusal.contains("NameError: name 'cmaera' is not defined"),
            "{refusal}"
        );
    }

    #[test]
    #[serial]
    fn a_compile_that_exits_successfully_printing_nothing_is_refused_saying_the_function_exited_without_compiling()
     {
        let project = ProjectWithAStubVenvInterpreter::running("exit 0");

        let refusal = project.refusal_of(None);

        assert!(refusal.contains("exited without compiling"), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_venv_that_cannot_import_tatolab_stream_is_refused_naming_tatolab_stream_and_uv_sync() {
        let project = ProjectWithAStubVenvInterpreter::running(
            "echo \"$0: Error while finding module specification for \
             'tatolab.stream._project_stream_compile_entry' (ModuleNotFoundError: No module \
             named 'tatolab')\" >&2\nexit 1",
        );

        let refusal = project.refusal_of(None);

        assert!(
            refusal.contains("cannot import `tatolab.stream`: add `tatolab-stream`"),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("run `uv sync` in {}", project.path().display())),
            "{refusal}"
        );
    }

    #[test]
    #[serial]
    fn a_standard_output_that_is_not_the_compile_document_is_refused_quoting_its_head() {
        let project = ProjectWithAStubVenvInterpreter::running("echo 'hello from sitecustomize'");

        let refusal = project.refusal_of(None);

        assert!(refusal.contains("is not the compile document"), "{refusal}");
        assert!(refusal.contains("hello from sitecustomize"), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_compile_past_its_bound_is_killed_and_refused_naming_the_bound() {
        let project = ProjectWithAStubVenvInterpreter::running("sleep 30");
        let started = Instant::now();

        let refusal = project
            .compile_within(None, Duration::from_millis(500))
            .expect_err("a compile past its bound is refused")
            .to_string();

        assert!(
            refusal.contains("did not finish compiling within 0.5s"),
            "{refusal}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the compile was killed at its bound, not waited for"
        );
    }

    #[test]
    #[serial]
    fn a_compile_the_host_interrupts_is_killed_and_refused_saying_the_runtime_is_shutting_down() {
        let project = ProjectWithAStubVenvInterpreter::running("sleep 30");
        let started = Instant::now();
        let interrupted_after = started + Duration::from_millis(200);

        let refusal = compile_the_stream_function_in_the_projects_interpreter_within(
            project.path(),
            None,
            Path::new(LEND_DIRECTORY_FOR_TEST),
            STREAM_FUNCTION_COMPILE_BOUND,
            &|| Instant::now() >= interrupted_after,
        )
        .expect_err("an interrupted compile is refused")
        .to_string();

        assert!(refusal.contains("began shutting down"), "{refusal}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}

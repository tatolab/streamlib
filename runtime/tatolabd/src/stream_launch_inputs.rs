// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What `tatolabd`'s flags name, read and checked before the engine exists.

use std::path::{Path, PathBuf};

use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::runtime::StreamEnvironment;

use crate::tatolabd_command_line::TatolabdCommandLine;

/// The stream's graph and the environment its processor interpreters start in.
pub(crate) struct StreamLaunchInputs {
    pub(crate) stream_graph: GraphSnapshot,
    pub(crate) stream_environment: StreamEnvironment,
}

impl StreamLaunchInputs {
    /// Read the graph file and check the project and interpreter, refusing the
    /// first that is wrong by name.
    pub(crate) fn read_from(command_line: &TatolabdCommandLine) -> Result<Self, String> {
        let stream_graph = read_the_stream_graph(&command_line.stream_graph)?;
        let project_directory =
            the_absolute_path_of("--project", &command_line.project).and_then(|project| {
                if project.is_dir() {
                    Ok(project)
                } else {
                    Err(format!(
                        "--project {} is not a directory",
                        command_line.project.display()
                    ))
                }
            })?;
        let interpreter = the_absolute_path_of("--interpreter", &command_line.interpreter)
            .and_then(refuse_an_interpreter_that_cannot_be_run)?;
        Ok(Self {
            stream_graph,
            stream_environment: StreamEnvironment {
                project_directory,
                interpreter,
            },
        })
    }
}

fn read_the_stream_graph(stream_graph_file: &Path) -> Result<GraphSnapshot, String> {
    let stream_graph_json = std::fs::read_to_string(stream_graph_file).map_err(|unreadable| {
        format!(
            "--stream-graph {} cannot be read: {unreadable}",
            stream_graph_file.display()
        )
    })?;
    GraphSnapshot::from_json_str(&stream_graph_json).map_err(|not_a_graph| {
        format!(
            "--stream-graph {} is not a graph this runtime loads: {not_a_graph}",
            stream_graph_file.display()
        )
    })
}

/// `path` made absolute against the working directory, but never
/// canonicalized: a venv's interpreter is a symlink whose own path is what
/// makes it that venv's.
fn the_absolute_path_of(flag: &str, path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|cannot_be_made_absolute| {
        format!(
            "{flag} {} cannot be made absolute: {cannot_be_made_absolute}",
            path.display()
        )
    })
}

fn refuse_an_interpreter_that_cannot_be_run(interpreter: PathBuf) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;

    let interpreter_metadata = std::fs::metadata(&interpreter).map_err(|not_there| {
        format!(
            "--interpreter {} does not exist: {not_there}",
            interpreter.display()
        )
    })?;
    if !interpreter_metadata.is_file() {
        return Err(format!(
            "--interpreter {} is not a file",
            interpreter.display()
        ));
    }
    if interpreter_metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "--interpreter {} is not executable",
            interpreter.display()
        ));
    }
    Ok(interpreter)
}

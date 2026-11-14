// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A test's project on disk, so every stream a test loads is a stream of a real project.

use std::path::Path;

use streamlib_project_repository::{PROJECT_FILE_NAME, TatolabProjectFile};
use uuid::Uuid;

use super::{OptionsForLoadingOneStream, StreamEnvironment, StreamLocation};

/// The name a test's project carries in its `tatolab-project.json`.
pub const THE_NAME_OF_A_PROJECT_ON_DISK_FOR_A_TEST: &str = "test-project";

/// The options loading the stream `stream_name` of the project in `project_directory` under
/// the id `stream_name`, writing the project's `tatolab-project.json` when it holds none.
pub fn options_loading_a_stream_of_the_test_project_at(
    project_directory: &Path,
    stream_name: &str,
) -> OptionsForLoadingOneStream {
    OptionsForLoadingOneStream::at_stream_location(
        stream_name,
        the_location_of_a_stream_of_the_test_project_at(project_directory, stream_name),
    )
}

/// Where the stream `stream_name` of the project in `project_directory` lives, writing the
/// project's `tatolab-project.json` when it holds none; its stream asset's UUID is derived
/// from the project's and the stream's name.
pub fn the_location_of_a_stream_of_the_test_project_at(
    project_directory: &Path,
    stream_name: &str,
) -> StreamLocation {
    let project = the_test_project_at(project_directory);
    StreamLocation {
        project_directory: project_directory.to_path_buf(),
        project,
        stream_asset: Uuid::new_v5(&project, stream_name.as_bytes()),
    }
}

fn the_test_project_at(project_directory: &Path) -> Uuid {
    if let Ok(project_file) = TatolabProjectFile::read_from_project_directory(project_directory) {
        return project_file.project;
    }
    let project_file =
        TatolabProjectFile::of_a_new_project(THE_NAME_OF_A_PROJECT_ON_DISK_FOR_A_TEST);
    std::fs::create_dir_all(project_directory).expect("a test project's directory");
    std::fs::write(
        project_directory.join(PROJECT_FILE_NAME),
        project_file
            .canonical_bytes()
            .expect("a test project's file serializes"),
    )
    .expect("a test project's tatolab-project.json");
    project_file.project
}

/// The options loading the stream `stream_name` of the test project `stream_environment`
/// lives in, under the id `stream_name`, starting its processor interpreters there.
pub fn options_loading_a_stream_of_the_test_project_in(
    stream_environment: StreamEnvironment,
    stream_name: &str,
) -> OptionsForLoadingOneStream {
    options_loading_a_stream_of_the_test_project_at(
        &stream_environment.project_directory,
        stream_name,
    )
    .in_stream_environment(stream_environment)
}

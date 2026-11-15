// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project imported from a zip: loaded and run as a project of its own, and refused over a folder
//! that exists.

use super::*;

/// Each file of `directory` outside its `.venv`, zipped into `archive` under `top_folder/`, as a
/// GitHub archive wraps a repository.
fn zip_the_project_under_a_top_folder(directory: &Path, archive: &Path, top_folder: &str) {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::fs::File::create(archive).expect("the archive"));
    let mut directories = vec![directory.to_path_buf()];
    while let Some(reading) = directories.pop() {
        for entry in std::fs::read_dir(&reading).expect("the folder reads") {
            let entry = entry.expect("the entry reads");
            if entry.file_name() == ".venv" {
                continue;
            }
            if entry.file_type().expect("the entry's type").is_dir() {
                directories.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(directory)
                .expect("the entry is in the project")
                .to_string_lossy()
                .into_owned();
            writer
                .start_file(
                    format!("{top_folder}/{relative}"),
                    zip::write::SimpleFileOptions::default(),
                )
                .expect("the entry starts");
            writer
                .write_all(&std::fs::read(entry.path()).expect("the file reads"))
                .expect("the entry is written");
        }
    }
    writer.finish().expect("the archive finishes");
}

/// A project zipped as a GitHub archive, unpacked by `import_project` into `project_directory`,
/// with the stub venv a run finds there: what it answered.
async fn a_project_imported_from_a_zip_into(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    zipped: &ProjectWithGraphDocuments,
    project_directory: &Path,
) -> Value {
    let archive = project_directory.with_extension("zip");
    zip_the_project_under_a_top_folder(zipped.path(), &archive, "cell-main");
    let imported = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "import_project",
            json!({ "source": archive, "project_directory": project_directory }),
        )
        .await,
    )
    .expect("the zip is imported");
    let interpreter = project_directory.join(".venv/bin/python");
    std::fs::create_dir_all(interpreter.parent().expect("the venv's bin"))
        .expect("the venv's bin directory");
    write_an_executable_script_from_a_child_process(
        &interpreter,
        STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE,
    );
    imported
}

/// A project unpacked by `import_project` loads its stream under the new project's UUID and
/// folder, never the zipped project's, so the same zip imported twice is two streams.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_imported_from_a_zip_of_one_loads_its_stream_as_a_project_of_its_own() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let zipped = ProjectWithGraphDocuments::holding(the_graph_of_main_with_a_node_uuid());
    let imported_under = tempfile::tempdir().expect("a folder to import into");
    let imported_under = std::fs::canonicalize(imported_under.path()).expect("the folder resolves");
    let project_directory = imported_under.join("cell");

    let imported = a_project_imported_from_a_zip_into(
        operations_on_the_loaded_streams,
        &zipped,
        &project_directory,
    )
    .await;
    let finished =
        the_run_of_the_project_once_finished(operations_on_the_loaded_streams, &project_directory)
            .await;

    assert_eq!(imported["project_directory"], json!(project_directory));
    assert_eq!(imported["name"], TEST_PROJECT_NAME);
    assert_ne!(imported["project"], json!(zipped.project.to_string()));
    assert!(
        imported["commit"]
            .as_str()
            .is_some_and(|commit| !commit.is_empty())
    );
    let stream_id = the_one_stream_of(&finished)["stream"]
        .as_str()
        .expect("the stream's id")
        .to_owned();
    assert_ne!(stream_id, zipped.stream_id_of("main"));
    let listing = the_listing_of(operations_on_the_loaded_streams, &stream_id).await;
    assert_eq!(
        listing["location"],
        json!({
            "project_directory": project_directory,
            "project": imported["project"],
            "stream_asset": zipped.stream_asset_of("main"),
        }),
        "{listing}"
    );
}

/// A project unpacked by `import_project` runs its stream.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_project_imported_from_a_zip_of_one_runs_its_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let zipped = ProjectWithGraphDocuments::holding(the_graph_of_main_with_a_node_uuid());
    let imported_under = tempfile::tempdir().expect("a folder to import into");
    let imported_under = std::fs::canonicalize(imported_under.path()).expect("the folder resolves");
    let project_directory = imported_under.join("cell");

    a_project_imported_from_a_zip_into(
        operations_on_the_loaded_streams,
        &zipped,
        &project_directory,
    )
    .await;
    the_run_of_the_project_once_finished(operations_on_the_loaded_streams, &project_directory)
        .await;
    let finished = the_project_action_once_finished(
        "start_playback",
        operations_on_the_loaded_streams,
        &project_directory,
    )
    .await;

    assert_eq!(finished["state"], "done", "{finished}");
    assert_eq!(
        the_one_stream_of(&finished)["state"],
        "running",
        "{finished}"
    );
}

/// `import_project` into a folder that already exists is refused in the runtime's own words.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_imported_into_a_folder_that_exists_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let zipped = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let unpacked_under = tempfile::tempdir().expect("a folder to import into");
    let archive = unpacked_under.path().join("cell-main.zip");
    zip_the_project_under_a_top_folder(zipped.path(), &archive, "cell-main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "import_project",
            json!({ "source": archive, "project_directory": unpacked_under.path() }),
        )
        .await,
    )
    .expect_err("an existing folder is refused");

    assert!(
        refusal.contains("import_project failed") && refusal.contains("already exists"),
        "{refusal}"
    );
}

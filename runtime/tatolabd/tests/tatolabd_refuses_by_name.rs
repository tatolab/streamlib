// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd`'s arguments and refusals: each wrong input is refused naming
//! what is wrong, with status 1 and nothing on standard output. An interrupt
//! before the stream starts is a shutdown, not a refusal.

mod common;

use std::path::Path;
use std::process::Output;
use std::time::Duration;

use common::{
    SpawnedTatolabd, TatolabdRunState, TemporaryRuntimeUnit,
    an_executable_standing_in_for_the_interpreter, run_to_exit_within,
};
use streamlib::sdk::processor_interpreter::lend_directory_in_the_runtime_unit;

const A_REFUSAL_BEFORE_ANY_STREAM_RUNS_EXITS_WITHIN: Duration = Duration::from_secs(60);

/// A graph of one node, valid as JSON and as a graph.
fn a_graph_of_one_test_pattern() -> serde_json::Value {
    serde_json::json!({
        "stream": "main",
        "nodes": [{"name": "testpattern", "type": "tatolab.stream:TestPatternSource"}],
    })
}

fn run_tatolabd(
    tatolabd: &Path,
    run_state: &TatolabdRunState,
    stream_graph_file: &Path,
    project_directory: &Path,
    interpreter: &Path,
    extra_environment: &[(&str, &str)],
) -> Output {
    let mut tatolabd_command = run_state.tatolabd_command(tatolabd);
    tatolabd_command
        .arg("--stream-graph")
        .arg(stream_graph_file)
        .arg("--project")
        .arg(project_directory)
        .arg("--interpreter")
        .arg(interpreter);
    for (name, value) in extra_environment {
        tatolabd_command.env(name, value);
    }
    run_to_exit_within(
        tatolabd_command,
        A_REFUSAL_BEFORE_ANY_STREAM_RUNS_EXITS_WITHIN,
    )
}

/// The refusal on standard error, after asserting it exited 1 with nothing on
/// standard output.
fn the_refusal_of(output: &Output) -> String {
    let standard_error = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(1),
        "a refusal exits 1, got {}:\n{standard_error}",
        output.status
    );
    assert!(
        output.stdout.is_empty(),
        "tatolabd's standard output carries nothing, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    standard_error
}

#[test]
fn each_missing_flag_is_a_usage_error_naming_it() {
    let every_flag = [
        ("--stream-graph", "graph.json"),
        ("--project", "."),
        ("--interpreter", "/bin/sh"),
    ];
    for (missing_flag, _) in every_flag {
        let mut tatolabd_command = std::process::Command::new(env!("CARGO_BIN_EXE_tatolabd"));
        for (flag, value) in every_flag {
            if flag != missing_flag {
                tatolabd_command.args([flag, value]);
            }
        }
        let output = tatolabd_command.output().expect("tatolabd runs");
        let standard_error = String::from_utf8_lossy(&output.stderr);

        assert_eq!(
            output.status.code(),
            Some(2),
            "missing {missing_flag}: {standard_error}"
        );
        assert!(
            standard_error.contains(missing_flag),
            "missing {missing_flag}: {standard_error}"
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn a_tatolabd_outside_a_runtime_unit_is_refused_naming_where_it_looked_for_the_lend() {
    let runtime_unit = TemporaryRuntimeUnit::without_a_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.write_stream_graph(&a_graph_of_one_test_pattern());

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[],
    ));

    let looked_for_the_lend_at = lend_directory_in_the_runtime_unit(
        &runtime_unit
            .runtime_unit_root()
            .canonicalize()
            .expect("the runtime unit's root exists"),
    );
    assert!(
        refusal.contains(&format!("no lend at {}", looked_for_the_lend_at.display())),
        "{refusal}"
    );
}

#[test]
fn a_missing_graph_file_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let missing_graph_file = run_state.path().join("no-such-graph.json");

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &missing_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[],
    ));

    assert!(
        refusal.contains(&format!(
            "--stream-graph {} cannot be read",
            missing_graph_file.display()
        )),
        "{refusal}"
    );
}

#[test]
fn a_graph_that_is_not_json_is_refused_naming_the_file_and_the_parse_error() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.path().join("stream-graph.json");
    std::fs::write(&stream_graph_file, "this is not JSON").unwrap();

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[],
    ));

    assert!(
        refusal.contains(&stream_graph_file.display().to_string()),
        "{refusal}"
    );
    assert!(refusal.contains("does not parse"), "{refusal}");
}

#[test]
fn a_graph_key_the_loader_does_not_know_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let mut stream_graph = a_graph_of_one_test_pattern();
    stream_graph["linkz"] = serde_json::json!([]);
    let stream_graph_file = run_state.write_stream_graph(&stream_graph);

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[],
    ));

    assert!(refusal.contains("linkz"), "{refusal}");
}

#[test]
fn a_project_that_is_not_a_directory_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.write_stream_graph(&a_graph_of_one_test_pattern());

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &stream_graph_file,
        &an_executable_standing_in_for_the_interpreter(),
        &[],
    ));

    assert!(
        refusal.contains(&format!(
            "--project {} is not a directory",
            stream_graph_file.display()
        )),
        "{refusal}"
    );
}

#[test]
fn an_interpreter_that_does_not_exist_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.write_stream_graph(&a_graph_of_one_test_pattern());
    let missing_interpreter = run_state.project_directory().join(".venv/bin/python");

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &missing_interpreter,
        &[],
    ));

    assert!(
        refusal.contains(&format!(
            "--interpreter {} does not exist",
            missing_interpreter.display()
        )),
        "{refusal}"
    );
}

#[test]
fn an_interpreter_that_is_not_executable_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.write_stream_graph(&a_graph_of_one_test_pattern());

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &stream_graph_file,
        &[],
    ));

    assert!(
        refusal.contains(&format!(
            "--interpreter {} is not executable",
            stream_graph_file.display()
        )),
        "{refusal}"
    );
}

#[test]
fn a_graph_holding_no_node_is_refused_naming_the_stream() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file =
        run_state.write_stream_graph(&serde_json::json!({"stream": "main", "nodes": []}));

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[],
    ));

    assert!(
        refusal.contains("the stream `main` holds no node"),
        "{refusal}"
    );
}

/// A graph that loads is logged loaded, under the stream's name as the engine
/// casts it and with its own nodes counted, before the engine starts — here to
/// be refused at the GPU, because the Vulkan loader is left no driver.
#[test]
fn a_graph_that_loads_is_logged_loaded_before_the_engine_starts() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.write_stream_graph(&serde_json::json!({
        "stream": "Front Camera",
        "nodes": [{"name": "testpattern", "type": "tatolab.stream:TestPatternSource"}],
    }));
    let no_vulkan_driver_file = run_state.path().join("no-vulkan-driver-here.json");
    let no_vulkan_driver_file = no_vulkan_driver_file.to_string_lossy();

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[
            ("VK_DRIVER_FILES", &no_vulkan_driver_file),
            ("VK_ICD_FILENAMES", &no_vulkan_driver_file),
        ],
    ));

    assert!(
        refusal.contains("the stream `front-camera` loaded with 1 nodes"),
        "{refusal}"
    );
}

/// Under `STREAMLIB_QUIET` no log mirror carries an engine refusal, so the
/// refusal still ends standard error as `tatolabd`'s own line.
#[test]
fn a_refusal_after_the_engine_is_built_is_named_on_standard_error_under_streamlib_quiet() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file =
        run_state.write_stream_graph(&serde_json::json!({"stream": "main", "nodes": []}));

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[("STREAMLIB_QUIET", "1")],
    ));

    let last_line = refusal.lines().last().unwrap_or_default();
    assert!(last_line.starts_with("tatolabd: "), "{refusal}");
    assert!(
        last_line.contains("the stream `main` holds no node"),
        "{refusal}"
    );
}

/// An interrupt while a describe runs kills the describe's process group and
/// ends the run without starting the stream: a shutdown, not a refusal.
#[test]
fn an_interrupt_while_the_graph_loads_ends_the_describe_and_exits_zero() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.write_stream_graph(&serde_json::json!({
        "stream": "main",
        "nodes": [{"name": "slow", "type": "slow_to_describe_module:SlowToDescribeNode"}],
    }));
    let describe_process_id_file = run_state.path().join("describe-process-id");
    let interpreter_that_never_finishes_describing = run_state.path().join("python");
    std::fs::write(
        &interpreter_that_never_finishes_describing,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 60\n",
            describe_process_id_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &interpreter_that_never_finishes_describing,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let mut tatolabd_command = run_state.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command
        .arg("--stream-graph")
        .arg(&stream_graph_file)
        .arg("--project")
        .arg(run_state.project_directory())
        .arg("--interpreter")
        .arg(&interpreter_that_never_finishes_describing);
    let mut spawned = SpawnedTatolabd::spawn(tatolabd_command);
    let describe_process_id = the_process_id_written_to(
        &describe_process_id_file,
        A_REFUSAL_BEFORE_ANY_STREAM_RUNS_EXITS_WITHIN,
    );

    spawned.deliver(libc::SIGINT);
    let exit_status = spawned.wait_for_exit_within(Duration::from_secs(20));

    let standard_error = spawned.standard_error();
    assert_eq!(exit_status.code(), Some(0), "{standard_error}");
    assert!(spawned.standard_output().is_empty());
    assert!(
        standard_error.contains("so the stream was never started"),
        "{standard_error}"
    );
    // SAFETY: `kill` reads only its two integer arguments.
    let describe_is_alive = unsafe { libc::kill(describe_process_id, 0) } == 0;
    assert!(!describe_is_alive, "the describe outlived the run");
}

/// Wait for a process id written to `process_id_file`, or panic after `budget`.
fn the_process_id_written_to(process_id_file: &Path, budget: Duration) -> i32 {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if let Some(process_id) = std::fs::read_to_string(process_id_file)
            .ok()
            .and_then(|written| written.trim().parse().ok())
        {
            return process_id;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no process id was written to {} within {budget:?}",
            process_id_file.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_runtime_name_that_is_not_one_address_chunk_is_refused_naming_the_character() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let stream_graph_file = run_state.write_stream_graph(&a_graph_of_one_test_pattern());

    let refusal = the_refusal_of(&run_tatolabd(
        &runtime_unit.tatolabd,
        &run_state,
        &stream_graph_file,
        &run_state.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        &[("STREAMLIB_RUNTIME_NAME", "a/b")],
    ));

    assert!(refusal.contains("STREAMLIB_RUNTIME_NAME"), "{refusal}");
    assert!(refusal.contains("'/'"), "{refusal}");
}

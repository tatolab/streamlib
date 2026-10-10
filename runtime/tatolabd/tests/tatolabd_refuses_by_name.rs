// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd`'s refusals: each is named on standard error, with status 1 and
//! nothing on standard output. Built with
//! `--features machine-directories-under-a-test-root`, so no run touches the
//! real machine's lock, runtime directory or state directory.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::{
    SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit, run_to_exit_within,
    the_refusal_of,
};
use streamlib::sdk::processor_interpreter::lend_directory_in_the_runtime_unit;

const A_REFUSAL_EXITS_WITHIN: Duration = Duration::from_secs(60);

fn run_tatolabd_to_its_refusal(
    runtime_unit: &TemporaryRuntimeUnit,
    machine_root: &TatolabTestMachineRoot,
    extra_environment: &[(&str, &str)],
) -> String {
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    for (name, value) in extra_environment {
        tatolabd_command.env(name, value);
    }
    the_refusal_of(&run_to_exit_within(
        tatolabd_command,
        A_REFUSAL_EXITS_WITHIN,
    ))
}

#[test]
fn a_tatolabd_outside_a_runtime_unit_is_refused_naming_where_it_looked_for_the_lend() {
    let runtime_unit = TemporaryRuntimeUnit::without_a_lend();
    let machine_root = TatolabTestMachineRoot::new();

    let refusal = run_tatolabd_to_its_refusal(&runtime_unit, &machine_root, &[]);

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
fn a_test_build_without_its_machine_root_is_refused_naming_the_variable() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env_remove("TATOLAB_TEST_MACHINE_ROOT");

    let refusal = the_refusal_of(&run_to_exit_within(
        tatolabd_command,
        A_REFUSAL_EXITS_WITHIN,
    ));

    assert!(
        refusal.contains("TATOLAB_TEST_MACHINE_ROOT is not set"),
        "{refusal}"
    );
}

/// The kernel names the first runtime to the second: its user, its pid and
/// its executable.
#[test]
fn a_second_tatolabd_on_the_machine_is_refused_naming_the_first_ones_user_pid_and_executable() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut first_runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );

    let refusal = run_tatolabd_to_its_refusal(&runtime_unit, &machine_root, &[]);

    assert!(
        refusal.contains("another runtime holds this machine"),
        "{refusal}"
    );
    assert!(
        refusal.contains(&format!("pid {}", first_runtime.process_id())),
        "{refusal}"
    );
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    let this_uid = unsafe { libc::getuid() };
    assert!(refusal.contains(&format!("uid {this_uid}")), "{refusal}");
    if let Some(this_user_name) = the_user_name_of(this_uid) {
        assert!(
            refusal.contains(&format!("user {this_user_name}")),
            "{refusal}"
        );
    }
    let first_runtimes_executable = runtime_unit
        .tatolabd
        .canonicalize()
        .expect("the first runtime's executable exists");
    assert!(
        refusal.contains(&first_runtimes_executable.display().to_string()),
        "{refusal}"
    );
    first_runtime.interrupt_and_expect_a_clean_exit();
}

/// A local API socket another live process answers on refuses the start
/// naming the socket: a runtime nobody can reach over its local API does not
/// run.
#[test]
fn a_local_api_socket_a_live_process_holds_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let _live_holder = a_live_process_holding_the_local_api_socket(&machine_root);

    let refusal = run_tatolabd_to_its_refusal(&runtime_unit, &machine_root, &[]);

    let last_line = refusal.lines().last().unwrap_or_default();
    assert!(
        last_line.contains(&format!(
            "{} is already bound by a live process",
            machine_root.local_api_socket_path().display()
        )),
        "{refusal}"
    );
    assert!(machine_root.local_api_socket_path().exists());
}

/// Under `STREAMLIB_QUIET` no log mirror carries an engine refusal, so the
/// refusal still ends standard error as `tatolabd`'s own line.
#[test]
fn a_refusal_after_the_engine_is_built_is_named_on_standard_error_under_streamlib_quiet() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let _live_holder = a_live_process_holding_the_local_api_socket(&machine_root);

    let refusal =
        run_tatolabd_to_its_refusal(&runtime_unit, &machine_root, &[("STREAMLIB_QUIET", "1")]);

    let last_line = refusal.lines().last().unwrap_or_default();
    assert!(last_line.starts_with("tatolabd: "), "{refusal}");
    assert!(
        last_line.contains("is already bound by a live process"),
        "{refusal}"
    );
}

#[test]
fn a_state_directory_path_that_is_not_a_directory_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    std::fs::write(machine_root.state_directory(), "not a directory").unwrap();

    let refusal = run_tatolabd_to_its_refusal(&runtime_unit, &machine_root, &[]);

    assert!(
        refusal.contains(&format!(
            "the Tatolab state directory {} exists and is not a directory",
            machine_root.state_directory().display()
        )),
        "{refusal}"
    );
}

#[test]
fn a_kept_streams_path_that_is_not_a_directory_is_refused_naming_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    std::fs::create_dir_all(machine_root.state_directory()).unwrap();
    std::fs::set_permissions(
        machine_root.state_directory(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(machine_root.kept_streams_directory(), "not a directory").unwrap();

    let refusal = run_tatolabd_to_its_refusal(&runtime_unit, &machine_root, &[]);

    assert!(
        refusal.contains("the runtime cannot keep streams"),
        "{refusal}"
    );
    assert!(
        refusal.contains(&machine_root.kept_streams_directory().display().to_string()),
        "{refusal}"
    );
}

#[test]
fn a_runtime_name_that_is_not_one_address_chunk_is_refused_naming_the_character() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();

    let refusal = run_tatolabd_to_its_refusal(
        &runtime_unit,
        &machine_root,
        &[("STREAMLIB_RUNTIME_NAME", "a/b")],
    );

    assert!(refusal.contains("STREAMLIB_RUNTIME_NAME"), "{refusal}");
    assert!(refusal.contains("'/'"), "{refusal}");
}

/// A listener bound at the machine's local API socket, in a runtime directory
/// created owner-only as the runtime creates it.
fn a_live_process_holding_the_local_api_socket(
    machine_root: &TatolabTestMachineRoot,
) -> std::os::unix::net::UnixListener {
    std::fs::create_dir_all(machine_root.runtime_directory()).unwrap();
    std::fs::set_permissions(
        machine_root.runtime_directory(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::os::unix::net::UnixListener::bind(machine_root.local_api_socket_path()).unwrap()
}

/// The passwd entry's name for `uid`, when it has one.
fn the_user_name_of(uid: u32) -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage or null; it is
    // read at once, before any other call could overwrite it.
    unsafe {
        let passwd_entry = libc::getpwuid(uid);
        if passwd_entry.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr((*passwd_entry).pw_name)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

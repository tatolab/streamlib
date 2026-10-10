// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd`: the machine's runtime. Holds the machine runtime lock, serves
//! the local API at its fixed socket, hosts every stream loaded into it and
//! re-loads the kept ones at its start, in the foreground and with no Python
//! in its process.

mod bundled_vulkan_driver_environment;
#[cfg(feature = "machine-directories-under-a-test-root")]
mod crash_on_demand_test_node;
mod machine_stream_hosting;
mod refusal_on_standard_error;
mod runtime_unit_lend;
mod tatolabd_command_line;

use std::process::ExitCode;

use clap::Parser;
use streamlib::sdk::runtime::{
    RuntimeRunInProgressRecord, pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread,
};
use streamlib_runtime_client_contract::machine_runtime_lock::MachineRuntimeLock;
use streamlib_runtime_client_contract::tatolab_state_directory::TatolabStateDirectory;

use crate::refusal_on_standard_error::write_refusal_to_standard_error;
use crate::tatolabd_command_line::TatolabdCommandLine;

fn main() -> ExitCode {
    let TatolabdCommandLine {} = TatolabdCommandLine::parse();

    let processor_interpreter_lend_directory =
        match runtime_unit_lend::the_lend_beside_this_executable() {
            Ok(lend_directory) => lend_directory,
            Err(refusal) => return write_refusal_to_standard_error(&refusal),
        };

    // Before any thread or Vulkan instance exists: the loader reads the
    // variable once, and setting it later races every thread reading the
    // environment.
    if let Some(vk_add_driver_files) =
        bundled_vulkan_driver_environment::vk_add_driver_files_naming_the_bundled_icd_manifest(
            &processor_interpreter_lend_directory,
            |name| std::env::var_os(name),
        )
    {
        // SAFETY: no other thread exists yet to read the environment.
        unsafe {
            std::env::set_var(
                bundled_vulkan_driver_environment::DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE,
                vk_add_driver_files,
            )
        };
    }

    // Held until the process exits: the kernel frees it with the process,
    // whichever way the process ends.
    let _machine_runtime_lock = match MachineRuntimeLock::take() {
        Ok(machine_runtime_lock) => machine_runtime_lock,
        Err(refusal) => return write_refusal_to_standard_error(&refusal.to_string()),
    };
    let tatolab_state_directory = match TatolabStateDirectory::resolve() {
        Ok(tatolab_state_directory) => tatolab_state_directory,
        Err(refusal) => return write_refusal_to_standard_error(&refusal.to_string()),
    };
    let (runtime_run_in_progress_record, how_the_previous_run_ended) =
        match RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(
            &tatolab_state_directory.runtime_run_in_progress_record_path(),
        ) {
            Ok(begun) => begun,
            Err(refusal) => return write_refusal_to_standard_error(&refusal.to_string()),
        };

    let hosted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        machine_stream_hosting::host_the_machines_streams_until_a_machine_shutdown(
            &tatolab_state_directory,
            processor_interpreter_lend_directory,
            runtime_run_in_progress_record,
            &how_the_previous_run_ended,
        )
    }));
    match hosted {
        Ok(exit_code) => exit_code,
        Err(panic_that_escaped_the_main_thread) => {
            pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread(
                &*panic_that_escaped_the_main_thread,
            );
            std::panic::resume_unwind(panic_that_escaped_the_main_thread)
        }
    }
}

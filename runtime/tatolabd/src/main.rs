// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd`: the runtime process. Hosts one stream in the foreground — the
//! engine, its built-ins, the local API and the engine's signal ladder — with
//! no Python in its process.

mod bundled_vulkan_driver_environment;
mod refusal_on_standard_error;
mod runtime_unit_lend;
mod stream_hosting;
mod stream_launch_inputs;
mod tatolabd_command_line;

use std::process::ExitCode;

use clap::Parser;

use crate::refusal_on_standard_error::write_refusal_to_standard_error;
use crate::tatolabd_command_line::TatolabdCommandLine;

fn main() -> ExitCode {
    let command_line = TatolabdCommandLine::parse();

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

    let stream_launch_inputs =
        match stream_launch_inputs::StreamLaunchInputs::read_from(&command_line) {
            Ok(stream_launch_inputs) => stream_launch_inputs,
            Err(refusal) => return write_refusal_to_standard_error(&refusal),
        };

    stream_hosting::host_the_stream_until_shutdown(
        stream_launch_inputs,
        processor_interpreter_lend_directory,
    )
}

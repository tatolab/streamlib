// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

use std::ffi::OsStr;
use std::ffi::OsString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use streamlib_ipc_types::FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED;

use crate::core::child_process_lifetime::{
    a_child_process_has_exited_without_being_reaped,
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::compiler::compiler_ops::subprocess_bridge::{
    ENGINE_BUILD_ID, EscalateTransport, HelperProcessShutdownCommand,
};
use crate::core::context::{
    CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE, CaptureDeviceAccessPrompting,
};
use crate::core::descriptors::ProcessorDescriptor;
use crate::core::error::Error;
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, GraphNodeWithComponents,
    HowItsHelpersRanAFunctionNode, ProcessorNode,
};
use crate::core::media_clock::MediaClock;
use crate::core::processors::DynGeneratedProcessor;
use crate::core::runtime::{ShutdownEscalationOfOneStream, StreamEnvironment};

mod helper_process_command_and_wiring_tests;
mod helper_process_lifetime_tests;

/// Read a built command's environment back as pairs, so a test can assert
/// what a child would inherit without starting one.
fn environment_of(command: &Command) -> Vec<(String, String)> {
    command
        .get_envs()
        .filter_map(|(name, value)| {
            Some((
                name.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect()
}

fn value_of<'a>(environment: &'a [(String, String)], name: &str) -> Option<&'a str> {
    environment
        .iter()
        .find(|(entry_name, _)| entry_name == name)
        .map(|(_, value)| value.as_str())
}

/// The stream environment every test host launches in.
fn stream_environment_for_test() -> StreamEnvironment {
    StreamEnvironment {
        project_directory: PathBuf::from("/home/someone/my_app"),
        interpreter: PathBuf::from("/home/someone/my_app/.venv/bin/python"),
    }
}

const LEND_DIRECTORY_FOR_TEST: &str = "/opt/tatolab/lib/tatolab/lend";

/// The stream every test host belongs to.
fn the_stream_a_test_helper_belongs_to() -> LoadedStreamAHelperProcessBelongsTo {
    LoadedStreamAHelperProcessBelongsTo {
        stream_tag: crate::core::runtime::LoadedStreamTag::next_in_this_process()
            .expect("a fresh stream tag"),
        shutdown_escalation: ShutdownEscalationOfOneStream::default(),
    }
}

fn spawn_host_for_test() -> ProcessorInterpreterSpawnHostProcessor {
    spawn_host_for_test_driven_in(ProcessExecution::Reactive)
}

fn spawn_host_for_test_driven_in(
    child_process_execution: ProcessExecution,
) -> ProcessorInterpreterSpawnHostProcessor {
    let import_path =
        crate::core::descriptors::ProcessorClassImportPath::new("my_app.filters:BlurProcessor")
            .unwrap();
    let node = ProcessorNode::new(
        import_path.clone(),
        None,
        "BlurProcessor",
        None,
        vec![],
        vec![],
    );
    let mut host = spawn_host_for_processor_node(
        "my_app.filters:BlurProcessor",
        &ProcessorDescriptor::new(
            crate::core::descriptors::ProcessorClassShortName::new("BlurProcessor").unwrap(),
            import_path,
            "a test double",
        ),
        ExecutionConfig::new(child_process_execution),
        &node,
        &stream_environment_for_test(),
        Path::new(LEND_DIRECTORY_FOR_TEST),
        the_stream_a_test_helper_belongs_to(),
    );
    host.processor_id = "Pblur".to_string();
    host
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod apply_processor_config_update_op;
mod open_iceoryx2_service_op;
mod prepare_processor_op;
pub(crate) mod processor_interpreter_describe;
pub(crate) mod processor_interpreter_shutdown_ladder;
pub(crate) mod processor_interpreter_spawn_host;
pub(crate) mod python_processor_declaration;
mod spawn_processor_op;
pub(crate) mod subprocess_bridge;
mod subprocess_escalate;
mod subprocess_escalate_wire_types;

pub(crate) use apply_processor_config_update_op::{
    ProcessorConfigUpdateOutcome, apply_processor_config_update,
};
pub(crate) use open_iceoryx2_service_op::{
    channel_service_name, find_the_source_a_caller_named, resolve_channel_sizing,
};
pub use open_iceoryx2_service_op::{close_iceoryx2_service, open_iceoryx2_service};
pub(crate) use prepare_processor_op::prepare_processor;
pub(crate) use spawn_processor_op::spawn_processor;

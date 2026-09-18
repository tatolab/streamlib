// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod apply_processor_config_update_op;
mod open_iceoryx2_service_op;
mod prepare_processor_op;
mod spawn_processor_op;
pub(crate) mod subprocess_bridge;
mod subprocess_escalate;
mod subprocess_escalate_wire_types;

pub(crate) use apply_processor_config_update_op::{
    ProcessorConfigUpdateOutcome, apply_processor_config_update,
};
pub use open_iceoryx2_service_op::{close_iceoryx2_service, open_iceoryx2_service};
pub(crate) use open_iceoryx2_service_op::{
    find_the_source_a_caller_named, open_the_channel_of_an_output_port_nothing_local_reads,
    resolve_channel_sizing,
};
pub(crate) use prepare_processor_op::prepare_processor;
pub(crate) use spawn_processor_op::spawn_processor;

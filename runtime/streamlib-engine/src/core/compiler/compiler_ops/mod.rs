// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod open_iceoryx2_service_op;
mod prepare_processor_op;
mod spawn_processor_op;
pub(crate) mod subprocess_bridge;
mod subprocess_escalate;
mod subprocess_escalate_wire_types;

pub(crate) use open_iceoryx2_service_op::{
    ChannelSizing, find_channel_source_port, resolve_channel_sizing,
};
pub use open_iceoryx2_service_op::{close_iceoryx2_service, open_iceoryx2_service};
pub(crate) use prepare_processor_op::prepare_processor;
pub(crate) use spawn_processor_op::spawn_processor;

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod native_lib_resolver;
mod open_iceoryx2_service_op;
mod prepare_processor_op;
mod spawn_deno_subprocess_op;
mod spawn_processor_op;
mod spawn_python_native_subprocess_op;
mod subprocess_bridge;
mod subprocess_escalate;

pub use open_iceoryx2_service_op::{close_iceoryx2_service, open_iceoryx2_service};
pub(crate) use open_iceoryx2_service_op::{
    ChannelSizing, find_channel_source_port, resolve_channel_sizing,
};
pub(crate) use prepare_processor_op::prepare_processor;
pub(crate) use spawn_deno_subprocess_op::create_deno_subprocess_host_constructor;
pub(crate) use spawn_processor_op::spawn_processor;
pub(crate) use spawn_python_native_subprocess_op::{
    create_python_native_subprocess_host_constructor, resolve_python_native_lib_path,
};

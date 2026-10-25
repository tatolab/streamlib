// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod axum_router_served_until_stopped;
#[cfg(test)]
mod control_plane_stub_support;
mod handlers;
mod local_api_served_for_an_engine;
mod local_api_socket;
mod loopback_http_listener;
mod mcp;
mod mcp_prompts;
mod mcp_resources;
mod mcp_stdio_upgrade;
#[cfg(test)]
mod relay_tools_tests;
mod state;
#[cfg(test)]
mod stream_tools_over_a_real_engine_tests;
#[cfg(test)]
mod two_streams_in_one_engine_tests;

pub use handlers::control_plane_openapi_spec;
pub use local_api_served_for_an_engine::{
    LocalApiServedForAnEngine, serve_the_local_api_for_an_engine,
};
pub use loopback_http_listener::{
    LoopbackHttpListenerServedForAnEngine, serve_the_loopback_http_listener,
};
pub use mcp_prompts::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH;

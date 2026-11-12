// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod asset_media_facts_reads;
mod axum_router_served_until_stopped;
mod control_json_rpc_adapter;
mod control_operation_dispatcher;
mod control_operations_on_loaded_streams;
mod control_operations_on_new_projects;
mod control_operations_on_one_project;
mod control_operations_on_project_archives;
mod control_operations_on_project_assets;
mod control_operations_on_project_dependencies;
mod control_operations_on_project_files;
mod control_operations_on_project_node_types;
mod control_operations_on_project_views;
mod control_operations_on_stream_graph_history;
#[cfg(test)]
mod control_plane_stub_support;
mod first_party_package_moves_recorded_in_the_open_projects;
mod handlers;
mod local_api_served_for_an_engine;
mod local_api_socket;
mod loopback_http_listener;
mod mcp;
mod mcp_project_archive_tools;
mod mcp_project_asset_tools;
mod mcp_project_change_feed;
mod mcp_project_dependency_tools;
mod mcp_project_file_tools;
mod mcp_project_node_type_tools;
mod mcp_project_tools;
mod mcp_project_view_tools;
mod mcp_prompts;
mod mcp_resources;
mod mcp_stdio_upgrade;
mod mcp_stream_action_resources;
mod mcp_stream_graph_tools;
mod new_node_file_template;
#[cfg(test)]
mod project_asset_tools_tests;
mod state;
mod stream_graph_transaction_executor;
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

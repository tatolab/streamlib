// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One runtime's local API, reached over one current-thread tokio runtime: the MCP client and the
//! HTTP/1.1 connection a verb drives it through, each opened on first use and kept for the run.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::local_api_mcp_tool_client::{LocalApiMcpToolClient, LocalApiMcpToolClientFailure};
use crate::local_api_unix_socket_http_client::{
    LocalApiHttpConnection, LocalApiHttpRequestFailure, LocalApiHttpResponse,
};

/// A runtime's local API as one verb run reaches it; closes the MCP client when dropped.
pub(crate) struct LocalApiConnection {
    local_api_tokio_runtime: tokio::runtime::Runtime,
    local_api_socket_path: PathBuf,
    request_timeout: Duration,
    connected_mcp_client: Option<LocalApiMcpToolClient>,
    local_api_http_connection: LocalApiHttpConnection,
}

impl LocalApiConnection {
    /// A connection to the local API at `local_api_socket_path`, every request bounded by
    /// `request_timeout`. Only the tokio runtime driving it starts here; nothing is dialed yet.
    pub(crate) fn open(
        local_api_socket_path: &Path,
        request_timeout: Duration,
    ) -> io::Result<Self> {
        Ok(Self {
            local_api_tokio_runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
            local_api_socket_path: local_api_socket_path.to_path_buf(),
            request_timeout,
            connected_mcp_client: None,
            local_api_http_connection: LocalApiHttpConnection::to_local_api_socket(
                local_api_socket_path,
            ),
        })
    }

    /// Connect the MCP client through `server/discover`, unless it already is.
    pub(crate) fn connect_mcp_client(&mut self) -> Result<(), LocalApiMcpToolClientFailure> {
        local_api_mcp_tool_client_connected_on_first_use(
            &self.local_api_tokio_runtime,
            &mut self.connected_mcp_client,
            &self.local_api_socket_path,
            self.request_timeout,
        )?;
        Ok(())
    }

    /// Call `tool_name` with `tool_arguments` through the MCP client, connecting it first when it
    /// is not yet, and answer the first text block of the result.
    pub(crate) fn call_tool(
        &mut self,
        tool_name: &str,
        tool_arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, LocalApiMcpToolClientFailure> {
        let connected_mcp_client = local_api_mcp_tool_client_connected_on_first_use(
            &self.local_api_tokio_runtime,
            &mut self.connected_mcp_client,
            &self.local_api_socket_path,
            self.request_timeout,
        )?;
        self.local_api_tokio_runtime
            .block_on(connected_mcp_client.call_tool(tool_name, tool_arguments))
    }

    /// `GET origin_form_request_target` over the kept HTTP/1.1 connection, answering the status,
    /// headers and whole body, whatever the status.
    pub(crate) fn get_whole_response(
        &mut self,
        origin_form_request_target: &str,
    ) -> Result<LocalApiHttpResponse, LocalApiHttpRequestFailure> {
        self.local_api_tokio_runtime.block_on(
            self.local_api_http_connection
                .get_whole_response(origin_form_request_target, self.request_timeout),
        )
    }
}

impl Drop for LocalApiConnection {
    fn drop(&mut self) {
        if let Some(connected_mcp_client) = self.connected_mcp_client.take() {
            self.local_api_tokio_runtime
                .block_on(connected_mcp_client.close());
        }
    }
}

/// The client `connected_mcp_client` holds, connected on `local_api_tokio_runtime` first when it
/// holds none.
fn local_api_mcp_tool_client_connected_on_first_use<'connection>(
    local_api_tokio_runtime: &tokio::runtime::Runtime,
    connected_mcp_client: &'connection mut Option<LocalApiMcpToolClient>,
    local_api_socket_path: &Path,
    request_timeout: Duration,
) -> Result<&'connection LocalApiMcpToolClient, LocalApiMcpToolClientFailure> {
    match connected_mcp_client {
        Some(connected_mcp_client) => Ok(connected_mcp_client),
        None => Ok(
            connected_mcp_client.insert(local_api_tokio_runtime.block_on(
                LocalApiMcpToolClient::connect(local_api_socket_path, request_timeout),
            )?),
        ),
    }
}

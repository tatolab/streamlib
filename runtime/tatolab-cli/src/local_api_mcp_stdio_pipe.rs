// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab mcp`: an MCP host's stdin and stdout, piped to a running runtime's MCP server.
//!
//! The verb sends the one `/mcp/stdio` upgrade over the runtime's local API socket and then only
//! copies bytes, stdin to socket and socket to stdout. It parses no message, so the protocol
//! revision is the runtime's alone, and stdout carries nothing but what the runtime wrote.

use std::io;
use std::path::Path;
use std::time::Duration;

use streamlib_runtime_client_contract::node_registry::NodeRegistryEntry;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::OBSERVATION_VERB_TOOL_CALL_TIMEOUT;
use crate::local_api_runtime_selection::{
    LocalApiRuntimeSelectionFailure, select_live_runtime_in_node_registry,
    this_users_node_registry_directory,
};
use crate::local_api_unix_socket_http_client::{
    UpgradedLocalApiMcpStdioStream, upgrade_local_api_connection_to_mcp_stdio,
};

/// The most bytes one read takes from stdin or from the socket.
const MCP_STDIO_PIPE_CHUNK_BYTES: usize = 64 * 1024;

/// The refusal when no runtime on this machine answers, `--node` or not.
const NO_LIVE_RUNTIME_FOR_THE_MCP_VERB_REFUSAL: &str = "no runtime is live on this machine \
     for `tatolab mcp` to reach; `tatolab nodes` lists the live ones.";

/// How copying the host's stdin to the runtime stopped. Every ending drops the socket's owned
/// write half, which shuts down its write direction, so the runtime reads the end of its input
/// and still answers what it owes.
#[derive(Debug)]
enum McpHostInputCopyEnding {
    /// Stdin reached its end.
    McpHostInputEnded,
    /// Stdin could not be read.
    McpHostInputUnreadable(io::Error),
    /// The runtime's side stopped taking bytes; the copy the other way reports it.
    RuntimeStoppedTakingBytes,
}

/// How copying the runtime's stream to the host's stdout stopped.
#[derive(Debug)]
enum RuntimeOutputCopyEnding {
    /// The runtime closed or reset its side.
    RuntimeClosedItsSide,
    /// Stdout's reader went away: the host stopped reading.
    McpHostStoppedReading,
    /// Stdout refused a write for another reason.
    McpHostOutputFailed(io::Error),
}

/// How the whole pipe ended, which decides the verb's exit.
#[derive(Debug)]
enum McpStdioPipeEnding {
    /// The runtime closed its side after stdin had ended: the conversation is over.
    RuntimeClosedAfterMcpHostInputEnded,
    /// The runtime closed or reset its side while stdin was still open.
    RuntimeClosedWithMcpHostInputOpen,
    /// Stdin could not be read; the runtime has since closed its side, or the host stopped
    /// reading.
    McpHostInputUnreadable(io::Error),
    /// The host stopped reading stdout; nobody is left to answer.
    McpHostStoppedReading,
    /// Stdout refused a write for another reason.
    McpHostOutputFailed(io::Error),
}

/// `tatolab mcp`: pipe this process's stdin and stdout to the MCP server of the live runtime
/// `requested_runtime_name_or_id` names, or of the sole live one.
pub(crate) fn pipe_stdio_to_the_selected_runtimes_mcp_server(
    requested_runtime_name_or_id: Option<&str>,
) -> Result<u8, TatolabCommandFailure> {
    pipe_mcp_host_io_to_a_runtime_in_node_registry(
        &this_users_node_registry_directory()?,
        requested_runtime_name_or_id,
        tokio::io::stdin(),
        tokio::io::stdout(),
        OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
    )
}

/// Select the live runtime in `node_registry_directory` and pipe the host's input and output to
/// its MCP server, refusing a runtime that does not answer the upgrade within
/// `mcp_stdio_upgrade_timeout`.
fn pipe_mcp_host_io_to_a_runtime_in_node_registry(
    node_registry_directory: &Path,
    requested_runtime_name_or_id: Option<&str>,
    mcp_host_input: impl AsyncRead + Unpin,
    mcp_host_output: impl AsyncWrite + Unpin,
    mcp_stdio_upgrade_timeout: Duration,
) -> Result<u8, TatolabCommandFailure> {
    let selected_runtime = match select_live_runtime_in_node_registry(
        node_registry_directory,
        requested_runtime_name_or_id,
    ) {
        Ok(selected_runtime) => selected_runtime,
        Err(LocalApiRuntimeSelectionFailure::NoRunningRuntime) => {
            return Err(TatolabCommandFailure::refused(
                NO_LIVE_RUNTIME_FOR_THE_MCP_VERB_REFUSAL.to_owned(),
            ));
        }
        Err(runtime_selection_failure) => return Err(runtime_selection_failure.into()),
    };
    pipe_mcp_host_io_to_the_runtimes_mcp_server(
        &selected_runtime,
        mcp_host_input,
        mcp_host_output,
        mcp_stdio_upgrade_timeout,
    )
}

/// Open `selected_runtime`'s MCP stream, waiting at most `mcp_stdio_upgrade_timeout` for its
/// `101`, and copy bytes both ways until the runtime closes it or the host stops reading.
fn pipe_mcp_host_io_to_the_runtimes_mcp_server(
    selected_runtime: &NodeRegistryEntry,
    mcp_host_input: impl AsyncRead + Unpin,
    mcp_host_output: impl AsyncWrite + Unpin,
    mcp_stdio_upgrade_timeout: Duration,
) -> Result<u8, TatolabCommandFailure> {
    let runtime_named_for_stderr = format!(
        "runtime `{}` ({})",
        selected_runtime.runtime_name, selected_runtime.runtime_id
    );
    let pipe_tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|runtime_start_failure| {
            TatolabCommandFailure::refused(format!(
                "{runtime_named_for_stderr} did not open its MCP stream: could not start the \
                 pipe's runtime: {runtime_start_failure}"
            ))
        })?;
    let pipe_outcome: Result<McpStdioPipeEnding, TatolabCommandFailure> = pipe_tokio_runtime
        .block_on(async {
            let upgraded_mcp_stdio_stream = match tokio::time::timeout(
                mcp_stdio_upgrade_timeout,
                upgrade_local_api_connection_to_mcp_stdio(&selected_runtime.local_api_socket_path),
            )
            .await
            {
                Ok(Ok(upgraded_mcp_stdio_stream)) => upgraded_mcp_stdio_stream,
                Ok(Err(upgrade_failure)) => {
                    return Err(TatolabCommandFailure::refused(format!(
                        "{runtime_named_for_stderr} did not open its MCP stream: \
                         {upgrade_failure}"
                    )));
                }
                Err(_elapsed) => {
                    return Err(TatolabCommandFailure::refused(format!(
                        "{runtime_named_for_stderr} did not open its MCP stream: it did not \
                         answer within {mcp_stdio_upgrade_timeout:?}"
                    )));
                }
            };
            Ok(copy_both_ways_until_the_pipe_ends(
                upgraded_mcp_stdio_stream,
                mcp_host_input,
                mcp_host_output,
            )
            .await)
        });
    // Stdin is read on a blocking thread no shutdown can cancel; waiting for it would hold the
    // exit until the host wrote another byte or closed stdin.
    pipe_tokio_runtime.shutdown_background();
    match pipe_outcome? {
        McpStdioPipeEnding::RuntimeClosedAfterMcpHostInputEnded
        | McpStdioPipeEnding::McpHostStoppedReading => Ok(0),
        McpStdioPipeEnding::RuntimeClosedWithMcpHostInputOpen => {
            Err(TatolabCommandFailure::refused(format!(
                "{runtime_named_for_stderr} closed its MCP stream."
            )))
        }
        McpStdioPipeEnding::McpHostInputUnreadable(input_failure) => {
            Err(TatolabCommandFailure::refused(format!(
                "cannot read stdin into the MCP stream of {runtime_named_for_stderr}: \
                 {input_failure}"
            )))
        }
        McpStdioPipeEnding::McpHostOutputFailed(output_failure) => {
            Err(TatolabCommandFailure::refused(format!(
                "cannot write the MCP stream of {runtime_named_for_stderr} to stdout: \
                 {output_failure}"
            )))
        }
    }
}

/// Copy the host's input to the runtime and the runtime's stream to the host's output until the
/// runtime's side ends or the host stops reading.
async fn copy_both_ways_until_the_pipe_ends(
    upgraded_mcp_stdio_stream: UpgradedLocalApiMcpStdioStream,
    mcp_host_input: impl AsyncRead + Unpin,
    mcp_host_output: impl AsyncWrite + Unpin,
) -> McpStdioPipeEnding {
    let UpgradedLocalApiMcpStdioStream {
        local_api_stream,
        bytes_streamed_behind_the_response_head,
    } = upgraded_mcp_stdio_stream;
    let (local_api_stream_read_half, local_api_stream_write_half) = local_api_stream.into_split();
    let mcp_host_input_to_the_runtime =
        copy_mcp_host_input_to_the_runtime(mcp_host_input, local_api_stream_write_half);
    let the_runtime_to_mcp_host_output = copy_the_runtime_to_mcp_host_output(
        &bytes_streamed_behind_the_response_head,
        local_api_stream_read_half,
        mcp_host_output,
    );
    tokio::pin!(mcp_host_input_to_the_runtime);
    tokio::pin!(the_runtime_to_mcp_host_output);
    let mut mcp_host_input_copy_ending = None;
    loop {
        tokio::select! {
            // Polled first, so a stdin end that lands with the runtime's close is counted.
            biased;
            host_input_copy_ending = &mut mcp_host_input_to_the_runtime,
                if mcp_host_input_copy_ending.is_none() =>
            {
                mcp_host_input_copy_ending = Some(host_input_copy_ending);
            }
            runtime_output_copy_ending = &mut the_runtime_to_mcp_host_output => {
                return match (runtime_output_copy_ending, mcp_host_input_copy_ending) {
                    (_, Some(McpHostInputCopyEnding::McpHostInputUnreadable(input_failure))) => {
                        McpStdioPipeEnding::McpHostInputUnreadable(input_failure)
                    }
                    (
                        RuntimeOutputCopyEnding::RuntimeClosedItsSide,
                        Some(McpHostInputCopyEnding::McpHostInputEnded),
                    ) => McpStdioPipeEnding::RuntimeClosedAfterMcpHostInputEnded,
                    (RuntimeOutputCopyEnding::RuntimeClosedItsSide, _) => {
                        McpStdioPipeEnding::RuntimeClosedWithMcpHostInputOpen
                    }
                    (RuntimeOutputCopyEnding::McpHostStoppedReading, _) => {
                        McpStdioPipeEnding::McpHostStoppedReading
                    }
                    (RuntimeOutputCopyEnding::McpHostOutputFailed(output_failure), _) => {
                        McpStdioPipeEnding::McpHostOutputFailed(output_failure)
                    }
                };
            }
        }
    }
}

/// Copy stdin to the runtime until stdin ends or either side fails.
async fn copy_mcp_host_input_to_the_runtime(
    mut mcp_host_input: impl AsyncRead + Unpin,
    mut local_api_stream_write_half: OwnedWriteHalf,
) -> McpHostInputCopyEnding {
    let mut mcp_host_input_chunk = vec![0_u8; MCP_STDIO_PIPE_CHUNK_BYTES];
    loop {
        let read_byte_count = match mcp_host_input.read(&mut mcp_host_input_chunk).await {
            Ok(0) => return McpHostInputCopyEnding::McpHostInputEnded,
            Ok(read_byte_count) => read_byte_count,
            Err(input_failure) if input_failure.kind() == io::ErrorKind::Interrupted => continue,
            Err(input_failure) => {
                return McpHostInputCopyEnding::McpHostInputUnreadable(input_failure);
            }
        };
        if local_api_stream_write_half
            .write_all(&mcp_host_input_chunk[..read_byte_count])
            .await
            .is_err()
        {
            return McpHostInputCopyEnding::RuntimeStoppedTakingBytes;
        }
    }
}

/// Copy the runtime's stream to stdout, starting with the bytes it streamed behind the `101`'s
/// head, flushing every chunk so the host reads each message as it lands.
async fn copy_the_runtime_to_mcp_host_output(
    bytes_streamed_behind_the_response_head: &[u8],
    mut local_api_stream_read_half: OwnedReadHalf,
    mut mcp_host_output: impl AsyncWrite + Unpin,
) -> RuntimeOutputCopyEnding {
    if let Err(output_failure) = write_and_flush_to_mcp_host_output(
        &mut mcp_host_output,
        bytes_streamed_behind_the_response_head,
    )
    .await
    {
        return output_failure;
    }
    let mut runtime_output_chunk = vec![0_u8; MCP_STDIO_PIPE_CHUNK_BYTES];
    loop {
        let read_byte_count = match local_api_stream_read_half
            .read(&mut runtime_output_chunk)
            .await
        {
            Ok(0) | Err(_) => return RuntimeOutputCopyEnding::RuntimeClosedItsSide,
            Ok(read_byte_count) => read_byte_count,
        };
        if let Err(output_failure) = write_and_flush_to_mcp_host_output(
            &mut mcp_host_output,
            &runtime_output_chunk[..read_byte_count],
        )
        .await
        {
            return output_failure;
        }
    }
}

async fn write_and_flush_to_mcp_host_output(
    mcp_host_output: &mut (impl AsyncWrite + Unpin),
    runtime_bytes: &[u8],
) -> Result<(), RuntimeOutputCopyEnding> {
    if runtime_bytes.is_empty() {
        return Ok(());
    }
    let written_and_flushed = async {
        mcp_host_output.write_all(runtime_bytes).await?;
        mcp_host_output.flush().await
    }
    .await;
    written_and_flushed.map_err(|output_failure| {
        if output_failure.kind() == std::io::ErrorKind::BrokenPipe {
            RuntimeOutputCopyEnding::McpHostStoppedReading
        } else {
            RuntimeOutputCopyEnding::McpHostOutputFailed(output_failure)
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use rmcp::model::{CallToolRequestParams, ContentBlock, ProtocolVersion};
    use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
    use serde_json::json;

    use super::*;
    use crate::isolated_node_registry::{
        IsolatedNodeRegistry, NOTHING_LISTENS_LOCAL_API_SOCKET_PATH, PID_NO_PROCESS_HAS,
        SCRIPTED_RUNTIME_ID, SCRIPTED_RUNTIME_NAME, a_registry_entry_hosted_by,
    };
    use crate::stub_local_api_server::{
        RecordedHttpRequestHead, RecordedToolCall, StubLocalApiScript, StubLocalApiServer,
        StubMcpStdioUpgradeAnswer, StubToolAnswer,
    };

    /// Longer than any pipe here takes; a pipe still running past it never ended.
    const PIPE_TEST_DEADLINE: Duration = Duration::from_secs(20);

    /// An upgrade bound short enough that a test waiting it out stays quick.
    const SHORT_MCP_STDIO_UPGRADE_TIMEOUT: Duration = Duration::from_millis(200);

    /// Stdin that fails every read, as a host's closed or broken stdin does.
    struct McpHostInputThatCannotBeRead;

    impl AsyncRead for McpHostInputThatCannotBeRead {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            _read_buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Err(io::Error::other("the host's stdin went away")))
        }
    }

    /// [`pipe_within_the_test_deadline_bounding_the_upgrade_by`] the verb's own upgrade bound.
    fn pipe_within_the_test_deadline<McpHostInput, McpHostOutput>(
        node_registry_directory: PathBuf,
        requested_runtime_name_or_id: Option<&'static str>,
        mcp_host_input: McpHostInput,
        mcp_host_output: McpHostOutput,
    ) -> (Result<u8, TatolabCommandFailure>, McpHostOutput)
    where
        McpHostInput: AsyncRead + Unpin + Send + 'static,
        McpHostOutput: AsyncWrite + Unpin + Send + 'static,
    {
        pipe_within_the_test_deadline_bounding_the_upgrade_by(
            node_registry_directory,
            requested_runtime_name_or_id,
            mcp_host_input,
            mcp_host_output,
            OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
        )
    }

    /// Run the pipe on a thread of its own against `node_registry_directory`, answering its
    /// outcome and the host output it wrote; fails the test if it outlives
    /// [`PIPE_TEST_DEADLINE`].
    fn pipe_within_the_test_deadline_bounding_the_upgrade_by<McpHostInput, McpHostOutput>(
        node_registry_directory: PathBuf,
        requested_runtime_name_or_id: Option<&'static str>,
        mcp_host_input: McpHostInput,
        mcp_host_output: McpHostOutput,
        mcp_stdio_upgrade_timeout: Duration,
    ) -> (Result<u8, TatolabCommandFailure>, McpHostOutput)
    where
        McpHostInput: AsyncRead + Unpin + Send + 'static,
        McpHostOutput: AsyncWrite + Unpin + Send + 'static,
    {
        let (pipe_finished, pipe_finishing) = mpsc::channel();
        std::thread::spawn(move || {
            let mut mcp_host_output = mcp_host_output;
            let pipe_outcome = pipe_mcp_host_io_to_a_runtime_in_node_registry(
                &node_registry_directory,
                requested_runtime_name_or_id,
                mcp_host_input,
                &mut mcp_host_output,
                mcp_stdio_upgrade_timeout,
            );
            let _test_still_waiting = pipe_finished.send((pipe_outcome, mcp_host_output));
        });
        pipe_finishing
            .recv_timeout(PIPE_TEST_DEADLINE)
            .expect("the pipe never ended")
    }

    /// The one line a refusal prints after `error: `, and the code it exits with.
    fn refusal_line_and_exit_code(pipe_outcome: Result<u8, TatolabCommandFailure>) -> (String, u8) {
        let refusal = pipe_outcome.expect_err("the pipe was refused");
        let refusal_line = refusal.message_for_the_user.expect("a refusal says why");
        assert!(!refusal_line.contains('\n'), "one line: {refusal_line:?}");
        (refusal_line, refusal.exit_code)
    }

    #[test]
    fn the_pipe_opens_the_stream_with_one_upgrade_and_copies_both_ways_untouched() {
        // Neither side is valid MCP: the pipe must not read what it carries.
        let mcp_host_bytes = b"{\"not\": \"inspected\", \"bytes\": \"\\u00e9\"}\n".to_vec();
        let runtime_bytes_once_upgraded = b"\x00\xffnot even json\n".to_vec();
        let runtime_bytes_once_stdin_ended = b"{\"sent\": \"after the half-close\"}\n".to_vec();
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                written_once_upgraded: runtime_bytes_once_upgraded.clone(),
                written_once_the_client_half_closed: runtime_bytes_once_stdin_ended.clone(),
            },
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            None,
            std::io::Cursor::new(mcp_host_bytes.clone()),
            Vec::new(),
        );

        assert_eq!(pipe_outcome.unwrap(), 0);
        let [upgrade_request_head] = <[RecordedHttpRequestHead; 1]>::try_from(
            stub_local_api_server.recorded_mcp_stdio_request_heads(),
        )
        .expect("exactly one upgrade request");
        let mut header_lines = upgrade_request_head.header_lines.clone();
        header_lines.sort();
        assert_eq!(
            RecordedHttpRequestHead {
                header_lines,
                ..upgrade_request_head
            },
            RecordedHttpRequestHead {
                method: "GET".to_owned(),
                request_target: "/mcp/stdio".to_owned(),
                header_lines: vec![
                    ("connection".to_owned(), "Upgrade".to_owned()),
                    ("host".to_owned(), "localhost".to_owned()),
                    ("upgrade".to_owned(), "mcp-stdio".to_owned()),
                ],
            }
        );
        assert_eq!(
            stub_local_api_server.recorded_mcp_stdio_client_bytes(),
            mcp_host_bytes
        );
        assert_eq!(
            mcp_host_output,
            [
                runtime_bytes_once_upgraded,
                mcp_host_bytes,
                runtime_bytes_once_stdin_ended
            ]
            .concat()
        );
    }

    #[test]
    fn stdin_ending_half_closes_the_stream_and_the_pipe_ends_when_the_runtime_closes() {
        let answer_owed_after_stdin_ended = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n";
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                written_once_upgraded: Vec::new(),
                written_once_the_client_half_closed: answer_owed_after_stdin_ended.to_vec(),
            },
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            None,
            std::io::Cursor::new(b"{\"id\":1}\n".to_vec()),
            Vec::new(),
        );

        assert_eq!(pipe_outcome.unwrap(), 0);
        assert_eq!(
            stub_local_api_server.recorded_mcp_stdio_client_bytes(),
            b"{\"id\":1}\n",
            "stdin's end reaches the runtime"
        );
        assert_eq!(
            mcp_host_output,
            [b"{\"id\":1}\n".as_slice(), answer_owed_after_stdin_ended].concat()
        );
    }

    #[test]
    fn the_runtime_closing_while_stdin_is_open_is_refused_naming_the_runtime() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::CloseOnceUpgraded,
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );
        let (_held_open_mcp_host_input_writer, held_open_mcp_host_input) = tokio::io::duplex(64);

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            None,
            held_open_mcp_host_input,
            Vec::new(),
        );

        assert_eq!(
            refusal_line_and_exit_code(pipe_outcome),
            (
                format!(
                    "runtime `{SCRIPTED_RUNTIME_NAME}` ({SCRIPTED_RUNTIME_ID}) closed its MCP \
                     stream."
                ),
                1
            )
        );
        assert_eq!(mcp_host_output, b"");
    }

    /// The stub writes its owed answer only once it reads the client's half-close, so the answer
    /// on the host's output proves the failed read still shut the write direction.
    #[test]
    fn an_unreadable_stdin_half_closes_the_stream_and_is_refused_naming_stdin() {
        let answer_owed_once_stdin_ended = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n";
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                written_once_upgraded: Vec::new(),
                written_once_the_client_half_closed: answer_owed_once_stdin_ended.to_vec(),
            },
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            None,
            McpHostInputThatCannotBeRead,
            Vec::new(),
        );

        assert_eq!(
            refusal_line_and_exit_code(pipe_outcome),
            (
                format!(
                    "cannot read stdin into the MCP stream of runtime `{SCRIPTED_RUNTIME_NAME}` \
                     ({SCRIPTED_RUNTIME_ID}): the host's stdin went away"
                ),
                1
            )
        );
        assert_eq!(mcp_host_output, answer_owed_once_stdin_ended);
        assert_eq!(stub_local_api_server.recorded_mcp_stdio_client_bytes(), b"");
    }

    #[test]
    fn an_upgrade_the_runtime_never_answers_is_refused_once_its_bound_elapses() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::NeverAnswer,
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );
        let pipe_started = std::time::Instant::now();

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline_bounding_the_upgrade_by(
            isolated_node_registry.node_registry_directory(),
            None,
            tokio::io::empty(),
            Vec::new(),
            SHORT_MCP_STDIO_UPGRADE_TIMEOUT,
        );

        assert_eq!(
            refusal_line_and_exit_code(pipe_outcome),
            (
                format!(
                    "runtime `{SCRIPTED_RUNTIME_NAME}` ({SCRIPTED_RUNTIME_ID}) did not open its \
                     MCP stream: it did not answer within 200ms"
                ),
                1
            )
        );
        assert!(
            pipe_started.elapsed() < PIPE_TEST_DEADLINE / 4,
            "the pipe took {:?}",
            pipe_started.elapsed()
        );
        assert_eq!(mcp_host_output, b"");
        assert_eq!(
            stub_local_api_server
                .recorded_mcp_stdio_request_heads()
                .len(),
            1
        );
    }

    #[test]
    fn a_runtime_refusing_the_upgrade_is_refused_naming_it_and_its_answer() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::RefuseTheUpgrade { http_status: 426 },
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            None,
            tokio::io::empty(),
            Vec::new(),
        );

        assert_eq!(
            refusal_line_and_exit_code(pipe_outcome),
            (
                format!(
                    "runtime `{SCRIPTED_RUNTIME_NAME}` ({SCRIPTED_RUNTIME_ID}) did not open its \
                     MCP stream: it answered `HTTP/1.1 426 Upgrade Required`"
                ),
                1
            )
        );
        assert_eq!(mcp_host_output, b"");
    }

    #[test]
    fn no_live_runtime_is_a_one_line_refusal_naming_tatolab_nodes() {
        let isolated_node_registry = IsolatedNodeRegistry::new();

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            None,
            tokio::io::empty(),
            Vec::new(),
        );

        assert_eq!(
            refusal_line_and_exit_code(pipe_outcome),
            (
                "no runtime is live on this machine for `tatolab mcp` to reach; `tatolab nodes` \
                 lists the live ones."
                    .to_owned(),
                1
            )
        );
        assert_eq!(mcp_host_output, b"");
    }

    #[test]
    fn no_live_runtime_is_the_same_refusal_when_node_names_one() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        isolated_node_registry.write_registry_entry(&a_registry_entry_hosted_by(
            "Rgone",
            Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            PID_NO_PROCESS_HAS,
        ));

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            Some("absent-runtime"),
            tokio::io::empty(),
            Vec::new(),
        );

        assert_eq!(
            refusal_line_and_exit_code(pipe_outcome),
            (NO_LIVE_RUNTIME_FOR_THE_MCP_VERB_REFUSAL.to_owned(), 1)
        );
        assert_eq!(mcp_host_output, b"");
    }

    #[test]
    fn a_node_flag_matching_no_live_runtime_is_refused_naming_it_and_the_live_ones() {
        let stub_local_api_server = StubLocalApiServer::serve_default();
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let (pipe_outcome, mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            Some("absent-runtime"),
            tokio::io::empty(),
            Vec::new(),
        );

        let (refusal_line, exit_code) = refusal_line_and_exit_code(pipe_outcome);
        assert_eq!(exit_code, 1);
        assert!(refusal_line.contains("`absent-runtime`"), "{refusal_line}");
        assert!(
            refusal_line.contains(SCRIPTED_RUNTIME_NAME),
            "the refusal lists the live runtimes: {refusal_line}"
        );
        assert_eq!(mcp_host_output, b"");
        assert_eq!(stub_local_api_server.recorded_mcp_stdio_request_heads(), []);
    }

    #[test]
    fn the_host_stopping_reading_ends_the_pipe_without_a_refusal() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                written_once_upgraded: b"{\"jsonrpc\":\"2.0\",\"method\":\"nobody reads\"}\n"
                    .to_vec(),
                written_once_the_client_half_closed: Vec::new(),
            },
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );
        let (_held_open_mcp_host_input_writer, held_open_mcp_host_input) = tokio::io::duplex(64);
        let (mcp_host_output_with_no_reader, mcp_host_output_reader) = tokio::io::duplex(64);
        drop(mcp_host_output_reader);

        let (pipe_outcome, _mcp_host_output) = pipe_within_the_test_deadline(
            isolated_node_registry.node_registry_directory(),
            None,
            held_open_mcp_host_input,
            mcp_host_output_with_no_reader,
        );

        assert_eq!(pipe_outcome.unwrap(), 0);
    }

    #[test]
    fn bytes_streamed_behind_the_101_reach_the_host_before_any_read_from_the_socket() {
        let (pipe_side_of_the_socket, mut runtime_side_of_the_socket) =
            std::os::unix::net::UnixStream::pair().unwrap();
        std::io::Write::write_all(&mut runtime_side_of_the_socket, b"read from the socket\n")
            .unwrap();
        drop(runtime_side_of_the_socket);
        pipe_side_of_the_socket.set_nonblocking(true).unwrap();

        let (pipe_ending, mcp_host_output) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut mcp_host_output = Vec::new();
                let pipe_ending = copy_both_ways_until_the_pipe_ends(
                    UpgradedLocalApiMcpStdioStream {
                        local_api_stream: tokio::net::UnixStream::from_std(pipe_side_of_the_socket)
                            .unwrap(),
                        bytes_streamed_behind_the_response_head: hyper::body::Bytes::from_static(
                            b"streamed with the 101\n",
                        ),
                    },
                    tokio::io::empty(),
                    &mut mcp_host_output,
                )
                .await;
                (pipe_ending, mcp_host_output)
            });

        assert!(
            matches!(
                pipe_ending,
                McpStdioPipeEnding::RuntimeClosedAfterMcpHostInputEnded
            ),
            "{pipe_ending:?}"
        );
        assert_eq!(
            mcp_host_output,
            b"streamed with the 101\nread from the socket\n"
        );
    }

    #[test]
    fn an_mcp_client_lists_and_calls_a_tool_through_the_pipe() {
        let stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
            fixed_tool_answer: Some(StubToolAnswer::tool_result(r#"{"nodes":[]}"#)),
            listed_tool_names: vec!["graph".to_owned()],
            ..StubLocalApiScript::default()
        });
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );
        let (mcp_client_side, pipe_side) = tokio::io::duplex(64 * 1024);
        let (pipe_side_input, pipe_side_output) = tokio::io::split(pipe_side);
        let node_registry_directory = isolated_node_registry.node_registry_directory();
        let pipe_thread = std::thread::spawn(move || {
            pipe_within_the_test_deadline(
                node_registry_directory,
                None,
                pipe_side_input,
                pipe_side_output,
            )
            .0
        });

        let (listed_tool_names, graph_tool_text) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(PIPE_TEST_DEADLINE, async {
                    let mcp_client = ()
                        .serve_with_lifecycle(
                            mcp_client_side,
                            ClientLifecycleMode::Discover {
                                preferred_versions: vec![ProtocolVersion::LATEST],
                            },
                        )
                        .await
                        .expect("the runtime answers `server/discover` through the pipe");
                    let listed_tool_names: Vec<String> = mcp_client
                        .list_all_tools()
                        .await
                        .unwrap()
                        .into_iter()
                        .map(|listed_tool| listed_tool.name.to_string())
                        .collect();
                    let graph_tool_result = mcp_client
                        .call_tool(
                            CallToolRequestParams::new("graph").with_arguments(
                                json!({ "through": "the pipe" })
                                    .as_object()
                                    .unwrap()
                                    .clone(),
                            ),
                        )
                        .await
                        .unwrap();
                    let graph_tool_text = match graph_tool_result.content.first() {
                        Some(ContentBlock::Text(text_block)) => text_block.text.clone(),
                        other_content => panic!("not a text block: {other_content:?}"),
                    };
                    let _client_quit_reason = mcp_client.cancel().await;
                    (listed_tool_names, graph_tool_text)
                })
                .await
                .expect("the MCP exchange finished in time")
            });

        assert_eq!(listed_tool_names, ["graph"]);
        assert_eq!(graph_tool_text, r#"{"nodes":[]}"#);
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "graph".to_owned(),
                tool_arguments: json!({ "through": "the pipe" }),
            }]
        );
        assert_eq!(pipe_thread.join().unwrap().unwrap(), 0);
    }
}

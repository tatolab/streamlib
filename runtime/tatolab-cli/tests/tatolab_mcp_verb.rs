// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab mcp` launched as an MCP host launches it, its stdio piped, against a stub local API:
//! what it writes to stdout and stderr, and how it exits, over the process's real stdin and
//! stdout. Each piping scenario is the in-crate tests'. The registry is isolated through
//! `XDG_RUNTIME_DIR`, which only Linux honours, so the tests that read one are Linux-only.

mod common;

use common::tatolab_binary_run::{
    run_tatolab_reading_no_runtime_directory, standard_error_text, standard_output_text,
};

#[test]
fn the_mcp_help_names_the_launch_line_and_the_runtime_flag() {
    let mcp_help = run_tatolab_reading_no_runtime_directory(&["mcp", "--help"]);

    assert!(
        mcp_help.status.success(),
        "{}",
        standard_error_text(&mcp_help)
    );
    let help_text = standard_output_text(&mcp_help);
    for named in [
        "claude mcp add tatolab -- tatolab mcp",
        "ssh <machine> tatolab mcp",
        "--node <RUNTIME_NAME_OR_ID>",
        "without reading them",
    ] {
        assert!(help_text.contains(named), "{named}:\n{help_text}");
    }
}

/// Control is reachable only through a runtime's local API socket, so the verb dials no address.
#[test]
fn mcp_takes_no_network_address_for_the_local_api() {
    let refused =
        run_tatolab_reading_no_runtime_directory(&["mcp", "--url", "http://127.0.0.1:9100"]);

    assert_eq!(refused.status.code(), Some(2));
    let refusal = standard_error_text(&refused);
    assert!(refusal.contains("unexpected argument '--url'"), "{refusal}");
}

#[cfg(target_os = "linux")]
mod against_an_isolated_registry {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use rmcp::model::{CallToolRequestParams, ContentBlock, ProtocolVersion};
    use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
    use rmcp::transport::TokioChildProcess;
    use serde_json::json;
    use tokio::io::AsyncReadExt;

    use super::common::isolated_node_registry::{
        IsolatedNodeRegistry, SCRIPTED_RUNTIME_ID, SCRIPTED_RUNTIME_NAME,
    };
    use super::common::stub_local_api_server::{
        RecordedHttpRequestHead, RecordedToolCall, StubLocalApiScript, StubLocalApiServer,
        StubMcpStdioUpgradeAnswer, StubToolAnswer,
    };
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};

    /// Longer than any run here takes; a `tatolab mcp` still running past it never exited.
    const TATOLAB_MCP_RUN_DEADLINE: Duration = Duration::from_secs(20);

    /// How long `rmcp`'s child-process transport waits for the child to exit once it closes the
    /// child's stdin, before killing it.
    const RMCP_CHILD_PROCESS_KILL_DEADLINE: Duration = Duration::from_secs(3);

    /// What the MCP host hands the verb as its stdin.
    enum McpHostStdin<'host_bytes> {
        /// A pipe the host writes these bytes into, then closes.
        WrittenThenClosed(&'host_bytes [u8]),
        /// A pipe the host holds open, writing nothing, until the verb exits.
        HeldOpen,
        /// A file the verb opens as stdin and cannot read: a directory.
        UnreadableDirectory(&'host_bytes Path),
    }

    /// Launch `tatolab mcp <mcp_verb_flags>` reading the registry under `xdg_runtime_dir`, its
    /// stdin as `mcp_host_stdin` says, and answer how it exited; stderr is always captured,
    /// stdout only when `mcp_host_stdout` is piped.
    fn run_tatolab_mcp(
        xdg_runtime_dir: &Path,
        mcp_verb_flags: &[&str],
        mcp_host_stdin: McpHostStdin<'_>,
        mcp_host_stdout: Stdio,
    ) -> Output {
        let mut tatolab_mcp_command = Command::new(env!("CARGO_BIN_EXE_tatolab"));
        tatolab_mcp_command
            .arg("mcp")
            .args(mcp_verb_flags)
            .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
            .stdout(mcp_host_stdout)
            .stderr(Stdio::piped());
        match mcp_host_stdin {
            McpHostStdin::UnreadableDirectory(directory) => {
                tatolab_mcp_command.stdin(std::fs::File::open(directory).unwrap())
            }
            McpHostStdin::WrittenThenClosed(_) | McpHostStdin::HeldOpen => {
                tatolab_mcp_command.stdin(Stdio::piped())
            }
        };
        let mut tatolab_mcp = tatolab_mcp_command.spawn().unwrap();
        let held_open_mcp_host_stdin = match mcp_host_stdin {
            McpHostStdin::WrittenThenClosed(mcp_host_bytes) => {
                let mut mcp_host_stdin_writer = tatolab_mcp.stdin.take().unwrap();
                mcp_host_stdin_writer.write_all(mcp_host_bytes).unwrap();
                None
            }
            McpHostStdin::HeldOpen => tatolab_mcp.stdin.take(),
            McpHostStdin::UnreadableDirectory(_) => None,
        };
        let tatolab_mcp_pid = libc::pid_t::try_from(tatolab_mcp.id()).unwrap();
        let (tatolab_mcp_exited, tatolab_mcp_exiting) = mpsc::channel();
        std::thread::spawn(move || {
            let _test_still_waiting = tatolab_mcp_exited.send(tatolab_mcp.wait_with_output());
        });
        let finished_run = match tatolab_mcp_exiting.recv_timeout(TATOLAB_MCP_RUN_DEADLINE) {
            Ok(finished_run) => finished_run.unwrap(),
            Err(_deadline_passed) => {
                // SAFETY: `kill` reads no memory; the pid is our own child's.
                unsafe { libc::kill(tatolab_mcp_pid, libc::SIGKILL) };
                panic!("`tatolab mcp` never exited");
            }
        };
        drop(held_open_mcp_host_stdin);
        finished_run
    }

    /// stderr's one line, asserting there is exactly one.
    fn the_one_stderr_line(finished_run: &Output) -> String {
        let standard_error = standard_error_text(finished_run);
        let stderr_lines: Vec<&str> = standard_error.lines().collect();
        assert_eq!(stderr_lines.len(), 1, "{stderr_lines:?}");
        stderr_lines[0].to_owned()
    }

    #[test]
    fn the_verb_opens_the_stream_with_one_upgrade_and_copies_both_ways_untouched() {
        // Neither side is valid MCP: the verb must not read what it carries.
        let mcp_host_bytes = b"{\"not\": \"inspected\", \"bytes\": \"\\u00e9\"}\n";
        let runtime_bytes_once_upgraded = b"\x00\xffnot even json\n";
        let runtime_bytes_once_stdin_ended = b"{\"sent\": \"after the half-close\"}\n";
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                written_once_upgraded: runtime_bytes_once_upgraded.to_vec(),
                written_once_the_client_half_closed: runtime_bytes_once_stdin_ended.to_vec(),
            },
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let piped = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &[],
            McpHostStdin::WrittenThenClosed(mcp_host_bytes),
            Stdio::piped(),
        );

        assert_eq!(
            piped.status.code(),
            Some(0),
            "{}",
            standard_error_text(&piped)
        );
        let [upgrade_request_head] = <[RecordedHttpRequestHead; 1]>::try_from(
            stub_local_api_server.recorded_mcp_stdio_request_heads(),
        )
        .expect("exactly one upgrade request");
        assert_eq!(upgrade_request_head.method, "GET");
        assert_eq!(upgrade_request_head.request_target, "/mcp/stdio");
        let mut header_lines = upgrade_request_head.header_lines;
        header_lines.sort();
        assert_eq!(
            header_lines,
            [
                ("connection".to_owned(), "Upgrade".to_owned()),
                ("host".to_owned(), "localhost".to_owned()),
                ("upgrade".to_owned(), "mcp-stdio".to_owned()),
            ]
        );
        assert_eq!(
            stub_local_api_server.recorded_mcp_stdio_client_bytes(),
            mcp_host_bytes
        );
        assert_eq!(
            piped.stdout,
            [
                runtime_bytes_once_upgraded.as_slice(),
                mcp_host_bytes,
                runtime_bytes_once_stdin_ended
            ]
            .concat(),
            "stdout carries what the runtime wrote and nothing else"
        );
        assert_eq!(standard_error_text(&piped), "");
    }

    /// Stdin is a pipe the host holds open, so the verb exits while its blocking read of stdin
    /// is still pending.
    #[test]
    fn the_runtime_closing_while_stdin_is_open_exits_non_zero_naming_the_runtime() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::CloseOnceUpgraded,
        );
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );

        let piped = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &[],
            McpHostStdin::HeldOpen,
            Stdio::piped(),
        );

        assert_eq!(piped.status.code(), Some(1));
        assert_eq!(
            the_one_stderr_line(&piped),
            format!(
                "error: runtime `{SCRIPTED_RUNTIME_NAME}` ({SCRIPTED_RUNTIME_ID}) closed its MCP \
                 stream."
            )
        );
        assert_eq!(piped.stdout, b"");
    }

    /// Reading a directory fails with `EISDIR`, so the verb's stdin cannot be read at all.
    #[test]
    fn an_unreadable_stdin_exits_non_zero_naming_stdin_after_the_runtime_answers_what_it_owes() {
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
        let directory_as_stdin = tempfile::tempdir().unwrap();

        let piped = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &[],
            McpHostStdin::UnreadableDirectory(directory_as_stdin.path()),
            Stdio::piped(),
        );

        assert_eq!(piped.status.code(), Some(1));
        let stderr_line = the_one_stderr_line(&piped);
        assert!(
            stderr_line.starts_with(&format!(
                "error: cannot read stdin into the MCP stream of runtime \
                 `{SCRIPTED_RUNTIME_NAME}` ({SCRIPTED_RUNTIME_ID}): "
            )),
            "{stderr_line}"
        );
        assert_eq!(
            piped.stdout, answer_owed_once_stdin_ended,
            "the stream was half-closed, so the runtime answered what it owed"
        );
    }

    #[test]
    fn no_live_runtime_is_a_one_line_refusal_naming_tatolab_nodes() {
        let isolated_node_registry = IsolatedNodeRegistry::new();

        let refused = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &[],
            McpHostStdin::HeldOpen,
            Stdio::piped(),
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(standard_output_text(&refused), "");
        assert_eq!(
            the_one_stderr_line(&refused),
            "error: no runtime is live on this machine for `tatolab mcp` to reach; `tatolab \
             nodes` lists the live ones."
        );
    }

    /// Writing to a pipe whose reader is gone fails with `EPIPE` rather than killing the verb.
    #[test]
    fn the_host_closing_stdout_ends_the_verb_without_a_refusal() {
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
        let (closed_mcp_host_stdout_reader, mcp_host_stdout_with_no_reader) =
            std::io::pipe().unwrap();
        drop(closed_mcp_host_stdout_reader);

        let finished = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &[],
            McpHostStdin::HeldOpen,
            Stdio::from(mcp_host_stdout_with_no_reader),
        );

        assert_eq!(
            finished.status.code(),
            Some(0),
            "{}",
            standard_error_text(&finished)
        );
        assert_eq!(standard_error_text(&finished), "");
    }

    /// The ticket's scripted exchange: `rmcp`'s client launches the real verb as a child and
    /// speaks MCP over its stdio, at the latest revision, through the verb to the stub's server.
    #[test]
    fn an_mcp_host_lists_and_calls_a_tool_through_the_verb() {
        let stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
            fixed_tool_answer: Some(StubToolAnswer::tool_result(r#"{"nodes":[]}"#)),
            listed_tool_names: vec!["graph".to_owned()],
            ..StubLocalApiScript::default()
        });
        let isolated_node_registry = IsolatedNodeRegistry::holding_one_live_runtime(
            &stub_local_api_server.local_api_socket_path,
        );
        let mut tatolab_mcp_command = tokio::process::Command::new(env!("CARGO_BIN_EXE_tatolab"));
        tatolab_mcp_command
            .arg("mcp")
            .env("XDG_RUNTIME_DIR", isolated_node_registry.xdg_runtime_dir());

        let (listed_tool_names, graph_tool_text, closing_took, verb_stderr) =
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    tokio::time::timeout(TATOLAB_MCP_RUN_DEADLINE, async {
                        let (tatolab_mcp_transport, tatolab_mcp_stderr) =
                            TokioChildProcess::builder(tatolab_mcp_command)
                                .stderr(Stdio::piped())
                                .spawn()
                                .unwrap();
                        let mcp_host_client = ()
                            .serve_with_lifecycle(
                                tatolab_mcp_transport,
                                ClientLifecycleMode::Discover {
                                    preferred_versions: vec![ProtocolVersion::LATEST],
                                },
                            )
                            .await
                            .expect("the runtime answers `server/discover` through the verb");
                        let listed_tool_names: Vec<String> = mcp_host_client
                            .list_all_tools()
                            .await
                            .unwrap()
                            .into_iter()
                            .map(|listed_tool| listed_tool.name.to_string())
                            .collect();
                        let graph_tool_result = mcp_host_client
                            .call_tool(
                                CallToolRequestParams::new("graph").with_arguments(
                                    json!({ "through": "tatolab mcp" })
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
                        let closing_started = Instant::now();
                        mcp_host_client.cancel().await.unwrap();
                        let closing_took = closing_started.elapsed();
                        let mut verb_stderr = String::new();
                        tatolab_mcp_stderr
                            .unwrap()
                            .read_to_string(&mut verb_stderr)
                            .await
                            .unwrap();
                        (
                            listed_tool_names,
                            graph_tool_text,
                            closing_took,
                            verb_stderr,
                        )
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
                tool_arguments: json!({ "through": "tatolab mcp" }),
            }]
        );
        assert!(
            closing_took < RMCP_CHILD_PROCESS_KILL_DEADLINE,
            "the verb exits by itself once the host closes stdin, rather than being killed \
             ({closing_took:?})"
        );
        assert_eq!(
            verb_stderr, "",
            "a verb that exits by itself without a word exited 0"
        );
    }
}

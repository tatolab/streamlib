// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab mcp` launched as an MCP host launches it, its stdio piped, against a stub local API.
//! The verb is a byte pipe, so its contract is bytes and exits: what it sends to open the stream,
//! that it copies both ways untouched, how stdin's end and the runtime's end each finish it, and
//! what it says on stderr. The registry is isolated through `XDG_RUNTIME_DIR`, which only Linux
//! honours, so the tests that read one are Linux-only; the in-crate tests drive the same logic on
//! every floor.

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

    use super::common::isolated_node_registry::{IsolatedNodeRegistry, a_registry_entry_named};
    use super::common::stub_local_api_server::{
        RecordedToolCall, StubLocalApiScript, StubLocalApiServer, StubMcpStdioUpgradeAnswer,
        StubToolAnswer,
    };
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};

    const SCRIPTED_RUNTIME_NAME: &str = "scripted-runtime";
    const SCRIPTED_RUNTIME_ID: &str = "Rscripted";

    /// Longer than any run here takes; a `tatolab mcp` still running past it never exited.
    const TATOLAB_MCP_RUN_DEADLINE: Duration = Duration::from_secs(20);

    /// How long `rmcp`'s child-process transport waits for the child to exit once it closes the
    /// child's stdin, before killing it.
    const RMCP_CHILD_PROCESS_KILL_DEADLINE: Duration = Duration::from_secs(3);

    /// What the MCP host does with the verb's stdin.
    enum McpHostStdin<'host_bytes> {
        /// Write these bytes, then close stdin.
        WrittenThenClosed(&'host_bytes [u8]),
        /// Hold stdin open, writing nothing, until the verb exits.
        HeldOpen,
    }

    /// A registry of the test's own holding one live runtime, the stub playing `stub_script`.
    fn one_live_runtime_playing(
        stub_script: StubLocalApiScript,
    ) -> (IsolatedNodeRegistry, StubLocalApiServer) {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve(stub_script);
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            SCRIPTED_RUNTIME_ID,
            SCRIPTED_RUNTIME_NAME,
            &stub_local_api_server.local_api_socket_path,
        ));
        (isolated_node_registry, stub_local_api_server)
    }

    fn one_live_runtime_answering_the_upgrade_with(
        mcp_stdio_upgrade_answer: StubMcpStdioUpgradeAnswer,
    ) -> (IsolatedNodeRegistry, StubLocalApiServer) {
        one_live_runtime_playing(StubLocalApiScript {
            mcp_stdio_upgrade_answer,
            ..StubLocalApiScript::default()
        })
    }

    /// Launch `tatolab mcp <mcp_verb_flags>` reading the registry under `xdg_runtime_dir`, feed
    /// its stdin as `mcp_host_stdin` says, and answer how it exited; stderr is always captured,
    /// stdout only when `mcp_host_stdout` is piped.
    fn run_tatolab_mcp(
        xdg_runtime_dir: &Path,
        mcp_verb_flags: &[&str],
        mcp_host_stdin: McpHostStdin<'_>,
        mcp_host_stdout: Stdio,
    ) -> Output {
        let mut tatolab_mcp = Command::new(env!("CARGO_BIN_EXE_tatolab"))
            .arg("mcp")
            .args(mcp_verb_flags)
            .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
            .stdin(Stdio::piped())
            .stdout(mcp_host_stdout)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut mcp_host_stdin_writer = tatolab_mcp.stdin.take().unwrap();
        let held_open_mcp_host_stdin = match mcp_host_stdin {
            McpHostStdin::WrittenThenClosed(mcp_host_bytes) => {
                mcp_host_stdin_writer.write_all(mcp_host_bytes).unwrap();
                drop(mcp_host_stdin_writer);
                None
            }
            McpHostStdin::HeldOpen => Some(mcp_host_stdin_writer),
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
        let (isolated_node_registry, stub_local_api_server) =
            one_live_runtime_answering_the_upgrade_with(
                StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                    written_once_upgraded: runtime_bytes_once_upgraded.to_vec(),
                    written_once_the_client_half_closed: runtime_bytes_once_stdin_ended.to_vec(),
                },
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
        let upgrade_request_heads = stub_local_api_server.recorded_mcp_stdio_request_heads();
        assert_eq!(upgrade_request_heads.len(), 1, "{upgrade_request_heads:?}");
        assert_eq!(upgrade_request_heads[0].method, "GET");
        assert_eq!(upgrade_request_heads[0].request_target, "/mcp/stdio");
        let mut header_lines = upgrade_request_heads[0].header_lines.clone();
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

    #[test]
    fn stdin_ending_half_closes_the_stream_and_the_verb_exits_when_the_runtime_closes() {
        let answer_owed_after_stdin_ended = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n";
        let (isolated_node_registry, stub_local_api_server) =
            one_live_runtime_answering_the_upgrade_with(
                StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                    written_once_upgraded: Vec::new(),
                    written_once_the_client_half_closed: answer_owed_after_stdin_ended.to_vec(),
                },
            );

        let piped = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &[],
            McpHostStdin::WrittenThenClosed(b"{\"id\":1}\n"),
            Stdio::piped(),
        );

        assert_eq!(
            piped.status.code(),
            Some(0),
            "{}",
            standard_error_text(&piped)
        );
        assert_eq!(
            stub_local_api_server.recorded_mcp_stdio_client_bytes(),
            b"{\"id\":1}\n",
            "stdin's end reaches the runtime"
        );
        assert_eq!(
            piped.stdout,
            [b"{\"id\":1}\n".as_slice(), answer_owed_after_stdin_ended].concat()
        );
    }

    #[test]
    fn the_runtime_closing_while_stdin_is_open_exits_non_zero_naming_the_runtime() {
        let (isolated_node_registry, _stub_local_api_server) =
            one_live_runtime_answering_the_upgrade_with(
                StubMcpStdioUpgradeAnswer::CloseOnceUpgraded,
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

    #[test]
    fn a_runtime_refusing_the_upgrade_exits_non_zero_naming_it_and_its_answer() {
        let (isolated_node_registry, _stub_local_api_server) =
            one_live_runtime_answering_the_upgrade_with(
                StubMcpStdioUpgradeAnswer::RefuseTheUpgrade { http_status: 426 },
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
                "error: runtime `{SCRIPTED_RUNTIME_NAME}` ({SCRIPTED_RUNTIME_ID}) did not open \
                 its MCP stream: it answered `HTTP/1.1 426 Upgrade Required`"
            )
        );
        assert_eq!(piped.stdout, b"");
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

    #[test]
    fn no_live_runtime_is_the_same_refusal_when_node_names_one() {
        let isolated_node_registry = IsolatedNodeRegistry::new();

        let refused = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &["--node", "absent-runtime"],
            McpHostStdin::HeldOpen,
            Stdio::piped(),
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(standard_output_text(&refused), "");
        assert!(
            the_one_stderr_line(&refused).contains("`tatolab nodes`"),
            "{}",
            standard_error_text(&refused)
        );
    }

    #[test]
    fn a_node_flag_matching_no_live_runtime_is_refused_naming_it() {
        let (isolated_node_registry, stub_local_api_server) =
            one_live_runtime_playing(StubLocalApiScript::default());

        let refused = run_tatolab_mcp(
            isolated_node_registry.xdg_runtime_dir(),
            &["--node", "absent-runtime"],
            McpHostStdin::HeldOpen,
            Stdio::piped(),
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(standard_output_text(&refused), "");
        let refusal = standard_error_text(&refused);
        assert!(refusal.contains("absent-runtime"), "{refusal}");
        assert!(
            refusal.contains(SCRIPTED_RUNTIME_NAME),
            "the refusal lists the live runtimes: {refusal}"
        );
        assert_eq!(stub_local_api_server.recorded_mcp_stdio_request_heads(), []);
    }

    #[test]
    fn the_host_closing_stdout_ends_the_verb_without_a_refusal() {
        let (isolated_node_registry, _stub_local_api_server) =
            one_live_runtime_answering_the_upgrade_with(
                StubMcpStdioUpgradeAnswer::EchoUntilTheClientHalfCloses {
                    written_once_upgraded: b"{\"jsonrpc\":\"2.0\",\"method\":\"nobody reads\"}\n"
                        .to_vec(),
                    written_once_the_client_half_closed: Vec::new(),
                },
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
        let (isolated_node_registry, stub_local_api_server) =
            one_live_runtime_playing(StubLocalApiScript {
                fixed_tool_answer: Some(StubToolAnswer::tool_result(r#"{"nodes":[]}"#)),
                listed_tool_names: vec!["graph".to_owned()],
                ..StubLocalApiScript::default()
            });
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

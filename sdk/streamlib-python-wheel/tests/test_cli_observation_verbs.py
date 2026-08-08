# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The observation verbs, driven without a running node.

`nodes` / `graph` / `tap` / `logs` are clients: of the on-disk node registry, of
a node's `POST /mcp`, and of the on-disk JSONL log. Each of those is stood up
here — a real HTTP server on a loopback port, a temp registry directory, a temp
log directory — so the whole surface is exercised in CI, where no GPU exists to
boot a real node with. `test_cli_launch.py` covers the live path on the rig.

The rendering assertions are the load-bearing ones: the JSONL schema and its
pretty form are durable contracts, and a record read by this CLI must come out
byte-identical to what the runtime mirrored to its own stdout.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Any, Optional

import pytest

from streamlib import cli
from streamlib._control_plane_client import (
    ControlPlaneError,
    call_tool,
    control_plane_answers,
    resolve_control_url,
)
from streamlib._node_registry import registry_directory, scan_check_and_prune
from streamlib._runtime_log_reader import (
    LogRecordFilters,
    RuntimeLogFile,
    enumerate_runtime_log_files,
    format_record_pretty,
    format_size,
    format_started_at,
    newest_log_file_for_runtime,
    read_log_file,
)

# A pid that cannot belong to a live process: above the kernel's pid_max on
# every platform the wheel targets, so `kill(pid, 0)` is ESRCH rather than a
# real process that happens to be running. Deliberately inside `pid_t` — the
# out-of-range case has its own test, because it raises OverflowError, which is
# not an OSError and once escaped the scan entirely.
UNUSED_PID = 4_000_000

#: Outside `pid_t`, which only a corrupt registry entry could carry.
PID_OUTSIDE_PID_T = 4_000_000_000


class StubControlPlane:
    """A loopback HTTP server standing in for a node's `POST /mcp`.

    Records every request body so a test can prove the verb marshalled what it
    claimed to, and answers from a queue so tool errors and auth rejections are
    reachable without a live runtime.
    """

    def __init__(self, status: int = 200, body: "Optional[str]" = None) -> None:
        self.recorded_bodies: "list[str]" = []
        self.recorded_authorizations: "list[Optional[str]]" = []
        self._status = status
        self._body = body if body is not None else _tool_result_body("{}")

        stub = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802 — BaseHTTPRequestHandler's name
                length = int(self.headers.get("content-length", "0"))
                stub.recorded_bodies.append(self.rfile.read(length).decode("utf-8"))
                stub.recorded_authorizations.append(self.headers.get("authorization"))
                payload = stub._body.encode("utf-8")
                self.send_response(stub._status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
                """Silence the default stderr access log."""

        self._server = HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self._server.server_port}"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)


def _tool_result_body(text: str, *, is_error: bool = False) -> str:
    return json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"content": [{"type": "text", "text": text}], "isError": is_error},
        }
    )


@pytest.fixture
def stub_control_plane():
    servers: "list[StubControlPlane]" = []

    def make(**kwargs: Any) -> StubControlPlane:
        server = StubControlPlane(**kwargs)
        servers.append(server)
        return server

    yield make
    for server in servers:
        server.close()


@pytest.fixture
def isolated_registry(tmp_path, monkeypatch):
    """Point the node registry at a temp dir so tests never see real nodes."""
    registry = tmp_path / "runtime-dir"
    registry.mkdir()
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(registry))
    monkeypatch.delenv("STREAMLIB_MCP_TOKEN", raising=False)
    return registry / "streamlib" / "nodes"


def write_registry_entry(
    registry: Path, runtime_id: str, control_url: str, *, pid: "Optional[int]" = None
) -> Path:
    registry.mkdir(parents=True, exist_ok=True)
    entry_path = registry / f"{runtime_id}.json"
    entry_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "runtime_id": runtime_id,
                "control_url": control_url,
                "pid": os.getpid() if pid is None else pid,
                "hint": "python (/tmp/app)",
            }
        ),
        encoding="utf-8",
    )
    return entry_path


# ─── The node registry ───────────────────────────────────────────────────────


def test_registry_directory_follows_xdg_runtime_dir(isolated_registry):
    assert registry_directory() == isolated_registry


def test_a_reachable_entry_is_listed_as_alive(isolated_registry, stub_control_plane):
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ralive", server.url)

    discovered = scan_check_and_prune()

    assert [(node.entry.runtime_id, node.reachable) for node in discovered] == [
        ("Ralive", True)
    ]


def test_an_entry_that_is_unreachable_and_dead_is_pruned(isolated_registry):
    # Port 1 is never bound by a normal user process, so the probe gets a
    # transport error rather than a slow answer.
    entry_path = write_registry_entry(
        isolated_registry, "Rdead", "http://127.0.0.1:1", pid=UNUSED_PID
    )

    assert scan_check_and_prune() == []
    assert not entry_path.exists(), "both liveness signals said dead — prune the entry"


def test_an_unreachable_entry_with_a_live_process_is_kept_but_not_alive(
    isolated_registry,
):
    # The node's process is this test's, which is unambiguously alive, while its
    # control plane answers nothing. Pruning here would delete a live node's
    # entry because it was briefly slow.
    entry_path = write_registry_entry(
        isolated_registry, "Rbusy", "http://127.0.0.1:1"
    )

    discovered = scan_check_and_prune()

    assert [(node.entry.runtime_id, node.reachable) for node in discovered] == [
        ("Rbusy", False)
    ]
    assert entry_path.exists(), "one dead signal is not enough to prune"


def test_a_pid_outside_pid_t_does_not_crash_the_scan(
    isolated_registry, stub_control_plane
):
    # `os.kill` raises OverflowError — not an OSError — for a pid this large, so
    # an unguarded liveness check takes the whole scan down and makes every
    # healthy node undiscoverable alongside the corrupt entry.
    server = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rcorrupt", "http://127.0.0.1:1", pid=PID_OUTSIDE_PID_T
    )
    write_registry_entry(isolated_registry, "Rgood", server.url)

    discovered = scan_check_and_prune()

    assert [node.entry.runtime_id for node in discovered] == ["Rgood"]


def test_a_malformed_entry_does_not_hide_the_others(
    isolated_registry, stub_control_plane
):
    server = stub_control_plane()
    isolated_registry.mkdir(parents=True, exist_ok=True)
    (isolated_registry / "Rgarbage.json").write_text("{ not json", encoding="utf-8")
    write_registry_entry(isolated_registry, "Rgood", server.url)

    discovered = scan_check_and_prune()

    assert [node.entry.runtime_id for node in discovered] == ["Rgood"]


def test_an_auth_rejection_still_counts_as_reachable(stub_control_plane):
    # A 401 proves a control plane is up. Treating it as dead would prune a
    # node that is merely gated.
    server = stub_control_plane(status=401, body='{"error":"unauthorized"}')

    assert control_plane_answers(server.url) is True


def test_nothing_listening_is_not_reachable():
    assert control_plane_answers("http://127.0.0.1:1") is False


# ─── Resolving which node a verb drives ──────────────────────────────────────


def test_an_explicit_url_wins_without_consulting_the_registry(isolated_registry):
    assert resolve_control_url("http://127.0.0.1:9999", None) == "http://127.0.0.1:9999"


def test_the_sole_live_node_is_the_default_target(isolated_registry, stub_control_plane):
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ronly", server.url)

    assert resolve_control_url(None, None) == server.url


def test_a_named_node_resolves_to_its_url(isolated_registry, stub_control_plane):
    first = stub_control_plane()
    second = stub_control_plane()
    write_registry_entry(isolated_registry, "Rfirst", first.url)
    write_registry_entry(isolated_registry, "Rsecond", second.url)

    assert resolve_control_url(None, "Rsecond") == second.url


def test_two_live_nodes_and_no_flag_is_an_error_that_lists_them(
    isolated_registry, stub_control_plane
):
    first = stub_control_plane()
    second = stub_control_plane()
    write_registry_entry(isolated_registry, "Rfirst", first.url)
    write_registry_entry(isolated_registry, "Rsecond", second.url)

    with pytest.raises(ControlPlaneError) as failure:
        resolve_control_url(None, None)

    message = str(failure.value)
    assert "Rfirst" in message and "Rsecond" in message
    assert "--node" in message, "the error must name the flag that resolves it"


def test_no_live_nodes_names_the_command_that_starts_one(isolated_registry):
    with pytest.raises(ControlPlaneError) as failure:
        resolve_control_url(None, None)

    assert "streamlib dev" in str(failure.value)


# ─── Driving a tool ──────────────────────────────────────────────────────────


def test_a_tool_call_marshals_the_jsonrpc_envelope(stub_control_plane):
    server = stub_control_plane(body=_tool_result_body('{"nodes":[]}'))

    result = call_tool(server.url, "tap", {"channel": "cam/video", "count": 4})

    assert result == '{"nodes":[]}'
    sent = json.loads(server.recorded_bodies[0])
    assert sent["method"] == "tools/call"
    assert sent["params"]["name"] == "tap"
    assert sent["params"]["arguments"] == {"channel": "cam/video", "count": 4}


def test_a_bearer_token_rides_as_an_authorization_header(stub_control_plane, monkeypatch):
    server = stub_control_plane()
    monkeypatch.setenv("STREAMLIB_MCP_TOKEN", "secret-token")

    call_tool(server.url, "graph", {})

    assert server.recorded_authorizations[0] == "Bearer secret-token"


def test_no_token_sends_no_authorization_header(stub_control_plane, monkeypatch):
    server = stub_control_plane()
    monkeypatch.delenv("STREAMLIB_MCP_TOKEN", raising=False)

    call_tool(server.url, "graph", {})

    assert server.recorded_authorizations[0] is None


def test_a_tool_level_error_is_raised_not_printed_as_a_result(stub_control_plane):
    server = stub_control_plane(
        body=_tool_result_body("no such channel", is_error=True)
    )

    with pytest.raises(ControlPlaneError, match="no such channel"):
        call_tool(server.url, "tap", {"channel": "nope"})


def test_a_jsonrpc_error_inside_an_http_200_is_raised(stub_control_plane):
    server = stub_control_plane(
        body=json.dumps(
            {"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "nope"}}
        )
    )

    with pytest.raises(ControlPlaneError, match="nope"):
        call_tool(server.url, "graph", {})


def test_a_non_2xx_status_is_raised_with_its_code(stub_control_plane):
    server = stub_control_plane(status=403, body="forbidden")

    with pytest.raises(ControlPlaneError, match="403"):
        call_tool(server.url, "graph", {})


def test_an_unreachable_node_names_the_url():
    with pytest.raises(ControlPlaneError, match="127.0.0.1:1"):
        call_tool("http://127.0.0.1:1", "graph", {})


# ─── Reading the on-disk log ─────────────────────────────────────────────────


def a_log_record(**overrides: Any) -> "dict[str, Any]":
    record = {
        "schema_version": 1,
        "host_ts": 1_786_136_667_573_387_556,
        "runtime_id": "Rabc",
        "source": "rust",
        "level": "info",
        "message": "Creating Runner",
        "target": "streamlib_engine::core::runtime",
        "intercepted": False,
    }
    record.update(overrides)
    return record


def write_log_file(
    directory: Path, runtime_id: str, millis: int, records
) -> RuntimeLogFile:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{runtime_id}-{millis}.jsonl"
    path.write_text(
        "".join(json.dumps(record) + "\n" for record in records), encoding="utf-8"
    )
    return RuntimeLogFile(
        runtime_id=runtime_id,
        started_at_millis=millis,
        path=path,
        size_bytes=path.stat().st_size,
    )


def test_a_record_renders_exactly_as_the_runtime_mirrored_it():
    # Byte-for-byte against the engine's `format_event_pretty`: the timestamp is
    # HH:MM:SS.mmm, the level column is five characters right-aligned, and the
    # separator is an em dash. Verified against the Rust CLI's output on a real
    # log file; a drift here means one record reads as two different records.
    rendered = format_record_pretty(a_log_record())

    assert rendered == (
        "21:04:27.573 [ INFO] [Rabc/rust] streamlib_engine::core::runtime — "
        "Creating Runner"
    )


def test_the_optional_columns_render_in_the_engine_s_order():
    rendered = format_record_pretty(
        a_log_record(
            pipeline_id="pipe", processor_id="proc", rhi_op="acquire_texture"
        )
    )

    assert rendered.endswith(
        " pipeline_id=pipe processor_id=proc rhi_op=acquire_texture"
    )


def test_attrs_render_as_compact_json_in_sorted_order():
    # The engine writes attrs through `serde_json::Value`'s Display, so a string
    # keeps its quotes and a number does not gain any. Sorted because the engine
    # holds them in a BTreeMap.
    rendered = format_record_pretty(
        a_log_record(attrs={"width": 1920, "origin": "Config::global_config()"})
    )

    assert rendered.endswith(' origin="Config::global_config()" width=1920')


def test_non_ascii_attrs_survive_unescaped():
    # `json.dumps` escapes non-ASCII by default; serde_json's escape table marks
    # 0x80-0xFF as no-escape, so the runtime's own mirror writes raw UTF-8. An
    # escaped rendering here makes one record read as two different records.
    rendered = format_record_pretty(a_log_record(attrs={"device": "Logitech Café"}))

    assert rendered.endswith(' device="Logitech Café"')


def test_a_float_attr_uses_the_shortest_round_trip_exponent():
    # Python spells the exponent `1e+20` / `1e-07`; serde uses ryu, which emits
    # `1e20` / `1e-7`. Same value, and both are shortest-round-trip — only the
    # spelling differed.
    rendered = format_record_pretty(a_log_record(attrs={"big": 1e20, "small": 1e-7}))

    assert rendered.endswith(" big=1e20 small=1e-7")


def test_a_started_at_stamp_reads_as_a_date_not_epoch_millis():
    # `--list` exists so a human can pick a runtime_id out of it.
    assert format_started_at(1_786_136_667_573) == "2026-08-07T21:04:27Z"


@pytest.mark.parametrize(
    "size_bytes,expected",
    [(512, "512 B"), (2048, "2.0 KiB"), (5 * 1024**2, "5.0 MiB"), (3 * 1024**3, "3.0 GiB")],
)
def test_a_size_reads_in_binary_units(size_bytes, expected):
    assert format_size(size_bytes) == expected


def test_the_newest_file_for_a_runtime_wins(tmp_path):
    write_log_file(tmp_path, "Rabc", 1000, [a_log_record(message="old")])
    write_log_file(tmp_path, "Rabc", 2000, [a_log_record(message="new")])

    newest = newest_log_file_for_runtime(tmp_path, "Rabc")

    assert newest is not None and newest.started_at_millis == 2000


def test_a_runtime_id_containing_dashes_still_parses(tmp_path):
    write_log_file(tmp_path, "R-with-dashes", 1234, [a_log_record()])

    found = enumerate_runtime_log_files(tmp_path)

    assert [(f.runtime_id, f.started_at_millis) for f in found] == [
        ("R-with-dashes", 1234)
    ]


@pytest.mark.parametrize(
    "filters,expected_messages",
    [
        (LogRecordFilters(), ["info-rust", "warn-python", "rhi-op", "intercepted"]),
        (LogRecordFilters(minimum_level="warn"), ["warn-python"]),
        (LogRecordFilters(source="python"), ["warn-python"]),
        (LogRecordFilters(rhi_only=True), ["rhi-op"]),
        (LogRecordFilters(intercepted_only=True), ["intercepted"]),
        (LogRecordFilters(processor="proc-1"), ["rhi-op"]),
        (LogRecordFilters(pipeline="pipe-1"), ["intercepted"]),
        (LogRecordFilters(processor="absent"), []),
    ],
)
def test_each_filter_narrows_to_the_records_it_names(
    tmp_path, filters, expected_messages
):
    log_file = write_log_file(
        tmp_path,
        "Rabc",
        1000,
        [
            a_log_record(message="info-rust"),
            a_log_record(message="warn-python", level="warn", source="python"),
            a_log_record(message="rhi-op", rhi_op="acquire", processor_id="proc-1"),
            a_log_record(message="intercepted", intercepted=True, pipeline_id="pipe-1"),
        ],
    )

    rendered = list(
        read_log_file(log_file, filters, follow=False, errors=io.StringIO())
    )

    assert [line.split(" — ", 1)[1].split(" ")[0] for line in rendered] == expected_messages


def test_a_malformed_line_is_reported_and_skipped_not_fatal(tmp_path):
    path = tmp_path / "Rabc-1000.jsonl"
    path.write_text(
        json.dumps(a_log_record(message="before")) + "\n"
        "{ truncated\n" + json.dumps(a_log_record(message="after")) + "\n",
        encoding="utf-8",
    )
    log_file = RuntimeLogFile("Rabc", 1000, path, path.stat().st_size)
    errors = io.StringIO()

    rendered = list(
        read_log_file(log_file, LogRecordFilters(), follow=False, errors=errors)
    )

    assert [line.split(" — ", 1)[1].split(" ")[0] for line in rendered] == ["before", "after"]
    assert "malformed" in errors.getvalue()


def test_follow_yields_lines_appended_after_the_drain(tmp_path):
    log_file = write_log_file(tmp_path, "Rabc", 1000, [a_log_record(message="first")])
    lines = read_log_file(
        log_file, LogRecordFilters(), follow=True, errors=io.StringIO(),
        log_directory=tmp_path,
    )

    assert next(lines).endswith("first")

    with log_file.path.open("a", encoding="utf-8") as appending:
        appending.write(json.dumps(a_log_record(message="second")) + "\n")

    assert next(lines).endswith("second")
    lines.close()


def test_follow_switches_to_a_newer_file_when_the_runtime_restarts(tmp_path):
    # A restart under a pinned STREAMLIB_RUNTIME_ID writes a SECOND file for the
    # same runtime. Without the switch the tail sits on a file that will never
    # grow again and goes silently quiet.
    first = write_log_file(tmp_path, "Rabc", 1000, [a_log_record(message="before")])
    errors = io.StringIO()
    lines = read_log_file(
        first, LogRecordFilters(), follow=True, errors=errors, log_directory=tmp_path
    )

    assert next(lines).endswith("before")

    write_log_file(tmp_path, "Rabc", 2000, [a_log_record(message="after-restart")])

    assert next(lines).endswith("after-restart")
    assert "rotated to a newer log file" in errors.getvalue()
    lines.close()


def test_an_entry_whose_schema_version_is_unknown_is_neither_listed_nor_deleted(
    isolated_registry,
):
    # The version field exists so a reader rejects what it cannot parse. Parsing
    # it far enough to prune it would delete a record written by a newer engine.
    isolated_registry.mkdir(parents=True, exist_ok=True)
    entry_path = isolated_registry / "Rfuture.json"
    entry_path.write_text(
        json.dumps(
            {
                "schema_version": 2,
                "runtime_id": "Rfuture",
                "control_url": "http://127.0.0.1:1",
                "pid": UNUSED_PID,
                "hint": "written by a newer engine",
            }
        ),
        encoding="utf-8",
    )

    assert scan_check_and_prune() == []
    assert entry_path.exists(), "a reader must not delete a record it cannot parse"


# ─── The CLI surface ─────────────────────────────────────────────────────────


def served_verbs() -> "list[str]":
    """Every subcommand `streamlib` parses, read off the built parser."""
    parser = cli.build_argument_parser()
    for action in parser._actions:
        if isinstance(action, argparse._SubParsersAction):
            return list(action.choices)
    raise AssertionError("the parser must carry a subcommand group")


def test_every_observation_verb_is_a_subcommand():
    for verb in ("nodes", "graph", "tap", "logs"):
        assert verb in served_verbs(), f"`streamlib {verb}` must be a real subcommand"


def test_the_not_yet_in_the_wheel_stopgap_is_gone():
    # It existed only until these verbs landed; leaving it would refuse a verb
    # this CLI now serves.
    assert not hasattr(cli, "OBSERVATION_VERBS_NOT_YET_IN_THE_WHEEL")


def test_the_wheel_serves_no_mcp_verb():
    # MCP is served by the node's own control plane at POST /mcp, on the node's
    # lifecycle — there is no CLI verb to start or attach one.
    assert "mcp" not in served_verbs()


def test_nodes_reports_an_empty_registry_without_failing(isolated_registry, capsys):
    assert cli.main(["nodes"]) == 0

    assert "No running nodes found" in capsys.readouterr().out


def test_nodes_renders_a_live_node_as_a_table(
    isolated_registry, stub_control_plane, capsys
):
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Rlisted", server.url)

    assert cli.main(["nodes"]) == 0

    printed = capsys.readouterr().out
    assert "RUNTIME_ID" in printed
    assert "Rlisted" in printed and server.url in printed
    assert "yes" in printed


def test_graph_prints_the_tool_result(isolated_registry, stub_control_plane, capsys):
    server = stub_control_plane(body=_tool_result_body('{"nodes":[]}'))
    write_registry_entry(isolated_registry, "Ronly", server.url)

    assert cli.main(["graph"]) == 0

    assert '{"nodes":[]}' in capsys.readouterr().out


def test_tap_sends_the_channel_and_count(
    isolated_registry, stub_control_plane, capsys
):
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ronly", server.url)

    assert cli.main(["tap", "cam/video", "--count", "3"]) == 0

    arguments = json.loads(server.recorded_bodies[-1])["params"]["arguments"]
    assert arguments == {"channel": "cam/video", "count": 3}


def test_a_control_target_with_on_disk_filters_is_refused(
    isolated_registry, stub_control_plane, capsys
):
    # The live event-stream tool takes a count and nothing else, so a filter
    # here would be silently ignored rather than applied.
    server = stub_control_plane()

    assert cli.main(["logs", "--url", server.url, "--level", "warn"]) == 1

    assert "--level" in capsys.readouterr().err


def test_a_count_without_a_control_target_is_refused(isolated_registry, capsys):
    assert cli.main(["logs", "Rabc", "--count", "5"]) == 1

    assert "--count" in capsys.readouterr().err


def test_logs_without_a_runtime_id_names_list(isolated_registry, capsys, monkeypatch):
    monkeypatch.setattr(
        "streamlib._runtime_log_reader.runtime_log_directory_path",
        lambda: Path("/nonexistent"),
    )

    assert cli.main(["logs"]) == 1

    assert "--list" in capsys.readouterr().err

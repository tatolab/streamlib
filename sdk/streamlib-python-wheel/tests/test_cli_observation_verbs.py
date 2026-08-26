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
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Any, Generator, NamedTuple, Optional

import pytest

from streamlib import cli
from streamlib._control_plane_client import (
    ControlPlaneError,
    SurfaceImageExchangeRefusal,
    call_tool,
    control_plane_answers,
    fetch_surface_image_png_bytes,
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


class StubSurfaceImageAnswer(NamedTuple):
    """How the stub answers one `GET /api/surfaces/{id}/image`.

    A `410` is the load-bearing one: the id was real and its pool slot has since
    been recycled, which is the refusal the channel form composes as a retry.
    """

    status: int
    png_image_bytes: bytes = b""
    source_surface_pixel_width: "Optional[int]" = None
    source_surface_pixel_height: "Optional[int]" = None
    error_message: str = ""


class StubControlPlane:
    """A loopback HTTP server standing in for a node's control plane.

    Records every request body so a test can prove the verb marshalled what it
    claimed to, and answers from a queue so tool errors and auth rejections are
    reachable without a live runtime.

    Both front ends of the exchange live here, because the CLI drives both: the
    `POST /mcp` the tool calls ride, and the binary `GET` the full-resolution
    image route serves.
    """

    def __init__(
        self,
        status: int = 200,
        body: "Optional[str]" = None,
        *,
        queued_bodies: "Optional[list[str]]" = None,
        surface_image_answers: "Optional[dict[str, StubSurfaceImageAnswer]]" = None,
    ) -> None:
        self.recorded_bodies: "list[str]" = []
        self.recorded_authorizations: "list[Optional[str]]" = []
        self.recorded_image_request_paths: "list[str]" = []
        self.recorded_image_authorizations: "list[Optional[str]]" = []
        self._status = status
        self._body = body if body is not None else _tool_result_body("{}")
        self._queued_bodies = list(queued_bodies or [])
        self._surface_image_answers = dict(surface_image_answers or {})

        stub = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802 — BaseHTTPRequestHandler's name
                length = int(self.headers.get("content-length", "0"))
                stub.recorded_bodies.append(self.rfile.read(length).decode("utf-8"))
                stub.recorded_authorizations.append(self.headers.get("authorization"))
                # The queue drains in order and then the fixed body answers
                # forever, so a test names only the rounds it cares about.
                body = (
                    stub._queued_bodies.pop(0) if stub._queued_bodies else stub._body
                )
                payload = body.encode("utf-8")
                self.send_response(stub._status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def do_GET(self) -> None:  # noqa: N802 — BaseHTTPRequestHandler's name
                stub.recorded_image_request_paths.append(self.path)
                stub.recorded_image_authorizations.append(
                    self.headers.get("authorization")
                )
                answer = stub._surface_image_answers.get(
                    _surface_id_in_image_route_path(self.path),
                    StubSurfaceImageAnswer(404, error_message="no such surface"),
                )
                if answer.status == 200:
                    self.send_response(200)
                    self.send_header("content-type", "image/png")
                    if answer.source_surface_pixel_width is not None:
                        self.send_header(
                            "x-streamlib-surface-pixel-width",
                            str(answer.source_surface_pixel_width),
                        )
                    if answer.source_surface_pixel_height is not None:
                        self.send_header(
                            "x-streamlib-surface-pixel-height",
                            str(answer.source_surface_pixel_height),
                        )
                    payload = answer.png_image_bytes
                else:
                    self.send_response(answer.status)
                    self.send_header("content-type", "application/json")
                    payload = json.dumps({"error": answer.error_message}).encode("utf-8")
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


def _surface_id_in_image_route_path(path: str) -> str:
    """The surface id out of `/api/surfaces/{surface_id}/image`, decoded."""
    segments = urllib.parse.urlparse(path).path.strip("/").split("/")
    if len(segments) != 4 or segments[0] != "api" or segments[1] != "surfaces":
        return ""
    return urllib.parse.unquote(segments[2])


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


@pytest.mark.parametrize("url", ["file:///etc/passwd", "ftp://example.invalid/x"])
def test_a_non_http_url_is_refused_before_any_request(url):
    # `urlopen` dispatches on the scheme, so an unchecked URL selects a handler
    # that is not HTTP at all — from `--url`, or from a corrupt `control_url`
    # read out of a registry entry.
    with pytest.raises(ControlPlaneError, match="http or https"):
        call_tool(url, "graph", {})


@pytest.mark.parametrize(
    "body",
    [
        '"a bare string"',
        "[1, 2, 3]",
        '{"jsonrpc":"2.0","id":1,"error":"not an object"}',
        '{"jsonrpc":"2.0","id":1,"result":"not an object"}',
        '{"jsonrpc":"2.0","id":1,"result":{"content":"not a list"}}',
    ],
)
def test_a_misshapen_200_surfaces_as_a_control_plane_error(stub_control_plane, body):
    # A server answering 200 with an unexpected shape must fail as itself, not
    # as an AttributeError traceback out of the parsing path.
    server = stub_control_plane(body=body)

    with pytest.raises(ControlPlaneError):
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


def test_an_out_of_range_stamp_degrades_instead_of_taking_the_listing_down():
    # The file name is parsed with an unbounded `int()`, so one stray file in
    # the log directory reaches this. Raising here would hide every healthy
    # runtime in the same listing.
    assert format_started_at(99999999999999999999) == "99999999999999999999"
    assert format_started_at(253402300800000) == "253402300800000"


@pytest.mark.parametrize(
    "value,expected",
    [
        (1e20, "1e20"),
        (1e-7, "1e-7"),
        (1e-6, "1e-6"),
        # ryu switches to decimal one decade earlier than Python at the small
        # end (its rule is decimal when -5 < kk <= 0), so this band is the one
        # place exponent-rewriting alone would still have diverged.
        (1e-5, "0.00001"),
        (2.5e-5, "0.000025"),
        (-1e-5, "-0.00001"),
        (1e-4, "0.0001"),
        (29.97, "29.97"),
    ],
)
def test_a_float_attr_matches_ryu_s_shortest_form(value, expected):
    rendered = format_record_pretty(a_log_record(attrs={"v": value}))

    assert rendered.endswith(f" v={expected}")


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
        read_log_file(
            log_file,
            filters,
            follow=False,
            errors=io.StringIO(),
            log_directory=tmp_path,
        )
    )

    assert [line.split(" — ", 1)[1].split(" ")[0] for line in rendered] == expected_messages


@pytest.mark.parametrize(
    "bad_record",
    [
        {"host_ts": None, "level": "info"},
        {"host_ts": 1, "level": 7},
        {"host_ts": 1, "level": "info", "attrs": "not a mapping"},
        ["not", "an", "object"],
    ],
)
def test_a_schema_invalid_record_is_skipped_like_a_malformed_line(tmp_path, bad_record):
    # It decodes as JSON but is not shaped like a record; carrying it into the
    # renderer would end the whole read over one bad line.
    path = tmp_path / "Rabc-1000.jsonl"
    path.write_text(
        json.dumps(bad_record) + "\n" + json.dumps(a_log_record(message="good")) + "\n",
        encoding="utf-8",
    )
    log_file = RuntimeLogFile("Rabc", 1000, path, path.stat().st_size)
    errors = io.StringIO()

    rendered = list(
        read_log_file(
            log_file,
            LogRecordFilters(),
            follow=False,
            errors=errors,
            log_directory=tmp_path,
        )
    )

    assert [line.split(" — ", 1)[1] for line in rendered] == ["good"]
    assert "skipping" in errors.getvalue()


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
        read_log_file(
            log_file,
            LogRecordFilters(),
            follow=False,
            errors=errors,
            log_directory=tmp_path,
        )
    )

    assert [line.split(" — ", 1)[1].split(" ")[0] for line in rendered] == ["before", "after"]
    assert "malformed" in errors.getvalue()


#: A follow assertion that is red rather than hung when the branch it locks is
#: reverted. The suite configures no pytest-timeout, so an unbounded `next()`
#: on a parked generator blocks CI instead of failing it.
FOLLOW_LINE_TIMEOUT_SECONDS = 15.0


def next_line_within_timeout(
    lines: "Generator[str, None, None]", what: str
) -> str:
    """The generator's next line, or a failure — never an unbounded wait."""
    collected: "list[str]" = []

    def pull() -> None:
        try:
            collected.append(next(lines))
        except StopIteration:
            pass

    puller = threading.Thread(target=pull, daemon=True)
    puller.start()
    puller.join(FOLLOW_LINE_TIMEOUT_SECONDS)
    assert collected, (
        f"expected {what} within {FOLLOW_LINE_TIMEOUT_SECONDS}s; the follow loop "
        f"is parked, which is what a reverted rotation/append branch looks like"
    )
    return collected[0]


def test_follow_yields_lines_appended_after_the_drain(tmp_path):
    log_file = write_log_file(tmp_path, "Rabc", 1000, [a_log_record(message="first")])
    lines = read_log_file(
        log_file,
        LogRecordFilters(),
        follow=True,
        errors=io.StringIO(),
        log_directory=tmp_path,
    )

    assert next_line_within_timeout(lines, "the drained line").endswith("first")

    with log_file.path.open("a", encoding="utf-8") as appending:
        appending.write(json.dumps(a_log_record(message="second")) + "\n")

    assert next_line_within_timeout(lines, "the appended line").endswith("second")
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

    assert next_line_within_timeout(lines, "the pre-restart line").endswith("before")

    write_log_file(tmp_path, "Rabc", 2000, [a_log_record(message="after-restart")])

    assert next_line_within_timeout(lines, "the post-restart line").endswith(
        "after-restart"
    )
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
    for verb in ("nodes", "graph", "tap", "logs", "exchange"):
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


def test_list_refuses_the_flags_it_would_otherwise_ignore(isolated_registry, capsys):
    # `--list` reads no log file, so it returns before these are consulted. The
    # control-target path already calls a silently-dropped flag a wiring error;
    # both modes should agree on that.
    assert cli.main(["logs", "--list", "--level", "warn"]) == 1

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


# ─── The surface exchange ────────────────────────────────────────────────────
#
# The wire fixtures below are written by hand rather than through the engine's
# own encoder: a decode this CLI depends on must be proved against the format
# the transport actually writes, not against a round trip with itself.

#: `[port_key_len: 1][port_key_name: 63][timestamp_ns: 8 LE][payload_len: 4 LE]`,
#: as `runtime/streamlib-ipc-types` lays it out.
FRAME_HEADER_SIZE = 76
PORT_KEY_SIZE = 64


def msgpack_named_map(entries: "dict[str, Any]") -> bytes:
    """A bag's msgpack bytes: a string-keyed map of strings and small ints."""
    encoded = bytearray([0x80 | len(entries)])
    for key, value in entries.items():
        encoded += _msgpack_scalar(key)
        encoded += _msgpack_scalar(value)
    return bytes(encoded)


def _msgpack_scalar(value: Any) -> bytes:
    if isinstance(value, str):
        text = value.encode("utf-8")
        if len(text) < 32:
            return bytes([0xA0 | len(text)]) + text
        return b"\xd9" + bytes([len(text)]) + text
    if isinstance(value, int) and 0 <= value < 128:
        return bytes([value])
    if isinstance(value, int) and 0 <= value <= 0xFFFFFFFF:
        return b"\xce" + value.to_bytes(4, "big")
    raise AssertionError(f"the fixture encoder carries no arm for {value!r}")


def framed_bag(payload: bytes, *, slice_capacity: int = 0) -> bytes:
    """One bag as the channel carries it: header, payload, then slice slack.

    `slice_capacity` pads the sample out the way iceoryx2 does — a fixed-capacity
    slice whose tail holds whatever an earlier, larger frame left behind.
    """
    port_name = b"cam/frame"
    sample = bytearray(max(slice_capacity, FRAME_HEADER_SIZE + len(payload)))
    sample[0] = len(port_name)
    sample[1 : 1 + len(port_name)] = port_name
    sample[PORT_KEY_SIZE : PORT_KEY_SIZE + 8] = (7_000).to_bytes(8, "little", signed=True)
    sample[PORT_KEY_SIZE + 8 : FRAME_HEADER_SIZE] = len(payload).to_bytes(4, "little")
    sample[FRAME_HEADER_SIZE : FRAME_HEADER_SIZE + len(payload)] = payload
    return bytes(sample)


def bag_publishing_surface_id(
    published_surface_id: str, *, field: str = "surface_id"
) -> bytes:
    """A framed bag whose `field` carries `published_surface_id`."""
    return framed_bag(
        msgpack_named_map({field: published_surface_id, "width": 640}),
        slice_capacity=1024,
    )


def tap_result_body(
    channel: str, framed_bags: "list[bytes]", *, hex_truncated: bool = False
) -> str:
    """One `tap` tool result carrying these bags, shaped as the tool shapes it."""
    return _tool_result_body(
        json.dumps(
            {
                "channel": channel,
                "requested": len(framed_bags),
                "received": len(framed_bags),
                "window_ms": 500,
                "dropped_bags": 0,
                "bags": [
                    {
                        "byte_len": len(bag),
                        "hex_preview": bag.hex(),
                        "hex_truncated": hex_truncated,
                    }
                    for bag in framed_bags
                ],
            }
        )
    )


def png_bytes_for(label: str) -> bytes:
    """Stand-in image bytes, distinguishable per surface so a test can tell
    which frame landed in which file."""
    return b"\x89PNG\r\n\x1a\n" + label.encode("utf-8")


def image_answer(label: str) -> StubSurfaceImageAnswer:
    return StubSurfaceImageAnswer(
        200,
        png_image_bytes=png_bytes_for(label),
        source_surface_pixel_width=1920,
        source_surface_pixel_height=1080,
    )


RECYCLED_FRAME_ANSWER = StubSurfaceImageAnswer(
    410, error_message="surface frame recycled: slot reused since that generation"
)


# ─── The REST spelling of the exchange ───────────────────────────────────────


def test_a_pooled_frame_id_is_percent_encoded_into_the_route(stub_control_plane):
    # A bare `#` would make the generation a URL fragment the node never sees,
    # so the exchange would resolve the wrong frame — or none.
    server = stub_control_plane(
        surface_image_answers={"cam/frame#7": image_answer("seven")}
    )

    exchanged = fetch_surface_image_png_bytes(server.url, "cam/frame#7")

    assert exchanged.png_image_bytes == png_bytes_for("seven")
    assert server.recorded_image_request_paths == [
        "/api/surfaces/cam%2Fframe%237/image"
    ]


def test_the_exchange_states_the_surface_s_own_extent(stub_control_plane):
    server = stub_control_plane(surface_image_answers={"s#1": image_answer("one")})

    exchanged = fetch_surface_image_png_bytes(server.url, "s#1")

    assert exchanged.source_surface_pixel_width == 1920
    assert exchanged.source_surface_pixel_height == 1080


def test_the_exchange_carries_the_bearer_token(stub_control_plane, monkeypatch):
    # It joins the bearer-gated set beside the tap WebSocket, so a gated node
    # must be reachable by this client at all.
    monkeypatch.setenv("STREAMLIB_MCP_TOKEN", "s3cret")
    server = stub_control_plane(surface_image_answers={"s#1": image_answer("one")})

    fetch_surface_image_png_bytes(server.url, "s#1")

    assert server.recorded_image_authorizations == ["Bearer s3cret"]


def test_a_recycled_frame_is_a_refusal_that_composes_as_a_retry(stub_control_plane):
    server = stub_control_plane(surface_image_answers={"s#1": RECYCLED_FRAME_ANSWER})

    with pytest.raises(SurfaceImageExchangeRefusal) as refused:
        fetch_surface_image_png_bytes(server.url, "s#1")

    assert refused.value.names_a_recycled_frame
    assert "recycled" in str(refused.value)


@pytest.mark.parametrize("status", [404, 501])
def test_a_refusal_that_is_not_a_recycled_frame_does_not_compose(
    stub_control_plane, status
):
    # A surface that never existed, or a format with no conversion arm, will
    # refuse identically forever — retrying it would spin rather than recover.
    server = stub_control_plane(
        surface_image_answers={"s#1": StubSurfaceImageAnswer(status, error_message="no")}
    )

    with pytest.raises(SurfaceImageExchangeRefusal) as refused:
        fetch_surface_image_png_bytes(server.url, "s#1")

    assert not refused.value.names_a_recycled_frame
    assert refused.value.http_status == status


def test_an_exchange_against_a_non_http_url_is_refused_before_any_request():
    with pytest.raises(ControlPlaneError, match="http or https"):
        fetch_surface_image_png_bytes("file:///etc/passwd", "s#1")


# ─── `streamlib exchange <SURFACE_ID>` ───────────────────────────────────────


def test_the_id_form_writes_the_exact_bytes_and_prints_the_path(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(
        surface_image_answers={"cam/frame#7": image_answer("seven")}
    )
    output_directory = tmp_path / "frames"

    assert (
        cli.main(
            ["exchange", "cam/frame#7", "--out", str(output_directory), "--url", server.url]
        )
        == 0
    )

    written = capsys.readouterr().out.strip()
    assert Path(written).read_bytes() == png_bytes_for("seven")
    assert Path(written).parent == output_directory


def test_the_id_form_creates_the_output_directory(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(surface_image_answers={"s#1": image_answer("one")})
    output_directory = tmp_path / "nested" / "frames"

    assert (
        cli.main(["exchange", "s#1", "--out", str(output_directory), "--url", server.url])
        == 0
    )

    assert output_directory.is_dir()
    assert len(list(output_directory.glob("*.png"))) == 1


def test_a_surface_id_that_does_not_resolve_fails_the_verb(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(surface_image_answers={})

    assert (
        cli.main(["exchange", "gone#1", "--out", str(tmp_path), "--url", server.url]) == 1
    )

    assert "gone#1" in capsys.readouterr().err


@pytest.mark.parametrize(
    "flag, value",
    [
        ("--count", "3"),
        ("--every", "2"),
        ("--field", "frame_id"),
        # Explicitly asking for the value the channel form would have defaulted
        # to is still asking for the channel form.
        ("--count", "1"),
    ],
)
def test_a_channel_form_flag_beside_a_surface_id_is_refused(
    isolated_registry, tmp_path, capsys, flag, value
):
    # These sample a channel; a surface id already names one frame, so applying
    # them would be silently ignored rather than honoured.
    assert cli.main(["exchange", "s#1", "--out", str(tmp_path), flag, value]) == 1

    assert flag in capsys.readouterr().err


def test_exchange_needs_a_surface_id_or_a_channel(isolated_registry, tmp_path, capsys):
    assert cli.main(["exchange", "--out", str(tmp_path)]) == 1

    assert "--channel" in capsys.readouterr().err


def test_exchange_refuses_a_surface_id_and_a_channel_together(
    isolated_registry, tmp_path, capsys
):
    assert (
        cli.main(["exchange", "s#1", "--channel", "cam/frame", "--out", str(tmp_path)])
        == 1
    )

    assert "not both" in capsys.readouterr().err


def test_the_output_directory_is_required(tmp_path):
    # Without it the verb would write PNGs into whatever directory it was run
    # from, which is never what a harness meant.
    with pytest.raises(SystemExit):
        cli.main(["exchange", "s#1"])


# ─── `streamlib exchange --channel` ──────────────────────────────────────────


def test_the_channel_form_taps_then_exchanges_each_sampled_id(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(
        queued_bodies=[
            tap_result_body(
                "cam/frame",
                [bag_publishing_surface_id("s#1"), bag_publishing_surface_id("s#2")],
            )
        ],
        surface_image_answers={"s#1": image_answer("one"), "s#2": image_answer("two")},
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--count",
                "2",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 0
    )

    tap_arguments = json.loads(server.recorded_bodies[0])["params"]["arguments"]
    assert tap_arguments == {"channel": "cam/frame", "count": 2}

    printed = capsys.readouterr()
    written = [Path(line) for line in printed.out.splitlines()]
    assert [path.read_bytes() for path in written] == [
        png_bytes_for("one"),
        png_bytes_for("two"),
    ]
    assert "exchanged 2 of 2" in printed.err


def test_the_engine_is_never_asked_to_read_a_bag(
    isolated_registry, stub_control_plane, tmp_path
):
    # The composition is the client's whole job: `tap` keeps its shipped
    # contract, gaining no field argument and no decode.
    server = stub_control_plane(
        queued_bodies=[
            tap_result_body("cam/frame", [bag_publishing_surface_id("s#1")])
        ],
        surface_image_answers={"s#1": image_answer("one")},
    )

    cli.main(
        [
            "exchange",
            "--channel",
            "cam/frame",
            "--field",
            "surface_id",
            "--out",
            str(tmp_path),
            "--url",
            server.url,
        ]
    )

    tap_arguments = json.loads(server.recorded_bodies[0])["params"]["arguments"]
    assert set(tap_arguments) == {"channel", "count"}


def test_the_field_override_reads_the_key_the_caller_named(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(
        queued_bodies=[
            tap_result_body(
                "cam/frame",
                [bag_publishing_surface_id("s#9", field="rendered_surface")],
            )
        ],
        surface_image_answers={"s#9": image_answer("nine")},
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--field",
                "rendered_surface",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 0
    )

    assert Path(capsys.readouterr().out.strip()).read_bytes() == png_bytes_for("nine")


def test_a_recycled_frame_is_retried_against_a_newer_bag_and_reported(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    # The loud half of the contract: the run recovers, and says which id it had
    # to give up on, so a sample can never quietly become a different frame.
    server = stub_control_plane(
        body=tap_result_body("cam/frame", []),
        queued_bodies=[
            tap_result_body("cam/frame", [bag_publishing_surface_id("stale#1")]),
            tap_result_body("cam/frame", [bag_publishing_surface_id("fresh#2")]),
        ],
        surface_image_answers={
            "stale#1": RECYCLED_FRAME_ANSWER,
            "fresh#2": image_answer("fresh"),
        },
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 0
    )

    printed = capsys.readouterr()
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("fresh")
    assert "retried 1 recycled frame" in printed.err
    assert "stale#1" in printed.err


def test_a_bag_without_the_named_field_is_counted_rather_than_fatal(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(
        body=tap_result_body("cam/frame", []),
        queued_bodies=[
            tap_result_body(
                "cam/frame",
                [
                    framed_bag(msgpack_named_map({"width": 640}), slice_capacity=1024),
                    bag_publishing_surface_id("s#1"),
                ],
            )
        ],
        surface_image_answers={"s#1": image_answer("one")},
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 0
    )

    printed = capsys.readouterr()
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("one")
    assert "1 bag carried no surface id" in printed.err


def test_every_nth_bag_selects_the_stride(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    labels = ["a", "b", "c", "d", "e", "f"]
    server = stub_control_plane(
        body=tap_result_body("cam/frame", []),
        queued_bodies=[
            tap_result_body(
                "cam/frame",
                [bag_publishing_surface_id(f"s#{label}") for label in labels],
            )
        ],
        surface_image_answers={f"s#{label}": image_answer(label) for label in labels},
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--count",
                "2",
                "--every",
                "3",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 0
    )

    written = [Path(line) for line in capsys.readouterr().out.splitlines()]
    assert [path.read_bytes() for path in written] == [
        png_bytes_for("a"),
        png_bytes_for("d"),
    ]
    # Enough bags to satisfy the stride were asked for, not just the frame count.
    assert json.loads(server.recorded_bodies[0])["params"]["arguments"]["count"] == 6


def test_a_short_sample_exits_nonzero(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    # A harness reading the directory must not take "fewer frames than I asked
    # for" as "this is all the channel had".
    server = stub_control_plane(
        body=tap_result_body("cam/frame", []),
        queued_bodies=[
            tap_result_body("cam/frame", [bag_publishing_surface_id("s#1")])
        ],
        surface_image_answers={"s#1": image_answer("one")},
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--count",
                "3",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 1
    )

    printed = capsys.readouterr()
    assert "exchanged 1 of 3" in printed.err
    # The one frame that did land is still real, and still named.
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("one")


def test_a_refusal_that_cannot_be_retried_stops_the_run(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(
        body=tap_result_body("cam/frame", []),
        queued_bodies=[
            tap_result_body("cam/frame", [bag_publishing_surface_id("s#1")])
        ],
        surface_image_answers={
            "s#1": StubSurfaceImageAnswer(501, error_message="no conversion arm")
        },
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 1
    )

    assert "no conversion arm" in capsys.readouterr().err


def test_a_bag_the_tap_truncated_stops_the_run_by_name(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    # The tap tool hex-previews only a bounded prefix of a large bag. Decoding
    # that prefix would hand back a bag missing its later fields — including,
    # possibly, the surface id.
    whole_bag = framed_bag(
        msgpack_named_map({"surface_id": "s#1", "filler": "x" * 200})
    )
    server = stub_control_plane(
        body=tap_result_body("cam/frame", []),
        queued_bodies=[tap_result_body("cam/frame", [whole_bag[:-32]])],
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 1
    )

    assert "truncated" in capsys.readouterr().err


def test_a_bag_past_the_taps_preview_cap_stops_the_run_and_names_the_size(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    # The tap tool says when it capped a bag. Counting one as "published no
    # surface id" would blame the channel for something this client could not
    # read, and retrying it would never converge.
    server = stub_control_plane(
        body=tap_result_body("cam/frame", []),
        queued_bodies=[
            tap_result_body(
                "cam/frame", [bag_publishing_surface_id("s#1")], hex_truncated=True
            )
        ],
        surface_image_answers={"s#1": image_answer("one")},
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--out",
                str(tmp_path),
                "--url",
                server.url,
            ]
        )
        == 1
    )

    reported = capsys.readouterr().err
    assert "past the prefix `tap` previews" in reported
    # The id form still reaches such a frame, and the message says so.
    assert "streamlib exchange <surface-id>" in reported


@pytest.mark.parametrize("flag, value", [("--count", "0"), ("--every", "0")])
def test_a_sample_bound_below_one_is_refused(
    isolated_registry, tmp_path, capsys, flag, value
):
    assert (
        cli.main(
            ["exchange", "--channel", "cam/frame", "--out", str(tmp_path), flag, value]
        )
        == 1
    )

    assert flag in capsys.readouterr().err

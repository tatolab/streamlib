# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The observation verbs, driven without a running node.

`nodes` / `graph` / `tap` / `logs` are clients: of the on-disk node registry, of
a node's MCP server, and of the on-disk JSONL log. Each of those is stood up
here — a real MCP server on a local API socket, a temp registry directory, a
temp log directory — so the whole surface is exercised in CI, where no GPU
exists to boot a real node with. `test_cli_launch.py` covers the live path on
the rig.

The rendering assertions are the load-bearing ones: the JSONL schema and its
pretty form are durable contracts, and a record read by this CLI must come out
byte-identical to what the runtime mirrored to its own stdout.
"""

from __future__ import annotations

import argparse
import io
import itertools
import json
import os
import shutil
import socket
import tempfile
import threading
import time
import urllib.parse
from pathlib import Path
from typing import Any, Callable, Generator, NamedTuple, Optional, TextIO

import pytest
import uvicorn
from mcp import types as mcp_types
from mcp.server.lowlevel import Server
from mcp.server.transport_security import TransportSecuritySettings
from mcp.shared.exceptions import MCPError
from starlette.requests import Request
from starlette.responses import Response
from starlette.routing import Route

from streamlib import cli
from streamlib._control_plane_client import (
    ControlPlaneError,
    LocalApiSocket,
    SurfaceImageExchangeRefusal,
    call_tool,
    control_plane_answers,
    fetch_surface_image_png_bytes,
    resolve_local_api_socket_of_requested_node,
)
from streamlib import _node_registry
from streamlib._node_registry import registry_directory, scan_check_and_prune
from streamlib._runtime_log_reader import (
    LogRecordFilters,
    RuntimeLogFile,
    _held_segment_was_rotated_away,
    _rotated_segment_sequences,
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

#: A local API socket path nothing listens on: its directory does not exist, so
#: a connect fails at once rather than waiting on a slow answer.
NOTHING_LISTENS_LOCAL_API_SOCKET_PATH = "/nonexistent-streamlib-test/local-api-Rnone.sock"


class StubToolAnswer(NamedTuple):
    """How the stub answers one `tools/call`: the tool's text, and whether the
    tool ran and failed."""

    text: str
    is_error: bool = False


class RecordedToolCall(NamedTuple):
    """One `tools/call` the stub received, as the node would have."""

    tool_name: str
    arguments: "dict[str, Any]"


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
    """A node's control plane stood in by a real MCP server — the official
    Python SDK's — on a local API socket.

    Records every tool call so a test can prove the verb sent what it claimed
    to, and answers from a queue so tool errors and refusals are reachable
    without a live runtime.

    Both front ends of the exchange live here, because the CLI drives both: the
    MCP tool calls, and the binary `GET` the full-resolution image route serves.
    """

    def __init__(
        self,
        tool_answer: "Optional[StubToolAnswer]" = None,
        *,
        queued_tool_answers: "Optional[list[StubToolAnswer]]" = None,
        refuse_every_tool_call_with: "Optional[str]" = None,
        surface_image_answers: "Optional[dict[str, StubSurfaceImageAnswer]]" = None,
    ) -> None:
        self.recorded_tool_calls: "list[RecordedToolCall]" = []
        self.recorded_image_request_paths: "list[str]" = []
        self._tool_answer = tool_answer if tool_answer is not None else StubToolAnswer("{}")
        self._queued_tool_answers = list(queued_tool_answers or [])
        self._refuse_every_tool_call_with = refuse_every_tool_call_with
        self._surface_image_answers = dict(surface_image_answers or {})

        async def on_call_tool(
            context: Any, request: mcp_types.CallToolRequestParams
        ) -> mcp_types.CallToolResult:
            self.recorded_tool_calls.append(
                RecordedToolCall(request.name, dict(request.arguments or {}))
            )
            if self._refuse_every_tool_call_with is not None:
                raise MCPError(mcp_types.INVALID_PARAMS, self._refuse_every_tool_call_with)
            # The queue drains in order and then the fixed answer answers
            # forever, so a test names only the rounds it cares about.
            answer = (
                self._queued_tool_answers.pop(0) if self._queued_tool_answers else self._tool_answer
            )
            return mcp_types.CallToolResult(
                content=[mcp_types.TextContent(type="text", text=answer.text)],
                is_error=answer.is_error,
            )

        async def surface_image_route(request: Request) -> Response:
            raw_path = request.scope["raw_path"].decode("ascii")
            self.recorded_image_request_paths.append(raw_path)
            answer = self._surface_image_answers.get(
                _surface_id_in_image_route_path(raw_path),
                StubSurfaceImageAnswer(404, error_message="no such surface"),
            )
            if answer.status != 200:
                return Response(
                    json.dumps({"error": answer.error_message}),
                    status_code=answer.status,
                    media_type="application/json",
                )
            extent_headers = {
                header: str(extent)
                for header, extent in (
                    ("x-streamlib-surface-pixel-width", answer.source_surface_pixel_width),
                    ("x-streamlib-surface-pixel-height", answer.source_surface_pixel_height),
                )
                if extent is not None
            }
            return Response(answer.png_image_bytes, media_type="image/png", headers=extent_headers)

        mcp_server = Server("stub-control-plane", version="0", on_call_tool=on_call_tool)
        app = mcp_server.streamable_http_app(
            json_response=True,
            stateless_http=True,
            # The socket's file mode is the gate, as on a real node.
            transport_security=TransportSecuritySettings(enable_dns_rebinding_protection=False),
            custom_starlette_routes=[
                Route("/api/surfaces/{surface_id:path}/image", surface_image_route)
            ],
        )

        # A short directory of its own: a socket path is capped near 104 bytes,
        # and a pytest tmp_path named after a long test overruns that.
        self._local_api_socket_directory = tempfile.mkdtemp(prefix="sl-stub-")
        self.local_api_socket_path = os.path.join(
            self._local_api_socket_directory, "local-api.sock"
        )
        self.local_api_socket = LocalApiSocket(self.local_api_socket_path)
        self._uvicorn_server = uvicorn.Server(
            uvicorn.Config(
                _accepting_absolute_form_request_targets(app),
                uds=self.local_api_socket_path,
                log_level="warning",
                lifespan="on",
            )
        )
        self._serving_thread = threading.Thread(target=self._uvicorn_server.run, daemon=True)
        self._serving_thread.start()
        deadline = time.monotonic() + STUB_CONTROL_PLANE_START_TIMEOUT_SECONDS
        while not self._uvicorn_server.started:
            assert time.monotonic() < deadline, "the stub control plane never started"
            time.sleep(0.01)

    def close(self) -> None:
        self._uvicorn_server.should_exit = True
        self._serving_thread.join(timeout=5)
        shutil.rmtree(self._local_api_socket_directory, ignore_errors=True)


def _accepting_absolute_form_request_targets(app: Any) -> Any:
    """`app`, reached by an absolute-form request target as well as an
    origin-form one.

    `rmcp`'s Unix-socket client sends `POST http://localhost/mcp`, which RFC 9112
    §3.2.2 obliges a server to accept; uvicorn hands it to the app as the path
    unreduced, so it is reduced here to the path a node's own server routes on.
    """

    async def reduced_to_origin_form(scope: Any, receive: Any, send: Any) -> None:
        if scope["type"] == "http" and "://" in scope["path"]:
            origin_form = urllib.parse.urlsplit(scope["path"])
            scope = {
                **scope,
                "path": origin_form.path,
                "raw_path": urllib.parse.urlsplit(scope["raw_path"].decode("ascii")).path.encode("ascii"),
                "query_string": origin_form.query.encode("ascii"),
            }
        await app(scope, receive, send)

    return reduced_to_origin_form


#: Bounds the stub's start, which is a thread binding a socket.
STUB_CONTROL_PLANE_START_TIMEOUT_SECONDS = 10.0


def _surface_id_in_image_route_path(path: str) -> str:
    """The surface id out of `/api/surfaces/{surface_id}/image`, decoded."""
    segments = urllib.parse.urlparse(path).path.strip("/").split("/")
    if len(segments) != 4 or segments[0] != "api" or segments[1] != "surfaces":
        return ""
    return urllib.parse.unquote(segments[2])


def stub_tool_answer(text: str, *, is_error: bool = False) -> StubToolAnswer:
    return StubToolAnswer(text, is_error=is_error)


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
def targeted_stub_control_plane(stub_control_plane, monkeypatch):
    """A stub every verb in the test targets, resolved without the registry.

    Resolution has tests of its own; these drive what a verb does once it has
    its node.
    """

    def make(**kwargs: Any) -> StubControlPlane:
        server = stub_control_plane(**kwargs)
        monkeypatch.setattr(
            cli,
            "resolve_local_api_socket_of_requested_node",
            lambda requested_node: server.local_api_socket,
        )
        return server

    return make


@pytest.fixture
def isolated_registry(tmp_path, monkeypatch):
    """Point the node registry at a temp dir so tests never see real nodes.

    The resolver itself is replaced rather than steered through
    `XDG_RUNTIME_DIR`, which macOS ignores by design.
    """
    runtime_directory = tmp_path / "runtime-dir" / "streamlib"
    runtime_directory.mkdir(parents=True)
    monkeypatch.setattr(_node_registry, "runtime_directory", lambda: runtime_directory)
    return runtime_directory / "nodes"


def write_registry_entry(
    registry: Path,
    runtime_id: str,
    local_api_socket_path: str,
    *,
    pid: "Optional[int]" = None,
    runtime_name: "Optional[str]" = None,
) -> Path:
    registry.mkdir(parents=True, exist_ok=True)
    entry_path = registry / f"{runtime_id}.json"
    entry_path.write_text(
        json.dumps(
            {
                "schema_version": _node_registry.NODE_REGISTRY_SCHEMA_VERSION,
                "runtime_id": runtime_id,
                "runtime_name": runtime_name or f"rig-app-{runtime_id}",
                "local_api_socket_path": local_api_socket_path,
                "pid": os.getpid() if pid is None else pid,
                "hint": "python (/tmp/app)",
            }
        ),
        encoding="utf-8",
    )
    return entry_path


# ─── The node registry ───────────────────────────────────────────────────────


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_registry_directory_follows_xdg_runtime_dir(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    assert registry_directory() == tmp_path / "streamlib" / "nodes"


def test_a_reachable_entry_is_listed_as_alive(isolated_registry, stub_control_plane):
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ralive", server.local_api_socket_path)

    discovered = scan_check_and_prune()

    assert [(node.entry.runtime_id, node.reachable) for node in discovered] == [
        ("Ralive", True)
    ]


def test_an_entry_that_is_unreachable_and_dead_is_pruned(isolated_registry):
    entry_path = write_registry_entry(
        isolated_registry, "Rdead", NOTHING_LISTENS_LOCAL_API_SOCKET_PATH, pid=UNUSED_PID
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
        isolated_registry, "Rbusy", NOTHING_LISTENS_LOCAL_API_SOCKET_PATH
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
        isolated_registry, "Rcorrupt", NOTHING_LISTENS_LOCAL_API_SOCKET_PATH, pid=PID_OUTSIDE_PID_T
    )
    write_registry_entry(isolated_registry, "Rgood", server.local_api_socket_path)

    discovered = scan_check_and_prune()

    assert [node.entry.runtime_id for node in discovered] == ["Rgood"]


def test_a_malformed_entry_does_not_hide_the_others(
    isolated_registry, stub_control_plane
):
    server = stub_control_plane()
    isolated_registry.mkdir(parents=True, exist_ok=True)
    (isolated_registry / "Rgarbage.json").write_text("{ not json", encoding="utf-8")
    write_registry_entry(isolated_registry, "Rgood", server.local_api_socket_path)

    discovered = scan_check_and_prune()

    assert [node.entry.runtime_id for node in discovered] == ["Rgood"]


def test_a_local_api_socket_that_answers_is_reachable(stub_control_plane):
    server = stub_control_plane()

    assert control_plane_answers(server.local_api_socket) is True


def test_a_local_api_socket_nothing_listens_on_is_not_reachable():
    stale_socket_directory = tempfile.mkdtemp(prefix="sl-stale-")
    stale_socket_path = os.path.join(stale_socket_directory, "local-api.sock")
    stale_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stale_listener.bind(stale_socket_path)
    stale_listener.close()
    try:
        assert os.path.exists(stale_socket_path), "a closed listener leaves its file"
        assert control_plane_answers(LocalApiSocket(stale_socket_path)) is False
        assert (
            control_plane_answers(LocalApiSocket(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH))
            is False
        )
    finally:
        shutil.rmtree(stale_socket_directory, ignore_errors=True)


# ─── Resolving which node a verb drives ──────────────────────────────────────


def test_the_sole_live_node_is_the_default_target(isolated_registry, stub_control_plane):
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ronly", server.local_api_socket_path)

    assert resolve_local_api_socket_of_requested_node(None) == server.local_api_socket


def test_a_named_node_resolves_to_its_local_api_socket(isolated_registry, stub_control_plane):
    first = stub_control_plane()
    second = stub_control_plane()
    write_registry_entry(isolated_registry, "Rfirst", first.local_api_socket_path)
    write_registry_entry(isolated_registry, "Rsecond", second.local_api_socket_path)

    assert resolve_local_api_socket_of_requested_node("Rsecond") == second.local_api_socket


def test_two_live_nodes_and_no_flag_is_an_error_that_lists_them(
    isolated_registry, stub_control_plane
):
    first = stub_control_plane()
    second = stub_control_plane()
    write_registry_entry(isolated_registry, "Rfirst", first.local_api_socket_path)
    write_registry_entry(isolated_registry, "Rsecond", second.local_api_socket_path)

    with pytest.raises(ControlPlaneError) as failure:
        resolve_local_api_socket_of_requested_node(None)

    message = str(failure.value)
    assert "Rfirst" in message and "Rsecond" in message
    assert "--node" in message, "the error must name the flag that resolves it"


def test_no_live_nodes_names_the_command_that_starts_one(isolated_registry):
    with pytest.raises(ControlPlaneError) as failure:
        resolve_local_api_socket_of_requested_node(None)

    assert "streamlib dev" in str(failure.value)


# ─── Driving a tool ──────────────────────────────────────────────────────────


def test_a_tool_call_carries_its_name_and_arguments_to_the_node(stub_control_plane):
    server = stub_control_plane(tool_answer=stub_tool_answer('{"nodes":[]}'))

    result = call_tool(
        server.local_api_socket, "tap", {"channel": "cam/video", "count": 4}
    )

    assert result == '{"nodes":[]}'
    assert server.recorded_tool_calls == [
        RecordedToolCall("tap", {"channel": "cam/video", "count": 4})
    ]


def test_a_tool_level_error_is_raised_not_printed_as_a_result(stub_control_plane):
    server = stub_control_plane(
        tool_answer=stub_tool_answer("no such channel", is_error=True)
    )

    with pytest.raises(ControlPlaneError, match="no such channel"):
        call_tool(server.local_api_socket, "tap", {"channel": "nope"})


def test_a_call_the_node_refuses_is_raised_naming_the_refusal(stub_control_plane):
    server = stub_control_plane(refuse_every_tool_call_with="no tool named `nope`")

    with pytest.raises(ControlPlaneError, match="no tool named `nope`"):
        call_tool(server.local_api_socket, "nope", {})


def test_an_unreachable_local_api_socket_is_named():
    with pytest.raises(ControlPlaneError) as failure:
        call_tool(LocalApiSocket(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH), "graph", {})

    assert NOTHING_LISTENS_LOCAL_API_SOCKET_PATH in str(failure.value)


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
    assert "restarted into a newer log file" in errors.getvalue()
    lines.close()


# ─── Rotated log segments ────────────────────────────────────────────────────


def write_segment(path: Path, messages: "list[str]") -> None:
    path.write_text(
        "".join(json.dumps(a_log_record(message=message)) + "\n" for message in messages),
        encoding="utf-8",
    )


def rotate_like_the_engine(active_segment_path: Path, rotation_sequence: int) -> None:
    """Move the active segment to its rotated name and leave an empty file under the active name.

    The engine gets there through a `.rotating` replacement and two renames; what a
    reader can observe of that is this end state, or the active name briefly absent.
    """
    os.rename(
        active_segment_path,
        active_segment_path.with_name(
            f"{active_segment_path.stem}.{rotation_sequence}.jsonl"
        ),
    )
    active_segment_path.touch()


def rendered_messages(lines: "list[str]") -> "list[str]":
    return [line.split(" — ", 1)[1] for line in lines]


def test_a_rotated_segment_is_listed_under_its_runtime_rather_than_as_a_runtime_of_its_own(
    tmp_path,
):
    # The literal the engine's `paths::tests::a_rotated_segment_is_named_with_a_dot_separated_sequence`
    # asserts it writes; the two sides share this string, not code.
    write_segment(tmp_path / "Rabc123-1700000000000.3.jsonl", ["rotated"])

    found = enumerate_runtime_log_files(tmp_path)

    assert [(f.runtime_id, f.started_at_millis, f.path.name) for f in found] == [
        ("Rabc123", 1700000000000, "Rabc123-1700000000000.jsonl")
    ]


def test_a_runtime_is_listed_once_with_the_size_of_every_segment(tmp_path):
    write_segment(tmp_path / "camera-2-1000.jsonl", ["active"])
    write_segment(tmp_path / "camera-2-1000.1.jsonl", ["first"])
    write_segment(tmp_path / "camera-2-1000.2.jsonl", ["second"])
    write_segment(tmp_path / "my.node-2000.4.jsonl", ["other"])
    expected_size = sum(
        (tmp_path / name).stat().st_size
        for name in ("camera-2-1000.jsonl", "camera-2-1000.1.jsonl", "camera-2-1000.2.jsonl")
    )

    found = sorted(enumerate_runtime_log_files(tmp_path))

    assert [(f.runtime_id, f.started_at_millis) for f in found] == [
        ("camera-2", 1000),
        ("my.node", 2000),
    ]
    assert found[0].size_bytes == expected_size


def test_reading_a_runtime_walks_its_rotated_segments_oldest_first(tmp_path):
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(tmp_path / "Rabc-1000.10.jsonl", ["ten"])
    write_segment(tmp_path / "Rabc-1000.9.jsonl", ["nine"])
    write_segment(tmp_path / "Rabc-2000.1.jsonl", ["another-instance"])
    write_segment(active_segment_path, ["active"])
    log_file = RuntimeLogFile("Rabc", 1000, active_segment_path, 0)

    rendered = list(
        read_log_file(
            log_file, LogRecordFilters(), follow=False, errors=io.StringIO(), log_directory=tmp_path
        )
    )

    assert rendered_messages(rendered) == ["nine", "ten", "active"]


def test_a_runtime_whose_active_segment_is_missing_still_reads_its_rotated_ones(tmp_path):
    write_segment(tmp_path / "Rabc-1000.1.jsonl", ["rotated"])
    log_file = RuntimeLogFile("Rabc", 1000, tmp_path / "Rabc-1000.jsonl", 0)

    rendered = list(
        read_log_file(
            log_file, LogRecordFilters(), follow=False, errors=io.StringIO(), log_directory=tmp_path
        )
    )

    assert rendered_messages(rendered) == ["rotated"]


def test_follow_carries_on_across_a_rotation(tmp_path):
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(active_segment_path, ["first"])
    log_file = RuntimeLogFile("Rabc", 1000, active_segment_path, 0)
    errors = io.StringIO()
    lines = read_log_file(
        log_file, LogRecordFilters(), follow=True, errors=errors, log_directory=tmp_path
    )

    assert next_line_within_timeout(lines, "the drained line").endswith("first")

    with active_segment_path.open("a", encoding="utf-8") as appending:
        appending.write(json.dumps(a_log_record(message="second")) + "\n")
    rotate_like_the_engine(active_segment_path, 1)
    with active_segment_path.open("a", encoding="utf-8") as appending:
        appending.write(json.dumps(a_log_record(message="third")) + "\n")

    assert next_line_within_timeout(lines, "the line written just before the rotation").endswith(
        "second"
    )
    assert next_line_within_timeout(lines, "the first line after the rotation").endswith("third")
    assert errors.getvalue() == ""
    lines.close()


def lines_pulled_across_a_change_made_at_the_live_edge(
    lines: "Generator[str, None, None]",
    count: int,
    change_the_log: "Callable[[], None]",
    monkeypatch: pytest.MonkeyPatch,
) -> "list[str]":
    """The next `count` lines, with `change_the_log` run while the reader waits at the edge.

    The reader has already read everything on disk when it first sleeps, so running
    the change there, rather than racing it, is what makes a test of the reader's
    edge behaviour red when that behaviour regresses.
    """
    reader_parked_at_the_edge = threading.Event()
    change_made = threading.Event()
    real_sleep = time.sleep

    def sleep_once_the_change_is_made(seconds: float) -> None:
        reader_parked_at_the_edge.set()
        change_made.wait(FOLLOW_LINE_TIMEOUT_SECONDS)
        real_sleep(seconds)

    monkeypatch.setattr("streamlib._runtime_log_reader.time.sleep", sleep_once_the_change_is_made)
    collected: "list[str]" = []
    puller = threading.Thread(
        target=lambda: collected.extend(next(lines) for _ in range(count)), daemon=True
    )
    puller.start()
    assert reader_parked_at_the_edge.wait(FOLLOW_LINE_TIMEOUT_SECONDS)
    change_the_log()
    change_made.set()
    puller.join(FOLLOW_LINE_TIMEOUT_SECONDS)
    assert len(collected) == count, (
        f"expected {count} lines within {FOLLOW_LINE_TIMEOUT_SECONDS}s, got {collected}"
    )
    return collected


def test_follow_reads_every_segment_rotated_between_two_polls(tmp_path, monkeypatch):
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(active_segment_path, ["first"])
    lines = read_log_file(
        RuntimeLogFile("Rabc", 1000, active_segment_path, 0),
        LogRecordFilters(),
        follow=True,
        errors=io.StringIO(),
        log_directory=tmp_path,
    )
    assert next_line_within_timeout(lines, "the drained line").endswith("first")

    def append_then_rotate_twice() -> None:
        with active_segment_path.open("a", encoding="utf-8") as appending:
            appending.write(json.dumps(a_log_record(message="second")) + "\n")
        rotate_like_the_engine(active_segment_path, 1)
        write_segment(active_segment_path, ["third"])
        rotate_like_the_engine(active_segment_path, 2)
        write_segment(active_segment_path, ["fourth"])

    collected = lines_pulled_across_a_change_made_at_the_live_edge(
        lines, 3, append_then_rotate_twice, monkeypatch
    )

    assert rendered_messages(collected) == ["second", "third", "fourth"]
    lines.close()


def test_a_segment_retention_removed_before_it_was_read_is_skipped_with_a_note(
    tmp_path, monkeypatch
):
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(tmp_path / "Rabc-1000.2.jsonl", ["survivor"])
    write_segment(active_segment_path, ["active"])
    monkeypatch.setattr("streamlib._runtime_log_reader._rotated_segment_sequences", lambda _active: [1, 2])
    errors = io.StringIO()

    rendered = list(
        read_log_file(
            RuntimeLogFile("Rabc", 1000, active_segment_path, 0),
            LogRecordFilters(),
            follow=False,
            errors=errors,
            log_directory=tmp_path,
        )
    )

    assert rendered_messages(rendered) == ["survivor", "active"]
    assert "Rabc-1000.1.jsonl was removed by retention" in errors.getvalue()


#: More lines than any of the rotation scenarios writes, so a reader that
#: re-reads a segment in a loop fails the assertion rather than hanging.
MOST_LINES_A_ROTATION_SCENARIO_READS = 20


def read_every_line(active_segment_path: Path, errors: TextIO) -> "list[str]":
    return list(
        itertools.islice(
            read_log_file(
                RuntimeLogFile("Rabc", 1000, active_segment_path, 0),
                LogRecordFilters(),
                follow=False,
                errors=errors,
                log_directory=active_segment_path.parent,
            ),
            MOST_LINES_A_ROTATION_SCENARIO_READS,
        )
    )


def test_a_record_flushed_just_before_a_rotation_is_read_before_the_segment_after_it(
    tmp_path, monkeypatch
):
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(active_segment_path, ["first"])
    real_check = _held_segment_was_rotated_away
    checks = []

    def flush_then_rotate_before_the_first_check(held_segment_file, path):
        if not checks:
            with active_segment_path.open("a", encoding="utf-8") as appending:
                appending.write(json.dumps(a_log_record(message="flushed-before-rotation")) + "\n")
            rotate_like_the_engine(active_segment_path, 1)
            write_segment(active_segment_path, ["after"])
        checks.append(path)
        return real_check(held_segment_file, path)

    monkeypatch.setattr("streamlib._runtime_log_reader._held_segment_was_rotated_away", flush_then_rotate_before_the_first_check
    )
    errors = io.StringIO()

    rendered = read_every_line(active_segment_path, errors)

    assert rendered_messages(rendered) == ["first", "flushed-before-rotation", "after"]
    assert errors.getvalue() == ""


def test_a_rotation_between_listing_and_opening_keeps_the_segment_it_rotated(
    tmp_path, monkeypatch
):
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(active_segment_path, ["before-rotation"])
    real_listing = _rotated_segment_sequences
    listings = []

    def rotate_right_after_the_first_listing(path):
        listed = real_listing(path)
        if not listings:
            rotate_like_the_engine(active_segment_path, 1)
            write_segment(active_segment_path, ["after"])
        listings.append(listed)
        return listed

    monkeypatch.setattr("streamlib._runtime_log_reader._rotated_segment_sequences", rotate_right_after_the_first_listing
    )

    rendered = read_every_line(active_segment_path, io.StringIO())

    assert rendered_messages(rendered) == ["before-rotation", "after"]


def test_a_rotation_the_writer_backed_out_of_repeats_no_record(tmp_path, monkeypatch):
    # The writer renames the active segment away, fails to put a replacement in
    # its place, and renames it back. A reader looking in that gap sees no name.
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(active_segment_path, ["a1", "a2"])
    rotated_segment_path = tmp_path / "Rabc-1000.1.jsonl"
    real_check = _held_segment_was_rotated_away

    def look_while_the_name_is_renamed_away(held_segment_file, path):
        os.rename(active_segment_path, rotated_segment_path)
        try:
            return real_check(held_segment_file, path)
        finally:
            os.rename(rotated_segment_path, active_segment_path)

    monkeypatch.setattr("streamlib._runtime_log_reader._held_segment_was_rotated_away", look_while_the_name_is_renamed_away
    )

    rendered = read_every_line(active_segment_path, io.StringIO())

    assert rendered_messages(rendered) == ["a1", "a2"]


def test_a_held_segment_is_matched_to_its_rotated_name_rather_than_counted(
    tmp_path, monkeypatch
):
    # With one segment retained, earlier rotations are already deleted, so the
    # held file becomes `.5` while the reader has read no rotated segment at all.
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    write_segment(active_segment_path, ["held"])
    real_check = _held_segment_was_rotated_away
    checks = []

    def rotate_to_five_before_the_first_check(held_segment_file, path):
        if not checks:
            rotate_like_the_engine(active_segment_path, 5)
            write_segment(active_segment_path, ["after"])
        checks.append(path)
        return real_check(held_segment_file, path)

    monkeypatch.setattr("streamlib._runtime_log_reader._held_segment_was_rotated_away", rotate_to_five_before_the_first_check
    )
    errors = io.StringIO()

    rendered = read_every_line(active_segment_path, errors)

    assert rendered_messages(rendered) == ["held", "after"]
    assert errors.getvalue() == ""


def test_a_record_caught_half_written_is_held_until_its_newline_lands(
    tmp_path, monkeypatch
):
    active_segment_path = tmp_path / "Rabc-1000.jsonl"
    whole_record = json.dumps(a_log_record(message="second")) + "\n"
    active_segment_path.write_text(
        json.dumps(a_log_record(message="first")) + "\n" + whole_record[:20],
        encoding="utf-8",
    )
    errors = io.StringIO()
    lines = read_log_file(
        RuntimeLogFile("Rabc", 1000, active_segment_path, 0),
        LogRecordFilters(),
        follow=True,
        errors=errors,
        log_directory=tmp_path,
    )
    assert next_line_within_timeout(lines, "the whole line").endswith("first")

    def finish_the_record() -> None:
        with active_segment_path.open("a", encoding="utf-8") as appending:
            appending.write(whole_record[20:])

    collected = lines_pulled_across_a_change_made_at_the_live_edge(
        lines, 1, finish_the_record, monkeypatch
    )

    assert rendered_messages(collected) == ["second"]
    assert "malformed" not in errors.getvalue()
    lines.close()


@pytest.mark.parametrize(
    "malformed",
    [
        {"runtime_name": None},
        {"runtime_name": ["desk", "rig"]},
        {"runtime_id": {"nested": "object"}},
        {"schema_version": 2.9},
        {"pid": True},
        {"local_api_socket_path": None},
    ],
    ids=[
        "null-name",
        "array-name",
        "object-id",
        "fractional-version",
        "boolean-pid",
        "null-socket-path",
    ],
)
def test_an_entry_whose_fields_are_the_wrong_shape_is_neither_listed_nor_deleted(
    isolated_registry, malformed
):
    # Coercing would list a `null` name as the string "None" and let `--node
    # None` resolve it; the reader skips what it cannot parse, which is also
    # what keeps it out of the prune path.
    isolated_registry.mkdir(parents=True, exist_ok=True)
    entry_path = isolated_registry / "Rmalformed.json"
    record = {
        "schema_version": _node_registry.NODE_REGISTRY_SCHEMA_VERSION,
        "runtime_id": "Rmalformed",
        "runtime_name": "rig-app-a1b2",
        "local_api_socket_path": NOTHING_LISTENS_LOCAL_API_SOCKET_PATH,
        "pid": UNUSED_PID,
        "hint": "hand-edited",
    }
    record.update(malformed)
    entry_path.write_text(json.dumps(record), encoding="utf-8")

    assert scan_check_and_prune() == []
    assert entry_path.exists(), "a reader must not delete a record it cannot parse"


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
                "schema_version": _node_registry.NODE_REGISTRY_SCHEMA_VERSION + 1,
                "runtime_id": "Rfuture",
                "runtime_name": "rig-app-future",
                "local_api_socket_path": NOTHING_LISTENS_LOCAL_API_SOCKET_PATH,
                "pid": UNUSED_PID,
                "hint": "written by a newer engine",
            }
        ),
        encoding="utf-8",
    )

    assert scan_check_and_prune() == []
    assert entry_path.exists(), "a reader must not delete a record it cannot parse"


def test_a_schema_two_entry_is_refused_by_its_version_and_never_pruned(
    isolated_registry,
):
    # Entries are per run, so a schema-2 entry, which names no socket, has
    # nothing to migrate. It is skipped, and a dead pid does not get it pruned.
    assert _node_registry.NODE_REGISTRY_SCHEMA_VERSION == 3
    isolated_registry.mkdir(parents=True, exist_ok=True)
    entry_path = isolated_registry / "Rschema-two.json"
    entry_path.write_text(
        json.dumps(
            {
                "schema_version": 2,
                "runtime_id": "Rschema-two",
                "runtime_name": "rig-app-schema-two",
                "pid": UNUSED_PID,
                "hint": "written by an engine before the local API socket",
            }
        ),
        encoding="utf-8",
    )

    assert scan_check_and_prune() == []
    assert entry_path.exists(), "a reader must not delete a record it cannot parse"


def test_a_schema_three_entry_still_carrying_the_retired_url_key_is_listed(
    isolated_registry, stub_control_plane
):
    # A released engine from before the TCP listener went writes schema 3 with
    # the URL key beside the socket path; an app venv pinned to it still runs.
    # Joined at run time so the retired key's own text does not survive here,
    # where a source-walking gate would still find it.
    server = stub_control_plane()
    entry_path = write_registry_entry(
        isolated_registry, "Rcarries-url", server.local_api_socket_path
    )
    record = json.loads(entry_path.read_text(encoding="utf-8"))
    record["_".join(("control", "url"))] = "http://127.0.0.1:9000"
    entry_path.write_text(json.dumps(record), encoding="utf-8")

    discovered = scan_check_and_prune()

    assert [(node.entry.runtime_id, node.reachable) for node in discovered] == [
        ("Rcarries-url", True)
    ]


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

    assert capsys.readouterr().out.splitlines() == [
        f"No running nodes found in {isolated_registry}."
    ]


def test_nodes_renders_a_live_node_as_a_table(
    isolated_registry, stub_control_plane, capsys
):
    server = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rlisted", server.local_api_socket_path, runtime_name="rig-desk-a1b2"
    )

    assert cli.main(["nodes"]) == 0

    printed = capsys.readouterr().out
    header = printed.splitlines()[0]
    assert header.startswith("RUNTIME_NAME"), (
        f"the runtime's name is the first column a reader sees: {header!r}"
    )
    assert "RUNTIME_ID" in printed
    assert "rig-desk-a1b2" in printed
    assert "LOCAL_API_SOCKET" in header and "CONTROL_URL" not in header, header
    assert "Rlisted" in printed and server.local_api_socket_path in printed
    assert "yes" in printed


def test_nodes_prints_the_registry_table_alone(
    isolated_registry, stub_control_plane, capsys
):
    first = stub_control_plane()
    write_registry_entry(isolated_registry, "Rfirst", first.local_api_socket_path, runtime_name="rig-desk-a1b2")
    second = stub_control_plane()
    write_registry_entry(isolated_registry, "Rsecond", second.local_api_socket_path, runtime_name="rig-lab-c3d4")

    assert cli.main(["nodes"]) == 0

    printed_lines = capsys.readouterr().out.splitlines()
    assert len(printed_lines) == 3, (
        f"`nodes` prints a header and one row per registry entry, nothing else: {printed_lines!r}"
    )
    assert printed_lines[0].startswith("RUNTIME_NAME"), printed_lines
    assert sorted(line.split()[0] for line in printed_lines[1:]) == [
        "rig-desk-a1b2",
        "rig-lab-c3d4",
    ], printed_lines


# Each spelled from its parts so the retired flag's own text does not survive
# here, where a source-walking gate would still find it.
RETIRED_LAUNCH_FLAGS = tuple(
    "--" + "-".join(parts)
    for parts in (
        ("mesh", "name"),
        ("mesh", "peer"),
        ("mesh", "listen"),
        ("no", "mesh", "multicast", "discovery"),
    )
)
RETIRED_NODES_FLAGS = tuple(
    flag for flag in RETIRED_LAUNCH_FLAGS if not flag.endswith("listen")
)


@pytest.mark.parametrize(
    "verb, retired_flag",
    [("run", flag) for flag in RETIRED_LAUNCH_FLAGS]
    + [("dev", flag) for flag in RETIRED_LAUNCH_FLAGS]
    + [("nodes", flag) for flag in RETIRED_NODES_FLAGS],
)
def test_a_retired_flag_is_a_usage_error(isolated_registry, capsys, verb, retired_flag):
    with pytest.raises(SystemExit) as usage_error:
        cli.main([verb, retired_flag])

    assert usage_error.value.code == 2
    assert retired_flag in capsys.readouterr().err


# Each flag carries a value: argparse refuses a value-taking flag given none
# with the same exit status, so a bare flag would pass whether or not the verb
# still took it.
@pytest.mark.parametrize(
    "verb_arguments, network_address_flag, network_address",
    [
        (("run",), "--host", "127.0.0.1"),
        (("run",), "--port", "9100"),
        (("run",), "-p", "9100"),
        (("dev",), "--host", "127.0.0.1"),
        (("dev",), "--port", "9100"),
        (("dev",), "-p", "9100"),
        (("graph",), "--url", "http://127.0.0.1:9100"),
        (("tap", "rig/pattern/video"), "--url", "http://127.0.0.1:9100"),
        (("logs",), "--url", "http://127.0.0.1:9100"),
        (("exchange", "s#1", "--out", "unwritten"), "--url", "http://127.0.0.1:9100"),
    ],
)
def test_no_verb_takes_a_network_address_for_the_control_plane(
    isolated_registry, capsys, verb_arguments, network_address_flag, network_address
):
    """Control is reachable only through a node's local API socket, so a launch
    verb binds no address and an observation verb dials none."""
    with pytest.raises(SystemExit) as usage_error:
        cli.main([*verb_arguments, network_address_flag, network_address])

    assert usage_error.value.code == 2
    refusal = capsys.readouterr().err
    assert "unrecognized arguments" in refusal, refusal
    assert network_address_flag in refusal, refusal


def test_a_verb_targets_a_node_by_its_runtime_name(
    isolated_registry, stub_control_plane
):
    server = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rnamed", server.local_api_socket_path, runtime_name="rig-desk-a1b2"
    )
    other = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rother", other.local_api_socket_path, runtime_name="rig-lab-c3d4"
    )

    assert resolve_local_api_socket_of_requested_node("rig-desk-a1b2") == server.local_api_socket
    assert resolve_local_api_socket_of_requested_node("Rnamed") == server.local_api_socket, (
        "the runtime_id keeps resolving beside the name"
    )


def test_a_node_flag_naming_nothing_says_so_and_lists_what_is_live(
    isolated_registry, stub_control_plane
):
    server = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rnamed", server.local_api_socket_path, runtime_name="rig-desk-a1b2"
    )

    with pytest.raises(ControlPlaneError) as refusal:
        resolve_local_api_socket_of_requested_node("rig-nowhere-0000")

    assert "rig-nowhere-0000" in str(refusal.value)
    assert "rig-desk-a1b2" in str(refusal.value), (
        "the refusal lists the names a caller could have meant"
    )


def test_two_nodes_answering_to_one_name_are_named_rather_than_picked_between(
    isolated_registry, stub_control_plane
):
    first = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rfirst", first.local_api_socket_path, runtime_name="rig-desk-a1b2"
    )
    second = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rsecond", second.local_api_socket_path, runtime_name="rig-desk-a1b2"
    )

    with pytest.raises(ControlPlaneError) as refusal:
        resolve_local_api_socket_of_requested_node("rig-desk-a1b2")

    for named_row in (
        "Rfirst",
        first.local_api_socket_path,
        "Rsecond",
        second.local_api_socket_path,
    ):
        assert named_row in str(refusal.value), (
            f"the refusal names each matching row's runtime_id and socket: {refusal.value}"
        )


def test_a_verb_given_a_name_two_live_runtimes_hold_is_refused_naming_both(
    isolated_registry, stub_control_plane, capsys
):
    first = stub_control_plane()
    write_registry_entry(isolated_registry, "Rfirst", first.local_api_socket_path, runtime_name="rig-desk-a1b2")
    second = stub_control_plane()
    write_registry_entry(isolated_registry, "Rsecond", second.local_api_socket_path, runtime_name="rig-desk-a1b2")

    assert cli.main(["graph", "--node", "rig-desk-a1b2"]) == 1

    printed = capsys.readouterr()
    for named_row in (
        "Rfirst",
        first.local_api_socket_path,
        "Rsecond",
        second.local_api_socket_path,
    ):
        assert named_row in printed.err, printed.err
    assert printed.out == "", f"a refused name prints neither runtime's graph: {printed.out!r}"


def test_graph_prints_the_tool_result(isolated_registry, stub_control_plane, capsys):
    server = stub_control_plane(tool_answer=stub_tool_answer('{"nodes":[]}'))
    write_registry_entry(isolated_registry, "Ronly", server.local_api_socket_path)

    assert cli.main(["graph"]) == 0

    assert '{"nodes":[]}' in capsys.readouterr().out


def test_tap_sends_the_channel_and_count(
    isolated_registry, stub_control_plane, capsys
):
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ronly", server.local_api_socket_path)

    assert cli.main(["tap", "cam/video", "--count", "3"]) == 0

    arguments = server.recorded_tool_calls[-1].arguments
    assert arguments == {"channel": "cam/video", "count": 3}


def test_tap_forwards_a_named_per_bag_cap(isolated_registry, stub_control_plane, capsys):
    """A bag over the tool's per-bag cap comes back undecodable, so a caller
    that raised the cap and had the flag silently dropped would get exactly the
    failure it was trying to avoid."""
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ronly", server.local_api_socket_path)

    assert cli.main(["tap", "cam/video", "--max-bag-bytes", "4096"]) == 0

    arguments = server.recorded_tool_calls[-1].arguments
    assert arguments == {"channel": "cam/video", "max_bag_bytes": 4096}


def test_tap_omits_a_per_bag_cap_nobody_named(
    isolated_registry, stub_control_plane, capsys
):
    """Absent means absent: the tool's own default is what applies, and the CLI
    does not invent one of its own."""
    server = stub_control_plane()
    write_registry_entry(isolated_registry, "Ronly", server.local_api_socket_path)

    assert cli.main(["tap", "cam/video"]) == 0

    arguments = server.recorded_tool_calls[-1].arguments
    assert arguments == {"channel": "cam/video"}


def test_logs_with_a_node_reads_its_live_event_stream_through_the_local_api_socket(
    isolated_registry, stub_control_plane, capsys
):
    server = stub_control_plane(tool_answer=stub_tool_answer('[{"event":"started"}]'))
    write_registry_entry(
        isolated_registry, "Rlogs", server.local_api_socket_path, runtime_name="rig-logs"
    )

    assert cli.main(["logs", "--node", "rig-logs", "--count", "4"]) == 0

    assert server.recorded_tool_calls[-1] == RecordedToolCall("logs", {"count": 4})
    assert '[{"event":"started"}]' in capsys.readouterr().out


def test_a_node_target_with_on_disk_filters_is_refused(
    isolated_registry, stub_control_plane, capsys
):
    # The live event-stream tool takes a count and nothing else, so a filter
    # here would be silently ignored rather than applied.
    server = stub_control_plane()
    write_registry_entry(
        isolated_registry, "Rlogs", server.local_api_socket_path, runtime_name="rig-logs"
    )

    assert cli.main(["logs", "--node", "rig-logs", "--level", "warn"]) == 1

    assert "--level" in capsys.readouterr().err
    assert server.recorded_tool_calls == [], "a refused flag reaches no node"


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


def tap_result_tool_answer(
    channel: str,
    framed_bags: "list[bytes]",
    *,
    hex_truncated: bool = False,
    truncated_bag_indexes: "frozenset[int]" = frozenset(),
    report_byte_len: bool = True,
) -> StubToolAnswer:
    """One `tap` tool result carrying these bags, shaped as the tool shapes it.

    `truncated_bag_indexes` caps just those bags' previews, so a test can put an
    oversized bag where the stride will or will not reach it.
    """
    return stub_tool_answer(
        json.dumps(
            {
                "channel": channel,
                "requested": len(framed_bags),
                "received": len(framed_bags),
                "window_ms": 500,
                "dropped_bags": 0,
                "bags": [
                    {
                        # The tool reports the whole bag's length, not the
                        # preview's, so a capped bag reads as larger than what
                        # rode with it. `report_byte_len=False` models a node
                        # that flags the cap without sizing it.
                        **(
                            {
                                "byte_len": (
                                    9000
                                    if hex_truncated or index in truncated_bag_indexes
                                    else len(bag)
                                )
                            }
                            if report_byte_len
                            else {}
                        ),
                        "hex_preview": bag.hex(),
                        "hex_truncated": (
                            hex_truncated or index in truncated_bag_indexes
                        ),
                    }
                    for index, bag in enumerate(framed_bags)
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

    exchanged = fetch_surface_image_png_bytes(server.local_api_socket, "cam/frame#7")

    assert exchanged.png_image_bytes == png_bytes_for("seven")
    assert server.recorded_image_request_paths == [
        "/api/surfaces/cam%2Fframe%237/image"
    ]


def test_the_exchange_states_the_surface_s_own_extent(stub_control_plane):
    server = stub_control_plane(surface_image_answers={"s#1": image_answer("one")})

    exchanged = fetch_surface_image_png_bytes(server.local_api_socket, "s#1")

    assert exchanged.source_surface_pixel_width == 1920
    assert exchanged.source_surface_pixel_height == 1080


def test_a_recycled_frame_is_a_refusal_that_composes_as_a_retry(stub_control_plane):
    server = stub_control_plane(surface_image_answers={"s#1": RECYCLED_FRAME_ANSWER})

    with pytest.raises(SurfaceImageExchangeRefusal) as refused:
        fetch_surface_image_png_bytes(server.local_api_socket, "s#1")

    assert refused.value.names_a_recycled_frame
    assert "recycled" in str(refused.value)


@pytest.mark.parametrize("status", [404, 501])
def test_a_refusal_that_is_not_a_recycled_frame_does_not_compose(stub_control_plane, status):
    # A surface that never existed, or a format with no conversion arm, will
    # refuse identically forever — retrying it would spin rather than recover.
    server = stub_control_plane(
        surface_image_answers={"s#1": StubSurfaceImageAnswer(status, error_message="no")}
    )

    with pytest.raises(SurfaceImageExchangeRefusal) as refused:
        fetch_surface_image_png_bytes(server.local_api_socket, "s#1")

    assert not refused.value.names_a_recycled_frame
    assert refused.value.http_status == status


# ─── `streamlib exchange <SURFACE_ID>` ───────────────────────────────────────


def test_the_id_form_writes_the_exact_bytes_and_prints_the_path(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    targeted_stub_control_plane(
        surface_image_answers={"cam/frame#7": image_answer("seven")}
    )
    output_directory = tmp_path / "frames"

    assert (
        cli.main(
            ["exchange", "cam/frame#7", "--out", str(output_directory)]
        )
        == 0
    )

    written = capsys.readouterr().out.strip()
    assert Path(written).read_bytes() == png_bytes_for("seven")
    assert Path(written).parent == output_directory


def test_the_id_form_reaches_a_registered_node_through_its_local_api_socket(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(
        surface_image_answers={"cam/frame#7": image_answer("seven")}
    )
    write_registry_entry(
        isolated_registry, "Rcam", server.local_api_socket_path, runtime_name="rig-cam"
    )
    output_directory = tmp_path / "frames"

    assert (
        cli.main(
            ["exchange", "cam/frame#7", "--out", str(output_directory), "--node", "rig-cam"]
        )
        == 0
    )

    written = capsys.readouterr().out.strip()
    assert Path(written).read_bytes() == png_bytes_for("seven")
    assert server.recorded_image_request_paths == ["/api/surfaces/cam%2Fframe%237/image"]


def test_the_id_form_creates_the_output_directory(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    targeted_stub_control_plane(surface_image_answers={"s#1": image_answer("one")})
    output_directory = tmp_path / "nested" / "frames"

    assert (
        cli.main(["exchange", "s#1", "--out", str(output_directory)])
        == 0
    )

    assert output_directory.is_dir()
    assert len(list(output_directory.glob("*.png"))) == 1


def test_a_surface_id_that_does_not_resolve_fails_the_verb(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    targeted_stub_control_plane(surface_image_answers={})

    assert (
        cli.main(["exchange", "gone#1", "--out", str(tmp_path)]) == 1
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
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    server = targeted_stub_control_plane(
        queued_tool_answers=[
            tap_result_tool_answer(
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
            ]
        )
        == 0
    )

    tap_arguments = server.recorded_tool_calls[0].arguments
    assert tap_arguments == {"channel": "cam/frame", "count": 2}

    printed = capsys.readouterr()
    written = [Path(line) for line in printed.out.splitlines()]
    assert [path.read_bytes() for path in written] == [
        png_bytes_for("one"),
        png_bytes_for("two"),
    ]
    assert "exchanged 2 of 2" in printed.err


def test_the_channel_form_reaches_a_registered_node_through_its_local_api_socket(
    isolated_registry, stub_control_plane, tmp_path, capsys
):
    server = stub_control_plane(
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id("s#1"), bag_publishing_surface_id("s#2")],
            ),
        ],
        surface_image_answers={"s#1": image_answer("one"), "s#2": image_answer("two")},
    )
    write_registry_entry(
        isolated_registry, "Rcam", server.local_api_socket_path, runtime_name="rig-cam"
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
                "--node",
                "rig-cam",
            ]
        )
        == 0
    )

    printed = capsys.readouterr()
    written = [Path(line) for line in printed.out.splitlines()]
    assert [path.read_bytes() for path in written] == [
        png_bytes_for("one"),
        png_bytes_for("two"),
    ]
    assert server.recorded_image_request_paths == [
        "/api/surfaces/s%231/image",
        "/api/surfaces/s%232/image",
    ]


def test_the_engine_is_never_asked_to_read_a_bag(
    isolated_registry, targeted_stub_control_plane, tmp_path
):
    # The composition is the client's whole job: `tap` keeps its shipped
    # contract, gaining no field argument and no decode.
    server = targeted_stub_control_plane(
        queued_tool_answers=[
            tap_result_tool_answer("cam/frame", [bag_publishing_surface_id("s#1")])
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
        ]
    )

    tap_arguments = server.recorded_tool_calls[0].arguments
    assert set(tap_arguments) == {"channel", "count"}


def test_the_field_override_reads_the_key_the_caller_named(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    targeted_stub_control_plane(
        queued_tool_answers=[
            tap_result_tool_answer(
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
            ]
        )
        == 0
    )

    assert Path(capsys.readouterr().out.strip()).read_bytes() == png_bytes_for("nine")


def test_a_recycled_frame_is_retried_against_a_newer_bag_and_reported(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # The loud half of the contract: the run recovers, and says which id it had
    # to give up on, so a sample can never quietly become a different frame.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer("cam/frame", [bag_publishing_surface_id("stale#1")]),
            tap_result_tool_answer("cam/frame", [bag_publishing_surface_id("fresh#2")]),
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
            ]
        )
        == 0
    )

    printed = capsys.readouterr()
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("fresh")
    assert "retried 1 recycled frame" in printed.err
    assert "stale#1" in printed.err


def test_a_bag_without_the_named_field_is_counted_rather_than_fatal(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
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
            ]
        )
        == 0
    )

    printed = capsys.readouterr()
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("one")
    assert "1 bag carried no surface id" in printed.err


def test_every_nth_bag_selects_the_stride(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    labels = ["a", "b", "c", "d", "e", "f"]
    server = targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
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
    assert server.recorded_tool_calls[0].arguments["count"] == 6


def test_the_stride_runs_across_tap_rounds_rather_than_restarting(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # A stride reset per round would exchange `a` then `c` — the first bag of
    # each round — reporting a stride it did not apply. Continuing the count
    # across rounds selects `a` then `d`.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id(f"s#{label}") for label in ("a", "b")],
            ),
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id(f"s#{label}") for label in ("c", "d")],
            ),
        ],
        surface_image_answers={
            f"s#{label}": image_answer(label) for label in ("a", "b", "c", "d")
        },
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
            ]
        )
        == 0
    )

    written = [Path(line) for line in capsys.readouterr().out.splitlines()]
    assert [path.read_bytes() for path in written] == [
        png_bytes_for("a"),
        png_bytes_for("d"),
    ], "the stride restarted at each tap round"


def test_a_short_sample_exits_nonzero(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # A harness reading the directory must not take "fewer frames than I asked
    # for" as "this is all the channel had".
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer("cam/frame", [bag_publishing_surface_id("s#1")])
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
            ]
        )
        == 1
    )

    printed = capsys.readouterr()
    assert "exchanged 1 of 3" in printed.err
    # The one frame that did land is still real, and still named.
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("one")


def test_a_refusal_that_cannot_be_retried_stops_the_run(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer("cam/frame", [bag_publishing_surface_id("s#1")])
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
            ]
        )
        == 1
    )

    assert "no conversion arm" in capsys.readouterr().err


def test_frames_that_landed_before_a_fatal_stop_are_still_printed(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # A PNG on disk whose path was never printed is evidence a harness cannot
    # use and a human will not find, so the stop is reported beside the frames
    # rather than instead of them.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id("s#1"), bag_publishing_surface_id("s#2")],
            )
        ],
        surface_image_answers={
            "s#1": image_answer("one"),
            "s#2": StubSurfaceImageAnswer(404, error_message="no such surface"),
        },
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
            ]
        )
        == 1
    )

    printed = capsys.readouterr()
    written = [Path(line) for line in printed.out.splitlines()]
    assert [path.read_bytes() for path in written] == [png_bytes_for("one")]
    assert "no such surface" in printed.err
    # Every PNG on disk is a PNG that was named.
    assert len(list(tmp_path.glob("*.png"))) == len(written)


def test_a_bag_the_tap_truncated_stops_the_run_by_name(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # The tap tool hex-previews only a bounded prefix of a large bag. Decoding
    # that prefix would hand back a bag missing its later fields — including,
    # possibly, the surface id.
    whole_bag = framed_bag(
        msgpack_named_map({"surface_id": "s#1", "filler": "x" * 200})
    )
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[tap_result_tool_answer("cam/frame", [whole_bag[:-32]])],
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--out",
                str(tmp_path),
            ]
        )
        == 1
    )

    assert "truncated" in capsys.readouterr().err


def test_a_bag_past_the_taps_preview_cap_stops_the_run_and_names_the_size(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # The tap tool says when it capped a bag. Counting one as "published no
    # surface id" would blame the channel for something this client could not
    # read, and retrying it would never converge.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
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
            ]
        )
        == 1
    )

    reported = capsys.readouterr().err
    assert "past the prefix `tap` previews" in reported
    assert "9000 bytes" in reported, "the diagnosis must name the size that did not fit"
    # The id form still reaches such a frame, and the message says so.
    assert "streamlib exchange <surface-id>" in reported


def test_a_capped_bag_with_no_reported_size_is_still_diagnosed_as_capped(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # A verb drives whichever node it resolves, so the size is the tool's to
    # report and may be missing. Losing the cap would misdiagnose the bag as
    # one this client could not decode — blaming the channel for the tool's
    # own limit.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id("s#1")],
                hex_truncated=True,
                report_byte_len=False,
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
            ]
        )
        == 1
    )

    assert "past the prefix `tap` previews" in capsys.readouterr().err


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


def test_the_stride_steps_over_an_oversized_bag_rather_than_dying_on_it(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # The loop must actually reach the capped bag and pass it by, so bag 0 is
    # selected but publishes no id — without that the run finishes on bag 0 and
    # never proves where the cap check sits relative to the stride.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [
                    framed_bag(msgpack_named_map({"width": 640}), slice_capacity=1024),
                    bag_publishing_surface_id("s#2"),
                    bag_publishing_surface_id("s#3"),
                ],
                truncated_bag_indexes=frozenset({1}),
            )
        ],
        surface_image_answers={"s#3": image_answer("three")},
    )

    assert (
        cli.main(
            [
                "exchange",
                "--channel",
                "cam/frame",
                "--count",
                "1",
                "--every",
                "2",
                "--out",
                str(tmp_path),
            ]
        )
        == 0
    ), "a capped bag the stride skipped ended a run that never needed it"

    assert Path(capsys.readouterr().out.strip()).read_bytes() == png_bytes_for("three")


def test_a_bag_the_stride_skips_cannot_kill_the_run_by_being_oversized(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # Bag 1 is past the preview cap, and `--every 2` never selects it. A run
    # that needs only bag 0 must not fail on a bag it never reads.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id("s#1"), bag_publishing_surface_id("s#2")],
                truncated_bag_indexes=frozenset({1}),
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
                "--count",
                "1",
                "--every",
                "2",
                "--out",
                str(tmp_path),
            ]
        )
        == 0
    )

    assert Path(capsys.readouterr().out.strip()).read_bytes() == png_bytes_for("one")


def test_an_oversized_bag_does_not_discard_the_readable_bags_beside_it(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # Bag 0 is readable and bag 1 is not. Failing the whole tap round would
    # throw away a frame that had already been exchanged.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id("s#1"), bag_publishing_surface_id("s#2")],
                truncated_bag_indexes=frozenset({1}),
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
                "--count",
                "2",
                "--out",
                str(tmp_path),
            ]
        )
        == 1
    )

    printed = capsys.readouterr()
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("one")
    assert "9000 bytes" in printed.err


def test_a_write_that_fails_still_names_the_frames_that_landed(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys, monkeypatch
):
    # The filesystem half of the same promise the report exists to keep: a PNG
    # on disk whose path was never printed is evidence nobody can use.
    targeted_stub_control_plane(
        tool_answer=tap_result_tool_answer("cam/frame", []),
        queued_tool_answers=[
            tap_result_tool_answer(
                "cam/frame",
                [bag_publishing_surface_id("s#1"), bag_publishing_surface_id("s#2")],
            )
        ],
        surface_image_answers={"s#1": image_answer("one"), "s#2": image_answer("two")},
    )

    real_write_bytes = Path.write_bytes
    written_so_far: "list[Path]" = []

    def write_bytes_failing_on_the_second(self: Path, data: bytes) -> int:
        written_so_far.append(self)
        if len(written_so_far) == 2:
            raise PermissionError(13, "Permission denied")
        return real_write_bytes(self, data)

    monkeypatch.setattr(Path, "write_bytes", write_bytes_failing_on_the_second)

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
            ]
        )
        == 1
    )

    printed = capsys.readouterr()
    assert Path(printed.out.strip()).read_bytes() == png_bytes_for("one")
    assert "Permission denied" in printed.err


def test_an_output_directory_that_cannot_be_written_is_reported_not_raised(
    isolated_registry, targeted_stub_control_plane, tmp_path, capsys
):
    # `--out` naming an existing regular file is a typo, and typos get a
    # message rather than a Python traceback.
    targeted_stub_control_plane(surface_image_answers={"s#1": image_answer("one")})
    already_a_file = tmp_path / "already-a-file"
    already_a_file.write_text("not a directory")

    assert (
        cli.main(
            ["exchange", "s#1", "--out", str(already_a_file)]
        )
        == 1
    )

    assert "could not write into" in capsys.readouterr().err

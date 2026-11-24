# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Each node of this wheel loads on `tatolabd`, alone and wired to the codec nodes, as an installed extension's node is added."""

import json
import os
import re
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import pytest
from tatolab.h264 import H264Decoder, H264Encoder
from tatolab.opus import OpusDecoder, OpusEncoder
from tatolab.stream import (
    StreamBuilder,
    graph_generator,
    run_graph_generator,
)
from tatolab.webrtc import WhepPlayer, WhipPublisher

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")


@graph_generator
def the_whip_publisher_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(WhipPublisher, input_values={"url": "https://example.invalid/x"})


@graph_generator
def the_whep_player_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(WhepPlayer, input_values={"url": "https://example.invalid/x"})


@graph_generator
def the_publisher_fed_by_both_encoders(stream_builder: StreamBuilder) -> None:
    video_encoder = stream_builder.add(H264Encoder)
    audio_encoder = stream_builder.add(OpusEncoder)
    publisher = stream_builder.add(
        WhipPublisher, input_values={"url": "https://example.invalid/whip"}
    )

    stream_builder.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    stream_builder.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))


@graph_generator
def the_player_feeding_both_decoders(stream_builder: StreamBuilder) -> None:
    player = stream_builder.add(WhepPlayer, input_values={"url": "https://example.invalid/whep"})
    video_decoder = stream_builder.add(H264Decoder)
    audio_decoder = stream_builder.add(OpusDecoder)

    stream_builder.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    stream_builder.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )


@graph_generator
def a_publish_and_play_round_trip(stream_builder: StreamBuilder) -> None:
    encoder = stream_builder.add(H264Encoder)
    publisher = stream_builder.add(
        WhipPublisher, input_values={"url": "https://example.invalid/whip"}
    )
    player = stream_builder.add(WhepPlayer, input_values={"url": "https://example.invalid/whep"})
    decoder = stream_builder.add(H264Decoder)

    stream_builder.connect(encoder.output("encoded_video"), publisher.input("tracks"))
    stream_builder.connect(player.output("encoded_video"), decoder.input("encoded_video"))


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
RUNTIME_UNIT_DIRECTORY = Path(
    os.environ.get("STREAMLIB_RUNTIME_UNIT_DIRECTORY")
    or REPOSITORY_ROOT / "target" / "tatolab-runtime"
)
TATOLABD_EXECUTABLE = RUNTIME_UNIT_DIRECTORY / "bin" / "tatolabd"
TATOLAB_EXECUTABLE = RUNTIME_UNIT_DIRECTORY / "bin" / "tatolab"

#: The line `tatolabd` logs as it begins starting a loaded stream.
STREAM_START_BEGAN_LINE = re.compile(r"\[start\] Starting the stream `(?P<stream_id>[^`]+)`")

STREAM_UNLOAD_REMOVED_NODES_LINE = re.compile(
    r"\[stop\] Queued removal of (?P<node_count>\d+) node\(s\)"
)

#: The Vulkan loader's variables naming the driver files to load. Pointed at a
#: path that does not exist they leave it none, so every stream's start is
#: refused at the GPU, before any device opens, on a machine with a GPU too.
VULKAN_LOADER_DRIVER_FILE_VARIABLES = ("VK_DRIVER_FILES", "VK_ICD_FILENAMES", "VK_ADD_DRIVER_FILES")

#: The engine's refusal when the Vulkan loader finds no driver, on every platform.
NO_VULKAN_DRIVER_REFUSAL = "No usable Vulkan driver (ICD) was found"

#: Engine variables of this process a runtime must not inherit.
ENGINE_VARIABLES_THE_RUNTIME_DOES_NOT_INHERIT = (
    "STREAMLIB_QUIET",
    "STREAMLIB_HOME",
    "STREAMLIB_ICEORYX2_DOMAIN_ROOT",
)

#: The variable a test build of the runtime and its CLI read their machine root from.
TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE = "TATOLAB_TEST_MACHINE_ROOT"

#: Not `tempfile.gettempdir()`: the local API socket under a machine root has to
#: fit `sun_path`, and macOS's per-user temporary directory is too long for it.
SHORT_TEMPORARY_DIRECTORY = "/tmp"

#: The variable a test build of the runtime reads its loopback HTTP port from;
#: on macOS that port's bind is the machine runtime lock, so it must be set there.
TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE = "TATOLAB_TEST_LOOPBACK_HTTP_PORT"

#: Covers starting each Python node's interpreter to describe it, on a cold runner.
LOAD_DEADLINE_SECONDS = 120.0
SERVING_DEADLINE_SECONDS = 30.0
LOCAL_API_PROBE_DEADLINE_SECONDS = 5.0
STOP_DEADLINE_SECONDS = 30.0


def _runtime_unit_executable(executable: Path) -> Path:
    if not os.access(executable, os.X_OK):
        pytest.fail(
            f"no {executable.name} at {executable} — build the runtime unit with "
            "`cargo xtask build-runtime`, or name one with STREAMLIB_RUNTIME_UNIT_DIRECTORY"
        )
    return executable


def _a_runtime_answers_at_the_local_api_socket(
    cli_environment: "dict[str, str]",
) -> bool:
    try:
        streams_probe = subprocess.run(
            [str(_runtime_unit_executable(TATOLAB_EXECUTABLE)), "streams"],
            env=cli_environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=LOCAL_API_PROBE_DEADLINE_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return False
    return streams_probe.returncode == 0


class TatolabdWithNoVulkanDriver:
    """A `tatolabd` from the runtime unit whose every stream start is refused at the GPU."""

    def __init__(
        self, process: "subprocess.Popen[str]", cli_environment: "dict[str, str]"
    ) -> None:
        self.process = process
        self.cli_environment = cli_environment
        self.standard_error_lines: "list[str]" = []
        self._standard_error_drain = threading.Thread(
            target=self._drain_standard_error, daemon=True
        )
        self._standard_error_drain.start()

    def _drain_standard_error(self) -> None:
        assert self.process.stderr is not None
        for standard_error_line in self.process.stderr:
            self.standard_error_lines.append(standard_error_line)

    def standard_error_text_since(self, line_count: int) -> str:
        return "".join(self.standard_error_lines[line_count:])

    def recent_standard_error(self) -> str:
        return "".join(self.standard_error_lines[-40:])

    def stop(self) -> None:
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
            try:
                self.process.wait(timeout=STOP_DEADLINE_SECONDS)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self._standard_error_drain.join(timeout=STOP_DEADLINE_SECONDS)


@pytest.fixture(scope="module")
def short_machine_root() -> "Iterator[Path]":
    """A fresh machine root a test build keeps its runtime and state under, and
    on Linux its lock."""
    with tempfile.TemporaryDirectory(
        prefix="tl-", dir=SHORT_TEMPORARY_DIRECTORY
    ) as machine_root_path:
        yield Path(machine_root_path)


def _a_tcp_port_free_on_loopback() -> int:
    with socket.create_server(("127.0.0.1", 0)) as probe:
        return probe.getsockname()[1]


@pytest.fixture(scope="module")
def tatolabd_with_no_vulkan_driver(
    tmp_path_factory: pytest.TempPathFactory, short_machine_root: Path
) -> "Iterator[TatolabdWithNoVulkanDriver]":
    """The machine's runtime for this module, started here and stopped after it.

    A test build of the runtime unit keeps its lock, local API socket and state
    under a machine root of this module's own, so it runs beside the user's
    runtime. A release build ignores that root: it takes the machine's runtime
    lock and serves this user's local API socket, so it is refused while
    another runtime holds the machine. Either way its state directory is its
    own — `XDG_STATE_HOME`, and `HOME` on macOS — so it loads none of this
    user's kept streams.
    """
    tatolabd_executable = _runtime_unit_executable(TATOLABD_EXECUTABLE)
    cli_environment = {
        **os.environ,
        TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE: str(short_machine_root),
    }
    if _a_runtime_answers_at_the_local_api_socket(cli_environment):
        pytest.fail(
            "a runtime already answers at this user's local API socket; stop it and run "
            "these again — they start the machine's runtime themselves"
        )
    runtime_directory = tmp_path_factory.mktemp("tatolabd")
    state_home = runtime_directory / "state"
    state_home.mkdir()
    no_such_driver_file = str(runtime_directory / "no-vulkan-driver-here.json")
    environment = {
        name: value
        for name, value in os.environ.items()
        if name not in ENGINE_VARIABLES_THE_RUNTIME_DOES_NOT_INHERIT
    }
    environment.update(
        {variable: no_such_driver_file for variable in VULKAN_LOADER_DRIVER_FILE_VARIABLES}
    )
    environment["RUST_LOG"] = "info,rmcp=warn"
    environment[TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE] = str(short_machine_root)
    environment["XDG_STATE_HOME"] = str(state_home)
    if sys.platform == "darwin":
        environment["HOME"] = str(state_home)
        environment[TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE] = str(
            _a_tcp_port_free_on_loopback()
        )
    tatolabd = TatolabdWithNoVulkanDriver(
        subprocess.Popen(
            [str(tatolabd_executable)],
            cwd=runtime_directory,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
        ),
        cli_environment,
    )
    try:
        serving_deadline = time.monotonic() + SERVING_DEADLINE_SECONDS
        while not _a_runtime_answers_at_the_local_api_socket(cli_environment):
            if tatolabd.process.poll() is not None:
                tatolabd.stop()
                pytest.fail(
                    f"tatolabd exited {tatolabd.process.returncode} before it served its "
                    f"local API:\n{tatolabd.recent_standard_error()}"
                )
            if time.monotonic() > serving_deadline:
                pytest.fail(
                    f"tatolabd did not serve its local API within {SERVING_DEADLINE_SECONDS} s:"
                    f"\n{tatolabd.recent_standard_error()}"
                )
            time.sleep(0.05)
        yield tatolabd
    finally:
        tatolabd.stop()


def write_a_project_holding_the_graph(
    project_directory: Path, stream_name: str, graph: "dict[str, Any]"
) -> None:
    """A project whose one graph document, `streams/<stream_name>.graph.json`, holds `graph`,
    each node given a UUID as the runtime gives one, and whose `.venv/bin/python` execs this
    test's interpreter, which holds the extension."""
    streams_directory = project_directory / "streams"
    streams_directory.mkdir(parents=True)
    (project_directory / "tatolab-project.json").write_text(
        json.dumps({"project": str(uuid.uuid4()), "name": "webrtc", "dependencies": []}),
        encoding="utf-8",
    )
    graph_document = {
        **graph,
        "asset": str(uuid.uuid4()),
        "nodes": [{"id": str(uuid.uuid4()), **node} for node in graph["nodes"]],
    }
    (streams_directory / f"{stream_name}.graph.json").write_text(
        json.dumps(graph_document), encoding="utf-8"
    )
    venv_interpreter = project_directory / ".venv" / "bin" / "python"
    venv_interpreter.parent.mkdir(parents=True)
    venv_interpreter.write_text(
        f'#!/bin/sh\nexec {shlex.quote(sys.executable)} "$@"\n', encoding="utf-8"
    )
    venv_interpreter.chmod(0o755)


def node_names_loaded_by_tatolabd(
    stream_function: "Callable[[StreamBuilder], None]",
    working_directory: Path,
    tatolabd: TatolabdWithNoVulkanDriver,
) -> "list[str]":
    graph = run_graph_generator(stream_function)
    stream_name = stream_function.__name__
    project_directory = working_directory / "project"
    write_a_project_holding_the_graph(project_directory, stream_name, graph)

    standard_error_mark = len(tatolabd.standard_error_lines)
    tatolab_run = subprocess.run(
        [
            str(_runtime_unit_executable(TATOLAB_EXECUTABLE)),
            "open",
            "--dir",
            str(project_directory),
            "--play",
        ],
        cwd=working_directory,
        env=tatolabd.cli_environment,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        errors="replace",
        timeout=LOAD_DEADLINE_SECONDS,
    )

    def what_the_run_and_the_runtime_said() -> str:
        return (
            f"`tatolab open --play` exited {tatolab_run.returncode}:\n{tatolab_run.stdout[-4000:]}"
            f"{tatolab_run.stderr[-4000:]}\n"
            f"tatolabd:\n{tatolabd.standard_error_text_since(standard_error_mark)[-8000:]}"
        )

    # The queued load runs after `tatolab open --play` exits, and its lines reach this
    # process through the drain thread.
    load_lines_deadline = time.monotonic() + LOAD_DEADLINE_SECONDS
    while True:
        standard_error_text = tatolabd.standard_error_text_since(standard_error_mark)
        start_began = STREAM_START_BEGAN_LINE.search(standard_error_text)
        refused_at_the_gpu = NO_VULKAN_DRIVER_REFUSAL in standard_error_text
        if (start_began is not None and refused_at_the_gpu) or (
            time.monotonic() > load_lines_deadline
        ):
            break
        time.sleep(0.05)

    if start_began is None:
        pytest.fail("tatolabd did not load the stream:\n" + what_the_run_and_the_runtime_said())
    assert f"-{stream_name}-" in start_began.group("stream_id"), what_the_run_and_the_runtime_said()
    assert tatolab_run.returncode == 0 and refused_at_the_gpu, (
        "the loaded stream was to be refused at its start for the missing Vulkan driver; "
        + what_the_run_and_the_runtime_said()
    )

    standard_error_mark_before_the_removal = len(tatolabd.standard_error_lines)
    removal_run = subprocess.run(
        [str(_runtime_unit_executable(TATOLAB_EXECUTABLE)), "rm", "--dir", str(project_directory)],
        cwd=working_directory,
        env=tatolabd.cli_environment,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        errors="replace",
        timeout=LOAD_DEADLINE_SECONDS,
    )
    assert removal_run.returncode == 0, (
        f"`tatolab rm` exited {removal_run.returncode}:\n{removal_run.stdout}{removal_run.stderr}"
    )
    unload_line_deadline = time.monotonic() + LOAD_DEADLINE_SECONDS
    while True:
        unload = STREAM_UNLOAD_REMOVED_NODES_LINE.search(
            tatolabd.standard_error_text_since(standard_error_mark_before_the_removal)
        )
        if unload is not None or time.monotonic() > unload_line_deadline:
            break
        time.sleep(0.05)
    assert unload is not None, what_the_run_and_the_runtime_said()
    assert int(unload.group("node_count")) == len(graph["nodes"])
    return [loaded_node["name"] for loaded_node in graph["nodes"]]


@pytest.mark.parametrize(
    ("stream_function", "node_class"),
    [(the_whip_publisher_alone, WhipPublisher), (the_whep_player_alone, WhepPlayer)],
)
def test_an_installed_extensions_node_is_added_like_any_other(
    tmp_path, tatolabd_with_no_vulkan_driver, stream_function, node_class
):
    assert node_names_loaded_by_tatolabd(
        stream_function, tmp_path, tatolabd_with_no_vulkan_driver
    ) == [
        node_class.__name__.lower()
    ]


def test_the_publisher_wires_to_both_encoders_without_an_adapter(tmp_path, tatolabd_with_no_vulkan_driver):
    """One fan-in port takes both encoders, which is what makes a WHIP session
    with video and audio a matter of wiring rather than of settings."""
    node_names_loaded_by_tatolabd(the_publisher_fed_by_both_encoders, tmp_path, tatolabd_with_no_vulkan_driver)


def test_the_player_wires_to_both_decoders_without_an_adapter(tmp_path, tatolabd_with_no_vulkan_driver):
    node_names_loaded_by_tatolabd(the_player_feeding_both_decoders, tmp_path, tatolabd_with_no_vulkan_driver)


def test_a_publish_and_play_round_trip_composes_as_published(tmp_path, tatolabd_with_no_vulkan_driver):
    """The shape the live proof drives: encode, publish, play back, decode."""
    node_names_loaded_by_tatolabd(a_publish_and_play_round_trip, tmp_path, tatolabd_with_no_vulkan_driver)

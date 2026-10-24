# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors as a graph sees them.

`stream_builder.add` with no adapter and no engine change is the whole claim of the
extension model, so it is what these check — each stream compiled here and loaded
with `tatolab run` on the machine's runtime, a real `tatolabd` from the runtime
unit, which needs no device to load one. The load is the subject: the runtime
reaches no Vulkan driver, so the start that follows every load is refused at the
GPU, and these require exactly that refusal.

The runtime unit is `$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else the checkout's
`target/tatolab-runtime` that `cargo xtask build-runtime` lays out. The helper-side
tests below import `tatolab.runtime` from its lend, on `PYTHONPATH`.
"""

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
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any, cast

import pytest

from tatolab.webrtc import WhepPlayer, WhepPlayerConfig, WhipPublisher
from tatolab.webrtc.processors import (
    ENCODED_AUDIO_OUTPUT_PORT,
    ENCODED_VIDEO_OUTPUT_PORT,
    FIRST_RECONNECT_DELAY_SECONDS,
    HELPER_LINK_PAYLOAD_CEILING_BYTES,
    LONGEST_RECONNECT_DELAY_SECONDS,
    LinkOutputWriteFailure,
    PublishedTrack,
    VideoOrAudio,
    TRACKS_INPUT_PORT,
    _native,
    published_track_from_catalog_entry,
    refuse_audio_rtp_cannot_carry,
    resolve_track_kind,
)
from helper_process_link_wiring import wire_one_link
from parent_catalog_entry_stand_in import ParentHoldingCatalogEntries
from played_media_stand_ins import (
    MEDIA_FRAME_OF_AN_ACCESS_UNIT,
    MEDIA_FRAME_OF_AN_OPUS_PACKET,
    MONO_OPUS_IDENTIFICATION_HEADER_HEX,
    AccessUnitTheSessionPlayed,
    OpusPacketTheSessionPlayed,
)
from tatolab.runtime import _engine
from tatolab.runtime._processor_hosting import construct_processor_instance
from tatolab.stream import (
    H264Decoder,
    H264Encoder,
    Message,
    OpusDecoder,
    OpusEncoder,
    StreamBuilder,
    compile_stream_to_graph,
    log,
    stream,
)
from tatolab.stream.catalog import AudioEntry, CatalogEntry, VideoEntry

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")


@stream
def the_whip_publisher_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(WhipPublisher, config={"url": "https://example.invalid/x"})


@stream
def the_whep_player_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(WhepPlayer, config={"url": "https://example.invalid/x"})


@stream
def the_publisher_fed_by_both_encoders(stream_builder: StreamBuilder) -> None:
    video_encoder = stream_builder.add(H264Encoder)
    audio_encoder = stream_builder.add(OpusEncoder)
    publisher = stream_builder.add(
        WhipPublisher, config={"url": "https://example.invalid/whip"}
    )

    stream_builder.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    stream_builder.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))


@stream
def the_player_feeding_both_decoders(stream_builder: StreamBuilder) -> None:
    player = stream_builder.add(WhepPlayer, config={"url": "https://example.invalid/whep"})
    video_decoder = stream_builder.add(H264Decoder)
    audio_decoder = stream_builder.add(OpusDecoder)

    stream_builder.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    stream_builder.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )


@stream
def a_publish_and_play_round_trip(stream_builder: StreamBuilder) -> None:
    encoder = stream_builder.add(H264Encoder)
    publisher = stream_builder.add(
        WhipPublisher, config={"url": "https://example.invalid/whip"}
    )
    player = stream_builder.add(WhepPlayer, config={"url": "https://example.invalid/whep"})
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

#: How the runtime starts a project's interpreter on the compile entry, ahead of its arguments.
COMPILE_ENTRY_INVOCATION = ("-I", "-m", "tatolab.stream._project_stream_compile_entry")

#: The line `tatolabd` logs as it begins starting a loaded stream.
STREAM_START_BEGAN_LINE = re.compile(r"\[start\] Starting the stream `(?P<stream_name>[^`]+)`")

#: The line `tatolabd` logs as a stream's unload removes the processors its load added.
STREAM_UNLOAD_REMOVED_PROCESSORS_LINE = re.compile(
    r"\[stop\] Queued removal of (?P<processor_count>\d+) processor\(s\)"
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
LOAD_AND_UNLOAD_LINES_DEADLINE_SECONDS = 10.0
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


def write_a_project_whose_compile_prints(
    project_directory: Path, graph: "dict[str, Any]"
) -> None:
    """A project whose `.venv/bin/python` hands the runtime `graph` as its compiled stream.

    The compile entry's invocation prints `graph` in the compile document,
    naming this tests directory as the project its Python nodes are described
    and run from; every other invocation execs this test's interpreter, which
    holds the extension.
    """
    compile_document_file = project_directory / "compile-document.json"
    venv_interpreter = project_directory / ".venv" / "bin" / "python"
    venv_interpreter.parent.mkdir(parents=True)
    compile_document_file.write_text(
        json.dumps(
            {"stream_graph": graph, "project_directory": str(Path(__file__).resolve().parent)}
        )
        + "\n",
        encoding="utf-8",
    )
    first, second, third = (shlex.quote(argument) for argument in COMPILE_ENTRY_INVOCATION)
    venv_interpreter.write_text(
        "#!/bin/sh\n"
        f'if [ "$1" = {first} ] && [ "$2" = {second} ] && [ "$3" = {third} ]; then\n'
        f"    exec cat {shlex.quote(str(compile_document_file))}\n"
        "fi\n"
        f'exec {shlex.quote(sys.executable)} "$@"\n',
        encoding="utf-8",
    )
    venv_interpreter.chmod(0o755)


def node_names_loaded_by_tatolabd(
    stream_function: "Callable[[StreamBuilder], None]",
    working_directory: Path,
    tatolabd: TatolabdWithNoVulkanDriver,
) -> "list[str]":
    """Compile `stream_function`, load it on `tatolabd` with `tatolab run`, and name the nodes it held.

    The engine loads the compiled names unsuffixed, so they are the names it
    holds. Its start is then refused at the GPU, and its unload counts the
    processors the load added. Fails naming the refusal when the load is refused.
    """
    graph = compile_stream_to_graph(stream_function)
    stream_name = stream_function.__name__
    project_directory = working_directory / "project"
    write_a_project_whose_compile_prints(project_directory, graph)

    standard_error_mark = len(tatolabd.standard_error_lines)
    tatolab_run = subprocess.run(
        [
            str(_runtime_unit_executable(TATOLAB_EXECUTABLE)),
            "run",
            "--dir",
            str(project_directory),
            "--name",
            stream_name,
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
            f"`tatolab run` exited {tatolab_run.returncode}:\n{tatolab_run.stderr[-4000:]}\n"
            f"tatolabd:\n{tatolabd.standard_error_text_since(standard_error_mark)[-8000:]}"
        )

    # Both lines reach this process through the drain thread, which can trail
    # `tatolab run`'s exit.
    load_and_unload_lines_deadline = time.monotonic() + LOAD_AND_UNLOAD_LINES_DEADLINE_SECONDS
    while True:
        standard_error_text = tatolabd.standard_error_text_since(standard_error_mark)
        start_began = STREAM_START_BEGAN_LINE.search(standard_error_text)
        unload = STREAM_UNLOAD_REMOVED_PROCESSORS_LINE.search(standard_error_text)
        if (start_began is not None and unload is not None) or (
            time.monotonic() > load_and_unload_lines_deadline
        ):
            break
        time.sleep(0.05)

    if start_began is None:
        pytest.fail("tatolabd did not load the stream:\n" + what_the_run_and_the_runtime_said())
    assert start_began.group("stream_name") == stream_name, what_the_run_and_the_runtime_said()
    assert tatolab_run.returncode == 1 and NO_VULKAN_DRIVER_REFUSAL in tatolab_run.stderr, (
        "the loaded stream was to be refused at its start for the missing Vulkan driver; "
        + what_the_run_and_the_runtime_said()
    )
    assert unload is not None, what_the_run_and_the_runtime_said()
    assert int(unload.group("processor_count")) == len(graph["nodes"])
    return [loaded_node["name"] for loaded_node in graph["nodes"]]


@pytest.mark.parametrize(
    ("stream_function", "processor_class"),
    [(the_whip_publisher_alone, WhipPublisher), (the_whep_player_alone, WhepPlayer)],
)
def test_an_installed_extensions_processor_is_added_like_any_other(
    tmp_path, tatolabd_with_no_vulkan_driver, stream_function, processor_class
):
    assert node_names_loaded_by_tatolabd(
        stream_function, tmp_path, tatolabd_with_no_vulkan_driver
    ) == [
        processor_class.__name__.lower()
    ]


def test_the_publisher_wires_to_both_encoders_without_an_adapter(tmp_path, tatolabd_with_no_vulkan_driver):
    """One fan-in port takes both encoders, which is what makes a WHIP session
    with video and audio a matter of wiring rather than of config."""
    node_names_loaded_by_tatolabd(the_publisher_fed_by_both_encoders, tmp_path, tatolabd_with_no_vulkan_driver)


def test_the_player_wires_to_both_decoders_without_an_adapter(tmp_path, tatolabd_with_no_vulkan_driver):
    node_names_loaded_by_tatolabd(the_player_feeding_both_decoders, tmp_path, tatolabd_with_no_vulkan_driver)


def test_a_publish_and_play_round_trip_composes_as_published(tmp_path, tatolabd_with_no_vulkan_driver):
    """The shape the live proof drives: encode, publish, play back, decode."""
    node_names_loaded_by_tatolabd(a_publish_and_play_round_trip, tmp_path, tatolabd_with_no_vulkan_driver)


H264_VIDEO_ENTRY = {
    "section": "video",
    "entry": {"codec": "avc1.42e01f", "codedWidth": 320, "codedHeight": 192},
}
MONO_OPUS_AUDIO_ENTRY = {
    "section": "audio",
    "entry": {
        "codec": "opus",
        "sampleRate": 48_000,
        "numberOfChannels": 1,
        "description": MONO_OPUS_IDENTIFICATION_HEADER_HEX,
    },
}


def opus_audio_entry_of(channels: int) -> "dict[str, Any]":
    return {
        "section": "audio",
        "entry": {**MONO_OPUS_AUDIO_ENTRY["entry"], "numberOfChannels": channels},
    }


@pytest.mark.parametrize("codec", ["avc1.42e01f", "avc1.640028", "avc3.64001f"])
def test_an_h264_video_entry_names_a_video_track(codec):
    entry = {"section": "video", "entry": {"codec": codec}}

    assert published_track_from_catalog_entry(
        entry, "encoder/encoded_video"
    ) == PublishedTrack(kind="video", channels=None)


def test_an_opus_audio_entry_names_an_audio_track_with_its_channel_count():
    assert published_track_from_catalog_entry(
        opus_audio_entry_of(2), "encoder/encoded_audio"
    ) == PublishedTrack(kind="audio", channels=2)


@pytest.mark.parametrize(
    "catalog_entry",
    [
        {"section": "video", "entry": {"codec": "hvc1.1.6.L93.B0"}},
        {"section": "video", "entry": {"codec": "av01.0.08M.08"}},
        {"section": "audio", "entry": {"codec": "mp4a.40.2"}},
        {"section": "audio", "entry": {"codec": "avc1.42e01f"}},
        {"section": "video", "entry": {"codec": "opus"}},
        {"section": "binary", "entry": {"mode": "snapshot"}},
    ],
    ids=["h265", "av1", "aac", "h264_under_audio", "opus_under_video", "a_data_track"],
)
def test_an_entry_naming_media_this_session_does_not_carry_is_refused_naming_it(
    catalog_entry,
):
    """H.265 included: the offer registers H.264 and Opus, so a stream this
    session never negotiated is a refusal and not a silent wrong track."""
    with pytest.raises(ValueError, match="encoder/output.*does not carry"):
        published_track_from_catalog_entry(catalog_entry, "encoder/output")


def test_a_link_whose_upstream_port_states_no_entry_is_refused_naming_it():
    with pytest.raises(ValueError, match="`numpy/frames` carries no catalog entry"):
        published_track_from_catalog_entry(None, "numpy/frames")


def test_two_links_publishing_one_medium_are_refused_naming_both():
    already: "dict[str, VideoOrAudio]" = {"first/encoded_video": "video"}

    with pytest.raises(ValueError, match="second/encoded_video.*first/encoded_video"):
        resolve_track_kind("video", "second/encoded_video", already)


def test_a_link_that_changes_medium_is_refused_naming_it():
    already: "dict[str, VideoOrAudio]" = {"encoder/tracks": "video"}

    with pytest.raises(ValueError, match="does not change medium"):
        resolve_track_kind("audio", "encoder/tracks", already)


def test_a_link_publishing_the_medium_it_already_claimed_is_unchanged():
    already: "dict[str, VideoOrAudio]" = {"encoder/encoded_video": "video"}

    assert resolve_track_kind("video", "encoder/encoded_video", already) == "video"


def test_an_oversized_media_frame_is_reported_once_rather_than_silently_dropped(
    monkeypatch,
):
    """The engine drops a message past the link ceiling at `debug` rather than
    raising, which downstream looks like a stream that simply stopped. The
    player is the only place that can say what actually happened — and a
    per-frame condition reported per frame is noise, so it says it once."""
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)
    player = WhepPlayer(WhepPlayerConfig(url="https://example.invalid/whep"))
    over_the_ceiling = b"\x00" * (HELPER_LINK_PAYLOAD_CEILING_BYTES + 1)

    player._report_a_media_frame_the_link_will_drop("encoded_video", over_the_ceiling)
    player._report_a_media_frame_the_link_will_drop("encoded_video", over_the_ceiling)

    assert len(reported) == 1
    assert "encoded_video" in reported[0]
    assert str(len(over_the_ceiling)) in reported[0]


def test_a_media_frame_inside_the_ceiling_is_not_reported(monkeypatch):
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)

    WhepPlayer(
        WhepPlayerConfig(
            url="https://example.invalid/whep"
        )
    )._report_a_media_frame_the_link_will_drop(
        "encoded_video", b"\x00" * 4096
    )

    assert reported == []


@pytest.mark.parametrize("channels", [3, 6, 8])
def test_multichannel_opus_is_refused_because_rtp_cannot_carry_it(channels):
    """`OpusEncoder` legally produces mapping-family-1 multistream packets for
    3-8 channels, and RFC 7587 defines an RTP payload format for mono and
    stereo only. Forwarded anyway, the far end decodes garbage and nothing
    upstream says why."""
    with pytest.raises(ValueError, match="mono and stereo only"):
        refuse_audio_rtp_cannot_carry(channels, "encoder/encoded_audio")


@pytest.mark.parametrize("channels", [1, 2])
def test_the_channel_counts_rtp_carries_are_published(channels):
    refuse_audio_rtp_cannot_carry(channels, "encoder/encoded_audio")


class PublisherWiredToEncoders:
    """A set-up `WhipPublisher` whose `tracks` input one link per encoder port feeds.

    A helper-process context opened directly on wired links is the same seam
    `setup()` and `process()` see in a real child, and it needs no runtime and
    no device.
    """

    def __init__(
        self,
        request: pytest.FixtureRequest,
        encoder_output_ports: "list[str]",
        config: "dict[str, Any]",
    ) -> None:
        unique = f"whipwired{os.getpid()}_{request.node.name}"
        self.parent = ParentHoldingCatalogEntries()
        destination_notify_service_name = f"{unique}_dest/notify"
        tracks_input = _engine.open_node_link_data_access_for_helper_process()
        self.encoder_outputs = _engine.open_node_link_data_access_for_helper_process()
        self.inbound_link_by_encoder_output_port: "dict[str, str]" = {}
        for encoder_output_port in encoder_output_ports:
            channel_service_name = f"{unique}/{encoder_output_port}"
            wire_one_link(
                self.encoder_outputs,
                encoder_output_port,
                tracks_input,
                TRACKS_INPUT_PORT,
                channel_service_name,
                destination_notify_service_name,
                notify_max_notifiers=len(encoder_output_ports),
            )
            self.inbound_link_by_encoder_output_port[encoder_output_port] = (
                channel_service_name
            )
        self.context = _engine.open_runtime_context_full_access_for_helper_process(
            {},
            tracks_input,
            "runtime-under-test",
            "processor-under-test",
            self.parent.escalate_request_to_parent,
        )
        # Constructed the way the helper constructs it: the mapping becomes
        # the class's config object, not something read off the context.
        self.publisher = construct_processor_instance(WhipPublisher, config, tracks_input)
        self.publisher.setup(self.context)

    def state_catalog_entry(
        self, encoder_output_port: str, catalog_entry: "dict[str, Any]"
    ) -> None:
        self.parent.catalog_entry_by_inbound_link[
            self.inbound_link_by_encoder_output_port[encoder_output_port]
        ] = catalog_entry

    def publish(
        self,
        encoder_output_port: str,
        payload: bytes,
        timestamp_ns: int = 0,
        starts_group: bool = True,
    ) -> None:
        self.encoder_outputs.write_to_output_port(
            encoder_output_port, payload, timestamp_ns, starts_group
        )
        self.publisher.process(
            _engine.limited_access_view_of_runtime_context_for_helper_process(
                self.context
            )
        )


def numbered_encoder_output_ports(count: int) -> "list[str]":
    return [f"encoder{index}_output" for index in range(count)]


AN_ENDPOINT = {"url": "https://example.invalid/whip"}


@pytest.mark.parametrize("inbound_links", [1, 2])
def test_a_publisher_sets_up_against_the_links_a_session_can_carry(
    request, inbound_links
):
    PublisherWiredToEncoders(request, numbered_encoder_output_ports(inbound_links), AN_ENDPOINT)


def test_a_publisher_with_nothing_wired_refuses_rather_than_publishing_silence(
    request,
):
    """A graph that forgot the connect would otherwise open a session and send
    nothing, which looks from the relay's side like a working publisher."""
    with pytest.raises(ValueError, match="nothing is connected"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(0), AN_ENDPOINT)


def test_more_links_than_a_session_can_carry_are_refused_naming_the_count(request):
    with pytest.raises(ValueError, match="3 links feed"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(3), AN_ENDPOINT)


@pytest.mark.parametrize("config", [{"url": ""}, {"url": 7}])
def test_a_publisher_whose_endpoint_is_not_an_address_is_refused_by_name(
    request, config
):
    with pytest.raises(ValueError, match="`url` is required"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(1), config)


def test_a_publisher_added_with_no_endpoint_at_all_is_refused_by_its_config_class(request):
    """`url` has no default, so the config class refuses the empty mapping and
    names the missing key before any of this wheel's own validation runs."""
    with pytest.raises(TypeError, match="missing 1 required positional argument"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(1), {})


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_config_reaches_a_processor_as_its_own_config_object(processor_class):
    """The shape `stream_builder.add(cls, config={...})` actually delivers.

    The graph records the config and the helper constructs the class's config
    class from it, so a class whose settings are not on that config class
    passes every graph-building test and then fails in the child on the first
    run. This is the engine's own mapping, called directly.
    """
    constructed = construct_processor_instance(
        processor_class,
        {"url": "https://example.invalid/x", "bearer_token": "a-token"},
        None,
    )

    assert isinstance(constructed, processor_class)


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_the_bearer_token_is_optional(processor_class):
    assert isinstance(
        construct_processor_instance(
            processor_class, {"url": "https://example.invalid/x"}, None
        ),
        processor_class,
    )


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_a_processor_added_without_an_endpoint_is_refused_at_construction(
    processor_class,
):
    with pytest.raises((TypeError, ValueError)):
        construct_processor_instance(processor_class, {}, None)


class RecordingWhipSession:
    """A `_native.WhipSession` stand-in keeping what the publisher offered and sent."""

    constructed: "list[RecordingWhipSession]" = []

    def __init__(self, endpoint_url: str, bearer_token: "str | None" = None) -> None:
        del endpoint_url, bearer_token
        self.is_connected = False
        self.offered: "tuple[bool, bool, int | None] | None" = None
        self.video_media_frames: "list[tuple[bytes, int]]" = []
        self.audio_media_frames: "list[bytes]" = []
        RecordingWhipSession.constructed.append(self)

    def connect(
        self, *, video: bool, audio: bool, audio_channels: "int | None" = None
    ) -> None:
        self.offered = (video, audio, audio_channels)
        self.is_connected = True

    def write_video_media_frame(self, media_frame: bytes, timestamp_ns: int) -> None:
        self.video_media_frames.append((media_frame, timestamp_ns))

    def write_audio_media_frame(self, media_frame: bytes) -> None:
        self.audio_media_frames.append(media_frame)

    def close(self) -> None:
        self.is_connected = False


def route_every_log_level_to(
    monkeypatch: pytest.MonkeyPatch, log_record_sink: "Callable[[str], None]"
) -> None:
    """`log` reaches a sink only in the interpreter the runtime starts for a node."""
    for log_level in ("debug", "info", "warn", "error"):
        monkeypatch.setattr(log, log_level, log_record_sink)


def discard_log_record(message: str) -> None:
    del message


@pytest.fixture
def recording_whip_session(monkeypatch) -> "type[RecordingWhipSession]":
    RecordingWhipSession.constructed = []
    monkeypatch.setattr(_native, "WhipSession", RecordingWhipSession)
    route_every_log_level_to(monkeypatch, discard_log_record)
    return RecordingWhipSession


def test_a_media_frame_reaches_the_session_whole_with_its_envelope_stamp(
    request, recording_whip_session
):
    """The varint stays on: the session's Rust strips it, and the envelope's
    stamp — the one a local reader is promised — places it on the RTP clock."""
    wired = PublisherWiredToEncoders(request, ["encoded_video"], AN_ENDPOINT)
    wired.state_catalog_entry("encoded_video", H264_VIDEO_ENTRY)

    wired.publish("encoded_video", MEDIA_FRAME_OF_AN_ACCESS_UNIT, timestamp_ns=1_234)

    (session,) = recording_whip_session.constructed
    assert session.offered == (True, False, None)
    assert session.video_media_frames == [(MEDIA_FRAME_OF_AN_ACCESS_UNIT, 1_234)]


def test_an_opus_media_frame_offers_the_channel_count_its_entry_names(
    request, recording_whip_session
):
    wired = PublisherWiredToEncoders(request, ["encoded_audio"], AN_ENDPOINT)
    wired.state_catalog_entry("encoded_audio", opus_audio_entry_of(2))

    wired.publish("encoded_audio", MEDIA_FRAME_OF_AN_OPUS_PACKET)

    (session,) = recording_whip_session.constructed
    assert session.offered == (False, True, 2)
    assert session.audio_media_frames == [MEDIA_FRAME_OF_AN_OPUS_PACKET]


def test_two_links_are_one_video_and_one_audio_track_by_their_entries(
    request, recording_whip_session
):
    wired = PublisherWiredToEncoders(
        request, ["encoded_video", "encoded_audio"], AN_ENDPOINT
    )
    wired.state_catalog_entry("encoded_video", H264_VIDEO_ENTRY)
    wired.state_catalog_entry("encoded_audio", MONO_OPUS_AUDIO_ENTRY)

    wired.publish("encoded_audio", MEDIA_FRAME_OF_AN_OPUS_PACKET)
    wired.publish("encoded_video", MEDIA_FRAME_OF_AN_ACCESS_UNIT, timestamp_ns=5)

    (session,) = recording_whip_session.constructed
    assert session.offered == (True, True, 1)
    assert session.audio_media_frames == [MEDIA_FRAME_OF_AN_OPUS_PACKET]
    assert session.video_media_frames == [(MEDIA_FRAME_OF_AN_ACCESS_UNIT, 5)]


def test_a_link_whose_upstream_states_no_entry_is_refused_before_any_session_opens(
    request, recording_whip_session
):
    """Any port can be wired into `tracks`, and the refusal is the only place
    that wiring mistake is caught."""
    wired = PublisherWiredToEncoders(request, ["frames"], AN_ENDPOINT)

    with pytest.raises(ValueError, match="carries no catalog entry") as refused:
        wired.publish("frames", MEDIA_FRAME_OF_AN_ACCESS_UNIT)

    inbound_link = wired.inbound_link_by_encoder_output_port["frames"]
    assert f"`{inbound_link}`" in str(refused.value)
    assert recording_whip_session.constructed[0].offered is None


def test_a_multichannel_entry_is_refused_before_any_session_is_opened(request):
    """The ordering, not just the refusal.

    Refusing after the connect would leave a live session at the relay that
    then publishes nothing — the phantom publisher the refusal exists to
    prevent. The endpoint here is unresolvable, so a connect attempted first
    would raise about signalling instead of about channels.
    """
    wired = PublisherWiredToEncoders(
        request, ["encoded_audio"], {"url": "https://unresolvable.invalid/whip"}
    )
    wired.state_catalog_entry("encoded_audio", opus_audio_entry_of(6))

    with pytest.raises(ValueError, match="mono and stereo only"):
        wired.publish("encoded_audio", MEDIA_FRAME_OF_AN_OPUS_PACKET)


def test_a_video_link_is_entered_at_its_first_group_start(
    request, recording_whip_session
):
    """A delta frame before the first keyframe references pictures the far end
    never had."""
    wired = PublisherWiredToEncoders(request, ["encoded_video"], AN_ENDPOINT)
    wired.state_catalog_entry("encoded_video", H264_VIDEO_ENTRY)

    wired.publish("encoded_video", b"\x01delta", timestamp_ns=1, starts_group=False)
    wired.publish("encoded_video", b"\x02keyframe", timestamp_ns=2, starts_group=True)
    wired.publish("encoded_video", b"\x03delta", timestamp_ns=3, starts_group=False)

    (session,) = recording_whip_session.constructed
    assert session.video_media_frames == [(b"\x02keyframe", 2), (b"\x03delta", 3)]


class TracksInputReplayingMessages:
    """`ctx.inputs` for a publisher, handing back scripted messages and one entry."""

    def __init__(
        self,
        inbound_links: "list[str]",
        catalog_entry: "dict[str, Any]",
        messages: "list[Message[bytes]]",
    ) -> None:
        self.inbound_links = inbound_links
        self.catalog_entry = catalog_entry
        self.messages_still_to_hand_back = list(messages)

    def inbound_link_names(self, port_name: str) -> "list[str]":
        del port_name
        return self.inbound_links

    def read_message(self, port_name: str) -> "Message[bytes] | None":
        del port_name
        if self.messages_still_to_hand_back:
            return self.messages_still_to_hand_back.pop(0)
        return None

    def inbound_link_catalog_entry(
        self, port_name: str, *, inbound_link: "str | None" = None
    ) -> "dict[str, Any]":
        del port_name, inbound_link
        return self.catalog_entry


class ContextWithTracksInput:
    def __init__(self, inputs: TracksInputReplayingMessages) -> None:
        self.inputs = inputs


def video_message(
    payload: bytes, *, starts_group: bool, lost_since_last_read: int = 0
) -> "Message[bytes]":
    return Message(
        payload=payload,
        timestamp_ns=0,
        starts_group=starts_group,
        lost_since_last_read=lost_since_last_read,
        inbound_link="encoder/encoded_video",
    )


def test_a_reported_loss_discards_to_the_next_group_start(recording_whip_session):
    """After a loss the next delta frame references a picture the far end never
    received; the group start reporting a loss is itself sent."""
    inputs = TracksInputReplayingMessages(
        ["encoder/encoded_video"],
        H264_VIDEO_ENTRY,
        [
            video_message(b"keyframe", starts_group=True),
            video_message(b"delta after a loss", starts_group=False, lost_since_last_read=2),
            video_message(b"delta in the broken group", starts_group=False),
            video_message(b"next keyframe", starts_group=True),
            video_message(b"keyframe reporting a loss", starts_group=True, lost_since_last_read=1),
            video_message(b"delta after it", starts_group=False),
        ],
    )
    context = cast(Any, ContextWithTracksInput(inputs))
    publisher = construct_processor_instance(WhipPublisher, AN_ENDPOINT, None)
    publisher.setup(context)

    for _ in range(6):
        publisher.process(context)

    (session,) = recording_whip_session.constructed
    assert [media_frame for media_frame, _ in session.video_media_frames] == [
        b"keyframe",
        b"next keyframe",
        b"keyframe reporting a loss",
        b"delta after it",
    ]


class RecordingWhepSession:
    """A `_native.WhepSession` stand-in whose connect outcome is scripted.

    The reconnect loop is the whole reason `WhepPlayer` survives an endpoint
    that is not yet live, and nothing about it needs a network, a device or the
    compiled module: the player resolves `_native.WhepSession` at call time, so
    a fake substituted on the module is what each attempt constructs.
    """

    #: Every session this class has minted, so a test can assert one per attempt.
    constructed: "list[RecordingWhepSession]" = []
    #: What each successive `connect()` does, popped in order: `None` succeeds,
    #: an `Exception` is raised.
    connect_outcomes: "list[Exception | None]" = []

    def __init__(self, url: str, bearer_token: "str | None" = None) -> None:
        del url, bearer_token
        self.connect_calls = 0
        self.closed = False
        RecordingWhepSession.constructed.append(self)

    def connect(self) -> None:
        self.connect_calls += 1
        outcome = RecordingWhepSession.connect_outcomes.pop(0)
        if isinstance(outcome, Exception):
            raise outcome

    def next_media(self, timeout_ms: int) -> None:
        del timeout_ms
        return None

    def close(self) -> None:
        self.closed = True


@pytest.fixture
def scripted_whep_session(monkeypatch):
    """Point `WhepPlayer` at the fake, and reset its class-level script.

    The player logs as it connects and backs off, and `log` reaches a sink only
    in the interpreter the runtime starts for a node, so each level is kept in a
    list here instead.
    """
    RecordingWhepSession.constructed = []
    RecordingWhepSession.connect_outcomes = []
    monkeypatch.setattr(_native, "WhepSession", RecordingWhepSession)
    kept_log_records: "list[str]" = []
    route_every_log_level_to(monkeypatch, kept_log_records.append)
    return RecordingWhepSession


def _player_for_a_drain_that_returns_immediately() -> WhepPlayer:
    return WhepPlayer(WhepPlayerConfig(url="https://example.invalid/whep"))


def test_a_refused_connect_is_retried_rather_than_ending_the_stream(
    scripted_whep_session, monkeypatch
):
    """The 409 a WHEP endpoint answers before its ingest is live is not the end.

    A player that gave up on the first refusal would stay alive and produce
    nothing, and nothing downstream could tell that from an endpoint carrying
    no media.
    """
    scripted_whep_session.connect_outcomes = [
        RuntimeError("409 Conflict: Live broadcast not started yet"),
        RuntimeError("409 Conflict: Live broadcast not started yet"),
        None,
    ]
    player = _player_for_a_drain_that_returns_immediately()

    waited: "list[float]" = []

    def stop_after_the_third_attempt(delay_seconds: float) -> None:
        waited.append(delay_seconds)

    monkeypatch.setattr(player._stop, "wait", stop_after_the_third_attempt)
    # The third connect succeeds, so the drain runs; end it there.
    monkeypatch.setattr(player, "_drain_until_stopped", lambda session, outputs: None)
    monkeypatch.setattr(
        player._stop, "is_set", lambda: len(scripted_whep_session.constructed) >= 3
    )

    player._play_until_stopped(outputs=None)

    assert len(scripted_whep_session.constructed) == 3, (
        "each attempt constructs its own session — a closed peer connection "
        "cannot be dialled again"
    )
    assert all(session.closed for session in scripted_whep_session.constructed)
    assert waited == [
        FIRST_RECONNECT_DELAY_SECONDS,
        FIRST_RECONNECT_DELAY_SECONDS * 2,
    ], "the backoff doubles between attempts"


def test_the_backoff_stops_doubling_at_its_ceiling(scripted_whep_session, monkeypatch):
    """An endpoint that is down for hours must not back off for hours."""
    refusals = 12
    scripted_whep_session.connect_outcomes = [
        RuntimeError("still down") for _ in range(refusals)
    ]
    player = _player_for_a_drain_that_returns_immediately()

    waited: "list[float]" = []
    monkeypatch.setattr(player._stop, "wait", waited.append)
    monkeypatch.setattr(
        player._stop, "is_set", lambda: len(waited) >= refusals - 1
    )

    player._play_until_stopped(outputs=None)

    ceiling = LONGEST_RECONNECT_DELAY_SECONDS
    assert max(waited) == ceiling
    assert waited[-1] == ceiling


def test_a_stop_during_the_backoff_ends_the_thread_without_reconnecting(
    scripted_whep_session, monkeypatch
):
    """`stop()` must return well inside the helper's five-second budget."""
    scripted_whep_session.connect_outcomes = [RuntimeError("down")]
    player = _player_for_a_drain_that_returns_immediately()

    def stop_arrives_mid_backoff(delay_seconds: float) -> None:
        del delay_seconds
        player._stop.set()

    monkeypatch.setattr(player._stop, "wait", stop_arrives_mid_backoff)

    player._play_until_stopped(outputs=None)

    assert len(scripted_whep_session.constructed) == 1, (
        "a stop arriving during the backoff opens no further session"
    )


def test_a_refused_write_names_its_port_and_stops_rather_than_reconnecting(
    scripted_whep_session, monkeypatch
):
    """A wire-contract violation is this player's fault, not the endpoint's.

    Retrying it would spend an endpoint's session forever on a write that will be
    refused every time, under a message naming neither the port nor the cause.
    """
    scripted_whep_session.connect_outcomes = [None]
    player = _player_for_a_drain_that_returns_immediately()

    def refuse_the_write(session, outputs):
        del session, outputs
        raise LinkOutputWriteFailure("encoded_video")

    monkeypatch.setattr(player, "_drain_until_stopped", refuse_the_write)

    errors: "list[str]" = []
    monkeypatch.setattr(log, "error", errors.append)
    monkeypatch.setattr(
        player._stop, "wait", lambda _: pytest.fail("a refused write must not be retried")
    )

    player._play_until_stopped(outputs=None)

    assert len(scripted_whep_session.constructed) == 1
    assert scripted_whep_session.constructed[0].closed
    assert any("encoded_video" in message for message in errors), (
        "the port that refused the write has to reach the log"
    )


class WhepSessionReplayingMedia:
    """A `_native.WhepSession` stand-in that hands back its media, then stops the player."""

    def __init__(
        self,
        player: WhepPlayer,
        media: "list[AccessUnitTheSessionPlayed | OpusPacketTheSessionPlayed]",
    ) -> None:
        self.player = player
        self.media_still_to_hand_back = list(media)

    def next_media(
        self, timeout_ms: int
    ) -> "AccessUnitTheSessionPlayed | OpusPacketTheSessionPlayed | None":
        del timeout_ms
        if self.media_still_to_hand_back:
            return self.media_still_to_hand_back.pop(0)
        self.player._stop.set()
        return None


class PlayerWiredToBothDecoders:
    """A `WhepPlayer`'s two output ports, each wired to a decoder's input read here."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        unique = f"whepwired{os.getpid()}_{request.node.name}"
        self.parent = ParentHoldingCatalogEntries()
        destination_notify_service_name = f"{unique}_dest/notify"
        decoder_inputs = _engine.open_node_link_data_access_for_helper_process()
        player_outputs = _engine.open_node_link_data_access_for_helper_process()
        output_ports = (ENCODED_VIDEO_OUTPUT_PORT, ENCODED_AUDIO_OUTPUT_PORT)
        for output_port in output_ports:
            wire_one_link(
                player_outputs,
                output_port,
                decoder_inputs,
                output_port,
                f"{unique}/{output_port}",
                destination_notify_service_name,
                notify_max_notifiers=len(output_ports),
            )
        self.decoders = _engine.open_runtime_context_full_access_for_helper_process(
            {}, decoder_inputs, "runtime-under-test", "decoders-under-test"
        )
        self.player_context = _engine.open_runtime_context_full_access_for_helper_process(
            {},
            player_outputs,
            "runtime-under-test",
            "player-under-test",
            self.parent.escalate_request_to_parent,
        )

    def play(
        self, media: "list[AccessUnitTheSessionPlayed | OpusPacketTheSessionPlayed]"
    ) -> None:
        player = WhepPlayer(WhepPlayerConfig(url="https://example.invalid/whep"))
        player._drain_until_stopped(
            cast(Any, WhepSessionReplayingMedia(player, media)),
            self.player_context.outputs,
        )


@pytest.fixture
def player_wired_to_both_decoders(request, monkeypatch) -> PlayerWiredToBothDecoders:
    monkeypatch.setattr(_native, "PlayedOpusPacket", OpusPacketTheSessionPlayed)
    route_every_log_level_to(monkeypatch, discard_log_record)
    return PlayerWiredToBothDecoders(request)


def test_only_a_sync_point_opens_a_group_on_the_video_port(
    player_wired_to_both_decoders: PlayerWiredToBothDecoders,
):
    """A decoder enters a stream only where a group starts, so a delta frame
    that claimed one would be entered with no reference picture."""
    player_wired_to_both_decoders.play(
        [
            AccessUnitTheSessionPlayed(is_sync_point=True, timestamp_ns=1_000),
            AccessUnitTheSessionPlayed(is_sync_point=False, timestamp_ns=2_000),
        ]
    )
    decoder = player_wired_to_both_decoders.decoders.inputs

    keyframe = decoder.read_message(ENCODED_VIDEO_OUTPUT_PORT)
    delta = decoder.read_message(ENCODED_VIDEO_OUTPUT_PORT)

    assert keyframe is not None and delta is not None
    assert (keyframe.starts_group, keyframe.timestamp_ns) == (True, 1_000)
    assert (delta.starts_group, delta.timestamp_ns) == (False, 2_000)
    assert delta.payload == MEDIA_FRAME_OF_AN_ACCESS_UNIT


def test_every_opus_packet_opens_a_group_on_the_audio_port(
    player_wired_to_both_decoders: PlayerWiredToBothDecoders,
):
    player_wired_to_both_decoders.play(
        [
            OpusPacketTheSessionPlayed(timestamp_ns=1_000),
            OpusPacketTheSessionPlayed(timestamp_ns=1_020),
        ]
    )
    decoder = player_wired_to_both_decoders.decoders.inputs

    first = decoder.read_message(ENCODED_AUDIO_OUTPUT_PORT)
    second = decoder.read_message(ENCODED_AUDIO_OUTPUT_PORT)

    assert first is not None and second is not None
    assert (first.starts_group, first.timestamp_ns) == (True, 1_000)
    assert (second.starts_group, second.timestamp_ns) == (True, 1_020)
    assert first.payload == MEDIA_FRAME_OF_AN_OPUS_PACKET
    assert decoder.read_message(ENCODED_VIDEO_OUTPUT_PORT) is None


def test_each_port_states_its_entry_in_hangs_own_field_names(
    player_wired_to_both_decoders: PlayerWiredToBothDecoders,
):
    """What reaches the parent is spelled in hang's catalog field names — the
    codec string and coded extent for video, and the codec, rate, channel count
    and `OpusHead` for audio — which the parent parses with hang's types."""
    player_wired_to_both_decoders.play(
        [AccessUnitTheSessionPlayed(), OpusPacketTheSessionPlayed()]
    )

    assert player_wired_to_both_decoders.parent.catalog_entries_stated == [
        (
            ENCODED_VIDEO_OUTPUT_PORT,
            "video",
            {"codec": "avc1.42e01f", "codedWidth": 320, "codedHeight": 192},
        ),
        (
            ENCODED_AUDIO_OUTPUT_PORT,
            "audio",
            {
                "codec": "opus",
                "sampleRate": 48_000,
                "numberOfChannels": 1,
                "description": MONO_OPUS_IDENTIFICATION_HEADER_HEX,
            },
        ),
    ]


class OutputsRecordingWritesAndEntries:
    """`ctx.outputs` for a player, keeping its writes and entries in one order."""

    def __init__(self, refuse_entries: bool = False) -> None:
        self.refuse_entries = refuse_entries
        self.recorded: "list[tuple[str, str, Any]]" = []

    def write(
        self,
        port_name: str,
        payload: bytes,
        timestamp_ns: "int | None" = None,
        starts_group: bool = True,
    ) -> None:
        del timestamp_ns, starts_group
        self.recorded.append(("write", port_name, payload))

    def set_catalog_entry(self, port_name: str, entry: CatalogEntry) -> None:
        if self.refuse_entries:
            raise ValueError(f"output port {port_name!r}: the entry does not parse")
        self.recorded.append(("entry", port_name, entry))


def play_into(
    outputs: OutputsRecordingWritesAndEntries,
    media: "list[AccessUnitTheSessionPlayed | OpusPacketTheSessionPlayed]",
) -> None:
    player = WhepPlayer(WhepPlayerConfig(url="https://example.invalid/whep"))
    player._drain_until_stopped(
        cast(Any, WhepSessionReplayingMedia(player, media)), cast(Any, outputs)
    )


def test_an_entry_is_stated_before_the_first_media_frame_and_again_only_on_change(
    monkeypatch,
):
    """A decoder reads its upstream entry when the first frame arrives, so the
    entry lands first; a stream that changes extent states the new one before
    the frame it describes."""
    monkeypatch.setattr(_native, "PlayedOpusPacket", OpusPacketTheSessionPlayed)
    route_every_log_level_to(monkeypatch, discard_log_record)
    outputs = OutputsRecordingWritesAndEntries()
    wider = AccessUnitTheSessionPlayed(
        codec="avc1.640028", coded_width=1920, coded_height=1088
    )

    play_into(
        outputs,
        [
            AccessUnitTheSessionPlayed(),
            AccessUnitTheSessionPlayed(is_sync_point=False),
            wider,
            OpusPacketTheSessionPlayed(),
            OpusPacketTheSessionPlayed(),
        ],
    )

    assert outputs.recorded == [
        (
            "entry",
            ENCODED_VIDEO_OUTPUT_PORT,
            VideoEntry(codec="avc1.42e01f", codedWidth=320, codedHeight=192),
        ),
        ("write", ENCODED_VIDEO_OUTPUT_PORT, MEDIA_FRAME_OF_AN_ACCESS_UNIT),
        ("write", ENCODED_VIDEO_OUTPUT_PORT, MEDIA_FRAME_OF_AN_ACCESS_UNIT),
        (
            "entry",
            ENCODED_VIDEO_OUTPUT_PORT,
            VideoEntry(codec="avc1.640028", codedWidth=1920, codedHeight=1088),
        ),
        ("write", ENCODED_VIDEO_OUTPUT_PORT, MEDIA_FRAME_OF_AN_ACCESS_UNIT),
        (
            "entry",
            ENCODED_AUDIO_OUTPUT_PORT,
            AudioEntry(
                codec="opus",
                sampleRate=48_000,
                numberOfChannels=1,
                description=MONO_OPUS_IDENTIFICATION_HEADER_HEX,
            ),
        ),
        ("write", ENCODED_AUDIO_OUTPUT_PORT, MEDIA_FRAME_OF_AN_OPUS_PACKET),
        ("write", ENCODED_AUDIO_OUTPUT_PORT, MEDIA_FRAME_OF_AN_OPUS_PACKET),
    ]


def test_a_refused_entry_names_its_port_and_writes_nothing(monkeypatch):
    """Refused, the entry is the player's fault and not the endpoint's — the
    same failure a refused write is, which ends the thread rather than
    reconnecting."""
    route_every_log_level_to(monkeypatch, discard_log_record)
    outputs = OutputsRecordingWritesAndEntries(refuse_entries=True)

    with pytest.raises(LinkOutputWriteFailure) as refused:
        play_into(outputs, [AccessUnitTheSessionPlayed()])

    assert refused.value.port == ENCODED_VIDEO_OUTPUT_PORT
    assert outputs.recorded == []

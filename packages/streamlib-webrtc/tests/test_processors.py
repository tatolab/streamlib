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
import subprocess
import sys
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import pytest

from tatolab.webrtc import WhepPlayer, WhepPlayerConfig, WhipPublisher
from tatolab.webrtc.processors import (
    FIRST_RECONNECT_DELAY_SECONDS,
    HELPER_LINK_PAYLOAD_CEILING_BYTES,
    LONGEST_RECONNECT_DELAY_SECONDS,
    LinkOutputWriteFailure,
    VideoOrAudio,
    _native,
    refuse_audio_rtp_cannot_carry,
    resolve_track_kind,
)
from tatolab.runtime import _engine
from tatolab.runtime._processor_hosting import construct_processor_instance
from tatolab.stream import (
    H264Decoder,
    H264Encoder,
    OpusDecoder,
    OpusEncoder,
    StreamBuilder,
    compile_stream_to_graph,
    log,
    stream,
)

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


def _a_runtime_answers_at_the_local_api_socket() -> bool:
    try:
        streams_probe = subprocess.run(
            [str(_runtime_unit_executable(TATOLAB_EXECUTABLE)), "streams"],
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

    def __init__(self, process: "subprocess.Popen[str]") -> None:
        self.process = process
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
def tatolabd_with_no_vulkan_driver(
    tmp_path_factory: pytest.TempPathFactory,
) -> "Iterator[TatolabdWithNoVulkanDriver]":
    """The machine's runtime for this module, started here and stopped after it.

    It takes the machine's runtime lock and serves this user's local API socket,
    so it is refused while another runtime holds the machine. Its state
    directory is its own — `XDG_STATE_HOME`, and `HOME` on macOS — so it loads
    none of this user's kept streams.
    """
    tatolabd_executable = _runtime_unit_executable(TATOLABD_EXECUTABLE)
    if _a_runtime_answers_at_the_local_api_socket():
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
    environment["XDG_STATE_HOME"] = str(state_home)
    if sys.platform == "darwin":
        environment["HOME"] = str(state_home)
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
        )
    )
    try:
        serving_deadline = time.monotonic() + SERVING_DEADLINE_SECONDS
        while not _a_runtime_answers_at_the_local_api_socket():
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


def test_each_codec_names_the_track_it_belongs_on():
    assert resolve_track_kind("h264", "encoder/encoded_video", {}) == "video"
    assert resolve_track_kind("opus", "encoder/encoded_audio", {}) == "audio"


@pytest.mark.parametrize("codec", ["h265", "av1", "", None, 5])
def test_a_codec_this_session_does_not_carry_is_refused_naming_it(codec):
    """H.265 included: the offer registers H.264 and Opus, so a stream this
    session never negotiated is a refusal and not a silent wrong track."""
    with pytest.raises(ValueError, match="does not carry"):
        resolve_track_kind(codec, "encoder/encoded_video", {})


def test_two_links_publishing_one_medium_are_refused_naming_both():
    already: "dict[str, VideoOrAudio]" = {"first/encoded_video": "video"}

    with pytest.raises(ValueError, match="second/encoded_video.*first/encoded_video"):
        resolve_track_kind("h264", "second/encoded_video", already)


def test_a_link_that_changes_medium_is_refused_naming_it():
    already: "dict[str, VideoOrAudio]" = {"encoder/tracks": "video"}

    with pytest.raises(ValueError, match="does not change medium"):
        resolve_track_kind("opus", "encoder/tracks", already)


def test_a_link_publishing_the_medium_it_already_claimed_is_unchanged():
    already: "dict[str, VideoOrAudio]" = {"encoder/encoded_video": "video"}

    assert resolve_track_kind("h264", "encoder/encoded_video", already) == "video"


def test_an_oversized_bag_is_reported_once_rather_than_silently_dropped(
    monkeypatch,
):
    """The engine drops a bag past the link ceiling at `debug` rather than
    raising, which downstream looks like a stream that simply stopped. The
    player is the only place that can say what actually happened — and a
    per-frame condition reported per frame is noise, so it says it once."""
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)
    player = WhepPlayer(WhepPlayerConfig(url="https://example.invalid/whep"))
    over_the_ceiling = b"\x00" * (HELPER_LINK_PAYLOAD_CEILING_BYTES + 1)

    player._report_a_bag_the_link_will_drop("encoded_video", over_the_ceiling)
    player._report_a_bag_the_link_will_drop("encoded_video", over_the_ceiling)

    assert len(reported) == 1
    assert "encoded_video" in reported[0]
    assert str(len(over_the_ceiling)) in reported[0]


def test_a_bag_inside_the_ceiling_is_not_reported(monkeypatch):
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)

    WhepPlayer(
        WhepPlayerConfig(
            url="https://example.invalid/whep"
        )
    )._report_a_bag_the_link_will_drop(
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


class _PublisherUnderTest:
    """A `WhipPublisher` whose `setup()` can be driven without a graph.

    A helper-process context opened directly on wired links is the same seam
    `setup()` sees in a real child, and it needs no runtime and no device.
    """

    @staticmethod
    def set_up_with(
        request: pytest.FixtureRequest,
        inbound_links: int,
        config: "dict[str, Any]",
    ) -> None:
        unique = f"whipsetup{os.getpid()}_{request.node.name}"
        link_data_access = _engine.open_node_link_data_access_for_helper_process()
        for index in range(inbound_links):
            link_data_access.wire_input_link(
                "tracks", f"{unique}/encoder{index}", f"{unique}/encoder{index}",
                f"{unique}_dest/notify",
                "read_next_in_order", 8, 8, 2, 4, f"L-{unique}-{index}",
            )  # fmt: skip
        context = _engine.open_runtime_context_full_access_for_helper_process(
            {}, link_data_access, "runtime-under-test", "processor-under-test"
        )
        # Constructed the way the helper constructs it: the mapping becomes
        # the class's config object, not something read off the context.
        construct_processor_instance(WhipPublisher, config, link_data_access).setup(
            context
        )


AN_ENDPOINT = {"url": "https://example.invalid/whip"}


@pytest.mark.parametrize("inbound_links", [1, 2])
def test_a_publisher_sets_up_against_the_links_a_session_can_carry(
    request, inbound_links
):
    _PublisherUnderTest.set_up_with(request, inbound_links, AN_ENDPOINT)


def test_a_publisher_with_nothing_wired_refuses_rather_than_publishing_silence(
    request,
):
    """A graph that forgot the connect would otherwise open a session and send
    nothing, which looks from the relay's side like a working publisher."""
    with pytest.raises(ValueError, match="nothing is connected"):
        _PublisherUnderTest.set_up_with(request, 0, AN_ENDPOINT)


def test_more_links_than_a_session_can_carry_are_refused_naming_the_count(request):
    with pytest.raises(ValueError, match="3 links feed"):
        _PublisherUnderTest.set_up_with(request, 3, AN_ENDPOINT)


@pytest.mark.parametrize("config", [{"url": ""}, {"url": 7}])
def test_a_publisher_whose_endpoint_is_not_an_address_is_refused_by_name(
    request, config
):
    with pytest.raises(ValueError, match="`url` is required"):
        _PublisherUnderTest.set_up_with(request, 1, config)


def test_a_publisher_added_with_no_endpoint_at_all_is_refused_by_its_config_class(request):
    """`url` has no default, so the config class refuses the empty mapping and
    names the missing key before any of this wheel's own validation runs."""
    with pytest.raises(TypeError, match="missing 1 required positional argument"):
        _PublisherUnderTest.set_up_with(request, 1, {})


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


def test_a_multichannel_bag_is_refused_before_any_session_is_opened(request):
    """The ordering, not just the refusal.

    Refusing after the connect would leave a live session at the relay that
    then publishes nothing — the phantom publisher the refusal exists to
    prevent. The endpoint here is unresolvable, so a connect attempted first
    would raise about signalling instead of about channels.
    """
    unique = f"whipmulti{os.getpid()}_{request.node.name}"
    channel = f"{unique}/encoder"
    notify = f"{unique}_dest/notify"

    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        "tracks", channel, channel, notify,
        "read_next_in_order", 8, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        "encoded_audio", channel, notify, 1024, 1 << 20, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip

    context = _engine.open_runtime_context_full_access_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test"
    )
    publisher = construct_processor_instance(
        WhipPublisher, {"url": "https://unresolvable.invalid/whip"}, destination
    )
    publisher.setup(context)
    source.write_to_output_port(
        "encoded_audio",
        {
            "codec": "opus",
            "bitstream": b"\x78\x01\x02",
            "is_sync_point": True,
            "group_index": 0,
            "sequence_index": 0,
            "sample_rate": 48_000,
            "channels": 6,
            "sample_count": 960,
            "pre_skip": 0,
        },
    )

    with pytest.raises(ValueError, match="mono and stereo only"):
        publisher.process(_engine.limited_access_view_of_runtime_context_for_helper_process(context))


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
    for log_level in ("debug", "info", "warn", "error"):
        monkeypatch.setattr(log, log_level, kept_log_records.append)
    return RecordingWhepSession


def _player_for_a_drain_that_returns_immediately() -> WhepPlayer:
    player = WhepPlayer(WhepPlayerConfig(url="https://example.invalid/whep"))
    # The drain would otherwise spin on `next_media` returning None; stopping
    # the player makes each attempt reach the backoff at once.
    return player


def test_a_refused_connect_is_retried_rather_than_ending_the_stream(
    scripted_whep_session, monkeypatch
):
    """The 409 a WHEP endpoint answers before its ingest is live is not the end.

    This is the failure the live proof found: the player gave up permanently on
    the first refusal, stayed alive, and produced nothing for the rest of the
    run with nothing downstream able to tell that from an endpoint carrying no
    media.
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


def test_a_refused_bag_names_its_port_and_stops_rather_than_reconnecting(
    scripted_whep_session, monkeypatch
):
    """A wire-contract violation is this player's fault, not the endpoint's.

    Retrying it would spend an endpoint's session forever on a bag that will be
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
        player._stop, "wait", lambda _: pytest.fail("a refused bag must not be retried")
    )

    player._play_until_stopped(outputs=None)

    assert len(scripted_whep_session.constructed) == 1
    assert scripted_whep_session.constructed[0].closed
    assert any("encoded_video" in message for message in errors), (
        "the port that refused the bag has to reach the log"
    )

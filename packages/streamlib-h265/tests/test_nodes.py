# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`H265Encoder` and `H265Decoder` as a graph and a helper see them, with no GPU.

The coding itself needs Vulkan Video and runs in the GPU rig. What runs here is
everything around it: the inputs a graph sets live, the pin types a link is judged by, the entry
point the node catalog reads, the ports a set-up encoder lends its thread, and a stop that ends
the thread inside its budget even when the owned device is refused.
The helper-side tests import `tatolab.runtime` from the runtime unit's lend, on
`PYTHONPATH`.
"""

import os
import time
from collections.abc import Callable
from importlib.metadata import distribution

import pytest

from helper_process_link_wiring import wire_one_link
from tatolab.h265 import H265Decoder, H265Encoder, VideoCodingSessionRefused
from tatolab.h265 import _native
from tatolab.h265.nodes import (
    ENCODED_VIDEO_OUTPUT_PORT,
    VIDEO_INPUT_PORT,
    DecodedPictureBufferBoundSetAlone,
    decoded_picture_buffer_bound,
)
from tatolab.runtime._node_function_running import NodeFunctionRunning
from tatolab.runtime._node_the_wheel_hosts import link_input_data_reader_of, link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.stream import log
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

#: Named so a helper started here never reaches a real GPU: the owned device is
#: refused by name and the thread drains `video` instead.
RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE = "STREAMLIB_RUNTIME_VULKAN_DEVICE_UUID"


#: How long letting go of a coding thread waits for it to end, as the wheel's Rust waits.
CODING_THREAD_END_WAIT_SECONDS = 2.0


def an_encoder(**input_values: object) -> NodeFunctionRunning:
    """An `H265Encoder` as its helper runs it, each pin bound to a stand-in: its thread reads
    and writes the ports it is lent."""
    return NodeFunctionRunning(
        H265Encoder,
        values_to_build_with=input_values,
        pins={VIDEO_INPUT_PORT: object(), ENCODED_VIDEO_OUTPUT_PORT: object()},
        bind_pins=lambda members: {},
        slot_counts={},
    )


def a_decoder(**input_values: object) -> NodeFunctionRunning:
    """An `H265Decoder` run as `an_encoder` runs an encoder."""
    return NodeFunctionRunning(
        H265Decoder,
        values_to_build_with=input_values,
        pins={ENCODED_VIDEO_OUTPUT_PORT: object(), VIDEO_INPUT_PORT: object()},
        bind_pins=lambda members: {},
        slot_counts={},
    )


def stopped_as_its_helper_stops_it(node: NodeFunctionRunning, context: object) -> None:
    """Note the parent's stop on the context, which the lent thread reads, then stop."""
    _engine.note_stop_request_from_parent_on_runtime_context(context, True)
    node.stop(context)


def the_port(node_function: object, port: str) -> "dict[str, object]":
    return next(
        declared
        for declared in (
            *getattr(node_function, NODE_DECLARATION_INPUT_PORTS_STAMP),
            *getattr(node_function, NODE_DECLARATION_OUTPUT_PORTS_STAMP),
        )
        if declared["name"] == port
    )


ENCODER_INPUTS = (
    "width",
    "height",
    "fps",
    "bitrate_bps",
    "keyframe_interval_seconds",
    "effort_level",
)


def test_every_input_takes_a_wire_or_a_value_set_live():
    """Mental-revert: declare a setting read at setup again, and its port says so."""
    for node_function, inputs in (
        (H265Encoder, ENCODER_INPUTS),
        (H265Decoder, ("max_width", "max_height")),
    ):
        for value_input in inputs:
            port = the_port(node_function, value_input)
            assert port["input_mode"] == {"mode": "wire_or_value"}, (node_function, value_input)
            assert port["pin_default"] is None, "unset, the codec chooses"


def test_a_decoder_given_both_bounds_of_its_picture_buffer_or_neither_opens_with_them():
    assert decoded_picture_buffer_bound(1920, 1088) == (1920, 1088)
    assert decoded_picture_buffer_bound(None, None) == (None, None)


def test_a_decoder_given_one_bound_of_its_picture_buffer_and_not_the_other_is_refused_by_kind():
    with pytest.raises(DecodedPictureBufferBoundSetAlone, match="set both, or neither"):
        decoded_picture_buffer_bound(1920, None)


def test_the_encoder_converts_a_texture_into_h265_and_the_decoder_back_into_rgba():
    assert the_port(H265Encoder, VIDEO_INPUT_PORT)["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {
            "type": "texture",
            "pixel_formats": [
                "rgba8_unorm",
                "rgba8_unorm_srgb",
                "bgra8_unorm",
                "bgra8_unorm_srgb",
            ],
        },
    }
    encoded_output = the_port(H265Encoder, ENCODED_VIDEO_OUTPUT_PORT)
    assert encoded_output["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "encoded_video", "codecs": ["h265"]},
    }
    assert the_port(H265Decoder, ENCODED_VIDEO_OUTPUT_PORT)["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "encoded_video", "codecs": ["h265"]},
    }
    assert the_port(H265Decoder, VIDEO_INPUT_PORT)["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "texture", "pixel_formats": ["rgba8_unorm"]},
    }


def test_the_distribution_declares_both_nodes_for_the_node_catalog():
    declared = {
        entry_point.name: entry_point
        for entry_point in distribution("tatolab-h265").entry_points
        if entry_point.group == "tatolab.nodes"
    }

    assert set(declared) == {"H265Decoder", "H265Encoder"}
    for node_function in (H265Decoder, H265Encoder):
        entry_point = declared[node_function.__qualname__]
        assert entry_point.value == f"{node_function.__module__}:{node_function.__qualname__}"
        assert entry_point.load() is node_function


class EncoderWiredBetweenASourceAndASink:
    """An upstream writing `video` into the encoder, and its `encoded_video` read downstream."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        unique = f"h265wired{os.getpid()}_{request.node.name}"
        source_outputs = _engine.open_node_link_data_access_for_helper_process()
        encoder_links = _engine.open_node_link_data_access_for_helper_process()
        sink_inputs = _engine.open_node_link_data_access_for_helper_process()
        source_outputs.declare_ports([], [VIDEO_INPUT_PORT])
        encoder_links.declare_ports([VIDEO_INPUT_PORT], [ENCODED_VIDEO_OUTPUT_PORT])
        wire_one_link(
            source_outputs,
            VIDEO_INPUT_PORT,
            encoder_links,
            VIDEO_INPUT_PORT,
            f"{unique}/video",
            f"{unique}_encoder/notify",
        )
        wire_one_link(
            encoder_links,
            ENCODED_VIDEO_OUTPUT_PORT,
            sink_inputs,
            ENCODED_VIDEO_OUTPUT_PORT,
            f"{unique}/encoded",
            f"{unique}_sink/notify",
        )
        self.source = _engine.open_runtime_context_full_access_for_helper_process(
            source_outputs, "runtime-under-test", "source-under-test"
        )
        self.encoder_context = _engine.open_runtime_context_full_access_for_helper_process(
            encoder_links, "runtime-under-test", "encoder-under-test"
        )
        self.sink = _engine.open_runtime_context_full_access_for_helper_process(
            sink_inputs, "runtime-under-test", "sink-under-test"
        )


def route_every_log_level_to(
    monkeypatch: pytest.MonkeyPatch, log_record_sink: "Callable[[str], None]"
) -> None:
    """`log` reaches a sink only in the interpreter the runtime starts for a node."""
    for log_level in ("debug", "info", "warn", "error"):
        monkeypatch.setattr(log, log_level, log_record_sink)


def discard_log_record(message: str) -> None:
    del message


@pytest.fixture
def encoder_wired(request, monkeypatch) -> EncoderWiredBetweenASourceAndASink:
    route_every_log_level_to(monkeypatch, discard_log_record)
    monkeypatch.delenv(RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE, raising=False)
    return EncoderWiredBetweenASourceAndASink(request)


def test_a_set_up_encoder_lends_both_ports_so_python_cannot_reach_them(encoder_wired):
    encoder = an_encoder()
    context = encoder_wired.encoder_context
    encoder.setup(context)
    try:
        with pytest.raises(Exception, match=ENCODED_VIDEO_OUTPUT_PORT):
            link_output_data_writer_of(context).write(ENCODED_VIDEO_OUTPUT_PORT, b"from python")
        with pytest.raises(Exception, match=VIDEO_INPUT_PORT):
            link_input_data_reader_of(context).read(VIDEO_INPUT_PORT)
    finally:
        stopped_as_its_helper_stops_it(encoder, context)


def test_an_encoder_whose_device_is_refused_drains_and_stops_inside_its_budget(encoder_wired):
    """No device UUID is named, so the owned device is refused; the thread keeps
    draining `video` so upstream sees a live consumer, and nothing is written."""
    encoder = an_encoder(bitrate_bps=4_000_000)
    context = encoder_wired.encoder_context
    encoder.setup(context)
    for _ in range(3):
        link_output_data_writer_of(encoder_wired.source).write(VIDEO_INPUT_PORT, b"a frame no device reads")
    time.sleep(0.3)

    stop_began = time.monotonic()
    stopped_as_its_helper_stops_it(encoder, context)
    encoder.teardown(context)

    assert time.monotonic() - stop_began < CODING_THREAD_END_WAIT_SECONDS + 0.5
    assert link_input_data_reader_of(encoder_wired.sink).read(ENCODED_VIDEO_OUTPUT_PORT) is None


def test_an_encoder_never_set_up_stops_and_tears_down_cleanly(encoder_wired):
    encoder = an_encoder()
    context = encoder_wired.encoder_context
    encoder.stop(context)
    encoder.teardown(context)


class DecoderWiredBetweenASourceAndASink:
    """An upstream writing `encoded_video` into the decoder, and its `video` read downstream."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        unique = f"h265decwired{os.getpid()}_{request.node.name}"
        source_outputs = _engine.open_node_link_data_access_for_helper_process()
        decoder_links = _engine.open_node_link_data_access_for_helper_process()
        sink_inputs = _engine.open_node_link_data_access_for_helper_process()
        source_outputs.declare_ports([], [ENCODED_VIDEO_OUTPUT_PORT])
        decoder_links.declare_ports([ENCODED_VIDEO_OUTPUT_PORT], [VIDEO_INPUT_PORT])
        wire_one_link(
            source_outputs,
            ENCODED_VIDEO_OUTPUT_PORT,
            decoder_links,
            ENCODED_VIDEO_OUTPUT_PORT,
            f"{unique}/encoded",
            f"{unique}_decoder/notify",
        )
        wire_one_link(
            decoder_links,
            VIDEO_INPUT_PORT,
            sink_inputs,
            VIDEO_INPUT_PORT,
            f"{unique}/video",
            f"{unique}_sink/notify",
        )
        self.source = _engine.open_runtime_context_full_access_for_helper_process(
            source_outputs, "runtime-under-test", "source-under-test"
        )
        self.decoder_context = _engine.open_runtime_context_full_access_for_helper_process(
            decoder_links, "runtime-under-test", "decoder-under-test"
        )
        self.sink = _engine.open_runtime_context_full_access_for_helper_process(
            sink_inputs, "runtime-under-test", "sink-under-test"
        )


@pytest.fixture
def decoder_wired(request, monkeypatch) -> DecoderWiredBetweenASourceAndASink:
    route_every_log_level_to(monkeypatch, discard_log_record)
    monkeypatch.delenv(RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE, raising=False)
    return DecoderWiredBetweenASourceAndASink(request)


def test_a_set_up_decoder_lends_both_ports_so_python_cannot_reach_them(decoder_wired):
    decoder = a_decoder()
    context = decoder_wired.decoder_context
    decoder.setup(context)
    try:
        with pytest.raises(Exception, match=VIDEO_INPUT_PORT):
            link_output_data_writer_of(context).write(VIDEO_INPUT_PORT, b"from python")
        with pytest.raises(Exception, match=ENCODED_VIDEO_OUTPUT_PORT):
            link_input_data_reader_of(context).read(ENCODED_VIDEO_OUTPUT_PORT)
    finally:
        stopped_as_its_helper_stops_it(decoder, context)


def test_a_decoder_whose_device_is_refused_drains_and_stops_inside_its_budget(decoder_wired):
    """No device UUID is named, so the owned device is refused; the thread keeps
    draining `encoded_video` so upstream sees a live consumer, and nothing is written."""
    decoder = a_decoder()
    context = decoder_wired.decoder_context
    decoder.setup(context)
    for _ in range(3):
        link_output_data_writer_of(decoder_wired.source).write(ENCODED_VIDEO_OUTPUT_PORT, b"\x25\x00\x00\x01\x65")
    time.sleep(0.3)

    stop_began = time.monotonic()
    stopped_as_its_helper_stops_it(decoder, context)
    decoder.teardown(context)

    assert time.monotonic() - stop_began < CODING_THREAD_END_WAIT_SECONDS + 0.5
    assert link_input_data_reader_of(decoder_wired.sink).read(VIDEO_INPUT_PORT) is None


def test_a_decoder_never_set_up_stops_and_tears_down_cleanly(decoder_wired):
    decoder = a_decoder()
    context = decoder_wired.decoder_context
    decoder.stop(context)
    decoder.teardown(context)


#: A bitrate `EncoderThreadStandIn` refuses a session at, as the codec refuses a setting.
A_BITRATE_THE_CODEC_REFUSES = 1


class EncodeSessionStandIn:
    def __init__(self, settings: "dict[str, object]") -> None:
        self.settings = settings
        self.closed = False

    def close(self) -> None:
        self.closed = True


class EncoderThreadStandIn:
    """A `_native.H265EncoderThread` stand-in keeping each session a build asked for."""

    started: "list[EncoderThreadStandIn]" = []

    def __init__(self, module_host_table: object) -> None:
        del module_host_table
        self.asked_for: "list[dict[str, object]]" = []
        self.opened: "list[EncodeSessionStandIn]" = []
        EncoderThreadStandIn.started.append(self)

    def open_session(self, **settings: object) -> EncodeSessionStandIn:
        self.asked_for.append(settings)
        if settings["bitrate_bps"] == A_BITRATE_THE_CODEC_REFUSES:
            raise VideoCodingSessionRefused("H265Encoder: the codec refused the bitrate")
        session = EncodeSessionStandIn(settings)
        self.opened.append(session)
        return session


def wait_until(condition: "Callable[[], bool]", what: str, timeout_s: float = 5.0) -> None:
    deadline = time.monotonic() + timeout_s
    while not condition():
        assert time.monotonic() < deadline, f"never: {what}"
        time.sleep(0.005)


@pytest.fixture
def encoder_thread_stand_in(monkeypatch) -> "type[EncoderThreadStandIn]":
    EncoderThreadStandIn.started = []
    monkeypatch.setattr(_native, "H265EncoderThread", EncoderThreadStandIn)
    return EncoderThreadStandIn


def test_a_bitrate_set_live_opens_a_session_of_it_on_the_running_thread_with_no_rebuild(
    encoder_wired, encoder_thread_stand_in
):
    """Mental-revert: read `bitrate_bps` at setup again, and the new value reaches no session
    until the node is stopped and built again."""
    encoder = an_encoder(bitrate_bps=2_000_000)
    context = encoder_wired.encoder_context
    encoder.setup(context)
    try:
        (thread,) = encoder_thread_stand_in.started
        wait_until(lambda: len(thread.opened) == 1, "the first session opened")

        encoder.write_inputs({"bitrate_bps": 6_000_000})
        wait_until(lambda: thread.opened[0].closed, "the first session closed once replaced")

        assert len(encoder_thread_stand_in.started) == 1, "the thread is never started again"
        assert [session.settings["bitrate_bps"] for session in thread.opened] == [
            2_000_000,
            6_000_000,
        ]
        assert not thread.opened[1].closed
    finally:
        stopped_as_its_helper_stops_it(encoder, context)


def test_a_bitrate_the_codec_refuses_leaves_the_session_coding_open(
    encoder_wired, encoder_thread_stand_in
):
    encoder = an_encoder(bitrate_bps=2_000_000)
    context = encoder_wired.encoder_context
    encoder.setup(context)
    try:
        (thread,) = encoder_thread_stand_in.started
        wait_until(lambda: len(thread.opened) == 1, "the first session opened")

        encoder.write_inputs({"bitrate_bps": A_BITRATE_THE_CODEC_REFUSES})
        wait_until(lambda: len(thread.asked_for) == 2, "the refused session was asked for")
        time.sleep(0.05)

        (coding,) = thread.opened
        assert not coding.closed, "the refused build keeps the last value it built"
    finally:
        stopped_as_its_helper_stops_it(encoder, context)

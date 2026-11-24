# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`H265Encoder` and `H265Decoder` as a graph and a helper see them, with no GPU.

The coding itself needs Vulkan Video and runs in the GPU rig. What runs here is
everything around it: the settings a graph delivers, the pin types a link is judged by, the entry point the node
catalog reads, the ports a started encoder lends its thread, and a stop that
ends the thread inside its budget even when the owned device is refused.
The helper-side tests import `tatolab.runtime` from the runtime unit's lend, on
`PYTHONPATH`.
"""

import os
import time
from collections.abc import Callable
from importlib.metadata import distribution

import pytest

from helper_process_link_wiring import wire_one_link
from tatolab.h265 import H265Decoder, H265Encoder
from tatolab.h265.nodes import (
    DECODING_THREAD_JOIN_TIMEOUT_SECONDS,
    ENCODED_VIDEO_OUTPUT_PORT,
    ENCODING_THREAD_JOIN_TIMEOUT_SECONDS,
    VIDEO_INPUT_PORT,
)
from tatolab.runtime._node_the_wheel_hosts import link_input_data_reader_of, link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.stream import log
from tatolab.stream._class_node_construction import construct_class_node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

#: Named so a helper started here never reaches a real GPU: the owned device is
#: refused by name and the thread drains `video` instead.
RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE = "STREAMLIB_RUNTIME_VULKAN_DEVICE_UUID"


def an_encoder(**field_values: object) -> H265Encoder:
    """An `H265Encoder` built as its helper builds it, each pin bound to a stand-in: its
    thread reads and writes the ports it is lent."""
    return construct_class_node(
        H265Encoder,
        field_values=field_values,
        pins={VIDEO_INPUT_PORT: object(), ENCODED_VIDEO_OUTPUT_PORT: object()},
    )


def a_decoder(**field_values: object) -> H265Decoder:
    """An `H265Decoder` built as `an_encoder` builds an encoder."""
    return construct_class_node(
        H265Decoder,
        field_values=field_values,
        pins={ENCODED_VIDEO_OUTPUT_PORT: object(), VIDEO_INPUT_PORT: object()},
    )


def the_port(node_class: type, port: str) -> "dict[str, object]":
    return next(
        declared
        for declared in (
            *getattr(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP),
            *getattr(node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP),
        )
        if declared["name"] == port
    )


ENCODER_SETTINGS = (
    "width",
    "height",
    "fps",
    "bitrate_bps",
    "keyframe_interval_seconds",
    "effort_level",
)


def test_every_setting_reaches_the_encoder_as_its_field():
    encoder = an_encoder(
        width=1280,
        height=720,
        fps=30,
        bitrate_bps=4_000_000,
        keyframe_interval_seconds=1,
        effort_level=2,
    )

    assert [getattr(encoder, setting) for setting in ENCODER_SETTINGS] == [
        1280,
        720,
        30,
        4_000_000,
        1,
        2,
    ]


def test_an_encoder_added_with_no_values_leaves_every_setting_to_the_codec():
    encoder = an_encoder()
    assert [getattr(encoder, setting) for setting in ENCODER_SETTINGS] == [None] * 6


def test_every_decoder_setting_reaches_the_decoder_as_its_field():
    decoder = a_decoder(max_width=1920, max_height=1088)
    assert (decoder.max_width, decoder.max_height) == (1920, 1088)


def test_a_decoder_added_with_no_values_sizes_its_picture_buffer_from_the_stream():
    decoder = a_decoder()
    assert (decoder.max_width, decoder.max_height) == (None, None)


def test_a_decoder_given_one_bound_of_its_picture_buffer_and_not_the_other_is_refused():
    with pytest.raises(ValueError, match="set both, or neither"):
        a_decoder(max_width=1920)


def test_each_setting_is_read_at_setup_and_takes_no_wire():
    for node_class, settings in (
        (H265Encoder, ENCODER_SETTINGS),
        (H265Decoder, ("max_width", "max_height")),
    ):
        for setting in settings:
            assert the_port(node_class, setting)["input_mode"] == {
                "mode": "setting_only",
                "read": "at_setup",
            }, (node_class, setting)


def test_the_encoder_converts_a_texture_into_h265_and_the_decoder_back_into_rgba():
    assert the_port(H265Encoder, VIDEO_INPUT_PORT)["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "texture"},
    }
    encoded_output = the_port(H265Encoder, ENCODED_VIDEO_OUTPUT_PORT)
    assert encoded_output["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "encoded_video", "codecs": ["h265"]},
    }
    assert encoded_output["family"] == {"family": "packets", "codec": "h265"}
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
    for node_class in (H265Decoder, H265Encoder):
        entry_point = declared[node_class.__qualname__]
        assert entry_point.value == f"{node_class.__module__}:{node_class.__qualname__}"
        assert entry_point.load() is node_class


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


def test_a_started_encoder_lends_both_ports_so_python_cannot_reach_them(encoder_wired):
    encoder = an_encoder()
    context = encoder_wired.encoder_context
    encoder.start(context)
    try:
        with pytest.raises(Exception, match=ENCODED_VIDEO_OUTPUT_PORT):
            link_output_data_writer_of(context).write(ENCODED_VIDEO_OUTPUT_PORT, b"from python")
        with pytest.raises(Exception, match=VIDEO_INPUT_PORT):
            link_input_data_reader_of(context).read(VIDEO_INPUT_PORT)
    finally:
        encoder.stop(context)


def test_an_encoder_whose_device_is_refused_drains_and_stops_inside_its_budget(encoder_wired):
    """No device UUID is named, so the owned device is refused; the thread keeps
    draining `video` so upstream sees a live consumer, and nothing is written."""
    encoder = an_encoder()
    context = encoder_wired.encoder_context
    encoder.start(context)
    for _ in range(3):
        link_output_data_writer_of(encoder_wired.source).write(VIDEO_INPUT_PORT, b"a frame no device reads")
    time.sleep(0.3)

    stop_began = time.monotonic()
    encoder.stop(context)

    assert time.monotonic() - stop_began < ENCODING_THREAD_JOIN_TIMEOUT_SECONDS + 0.5
    assert encoder._encoding_thread is None
    assert link_input_data_reader_of(encoder_wired.sink).read(ENCODED_VIDEO_OUTPUT_PORT) is None


def test_an_encoder_never_started_stops_and_tears_down_cleanly(encoder_wired):
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


def test_a_started_decoder_lends_both_ports_so_python_cannot_reach_them(decoder_wired):
    decoder = a_decoder()
    context = decoder_wired.decoder_context
    decoder.start(context)
    try:
        with pytest.raises(Exception, match=VIDEO_INPUT_PORT):
            link_output_data_writer_of(context).write(VIDEO_INPUT_PORT, b"from python")
        with pytest.raises(Exception, match=ENCODED_VIDEO_OUTPUT_PORT):
            link_input_data_reader_of(context).read(ENCODED_VIDEO_OUTPUT_PORT)
    finally:
        decoder.stop(context)


def test_a_decoder_whose_device_is_refused_drains_and_stops_inside_its_budget(decoder_wired):
    """No device UUID is named, so the owned device is refused; the thread keeps
    draining `encoded_video` so upstream sees a live consumer, and nothing is written."""
    decoder = a_decoder()
    context = decoder_wired.decoder_context
    decoder.start(context)
    for _ in range(3):
        link_output_data_writer_of(decoder_wired.source).write(ENCODED_VIDEO_OUTPUT_PORT, b"\x25\x00\x00\x01\x65")
    time.sleep(0.3)

    stop_began = time.monotonic()
    decoder.stop(context)

    assert time.monotonic() - stop_began < DECODING_THREAD_JOIN_TIMEOUT_SECONDS + 0.5
    assert decoder._decoding_thread is None
    assert link_input_data_reader_of(decoder_wired.sink).read(VIDEO_INPUT_PORT) is None


def test_a_decoder_never_started_stops_and_tears_down_cleanly(decoder_wired):
    decoder = a_decoder()
    context = decoder_wired.decoder_context
    decoder.stop(context)
    decoder.teardown(context)

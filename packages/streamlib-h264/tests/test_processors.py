# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`H264Encoder` as a graph and a helper see it, with no GPU.

The encode itself needs Vulkan Video and runs in the GPU rig. What runs here is
everything around it: the config a graph delivers, the entry point the node
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
from tatolab.h264 import H264Encoder, H264EncoderConfig
from tatolab.h264.processors import (
    ENCODED_VIDEO_OUTPUT_PORT,
    ENCODING_THREAD_JOIN_TIMEOUT_SECONDS,
    VIDEO_INPUT_PORT,
)
from tatolab.runtime import _engine
from tatolab.runtime._processor_hosting import construct_processor_instance
from tatolab.stream import log, msgpack

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

#: Named so a helper started here never reaches a real GPU: the owned device is
#: refused by name and the thread drains `video` instead.
RUNTIME_VULKAN_DEVICE_UUID_ENVIRONMENT_VARIABLE = "STREAMLIB_RUNTIME_VULKAN_DEVICE_UUID"


def test_every_config_key_reaches_the_encoder_as_its_config_object():
    constructed = construct_processor_instance(
        H264Encoder,
        {
            "width": 1280,
            "height": 720,
            "fps": 30,
            "bitrate_bps": 4_000_000,
            "keyframe_interval_seconds": 1,
            "effort_level": 2,
        },
        None,
    )

    assert isinstance(constructed, H264Encoder)
    assert constructed._config == H264EncoderConfig(
        width=1280,
        height=720,
        fps=30,
        bitrate_bps=4_000_000,
        keyframe_interval_seconds=1,
        effort_level=2,
    )


def test_an_encoder_added_with_no_config_takes_every_default():
    constructed = construct_processor_instance(H264Encoder, {}, None)
    assert constructed._config == H264EncoderConfig()


def test_the_distribution_declares_the_encoder_for_the_node_catalog():
    declared = {
        entry_point.name: entry_point
        for entry_point in distribution("tatolab-h264").entry_points
        if entry_point.group == "tatolab.nodes"
    }

    assert set(declared) == {"H264Encoder"}
    entry_point = declared["H264Encoder"]
    assert entry_point.value == f"{H264Encoder.__module__}:{H264Encoder.__qualname__}"
    assert entry_point.load() is H264Encoder


class EncoderWiredBetweenASourceAndASink:
    """An upstream writing `video` into the encoder, and its `encoded_video` read downstream."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        unique = f"h264wired{os.getpid()}_{request.node.name}"
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
            {}, source_outputs, "runtime-under-test", "source-under-test"
        )
        self.encoder_context = _engine.open_runtime_context_full_access_for_helper_process(
            {}, encoder_links, "runtime-under-test", "encoder-under-test"
        )
        self.sink = _engine.open_runtime_context_full_access_for_helper_process(
            {}, sink_inputs, "runtime-under-test", "sink-under-test"
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
    encoder = H264Encoder(H264EncoderConfig())
    context = encoder_wired.encoder_context
    encoder.start(context)
    try:
        with pytest.raises(Exception, match=ENCODED_VIDEO_OUTPUT_PORT):
            context.outputs.write(ENCODED_VIDEO_OUTPUT_PORT, b"from python")
        with pytest.raises(Exception, match=VIDEO_INPUT_PORT):
            context.inputs.read(VIDEO_INPUT_PORT)
    finally:
        encoder.stop(context)


def test_an_encoder_whose_device_is_refused_drains_and_stops_inside_its_budget(encoder_wired):
    """No device UUID is named, so the owned device is refused; the thread keeps
    draining `video` so upstream sees a live consumer, and nothing is written."""
    encoder = H264Encoder(H264EncoderConfig())
    context = encoder_wired.encoder_context
    encoder.start(context)
    video_frame = msgpack.pack(
        {"surface_id": "surface-never-published", "width": 64, "height": 64, "timestamp_ns": 1}
    )
    for _ in range(3):
        encoder_wired.source.outputs.write(VIDEO_INPUT_PORT, video_frame)
    time.sleep(0.3)

    stop_began = time.monotonic()
    encoder.stop(context)

    assert time.monotonic() - stop_began < ENCODING_THREAD_JOIN_TIMEOUT_SECONDS + 0.5
    assert encoder._encoding_thread is None
    assert encoder_wired.sink.inputs.read(ENCODED_VIDEO_OUTPUT_PORT) is None


def test_an_encoder_never_started_stops_and_tears_down_cleanly(encoder_wired):
    encoder = H264Encoder(H264EncoderConfig())
    context = encoder_wired.encoder_context
    encoder.stop(context)
    encoder.teardown(context)

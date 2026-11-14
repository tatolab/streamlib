# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The processors this wheel supplies.

`H264Encoder` and `H264Decoder` lend their ports to a thread of this wheel's Rust
through the module host table. The thread codes on a Vulkan device the helper
owns — on the runtime's GPU, never the runtime's own device — and reads and
writes with no Python attached. The engine is never on the data path.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import Image, Input, Output, Packets, RuntimeContextFullAccess, log, node

from . import _native

VIDEO_INPUT_PORT = "video"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"

#: How long `stop()` waits for the coding thread. Its own wait for a message
#: is bounded at a tenth of a second; an encode or decode in flight finishes
#: first.
ENCODING_THREAD_JOIN_TIMEOUT_SECONDS = 2.0
DECODING_THREAD_JOIN_TIMEOUT_SECONDS = ENCODING_THREAD_JOIN_TIMEOUT_SECONDS


@dataclasses.dataclass
class H264EncoderConfig:
    """What an `H264Encoder` is configured with; every key is optional."""

    width: Annotated[
        "int | None",
        "Expected frame width — a guardrail, not a resize: a mismatching frame "
        "wins with a warning.",
    ] = None
    height: Annotated["int | None", "Expected frame height — a guardrail like `width`."] = None
    fps: Annotated["int | None", "The frame rate the encoder's rate control assumes. Absent: 60."] = None
    bitrate_bps: Annotated[
        "int | None",
        "Target bitrate in bits per second. Absent: constant-QP encoding at the "
        "session's balanced preset.",
    ] = None
    keyframe_interval_seconds: Annotated[
        "int | None", "Seconds between IDR sync points. Absent: 2."
    ] = None
    effort_level: Annotated[
        "int | None",
        "Encoder-effort index (the hardware encoder's analysis budget, not a codec "
        "quality knob). Absent: the codec's default.",
    ] = None


@node.meta(description="Encodes published video surfaces to H.264 on Vulkan Video or VideoToolbox")
@node.process(execution="manual")
class H264Encoder:
    """Video frames in, H.264 access units out, encoded with Vulkan Video or VideoToolbox.

    `video` takes 8-bit RGBA or BGRA Image frames, buffer-backed (camera) or
    texture-backed (a test pattern, a GPU effect, a decoder), the colour their
    descriptor states written into the stream's VUI; any other format, and a
    message that is not an Image frame, is refused by name and dropped.
    `encoded_video` publishes one bare Annex-B access unit per message, stamped
    on the message, parameter sets inline before each keyframe, and a keyframe
    opens a group. After the first
    keyframe the port's stream facts are `PacketsStreamFacts` naming the codec
    string the SPS spells and the coded extent. The session mints from the first
    frame's dimensions and re-mints when the upstream extent changes; a write
    the channel refuses makes the next frame a keyframe.

    On Linux the encoder runs on a Vulkan device this helper creates on the
    runtime's GPU, so a fault in the video engine ends this helper's stream and
    nothing else; a device without Vulkan Video encode refuses the device once.
    On macOS it runs on VideoToolbox's hardware encoder, the runtime converting
    each frame into an NV12 IOSurface for it, and refuses `effort_level`.
    Either way a refused encoder discards every frame after with no exception
    reaching Python.
    """

    def __init__(self, config: H264EncoderConfig) -> None:
        self._config = config
        self._encoding_thread: "_native.H264EncoderThread | None" = None

    video = Input(Image, delivery="ordered", description="Video frames to encode")
    encoded_video = Output(
        Packets, codec="h264", description="H.264 access units, Annex-B, one per message"
    )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend both ports to an encoding thread of this wheel's Rust.

        The owned device is created on that thread, so its start-up never
        spends the helper's start-up budget.
        """
        module_host_table = ctx.open_module_host_table(
            input_ports=[VIDEO_INPUT_PORT], output_ports=[ENCODED_VIDEO_OUTPUT_PORT]
        )
        self._encoding_thread = _native.H264EncoderThread(
            module_host_table,
            width=self._config.width,
            height=self._config.height,
            fps=self._config.fps,
            bitrate_bps=self._config.bitrate_bps,
            keyframe_interval_seconds=self._config.keyframe_interval_seconds,
            effort_level=self._config.effort_level,
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_encoding_thread()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_encoding_thread()

    def _stop_the_encoding_thread(self) -> None:
        if self._encoding_thread is None:
            return
        if not self._encoding_thread.stop_and_join(ENCODING_THREAD_JOIN_TIMEOUT_SECONDS):
            log.warn("H264Encoder: the encoding thread is still finishing an encode")
        self._encoding_thread = None


@dataclasses.dataclass
class H264DecoderConfig:
    """What an `H264Decoder` is configured with; every key is optional."""

    max_width: Annotated[
        "int | None",
        "Upper bound on the coded width the decoded picture buffer is allocated "
        "for, together with `max_height` or not at all. Absent: sized from the "
        "stream's first SPS.",
    ] = None
    max_height: Annotated[
        "int | None", "Upper bound on the coded height, paired with `max_width`."
    ] = None


@node.meta(
    description="Decodes H.264 access units to published video textures on Vulkan Video or VideoToolbox",
)
@node.process(execution="manual")
class H264Decoder:
    """H.264 access units in, video frames out, decoded with Vulkan Video or VideoToolbox.

    `encoded_video` takes bare Annex-B access units, one per message as
    `H264Encoder` publishes them, a keyframe opening a group, from any producer; a message that is not a Packets unit of this codec is
    refused by name.
    The decoder enters the stream at a group start and, after a read reports a
    loss, discards to the next one, so it never publishes a picture built from
    references it did not see. A keyframe whose parameter sets differ from the
    ones before it reconfigures the decoder.

    `video` publishes an Image frame on an RGBA texture from this node's output
    pool at the conformance-windowed extent, carrying the access unit's
    timestamp and the colour the picture was converted with: the SPS
    VUI's, its matrix and range filled when the stream leaves them out (BT.709
    for HD, BT.601 for SD, limited range). The host converts each picture from
    the NV12 buffer the decoder exports on Linux, or the NV12 IOSurface
    VideoToolbox decodes into on macOS.

    On Linux the decoder runs on a Vulkan device this helper creates on the
    runtime's GPU, so a fault in the video engine ends this helper's stream and
    nothing else; a device without Vulkan Video decode refuses the device once.
    On macOS it runs on VideoToolbox's hardware decoder. Either way a refused
    decoder discards every access unit after with no exception reaching Python.
    """

    def __init__(self, config: H264DecoderConfig) -> None:
        self._config = config
        self._decoding_thread: "_native.H264DecoderThread | None" = None

    encoded_video = Input(
        Packets,
        delivery="ordered",
        requires_codec=("h264",),
        description="H.264 access units to decode",
    )
    video = Output(Image, description="Decoded video frames")

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend both ports to a decoding thread of this wheel's Rust.

        The owned device is created on that thread, so its start-up never
        spends the helper's start-up budget.
        """
        module_host_table = ctx.open_module_host_table(
            input_ports=[ENCODED_VIDEO_OUTPUT_PORT], output_ports=[VIDEO_INPUT_PORT]
        )
        self._decoding_thread = _native.H264DecoderThread(
            module_host_table,
            max_width=self._config.max_width,
            max_height=self._config.max_height,
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_decoding_thread()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_decoding_thread()

    def _stop_the_decoding_thread(self) -> None:
        if self._decoding_thread is None:
            return
        if not self._decoding_thread.stop_and_join(DECODING_THREAD_JOIN_TIMEOUT_SECONDS):
            log.warn("H264Decoder: the decoding thread is still finishing a decode")
        self._decoding_thread = None

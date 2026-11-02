# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor this wheel supplies.

`H265Encoder` lends its `video` input and its `encoded_video` output to a thread
of this wheel's Rust through the module host table. The thread checks each
published frame out over surface-share, encodes it on a Vulkan device the helper
owns — on the runtime's GPU, never the runtime's own device — and writes media
frames with no Python attached. The engine is never on the data path.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import RuntimeContextFullAccess, log, node

from . import _native

VIDEO_INPUT_PORT = "video"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"

#: How long `stop()` waits for the encoding thread. Its own wait for a frame
#: is bounded at a tenth of a second; an encode in flight finishes first.
ENCODING_THREAD_JOIN_TIMEOUT_SECONDS = 2.0


@dataclasses.dataclass
class H265EncoderConfig:
    """What an `H265Encoder` is configured with; every key is optional."""

    width: Annotated[
        "int | None",
        "Expected frame width — a guardrail, not a resize: a mismatching frame "
        "wins with a warning.",
    ] = None
    height: Annotated["int | None", "Expected frame height — a guardrail like `width`."] = None
    fps: Annotated["int | None", "Frame rate used when the incoming frames carry none. Absent: 60."] = None
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


@node(
    execution="manual",
    description="Encodes published video surfaces to H.265 (HEVC) on a Vulkan device the module owns",
)
class H265Encoder:
    """Video frames in, H.265 media frames out, encoded with Vulkan Video on Linux.

    `video` takes buffer-backed `rgba32` frames (camera, test pattern); a
    texture-backed or other-format frame is refused by name and dropped.
    `encoded_video` publishes one media frame per access unit — a QUIC varint
    timestamp in microseconds, then the Annex-B access unit, parameter sets
    inline before each keyframe — and a keyframe opens a group. After the first
    keyframe the port's catalog entry is a `VideoEntry` naming the codec string
    the SPS spells and the coded extent. The session mints from the first
    frame's dimensions and re-mints when the upstream extent changes; a write
    the channel refuses makes the next frame a keyframe.

    The encoder runs on a Vulkan device this helper creates on the runtime's
    GPU, so a fault in the video engine ends this helper's stream and nothing
    else. A device without Vulkan Video encode refuses the device once, and
    every frame after is discarded with no exception reaching Python.
    """

    def __init__(self, config: H265EncoderConfig) -> None:
        self._config = config
        self._encoding_thread: "_native.H265EncoderThread | None" = None

    @node.input(delivery_profile="ordered")
    def video(self) -> None:
        """Video frames to encode."""

    @node.output()
    def encoded_video(self) -> None:
        """H.265 media frames, one access unit each."""

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend both ports to an encoding thread of this wheel's Rust.

        The owned device is created on that thread, so its start-up never
        spends the helper's start-up budget.
        """
        module_host_table = ctx.open_module_host_table(
            input_ports=[VIDEO_INPUT_PORT], output_ports=[ENCODED_VIDEO_OUTPUT_PORT]
        )
        self._encoding_thread = _native.H265EncoderThread(
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
            log.warn("H265Encoder: the encoding thread is still finishing an encode")
        self._encoding_thread = None

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The nodes this wheel supplies.

`H265Encoder` and `H265Decoder` lend their ports to a thread of this wheel's Rust
through the module host table. The thread codes on a Vulkan device the helper
owns — on the runtime's GPU, never the runtime's own device — and reads and
writes with no Python attached. The engine is never on the data path.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import (
    Description,
    EncodedCodecs,
    EncodedVideo,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    SettingOnly,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    log,
    node,
)

from . import _native

VIDEO_INPUT_PORT = "video"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"
CODEC = "h265"
DECODED_PICTURE_TEXTURE_FORMAT = "rgba8_unorm"

#: How long `stop()` waits for the coding thread. Its own wait for a message
#: is bounded at a tenth of a second; an encode or decode in flight finishes
#: first.
ENCODING_THREAD_JOIN_TIMEOUT_SECONDS = 2.0
DECODING_THREAD_JOIN_TIMEOUT_SECONDS = ENCODING_THREAD_JOIN_TIMEOUT_SECONDS


@node.meta(
    description="Encodes published video surfaces to H.265 (HEVC) on Vulkan Video or VideoToolbox",
    conversion=True,
)
@node(execution="manual")
class H265Encoder:
    """Video frames in, H.265 access units out, encoded with Vulkan Video or VideoToolbox.

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

    width: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=1),
        Description(
            "Expected frame width — a guardrail, not a resize: a mismatching frame wins with a "
            "warning."
        ),
    ] = None
    height: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("Expected frame height — a guardrail like `width`."),
    ] = None
    fps: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("The frame rate the encoder's rate control assumes. Unset: 60."),
    ] = None
    bitrate_bps: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=1),
        Description(
            "Target bitrate in bits per second. Unset: constant-QP encoding at the session's "
            "balanced preset."
        ),
    ] = None
    keyframe_interval_seconds: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("Seconds between IDR sync points. Unset: 2."),
    ] = None
    effort_level: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=0),
        Description(
            "Encoder-effort index (the hardware encoder's analysis budget, not a codec quality "
            "knob). Unset: the codec's default."
        ),
    ] = None
    video: InputPin[
        Annotated[Texture, StreamDelivery("ordered"), Description("Video frames to encode")]
    ]
    encoded_video: OutputPin[
        Annotated[
            EncodedVideo,
            EncodedCodecs(CODEC),
            Description("H.265 access units, Annex-B, one per message"),
        ]
    ]

    def __init__(self) -> None:
        self._encoding_thread: "_native.H265EncoderThread | None" = None

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
            width=self.width,
            height=self.height,
            fps=self.fps,
            bitrate_bps=self.bitrate_bps,
            keyframe_interval_seconds=self.keyframe_interval_seconds,
            effort_level=self.effort_level,
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


@node.meta(
    description="Decodes H.265 access units to published video textures on Vulkan Video or VideoToolbox",
    conversion=True,
)
@node(execution="manual")
class H265Decoder:
    """H.265 access units in, video frames out, decoded with Vulkan Video or VideoToolbox.

    `encoded_video` takes bare Annex-B access units, one per message as
    `H265Encoder` publishes them, a keyframe opening a group, from any producer; a message that is not a Packets unit of this codec is
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

    max_width: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=1),
        Description(
            "Upper bound on the coded width the decoded picture buffer is allocated for, "
            "together with `max_height` or not at all. Unset: sized from the stream's first SPS."
        ),
    ] = None
    max_height: Annotated[
        int | None,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("Upper bound on the coded height, paired with `max_width`."),
    ] = None
    encoded_video: InputPin[
        Annotated[
            EncodedVideo,
            EncodedCodecs(CODEC),
            StreamDelivery("ordered"),
            Description("H.265 access units to decode"),
        ]
    ]
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(DECODED_PICTURE_TEXTURE_FORMAT),
            Description("Decoded video frames"),
        ]
    ]

    def __init__(self) -> None:
        if (self.max_width is None) != (self.max_height is None):
            raise ValueError(
                f"H265Decoder: `max_width` is {self.max_width!r} and `max_height` is "
                f"{self.max_height!r}; set both, or neither to size the picture buffer from the "
                f"stream's first SPS"
            )
        self._decoding_thread: "_native.H265DecoderThread | None" = None

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend both ports to a decoding thread of this wheel's Rust.

        The owned device is created on that thread, so its start-up never
        spends the helper's start-up budget.
        """
        module_host_table = ctx.open_module_host_table(
            input_ports=[ENCODED_VIDEO_OUTPUT_PORT], output_ports=[VIDEO_INPUT_PORT]
        )
        self._decoding_thread = _native.H265DecoderThread(
            module_host_table,
            max_width=self.max_width,
            max_height=self.max_height,
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
            log.warn("H265Decoder: the decoding thread is still finishing a decode")
        self._decoding_thread = None

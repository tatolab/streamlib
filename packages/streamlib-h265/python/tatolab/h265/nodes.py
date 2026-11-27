# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The nodes this wheel supplies.

`H265Encoder` and `H265Decoder` are each one function: the body lends both ports
to a thread of this wheel's Rust through the module host table, and a computed
value opens the codec session of the node's inputs on that thread. The thread
codes on a Vulkan device the helper owns — on the runtime's GPU, never the
runtime's own device — and reads and writes with no Python attached. The engine
is never on the data path.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import (
    Description,
    EncodedCodecs,
    EncodedVideo,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    computed,
    node,
)

from . import _native

VIDEO_INPUT_PORT = "video"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"
CODEC = "h265"
DECODED_PICTURE_TEXTURE_FORMAT = "rgba8_unorm"
#: The 8-bit formats the encoder reads a picture in, so a link from an output of any other,
#: such as an HDR `rgba16_float`, is refused when it is made, not frame by frame.
ENCODED_PICTURE_TEXTURE_FORMATS = (
    "rgba8_unorm",
    "rgba8_unorm_srgb",
    "bgra8_unorm",
    "bgra8_unorm_srgb",
)


class DecodedPictureBufferBoundSetAlone(ValueError):
    """`max_width` or `max_height` was set without the other; the decoded picture buffer is
    bounded by both, or sized from the stream's first SPS."""


def decoded_picture_buffer_bound(
    max_width: "int | None", max_height: "int | None"
) -> "tuple[int | None, int | None]":
    """The bound a decoder session's picture buffer is allocated for: both, or neither."""
    if (max_width is None) != (max_height is None):
        raise DecodedPictureBufferBoundSetAlone(
            f"H265Decoder: `max_width` is {max_width!r} and `max_height` is {max_height!r}; set "
            f"both, or neither to size the picture buffer from the stream's first SPS"
        )
    return max_width, max_height


@node.meta(
    description="Encodes published video surfaces to H.265 (HEVC) on Vulkan Video or VideoToolbox",
    conversion=True,
)
@node(execution="manual")
def H265Encoder(
    *,
    width: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1),
            Description(
                "Expected frame width — a guardrail, not a resize: a mismatching frame wins with "
                "a warning."
            ),
        ]
    ] = Input(None),
    height: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1),
            Description("Expected frame height — a guardrail like `width`."),
        ]
    ] = Input(None),
    fps: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1),
            Description("The frame rate the encoder's rate control assumes. Unset: 60."),
        ]
    ] = Input(None),
    bitrate_bps: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1),
            Description(
                "Target bitrate in bits per second. Unset: constant-QP encoding at the session's "
                "balanced preset."
            ),
        ]
    ] = Input(None),
    keyframe_interval_seconds: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1),
            Description("Seconds between IDR sync points. Unset: 2."),
        ]
    ] = Input(None),
    effort_level: Input[
        Annotated[
            int | None,
            NumberRange(minimum=0),
            Description(
                "Encoder-effort index (the hardware encoder's analysis budget, not a codec "
                "quality knob). Unset: the codec's default."
            ),
        ]
    ] = Input(None),
    video: InputPin[
        Annotated[
            Texture,
            TexturePixelFormats(*ENCODED_PICTURE_TEXTURE_FORMATS),
            StreamDelivery("ordered"),
            Description("Video frames to encode"),
        ]
    ],
    encoded_video: OutputPin[
        Annotated[
            EncodedVideo,
            EncodedCodecs(CODEC),
            Description("H.265 access units, Annex-B, one per message"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
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

    Every input is live: a new value, a bitrate say, opens a session of it
    beside the one encoding, with no stop and no rebuild of the node, and the
    next frame is encoded with it as a keyframe. The session opens on the
    coding thread, which owns the device, so frames wait on `video` while it
    opens and are encoded once it has. A session
    the codec refuses raises `VideoCodingSessionRefused` on the node, and the
    one before keeps encoding; while no session is open, every frame is
    discarded with no exception reaching Python.
    """
    del video, encoded_video
    # Both ports go to the thread, which creates the owned device itself, so its start-up
    # never spends the helper's start-up budget.
    encoding_thread = _native.H265EncoderThread(
        ctx.open_module_host_table(
            input_ports=[VIDEO_INPUT_PORT], output_ports=[ENCODED_VIDEO_OUTPUT_PORT]
        )
    )
    computed(
        lambda: encoding_thread.open_session(
            width=width.get(),
            height=height.get(),
            fps=fps.get(),
            bitrate_bps=bitrate_bps.get(),
            keyframe_interval_seconds=keyframe_interval_seconds.get(),
            effort_level=effort_level.get(),
        )
    )


@node.meta(
    description="Decodes H.265 access units to published video textures on Vulkan Video or VideoToolbox",
    conversion=True,
)
@node(execution="manual")
def H265Decoder(
    *,
    max_width: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1),
            Description(
                "Upper bound on the coded width the decoded picture buffer is allocated for, "
                "together with `max_height` or not at all. Unset: sized from the stream's first "
                "SPS."
            ),
        ]
    ] = Input(None),
    max_height: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1),
            Description("Upper bound on the coded height, paired with `max_width`."),
        ]
    ] = Input(None),
    encoded_video: InputPin[
        Annotated[
            EncodedVideo,
            EncodedCodecs(CODEC),
            StreamDelivery("ordered"),
            Description("H.265 access units to decode"),
        ]
    ],
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(DECODED_PICTURE_TEXTURE_FORMAT),
            Description("Decoded video frames"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
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
    On macOS it runs on VideoToolbox's hardware decoder.

    Both inputs are live: a new bound opens a session of it beside the one
    decoding, off the frame thread, and the next group start is decoded with it.
    A session the codec refuses raises `VideoCodingSessionRefused` on the node,
    and the one before keeps decoding; while no session is open, every access
    unit is discarded with no exception reaching Python.
    """
    del encoded_video, video
    # Both ports go to the thread, which creates the owned device itself, so its start-up
    # never spends the helper's start-up budget.
    decoding_thread = _native.H265DecoderThread(
        ctx.open_module_host_table(
            input_ports=[ENCODED_VIDEO_OUTPUT_PORT], output_ports=[VIDEO_INPUT_PORT]
        )
    )

    def open_a_session() -> "_native.H265DecodeSession":
        bound_width, bound_height = decoded_picture_buffer_bound(max_width.get(), max_height.get())
        return decoding_thread.open_session(max_width=bound_width, max_height=bound_height)

    computed(open_a_session)

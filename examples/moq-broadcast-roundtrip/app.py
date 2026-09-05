# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera and microphone out to a MoQ relay, and back again.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. One graph carries both halves: a publisher sending two tracks to a
draft-16 relay, and a subscriber pulling the same broadcast back down and
decoding it to a window and the speakers. Everything between the two crosses the
network, so what reaches the glass came off the relay and not out of a local
link.

`MoqBroadcastPublisher` and `MoqBroadcastSubscriber` ship in the
`streamlib-moq` extension wheel, not in the engine. Each runs in its own helper
process, and the wheel's own Rust owns each session.
"""

import os

from streamlib import (
    CameraSource,
    DisplayWindow,
    H264Decoder,
    H264Encoder,
    MicrophoneSource,
    OpusDecoder,
    OpusEncoder,
    Runtime,
    SpeakerSink,
)
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber

#: What a subscriber calls the tracks when the broadcast is laid out as CMAF.
#: The container names them, not the author: `{track_id}.m4s` numbered from one
#: in declaration order, which is `rt.connect` order — so wiring video into
#: `tracks` first is what makes video track one. It is the reference
#: publisher's fallback contract, hardcoded by any subscriber not asked to
#: fetch a catalog, so it is not this wheel's to vary.
VIDEO_TRACK_NAME = "1.m4s"
AUDIO_TRACK_NAME = "2.m4s"

#: One name both halves agree on, so the subscriber can find what the publisher
#: sent. Left to the publisher's own default it would be `streamlib/<runtime_id>`
#: — a cuid2 minted at startup, which nothing could subscribe to without reading
#: it back out of the graph first.
DEFAULT_BROADCAST_NAME = "streamlib/moq-broadcast-roundtrip"


def _required_relay_url() -> str:
    """The relay, from the environment and never from this file.

    A draft-16 relay is provisioned per account and carries its authentication
    token as a path segment, so the URL *is* the credential and no address this
    file could ship would reach one. The app refuses to start without it rather
    than falling back to a placeholder that would fail later and further away.
    """
    relay_url = os.environ.get("STREAMLIB_MOQ_RELAY_URL")
    if not relay_url:
        raise ValueError(
            "STREAMLIB_MOQ_RELAY_URL is unset. It is the relay to publish "
            "through, token included — for Cloudflare it reads "
            "`https://draft-16.cloudflare.mediaoverquic.com/<token>`. It is a "
            "credential: export it, never commit it."
        )
    return relay_url


def setup(rt: Runtime) -> None:
    relay_url = _required_relay_url()
    # `or`, not a default argument: the variable set to the empty string is the
    # shape that splits the two halves. The publisher reads it as unset and
    # falls back to its own `streamlib/<runtime_id>`, while the subscriber
    # refuses it — so the graph would fail at setup rather than use the name
    # below.
    broadcast = os.environ.get("STREAMLIB_MOQ_BROADCAST") or DEFAULT_BROADCAST_NAME

    publisher = rt.add(
        MoqBroadcastPublisher,
        config={"relay_url": relay_url, "broadcast": broadcast},
        display_name="publisher",
    )
    subscriber = rt.add(
        MoqBroadcastSubscriber,
        config={
            "relay_url": relay_url,
            "broadcast": broadcast,
            "video_track": VIDEO_TRACK_NAME,
            "audio_track": AUDIO_TRACK_NAME,
        },
        display_name="subscriber",
    )

    camera_configuration: dict[str, object] = {}
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    # Every codec block bare: each sizes itself from what reaches it — the
    # encoder's session from the first frame's extent, the decoder's from the
    # stream's first SPS, the libopus encoder from the first window's channels.
    video_encoder = rt.add(H264Encoder)
    microphone = rt.add(MicrophoneSource)
    audio_encoder = rt.add(OpusEncoder)

    video_decoder = rt.add(H264Decoder)
    audio_decoder = rt.add(OpusDecoder)
    window = rt.add(
        DisplayWindow,
        config={
            "title": "StreamLib MoQ Broadcast Round-Trip",
            "width": 1920,
            "height": 1080,
            "scaling": "fit",
        },
    )
    speaker = rt.add(SpeakerSink)

    # Video into `tracks` first: declaration order is track-id order under
    # CMAF, and the two track names above are read from it.
    rt.connect(camera.output("video"), video_encoder.input("video"))
    rt.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))

    rt.connect(microphone.output("audio"), audio_encoder.input("audio"))
    rt.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))

    rt.connect(subscriber.output("encoded_video"), video_decoder.input("encoded_video"))
    rt.connect(video_decoder.output("video"), window.input("video"))

    rt.connect(subscriber.output("encoded_audio"), audio_decoder.input("encoded_audio"))
    rt.connect(audio_decoder.output("audio"), speaker.input("audio"))

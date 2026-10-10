# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A camera and the known signal recorded into one file, two tracks.

`CameraSource -> <codec>Encoder -> Mp4Sink` and
`KnownAudioSignalSource -> OpusEncoder -> Mp4Sink`. Two producers into the
sink's one `tracks` input, so the file owes two tracks and nothing between
them is configured — the sink enumerates its inbound links at `setup()` and
names each track after the channel it subscribed to.

The twin of `codec_roundtrip_stream.py`: same camera, same encoder, same
authoring surface, with the container where the decoder was. That is what
makes the decode-back a real comparison — `e2e_fixture_recording.sh` replays
this file's video track back through the same decoder and locks it to the
same vivid baseline the live path locks to, with one file in between.

No display and no audio device. The known signal is generated rather than
captured for the same reason `opus_roundtrip_stream.py` generates it: what is
being measured is the engine, not the rig's sound card. The signal runs for
its own length and then stops, which is a legal recording — a `moof` owes a
`traf` to no track — so the audio track is shorter than the video one by
design.

A stream takes no argv, so its settings are the environment of the `tatolabd`
it is loaded into, which its compile inherits:
`STREAMLIB_RECORDING_PATH` is the file to record into, created or truncated at
startup; `STREAMLIB_FIXTURE_VIDEO_CODEC` picks the codec (`h264` by default);
`STREAMLIB_CAMERA_DEVICE` names the V4L2 node, else the first the engine finds.

The node names are for reading a run: they are what this stream's own log
lines and `tatolab graph` show. Nothing downstream keys on them — a track is
named by the channel its link subscribed to, which carries the engine-minted
processor id, so `e2e_fixture_recording.sh` checks the recorded track names by
their `/encoded_video` and `/encoded_audio` suffixes instead.
"""

import os

import tatolab.stream
from known_audio_signal_source import KnownAudioSignalSource
from tatolab.stream import StreamBuilder, stream

_VIDEO_ENCODER_MARKERS_BY_CODEC: dict[str, type] = {
    "h264": tatolab.stream.H264Encoder,
    "h265": tatolab.stream.H265Encoder,
}

# Stated rather than left to the encoder's own default, because the fragment
# rule follows it: with a video track wired, `Mp4Sink` closes a fragment at
# that track's sync points, so this is also how often the recording becomes
# playable a little further.
ENCODER_KEYFRAME_INTERVAL_SECONDS = 2


@stream
def camera_and_known_signal_recorded_into_one_file(stream_builder: StreamBuilder) -> None:
    recording_path = os.environ.get("STREAMLIB_RECORDING_PATH")
    if not recording_path:
        raise ValueError("STREAMLIB_RECORDING_PATH names no file to record into")
    codec = os.environ.get("STREAMLIB_FIXTURE_VIDEO_CODEC") or "h264"
    if codec not in _VIDEO_ENCODER_MARKERS_BY_CODEC:
        raise ValueError(
            f"STREAMLIB_FIXTURE_VIDEO_CODEC={codec!r} names no codec this recording "
            f"carries; it carries {', '.join(sorted(_VIDEO_ENCODER_MARKERS_BY_CODEC))}"
        )
    camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")

    recorder = stream_builder.add(
        tatolab.stream.Mp4Sink,
        name="recorder",
        config={"path": recording_path},
    )

    camera = stream_builder.add(
        tatolab.stream.CameraSource,
        name="camera",
        config={"device_id": camera_device} if camera_device else {},
    )
    video_encoder = stream_builder.add(
        _VIDEO_ENCODER_MARKERS_BY_CODEC[codec],
        name="video_encoder",
        config={"keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS},
    )
    stream_builder.connect(camera.output("video"), video_encoder.input("video"))
    stream_builder.connect(video_encoder.output("encoded_video"), recorder.input("tracks"))

    signal = stream_builder.add(KnownAudioSignalSource, name="known_signal")
    audio_encoder = stream_builder.add(tatolab.stream.OpusEncoder, name="audio_encoder")
    stream_builder.connect(signal.output("audio"), audio_encoder.input("audio"))
    stream_builder.connect(audio_encoder.output("encoded_audio"), recorder.input("tracks"))

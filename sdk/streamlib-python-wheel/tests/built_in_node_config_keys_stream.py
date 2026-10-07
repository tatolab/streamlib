# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# pyright: reportUnnecessaryTypeIgnoreComment=true

"""A stream pyright holds to the built-ins' config shapes, as an author's editor does.

Each wrong config below carries an ignore naming the error it raises; with
unnecessary ignores reported, a wrong key that stops being an error fails the
type-check job.
"""

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    DisplayWindow,
    Mp4Sink,
    OpusDecoder,
    StreamBuilder,
    stream,
)


@stream
def built_in_nodes_configured_by_their_typed_keys(stream_builder: StreamBuilder) -> None:
    stream_builder.add(CameraSource, config={"device_id": "/dev/video2"})
    camera_config: CameraSourceConfig = {"max_width": 1280, "max_height": 720}
    stream_builder.add(CameraSource, name="capped", config=camera_config)
    stream_builder.add(DisplayWindow, config={"title": "Preview", "scaling": "fill"})
    stream_builder.add(Mp4Sink, config={"path": "recording.mp4"})
    stream_builder.add(OpusDecoder)


@stream
def built_in_nodes_configured_wrongly(stream_builder: StreamBuilder) -> None:
    stream_builder.add(CameraSource, config={"devce_id": "/dev/video2"})  # pyright: ignore[reportCallIssue, reportArgumentType]
    stream_builder.add(CameraSource, config={"device_id": 2})  # pyright: ignore[reportCallIssue, reportArgumentType]
    stream_builder.add(DisplayWindow, config={"scaling": "zoom"})  # pyright: ignore[reportCallIssue, reportArgumentType]
    stream_builder.add(Mp4Sink, config={})  # pyright: ignore[reportCallIssue, reportArgumentType]

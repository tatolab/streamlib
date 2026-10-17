# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# pyright: reportUnnecessaryTypeIgnoreComment=true

"""A message's payload is bytes, as pyright reads `ctx.inputs` and `ctx.outputs`.

Pyright checks this file and reports an ignore comment that silences nothing,
so the `assert_type` calls are the proof of each read's narrowing and the
ignore on the dict write is the proof that pyright refuses a dict payload. The
functions also run, over an in-memory port standing in for the runtime's.
"""

from __future__ import annotations

from typing import Any, TypedDict, cast

from typing_extensions import assert_type

from tatolab.stream import (
    AudioBlock,
    LinkInputDataReader,
    LinkOutputDataWriter,
    Message,
    VideoFrame,
    msgpack_typed_dict_cast,
)
from tatolab.stream.msgpack import pack

FRAME_ENTRIES: "dict[str, Any]" = {
    "surface_id": "surface-7",
    "width": 1280,
    "height": 720,
    "timestamp_ns": 123_456_789,
}

INBOUND_LINK_NAME = "camera/video"


class DetectionRecord(TypedDict):
    label: str
    confidence: float


class InMemoryPortStandingInForTheRuntimes:
    """Hands back one queued payload per read, cast by `into` when given."""

    def __init__(self, queued_payload: bytes) -> None:
        self.queued_payload = queued_payload
        self.written: "list[tuple[str, bytes, int | None, bool]]" = []

    def read(self, port_name: str, *, into: Any = None) -> Any:
        return self.queued_payload if into is None else into.from_bytes(self.queued_payload)

    def read_with_timestamp(self, port_name: str) -> Any:
        return self.queued_payload, FRAME_ENTRIES["timestamp_ns"]

    def read_from_inbound_link(self, port_name: str, *, into: Any = None) -> Any:
        return self.read(port_name, into=into), INBOUND_LINK_NAME

    def read_message(self, port_name: str, *, into: Any = None) -> Any:
        return Message(
            payload=self.read(port_name, into=into),
            timestamp_ns=FRAME_ENTRIES["timestamp_ns"],
            starts_group=True,
            lost_since_last_read=0,
            inbound_link=INBOUND_LINK_NAME,
        )

    def write(
        self,
        port_name: str,
        payload: bytes,
        timestamp_ns: "int | None" = None,
        starts_group: bool = True,
    ) -> None:
        self.written.append((port_name, payload, timestamp_ns, starts_group))


def read_every_spelling(inputs: LinkInputDataReader) -> None:
    payload = inputs.read("video")
    assert_type(payload, "bytes | None")

    frame = inputs.read("video", into=VideoFrame)
    assert_type(frame, "VideoFrame | None")
    assert frame is not None and frame.width == 1280

    record = inputs.read("video", into=msgpack_typed_dict_cast(DetectionRecord))
    assert_type(record, "DetectionRecord | None")

    payload_and_stamp = inputs.read_with_timestamp("video")
    assert_type(payload_and_stamp, "tuple[bytes, int] | tuple[None, None]")

    frame_and_link = inputs.read_from_inbound_link("video", into=VideoFrame)
    assert_type(frame_and_link, "tuple[VideoFrame, str] | None")

    payload_and_link = inputs.read_from_inbound_link("video")
    assert_type(payload_and_link, "tuple[bytes, str] | None")

    message = inputs.read_message("video")
    assert_type(message, "Message[bytes] | None")
    assert message is not None
    assert_type(message.starts_group, bool)
    assert_type(message.lost_since_last_read, int)

    frame_message = inputs.read_message("video", into=VideoFrame)
    assert_type(frame_message, "Message[VideoFrame] | None")
    assert frame_message is not None and frame_message.payload.width == 1280


def write_every_spelling(outputs: LinkOutputDataWriter, frame: VideoFrame) -> None:
    outputs.write("video", frame.to_bytes())
    outputs.write("video", pack(FRAME_ENTRIES), timestamp_ns=5)
    outputs.write("video", b"\x00", timestamp_ns=6, starts_group=False)
    outputs.write("video", FRAME_ENTRIES)  # type: ignore[arg-type]


def refuse_a_read_target_without_from_bytes(inputs: LinkInputDataReader) -> None:
    inputs.read("video", into=DetectionRecord)  # type: ignore[arg-type]


def refuse_a_cast_that_reads_another_type(inputs: LinkInputDataReader) -> None:
    block: "AudioBlock | None" = inputs.read("video", into=VideoFrame)  # type: ignore[assignment]
    del block


def test_every_read_spelling_runs_over_a_payload():
    port = InMemoryPortStandingInForTheRuntimes(pack(FRAME_ENTRIES))

    read_every_spelling(cast(LinkInputDataReader, port))


def test_every_write_spelling_hands_the_payload_on_as_bytes():
    port = InMemoryPortStandingInForTheRuntimes(pack(FRAME_ENTRIES))
    frame = VideoFrame(**FRAME_ENTRIES)

    write_every_spelling(cast(LinkOutputDataWriter, port), frame)

    assert [(timestamp_ns, starts_group) for _, _, timestamp_ns, starts_group in port.written] == [
        (None, True),
        (5, True),
        (6, False),
        (None, True),
    ]
    assert VideoFrame.from_bytes(port.written[0][1]) == frame

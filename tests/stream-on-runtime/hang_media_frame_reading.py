# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reading an encoder port's media frame from Python the way a moq-dev player
does: hang's legacy container, a QUIC varint timestamp in microseconds (RFC
9000 §16) and then the codec payload."""


from collections.abc import Buffer


def split_hang_legacy_container_media_frame(media_frame: Buffer) -> tuple[int, bytes]:
    """The varint timestamp in microseconds, and the codec payload after it."""
    payload = memoryview(media_frame).cast("B")
    if not payload:
        raise ValueError("an empty message is not a media frame")
    varint_length = 1 << (payload[0] >> 6)
    if len(payload) < varint_length:
        raise ValueError(
            f"a {len(payload)}-byte message ends inside its {varint_length}-byte varint"
        )
    timestamp_microseconds = payload[0] & 0x3F
    for varint_byte in payload[1:varint_length]:
        timestamp_microseconds = (timestamp_microseconds << 8) | varint_byte
    return timestamp_microseconds, bytes(payload[varint_length:])

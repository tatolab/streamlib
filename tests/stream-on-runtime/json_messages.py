# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Free-form JSON messages for the fixtures whose test reads every message in order.

A bundle travels as a stored value, so a reader takes only its newest. A fixture that needs
every message, with its stamp and its loss count, writes each JSON object as the bytes of
one unit of a plain byte stream instead, and reads it off an `InputPin[JsonMessagesInOrder]`.
"""

import json
from typing import Annotated

from tatolab.stream import EncodedVideo, StreamDelivery

#: An input that takes every message in order.
JsonMessagesInOrder = Annotated[EncodedVideo, StreamDelivery("ordered")]


def json_bytes(written: "dict[str, object]") -> bytes:
    """The bytes a JSON message is written as."""
    return json.dumps(written).encode()


def json_read(unit) -> "dict":
    """The JSON object a Packets unit read off a `JsonMessagesInOrder` input carries."""
    return json.loads(bytes(unit.payload))

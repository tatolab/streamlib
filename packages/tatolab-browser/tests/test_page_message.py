# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `PageMessage` travels as one JSON object, the shape the page and the node's native half read."""

import json

import pytest

from tatolab.browser.page_message import PageMessage


def test_a_message_travels_as_its_json_object_and_comes_back_whole() -> None:
    message = PageMessage({"title": "Jane Doe", "variant": 2, "live": True, "tags": ["a", None]})

    payload = message.to_bytes()

    assert json.loads(payload) == {"title": "Jane Doe", "variant": 2, "live": True, "tags": ["a", None]}
    assert PageMessage.from_bytes(payload) == message


def test_bytes_that_are_not_one_json_object_are_refused() -> None:
    with pytest.raises(ValueError, match="one JSON object"):
        PageMessage.from_bytes(b"[1, 2]")


def test_a_float_json_cannot_hold_is_refused_rather_than_sent() -> None:
    with pytest.raises(ValueError, match="not JSON compliant"):
        PageMessage({"ratio": float("nan")}).to_bytes()


def test_any_mapping_travels_as_its_json_object_nested_or_not() -> None:
    from collections import UserDict
    from types import MappingProxyType

    message = PageMessage(MappingProxyType({"speaker": UserDict({"name": "Ada"})}))

    assert json.loads(message.to_bytes()) == {"speaker": {"name": "Ada"}}

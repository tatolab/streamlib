# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Regression tests for `PortKey.from_str` over-length handling (#1416).

The Rust `PortKey::new` and this Python ctypes mirror both refuse a port /
channel name longer than the fixed wire capacity instead of silently
truncating it. A clipped name used to route frames to a different (shorter)
port than the one the author named — a silent cross-wiring defect.
"""

from __future__ import annotations

import pytest

from streamlib.frame_payload import MAX_PORT_KEY_SIZE, PortKey

MAX_NAME_BYTES = MAX_PORT_KEY_SIZE - 1


def test_port_key_accepts_max_length_name():
    name = "a" * MAX_NAME_BYTES
    key = PortKey.from_str(name)
    assert key.as_str() == name


def test_port_key_rejects_over_length_name_instead_of_truncating():
    # Mental-revert guard: restore the old `min(len(data), 63)` truncation and
    # this fails — `from_str` would succeed and `as_str()` would return the
    # clipped 63-byte prefix rather than raising.
    over = "b" * (MAX_NAME_BYTES + 1)
    assert len(over.encode("utf-8")) == 64
    with pytest.raises(ValueError, match="exceeding the fixed wire capacity"):
        PortKey.from_str(over)

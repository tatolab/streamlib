# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for the engine↔SDK subprocess protocol-version handshake.

The handshake replaces the old compatibility-by-injection guarantee now that
`streamlib` is resolved from a registry by version. These lock the SDK side of
the gate: the SDK refuses to run against an engine protocol it can't speak.
Mentally revert `assert_engine_compatible` to a no-op and the rejection cases
go green for the wrong reason — so they lock the gate, not just exercise it.
"""

import pytest

import streamlib
from streamlib import _protocol
from streamlib._protocol import (
    MIN_ENGINE_PROTOCOL,
    PROTOCOL_VERSION,
    ProtocolMismatchError,
    assert_engine_compatible,
    engine_protocol_from_env,
)


def test_protocol_version_is_exported_and_consistent():
    # The handshake coordinate is public on the package surface and matches the
    # module constant the runner asserts with.
    assert streamlib.PROTOCOL_VERSION == PROTOCOL_VERSION
    assert MIN_ENGINE_PROTOCOL <= PROTOCOL_VERSION


def test_accepts_engine_versions_in_range():
    # The engine's current and minimum protocol versions are both speakable.
    assert_engine_compatible(PROTOCOL_VERSION)
    assert_engine_compatible(MIN_ENGINE_PROTOCOL)


def test_rejects_engine_newer_than_sdk():
    with pytest.raises(ProtocolMismatchError):
        assert_engine_compatible(PROTOCOL_VERSION + 1)


def test_rejects_engine_older_than_min():
    if MIN_ENGINE_PROTOCOL > 0:
        with pytest.raises(ProtocolMismatchError):
            assert_engine_compatible(MIN_ENGINE_PROTOCOL - 1)


def test_env_reader_requires_the_var(monkeypatch):
    # A process not launched by a streamlib runtime (or an engine too old to
    # advertise the protocol) has no env var → refuse, don't guess a version.
    monkeypatch.delenv(_protocol.ENGINE_PROTOCOL_ENV, raising=False)
    with pytest.raises(ProtocolMismatchError):
        engine_protocol_from_env()


def test_env_reader_rejects_non_integer(monkeypatch):
    monkeypatch.setenv(_protocol.ENGINE_PROTOCOL_ENV, "not-a-number")
    with pytest.raises(ProtocolMismatchError):
        engine_protocol_from_env()


def test_env_reader_parses_integer(monkeypatch):
    monkeypatch.setenv(_protocol.ENGINE_PROTOCOL_ENV, str(PROTOCOL_VERSION))
    assert engine_protocol_from_env() == PROTOCOL_VERSION

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Regression tests for `subprocess_runner.main()` cleanup-once invariant (#469).

Pre-fix, the outer `cmd == "teardown"` branch called `_cleanup_native()` and
then `break`, which fell through to a redundant trailing `_cleanup_native()`
at the end of `main()`. The second call invoked `slpn_surface_disconnect` /
`slpn_context_destroy` on already-freed pointers, segfaulting the subprocess
on every teardown.

These tests script the lifecycle messages the host would send (setup → stop →
teardown), mock the FFI lib so the cleanup symbols are countable, and assert
each FFI cleanup symbol is called *exactly once*. Reverting the fix (re-adding
the inline `_cleanup_native(...)` in the outer teardown branch) makes both
counts equal 2 and these tests fail.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from streamlib import subprocess_runner


# ============================================================================
# Mock FFI lib — counts the cleanup symbols
# ============================================================================


class _MockNativeLib:
    """Stand-in for `ctypes.CDLL(libstreamlib_python_native.so)` in tests.

    Records every cleanup call so the test can assert exactly-once semantics.
    Returns sentinel non-zero pointers from create/connect so the runner's
    `if native_lib and native_ctx_ptr` guards behave like the real flow.
    """

    CTX_PTR = 0xC0FFEE_CAFE
    HANDLE_PTR = 0xBEEF_DEAD

    def __init__(self) -> None:
        self.disconnect_calls: list[int] = []
        self.destroy_calls: list[int] = []

    # Lifecycle
    def slpn_context_create(self, _processor_id: bytes) -> int:
        return self.CTX_PTR

    def slpn_context_destroy(self, ctx: int) -> None:
        self.destroy_calls.append(ctx)

    def slpn_surface_connect(self, _endpoint: bytes, _runtime_id: bytes) -> int:
        return self.HANDLE_PTR

    def slpn_surface_disconnect(self, handle: int) -> None:
        self.disconnect_calls.append(handle)

    # I/O wiring (the runner's setup walks input/output port lists)
    def slpn_input_subscribe(self, _ctx: int, _service: bytes) -> int:
        return 0

    def slpn_input_set_read_mode(self, _ctx: int, _port: bytes, _mode: int) -> None:
        pass

    def slpn_output_publish(self, *_args: Any) -> int:
        return 0


class _NoopProcessor:
    """Processor stub — every lifecycle hook is a no-op so the test exercises
    only the runner's dispatch logic."""

    def setup(self, _ctx: Any) -> None:
        pass

    def stop(self, _ctx: Any) -> None:
        pass

    def teardown(self, _ctx: Any) -> None:
        pass


# ============================================================================
# Fixtures
# ============================================================================


@pytest.fixture
def env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Set the env vars `main()` reads before it dispatches."""
    monkeypatch.setenv("STREAMLIB_ENTRYPOINT", "tests.fake:Fake")
    monkeypatch.setenv("STREAMLIB_PROJECT_PATH", "")
    monkeypatch.setenv("STREAMLIB_PYTHON_NATIVE_LIB", "/tmp/fake-libstreamlib_python_native.so")
    monkeypatch.setenv("STREAMLIB_PROCESSOR_ID", "test-469")
    monkeypatch.setenv("STREAMLIB_EXECUTION_MODE", "reactive")
    monkeypatch.setenv("STREAMLIB_RUNTIME_ID", "runtime-469")
    monkeypatch.setenv("STREAMLIB_ESCALATE_FD", "999")  # never actually opened
    # Force the surface-share connect path so handle_ptr is populated.
    monkeypatch.setenv("STREAMLIB_SURFACE_SOCKET", "/tmp/fake-surface.sock")


def _patch_runner(
    monkeypatch: pytest.MonkeyPatch,
    mock_lib: _MockNativeLib,
    scripted_messages: list[dict[str, Any]],
) -> None:
    """Replace every external dependency of `main()` so it dispatches the
    scripted messages against the mock FFI without touching real fds."""

    # Stub the escalate fd / channel — the runner never actually reads from
    # them in this test because we replace `bridge_read_message` below.
    class _DummyStream:
        def write(self, _b: bytes) -> int:
            return 0

        def flush(self) -> None:
            pass

        def read(self, _n: int) -> bytes:
            return b""

    class _DummySocket:
        def close(self) -> None:
            pass

    def _fake_open_escalate_fd_stream() -> tuple[Any, Any, Any]:
        return _DummyStream(), _DummyStream(), _DummySocket()

    monkeypatch.setattr(
        subprocess_runner, "_open_escalate_fd_stream", _fake_open_escalate_fd_stream
    )

    class _DummyChannel:
        def __init__(self, *_a: Any, **_kw: Any) -> None:
            pass

        def has_deferred_lifecycle_messages(self) -> bool:
            return False

    monkeypatch.setattr(subprocess_runner, "EscalateChannel", _DummyChannel)
    monkeypatch.setattr(subprocess_runner, "install_channel", lambda _c: None)

    # Logging — the runner installs interceptors that hijack stdout. Skip them.
    monkeypatch.setattr(subprocess_runner.log, "set_processor_id", lambda _p: None)
    monkeypatch.setattr(subprocess_runner.log, "install", lambda _c: None)
    monkeypatch.setattr(subprocess_runner.log, "shutdown", lambda: None)

    monkeypatch.setattr(
        subprocess_runner, "_load_processor_class", lambda _e, _p: _NoopProcessor
    )
    # `_setup_native_state` calls `load_native_lib` from the
    # `processor_context` module — patch the import the runner re-exported
    # *and* the symbol on the source module so both lookups resolve to the
    # mock.
    import streamlib.processor_context as pc

    monkeypatch.setattr(subprocess_runner, "load_native_lib", lambda _path: mock_lib)
    monkeypatch.setattr(pc, "load_native_lib", lambda _path: mock_lib)

    # Script the bridge message sequence.
    msg_iter = iter(scripted_messages)

    def _fake_read(_stdin: Any) -> dict[str, Any]:
        try:
            return next(msg_iter)
        except StopIteration as e:
            # Loop is supposed to exit before we run out — surfacing as
            # EOFError mimics the host closing stdin.
            raise EOFError("scripted messages exhausted") from e

    sent: list[dict[str, Any]] = []

    def _fake_send(_stdout: Any, msg: dict[str, Any]) -> None:
        sent.append(msg)

    monkeypatch.setattr(subprocess_runner, "bridge_read_message", _fake_read)
    monkeypatch.setattr(subprocess_runner, "bridge_send_message", _fake_send)


# ============================================================================
# Tests
# ============================================================================


def test_cleanup_called_once_on_setup_then_teardown(
    env: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Outer-loop teardown path: cleanup runs in the `finally` block exactly
    once. Pre-fix this branch called cleanup inline AND fell through to a
    second cleanup call after the loop, double-freeing both pointers."""
    mock_lib = _MockNativeLib()
    _patch_runner(
        monkeypatch,
        mock_lib,
        scripted_messages=[
            {
                "cmd": "setup",
                "capability": "full",
                "config": {},
                "ports": {"inputs": [], "outputs": []},
            },
            {"cmd": "teardown", "capability": "full"},
        ],
    )

    # main() returns normally on a clean teardown — no SystemExit expected.
    subprocess_runner.main()

    assert mock_lib.disconnect_calls == [
        _MockNativeLib.HANDLE_PTR
    ], "slpn_surface_disconnect must be called exactly once"
    assert mock_lib.destroy_calls == [
        _MockNativeLib.CTX_PTR
    ], "slpn_context_destroy must be called exactly once"


def test_cleanup_called_once_on_setup_stop_teardown(
    env: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The exact host-issued sequence the live scenario triggers: stop is
    sent first, then teardown. Pre-fix this hit the same fall-through bug
    because stop didn't enter (or had already exited) the run loop and
    teardown was processed by the outer dispatch."""
    mock_lib = _MockNativeLib()
    _patch_runner(
        monkeypatch,
        mock_lib,
        scripted_messages=[
            {
                "cmd": "setup",
                "capability": "full",
                "config": {},
                "ports": {"inputs": [], "outputs": []},
            },
            {"cmd": "stop", "capability": "full"},
            {"cmd": "teardown", "capability": "full"},
        ],
    )

    subprocess_runner.main()

    assert mock_lib.disconnect_calls == [_MockNativeLib.HANDLE_PTR]
    assert mock_lib.destroy_calls == [_MockNativeLib.CTX_PTR]


def test_cleanup_skipped_when_setup_never_ran(
    env: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    """If the host closes stdin before sending `setup`, no FFI handles were
    ever allocated — cleanup must be a no-op rather than calling
    `slpn_*_destroy(NULL)` (which the FFI tolerates, but the runner avoids)."""
    mock_lib = _MockNativeLib()
    _patch_runner(
        monkeypatch,
        mock_lib,
        scripted_messages=[],  # immediate EOF
    )

    subprocess_runner.main()

    assert mock_lib.disconnect_calls == []
    assert mock_lib.destroy_calls == []

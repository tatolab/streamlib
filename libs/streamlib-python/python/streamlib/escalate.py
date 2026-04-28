# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot GPU escalation channel — subprocess → host IPC.

A Python processor running in a subprocess only sees a
``GpuContextLimitedAccess`` sandbox (no raw allocations). When it needs the
privileged ``GpuContextFullAccess`` surface — e.g. to acquire a new-shape
pixel buffer mid-stream — it sends an ``escalate_request`` to the Rust host
over the subprocess's stdout, and the host replies with an
``escalate_response`` on stdin. The host runs the work inside
``GpuContextLimitedAccess::escalate``, which serializes against every other
escalation in the runtime.

This module is small on purpose: it owns the request-id bookkeeping and the
deferred-lifecycle buffer that lets ``EscalateChannel.request()`` step over
any lifecycle commands (``on_pause``, ``stop``, …) that happen to arrive
while it's blocked waiting for its correlated response. The outer
``subprocess_runner`` loop drains those buffered messages through
``EscalateChannel.take_deferred_lifecycle_messages()`` before polling stdin
again.
"""

from __future__ import annotations

import threading
import uuid
from typing import Any, Dict, List, Optional, Sequence

from .processor_context import bridge_read_message, bridge_send_message


ESCALATE_REQUEST_RPC = "escalate_request"
ESCALATE_RESPONSE_RPC = "escalate_response"


class EscalateError(RuntimeError):
    """Raised when the host returns an ``Err`` escalate response."""


class EscalateChannel:
    """Synchronous request/response channel over the subprocess's stdio pipes.

    The channel is single-flight: only one escalate request is in flight at
    a time, matching the single-threaded structure of the Python subprocess
    runner. Lifecycle messages that the host sends while we're waiting on a
    correlated response are captured in ``_deferred_lifecycle`` and drained
    by the outer loop through
    :meth:`take_deferred_lifecycle_messages`.
    """

    def __init__(self, stdin, stdout) -> None:
        self._stdin = stdin
        self._stdout = stdout
        self._send_lock = threading.Lock()
        self._deferred_lifecycle: List[Dict[str, Any]] = []

    # -------------------- public SDK surface --------------------

    def acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> Dict[str, Any]:
        """Request a new-shape pixel buffer from the host.

        Returns the ``ok``-payload dict; on failure raises
        :class:`EscalateError`.
        """
        return self.request(
            {
                "op": "acquire_pixel_buffer",
                "width": int(width),
                "height": int(height),
                "format": format,
            }
        )

    def acquire_texture(
        self,
        width: int,
        height: int,
        format: str,
        usage: Sequence[str],
    ) -> Dict[str, Any]:
        """Request a pooled GPU texture from the host.

        ``format`` is a wire token such as ``"rgba8_unorm"``,
        ``"bgra8_unorm_srgb"``, ``"rgba16_float"``, ``"nv12"``.
        ``usage`` is a non-empty iterable of tokens drawn from
        ``copy_src``, ``copy_dst``, ``texture_binding``, ``storage_binding``,
        ``render_attachment``.

        Returns the ``ok``-payload dict; on failure raises
        :class:`EscalateError`.
        """
        usage_list = [str(u) for u in usage]
        if not usage_list:
            raise ValueError("acquire_texture: usage must not be empty")
        return self.request(
            {
                "op": "acquire_texture",
                "width": int(width),
                "height": int(height),
                "format": format,
                "usage": usage_list,
            }
        )

    def run_cpu_readback_copy(
        self, surface_id: int, direction: str
    ) -> Dict[str, Any]:
        """Trigger the host-side cpu-readback copy for an already-registered
        surface. ``direction`` is ``"image_to_buffer"`` (host runs
        `vkCmdCopyImageToBuffer` so the consumer can read the latest
        image bytes) or ``"buffer_to_image"`` (host runs
        `vkCmdCopyBufferToImage` so the consumer's writes land back in
        the source `VkImage`).

        Returns the ``ok``-payload dict, which includes
        ``timeline_value`` — the decimal-string `u64` the host-adapter
        signaled on its shared timeline semaphore. The consumer is
        expected to wait on the imported timeline at this value before
        reading / after writing the staging buffer's mapped bytes.

        Blocking — on host-side contention this call waits until the
        adapter can dispatch the copy. Use :meth:`try_run_cpu_readback_copy`
        to probe-and-skip instead.
        """
        if direction not in ("image_to_buffer", "buffer_to_image"):
            raise ValueError(
                "run_cpu_readback_copy: direction must be 'image_to_buffer' "
                f"or 'buffer_to_image', got {direction!r}"
            )
        return self.request(
            {
                "op": "run_cpu_readback_copy",
                "surface_id": str(int(surface_id)),
                "direction": direction,
            }
        )

    def try_run_cpu_readback_copy(
        self, surface_id: int, direction: str
    ) -> Optional[Dict[str, Any]]:
        """Non-blocking variant of :meth:`run_cpu_readback_copy`.

        Returns the same ``ok``-payload dict on success. Returns
        ``None`` when the host's cpu-readback bridge reports the
        surface as contended; raises :class:`EscalateError` for hard
        failures.
        """
        if direction not in ("image_to_buffer", "buffer_to_image"):
            raise ValueError(
                "try_run_cpu_readback_copy: direction must be 'image_to_buffer' "
                f"or 'buffer_to_image', got {direction!r}"
            )
        return self.request(
            {
                "op": "try_run_cpu_readback_copy",
                "surface_id": str(int(surface_id)),
                "direction": direction,
            },
            allow_contended=True,
        )

    def release_handle(self, handle_id: str) -> Dict[str, Any]:
        """Tell the host to drop its strong reference to ``handle_id``."""
        return self.request(
            {
                "op": "release_handle",
                "handle_id": handle_id,
            }
        )

    # -------------------- core request/response --------------------

    def request(
        self, op: Dict[str, Any], *, allow_contended: bool = False
    ) -> Optional[Dict[str, Any]]:
        """Send an escalate request and block until the correlated response.

        When ``allow_contended`` is true, a ``"contended"`` response is
        returned as ``None`` instead of raising. Used by
        :meth:`try_acquire_cpu_readback` and any future ``try_*`` op that
        opts into the contended-skip shape — every other op still treats
        contention as a protocol violation (raises
        :class:`EscalateError`) so a buggy host can't silently degrade
        an op that was supposed to be blocking.
        """
        request_id = str(uuid.uuid4())
        req = {"rpc": ESCALATE_REQUEST_RPC, "request_id": request_id, **op}
        with self._send_lock:
            bridge_send_message(self._stdout, req)
            return self._await_response(request_id, allow_contended=allow_contended)

    def log_fire_and_forget(self, payload: Dict[str, Any]) -> None:
        """Send a fire-and-forget escalate op (currently `log`).

        No response correlation — the host enqueues the record into the
        unified logging pathway and returns nothing. `bridge_send_message`
        is already frame-atomic via its module lock, so no additional
        synchronization is required here.
        """
        req = {"rpc": ESCALATE_REQUEST_RPC, **payload}
        bridge_send_message(self._stdout, req)

    def _await_response(
        self, request_id: str, *, allow_contended: bool = False
    ) -> Optional[Dict[str, Any]]:
        while True:
            msg = bridge_read_message(self._stdin)
            rpc = msg.get("rpc", "")
            if rpc == ESCALATE_RESPONSE_RPC and msg.get("request_id") == request_id:
                result = msg.get("result")
                if result == "ok":
                    return msg
                if result == "contended":
                    if allow_contended:
                        return None
                    raise EscalateError(
                        "escalate returned contended for an op that does not allow it"
                    )
                raise EscalateError(msg.get("message") or "escalate failed")
            # Any other message during our blocking read is a lifecycle
            # command (stop / teardown / on_pause / on_resume / update_config).
            # Defer it so the outer loop consumes it in FIFO order.
            self._deferred_lifecycle.append(msg)

    # -------------------- lifecycle cooperation --------------------

    def take_deferred_lifecycle_messages(self) -> List[Dict[str, Any]]:
        """Drain and return buffered lifecycle messages, FIFO."""
        out = self._deferred_lifecycle
        self._deferred_lifecycle = []
        return out

    def has_deferred_lifecycle_messages(self) -> bool:
        return bool(self._deferred_lifecycle)


_channel_singleton: Optional[EscalateChannel] = None


def install_channel(channel: EscalateChannel) -> None:
    """Install the process-wide escalate channel.

    Called once by ``subprocess_runner.main`` after it opens the stdio pipes.
    Subsequent calls replace the channel, which is only sensible in test
    setups.
    """
    global _channel_singleton
    _channel_singleton = channel


def channel() -> EscalateChannel:
    """Return the process-wide escalate channel.

    Raises ``RuntimeError`` if :func:`install_channel` hasn't been called
    yet — that only happens when processor code runs outside the normal
    subprocess_runner lifecycle (e.g. bare unit tests without a host).
    """
    if _channel_singleton is None:
        raise RuntimeError(
            "escalate channel not installed — ctx.escalate is only available "
            "inside the subprocess lifecycle"
        )
    return _channel_singleton

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot GPU escalation channel — subprocess → host IPC.

A Python processor running in a subprocess only sees a
``GpuContextLimitedAccess`` sandbox (no raw allocations). When it needs the
privileged ``GpuContextFullAccess`` surface — e.g. to acquire a new-shape
pixel buffer mid-stream — it sends an ``escalate_request`` to the Rust host
over the dedicated escalate socketpair, and the host replies with an
``escalate_response``. The host runs the work inside
``GpuContextLimitedAccess::escalate``, which serializes against every other
escalation in the runtime.

This module owns the request-id bookkeeping and the response demultiplexer
that lets multiple worker threads issue escalate calls concurrently. A
single reader thread (started by :func:`start_reader_thread`) consumes the
escalate fd, routes ``escalate_response`` messages to their waiting caller
by ``request_id``, and forwards every other message into a lifecycle queue
the runner's outer command loop drains.
"""

from __future__ import annotations

import queue
import threading
import uuid
from typing import Any, Dict, Optional, Sequence

from .processor_context import bridge_read_message, bridge_send_message


ESCALATE_REQUEST_RPC = "escalate_request"
ESCALATE_RESPONSE_RPC = "escalate_response"


class EscalateError(RuntimeError):
    """Raised when the host returns an ``Err`` escalate response."""


class _PendingResponse:
    """One slot per in-flight escalate request — Event + landing pad."""

    __slots__ = ("event", "message")

    def __init__(self) -> None:
        self.event = threading.Event()
        self.message: Optional[Dict[str, Any]] = None


class EscalateChannel:
    """Concurrent-safe request/response channel over the escalate socketpair.

    Multiple worker threads may issue concurrent :meth:`request` calls; each
    call registers a per-``request_id`` slot keyed off
    :class:`_PendingResponse` and blocks on its event. The dedicated reader
    thread (see :func:`start_reader_thread`) demuxes incoming
    ``escalate_response`` frames against this map and routes anything else
    into the runner's lifecycle queue.

    Frame writes are serialized by ``bridge_send_message``'s module lock, so
    request payloads never interleave on the wire.
    """

    def __init__(self, stdout) -> None:
        self._stdout = stdout
        self._pending: Dict[str, _PendingResponse] = {}
        self._pending_lock = threading.Lock()
        self._closed = False

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

    def register_compute_kernel(
        self, spv: bytes, push_constant_size: int
    ) -> Dict[str, Any]:
        """Register a compute kernel on the host. Returns the ``ok``-payload
        whose ``handle_id`` is the SHA-256 hex of the SPIR-V — re-registering
        identical SPIR-V hits the host-side cache and returns the same id.

        The host derives the kernel's binding shape from `rspirv-reflect`
        and persists driver-compiled pipeline state to
        ``$STREAMLIB_PIPELINE_CACHE_DIR`` (or ``$XDG_CACHE_HOME/streamlib/
        pipeline-cache``) so first-inference latency after a host process
        restart is fast.

        On failure raises :class:`EscalateError`.
        """
        return self.request(
            {
                "op": "register_compute_kernel",
                "spv_hex": spv.hex(),
                "push_constant_size": int(push_constant_size),
            }
        )

    def run_compute_kernel(
        self,
        kernel_id: str,
        surface_uuid: str,
        push_constants: bytes,
        group_count_x: int,
        group_count_y: int,
        group_count_z: int,
    ) -> Dict[str, Any]:
        """Dispatch a previously-registered compute kernel against the
        surface registered under ``surface_uuid``. Compute is synchronous
        host-side: the call returns once the GPU work has retired, after
        which the consumer can advance its surface-share timeline.

        ``kernel_id`` is the value returned by an earlier
        :meth:`register_compute_kernel` response. ``surface_uuid`` is
        the surface-share UUID under which the host registered the
        target render-target image (the same UUID
        :meth:`VulkanContext.acquire_write` was opened with).
        ``push_constants`` is a `bytes` payload whose length must equal
        the kernel's declared ``push_constant_size``.

        On failure raises :class:`EscalateError`.
        """
        return self.request(
            {
                "op": "run_compute_kernel",
                "kernel_id": kernel_id,
                "surface_uuid": str(surface_uuid),
                "push_constants_hex": push_constants.hex(),
                "group_count_x": int(group_count_x),
                "group_count_y": int(group_count_y),
                "group_count_z": int(group_count_z),
            }
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

        Safe to call from any thread, including concurrently with other
        :meth:`request` calls. The reader thread routes incoming
        ``escalate_response`` frames by ``request_id`` so concurrent calls
        can't steal each other's responses.

        When ``allow_contended`` is true, a ``"contended"`` response is
        returned as ``None`` instead of raising. Used by
        :meth:`try_run_cpu_readback_copy` and any future ``try_*`` op that
        opts into the contended-skip shape — every other op still treats
        contention as a protocol violation (raises
        :class:`EscalateError`) so a buggy host can't silently degrade
        an op that was supposed to be blocking.
        """
        request_id = str(uuid.uuid4())
        slot = _PendingResponse()
        with self._pending_lock:
            if self._closed:
                raise EscalateError("escalate channel is closed")
            self._pending[request_id] = slot

        try:
            req = {"rpc": ESCALATE_REQUEST_RPC, "request_id": request_id, **op}
            bridge_send_message(self._stdout, req)
            slot.event.wait()
            msg = slot.message
        finally:
            with self._pending_lock:
                self._pending.pop(request_id, None)

        if msg is None:
            raise EscalateError("escalate channel closed before response arrived")
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

    def log_fire_and_forget(self, payload: Dict[str, Any]) -> None:
        """Send a fire-and-forget escalate op (currently `log`).

        No response correlation — the host enqueues the record into the
        unified logging pathway and returns nothing. `bridge_send_message`
        is already frame-atomic via its module lock, so no additional
        synchronization is required here.
        """
        req = {"rpc": ESCALATE_REQUEST_RPC, **payload}
        bridge_send_message(self._stdout, req)

    # -------------------- demux + shutdown --------------------

    def deliver_response(self, msg: Dict[str, Any]) -> bool:
        """Route an incoming ``escalate_response`` to its waiting caller.

        Called from the reader thread for every frame where
        ``rpc == ESCALATE_RESPONSE_RPC``. Returns ``True`` when the
        response was delivered to a registered waiter, ``False`` if the
        ``request_id`` is unknown (orphaned / late response — dropped).
        """
        request_id = msg.get("request_id")
        if not isinstance(request_id, str):
            return False
        with self._pending_lock:
            slot = self._pending.get(request_id)
        if slot is None:
            return False
        slot.message = msg
        slot.event.set()
        return True

    def close(self) -> None:
        """Wake every in-flight ``request`` with a closed-channel error.

        Called from the reader thread when the escalate fd reaches EOF, or
        from the runner during shutdown. Idempotent.
        """
        with self._pending_lock:
            self._closed = True
            pending = list(self._pending.values())
            self._pending.clear()
        for slot in pending:
            slot.message = None
            slot.event.set()


# ============================================================================
# Reader thread — demultiplexes the escalate fd
# ============================================================================


class BridgeReaderThread:
    """Background thread that owns the escalate fd reader.

    Splits incoming frames between two consumers:

    - ``escalate_response`` frames go to :meth:`EscalateChannel.deliver_response`.
    - everything else goes to ``lifecycle_queue`` for the runner's outer
      command loop to dispatch.

    The thread exits on EOF (host closed the fd) or when ``stop()`` is
    called. EOF is signaled to the lifecycle queue with a sentinel so the
    runner can shut down.
    """

    EOF_SENTINEL: Dict[str, Any] = {"__bridge_eof__": True}

    def __init__(
        self,
        stdin,
        escalate_channel: EscalateChannel,
        lifecycle_queue: "queue.Queue[Dict[str, Any]]",
    ) -> None:
        self._stdin = stdin
        self._escalate_channel = escalate_channel
        self._lifecycle_queue = lifecycle_queue
        self._thread: Optional[threading.Thread] = None
        self._stop = threading.Event()

    def start(self) -> None:
        """Start the reader thread. Idempotent — second call is a no-op."""
        if self._thread is not None:
            return
        self._thread = threading.Thread(
            target=self._loop,
            name="streamlib-bridge-reader",
            daemon=True,
        )
        self._thread.start()

    def stop(self, *, join_timeout: float = 1.0) -> None:
        """Request the reader to exit and join it."""
        self._stop.set()
        thread = self._thread
        if thread is not None and thread.is_alive():
            thread.join(timeout=join_timeout)

    def _loop(self) -> None:
        try:
            while not self._stop.is_set():
                try:
                    msg = bridge_read_message(self._stdin)
                except EOFError:
                    break
                except Exception:
                    # Defensive: a malformed frame shouldn't kill the runner;
                    # log via the standard channel and keep going. The host's
                    # bridge writer is the source of truth — drift here means
                    # something upstream is wrong, but we don't want to mask
                    # the issue by silently exiting.
                    from . import log

                    log.error(
                        "bridge reader: malformed frame; continuing",
                    )
                    continue

                if msg.get("rpc") == ESCALATE_RESPONSE_RPC:
                    if not self._escalate_channel.deliver_response(msg):
                        # Late / orphan response — log and drop.
                        from . import log

                        log.warn(
                            "bridge reader: orphan escalate response",
                            request_id=msg.get("request_id"),
                        )
                    continue

                # Lifecycle command (or unknown rpc) — hand off to the runner.
                self._lifecycle_queue.put(msg)
        finally:
            # Wake every in-flight escalate caller so they don't deadlock,
            # and signal the runner that no more lifecycle messages will
            # arrive on this channel.
            self._escalate_channel.close()
            self._lifecycle_queue.put(self.EOF_SENTINEL)


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

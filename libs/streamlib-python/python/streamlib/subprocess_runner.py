# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# streamlib:lint-logging:allow-file — subprocess bootstrap; writes to sys.stderr before the log pipeline is installed

"""Subprocess runner for Python processors (native FFI mode).

Entry point for Python subprocess processors spawned by the Rust runtime.
Lifecycle commands (setup/run/stop/teardown) use length-prefixed JSON over
a dedicated Unix-domain socketpair advertised via STREAMLIB_ESCALATE_FD;
fd1/fd2 stay as free log pipes that the host captures as `intercepted`
records in the unified JSONL. Data I/O uses direct iceoryx2 FFI via
libstreamlib_python_native.

A single :class:`BridgeReaderThread` owns the escalate fd: it demultiplexes
``escalate_response`` frames into their waiting caller (any thread that
called ``ctx.escalate_*``) and forwards every other frame into a
lifecycle queue this runner drains. That decoupling is what lets manual-
mode worker threads issue concurrent escalate calls while the runner's
main thread keeps draining lifecycle commands (#604).

Usage:
    python -m streamlib.subprocess_runner

Environment variables:
    STREAMLIB_ENTRYPOINT: e.g., "passthrough_processor:PassthroughProcessor"
    STREAMLIB_PROJECT_PATH: Path to Python processor project
    STREAMLIB_PYTHON_NATIVE_LIB: Path to libstreamlib_python_native.dylib
    STREAMLIB_PROCESSOR_ID: Unique processor ID
    STREAMLIB_EXECUTION_MODE: "reactive", "continuous", or "manual"
    STREAMLIB_ESCALATE_FD: Inherited child-end fd of the escalate IPC
        socketpair (decimal). The host sets this before spawn.
"""

import importlib
import os
import queue
import select
import socket
import sys
import time
import traceback

from . import clock, log
from .escalate import BridgeReaderThread, EscalateChannel, install_channel
from .processor_context import (
    NativeProcessorState,
    NativeRuntimeContextFullAccess,
    NativeRuntimeContextLimitedAccess,
    bridge_send_message,
    compute_read_buf_bytes,
    load_native_lib,
)


def _open_escalate_fd_stream():
    """Resolve STREAMLIB_ESCALATE_FD and return `(read_stream, write_stream, socket)`.

    Both streams wrap the inherited socketpair fd; the `socket` return is
    kept alive so the underlying fd stays open for the life of the
    subprocess. Exits with code 1 if the env var is missing or unparseable
    — the host always sets it, so a missing value indicates a spawn-path
    regression.
    """
    raw = os.environ.get("STREAMLIB_ESCALATE_FD")
    if not raw:
        sys.stderr.write(
            "[streamlib] STREAMLIB_ESCALATE_FD not set — "
            "escalate IPC transport unavailable\n"
        )
        sys.stderr.flush()
        sys.exit(1)
    try:
        fd = int(raw)
    except ValueError:
        sys.stderr.write(
            f"[streamlib] STREAMLIB_ESCALATE_FD is not an integer: {raw!r}\n"
        )
        sys.stderr.flush()
        sys.exit(1)

    sock = socket.socket(fileno=fd)
    reader = sock.makefile("rb", buffering=0)
    writer = sock.makefile("wb", buffering=0)
    return reader, writer, sock


def _load_processor_class(entrypoint: str, project_path: str):
    """Load a processor class from an entrypoint string."""
    if project_path and project_path not in sys.path:
        sys.path.insert(0, project_path)

    module_path, class_name = entrypoint.rsplit(":", 1)
    module = importlib.import_module(module_path)
    return getattr(module, class_name)


def _setup_native_state(msg, native_lib_path, processor_id, escalate_channel=None):
    """Set up native FFI state with iceoryx2 subscriptions and publishers."""
    config = msg.get("config")
    ports = msg.get("ports", {})

    lib = load_native_lib(native_lib_path)

    # Wire the cdylib for streamlib.clock.MonotonicTimer. Done here, before
    # any processor lifecycle method runs, so user `start()` callbacks can
    # construct timers freely.
    clock.install_timerfd(lib)

    # Create native context
    ctx_ptr = lib.slpn_context_create(processor_id.encode("utf-8"))
    if not ctx_ptr:
        raise RuntimeError("Failed to create native context")

    # Subscribe to input iceoryx2 services
    inputs = ports.get("inputs", [])
    read_buf_bytes = compute_read_buf_bytes(inputs)
    for inp in inputs:
        port_name = inp["name"]
        service_name = inp["service_name"]
        read_mode = inp.get("read_mode", "skip_to_latest")
        log.info(
            "Subscribing to input",
            port=port_name,
            service=service_name,
            read_mode=read_mode,
            max_payload_bytes=inp.get("max_payload_bytes"),
        )
        result = lib.slpn_input_subscribe(ctx_ptr, service_name.encode("utf-8"))
        if result != 0:
            log.error("Failed to subscribe to input", service=service_name)
        # Configure per-port read mode (0 = skip_to_latest, 1 = read_next_in_order)
        mode_int = 0 if read_mode == "skip_to_latest" else 1
        lib.slpn_input_set_read_mode(ctx_ptr, port_name.encode("utf-8"), mode_int)

    # All inputs share one destination-paired Notify service. Pick the first
    # non-empty notify_service_name and subscribe (slpn_event_subscribe is
    # idempotent so repeating it is harmless).
    notify_service_name = next(
        (inp.get("notify_service_name", "") for inp in inputs
         if inp.get("notify_service_name")),
        "",
    )
    if notify_service_name:
        result = lib.slpn_event_subscribe(
            ctx_ptr, notify_service_name.encode("utf-8"),
        )
        if result != 0:
            log.warn(
                "Failed to subscribe to notify service",
                service=notify_service_name,
            )

    # Create publishers for output iceoryx2 services
    for out in ports.get("outputs", []):
        port_name = out["name"]
        dest_port = out["dest_port"]
        dest_service = out["dest_service_name"]
        schema_name = out.get("schema_name", "")
        dest_notify_service = out.get("dest_notify_service_name", "")
        log.info(
            "Publishing to output",
            port=port_name,
            dest=dest_port,
            service=dest_service,
            notify_service=dest_notify_service or None,
        )
        result = lib.slpn_output_publish(
            ctx_ptr,
            dest_service.encode("utf-8"),
            port_name.encode("utf-8"),
            dest_port.encode("utf-8"),
            schema_name.encode("utf-8"),
            out.get("max_payload_bytes", 65536),
            dest_notify_service.encode("utf-8"),
        )
        if result != 0:
            log.error("Failed to create publisher", service=dest_service)

    # Connect to the surface-share service for surface resolution.
    #
    # macOS: STREAMLIB_XPC_SERVICE_NAME is the launchd mach-service name.
    # Linux: STREAMLIB_SURFACE_SOCKET is the Unix-socket path the per-runtime
    #        service listens on. Both endpoints funnel through the same FFI
    #        entry (`slpn_surface_connect`) — the native lib's
    #        platform-specific surface_client module interprets the C string
    #        accordingly.
    handle_ptr = None
    if sys.platform == "darwin":
        surface_endpoint = os.environ.get("STREAMLIB_XPC_SERVICE_NAME", "")
        surface_endpoint_desc = "xpc_service_name"
    else:
        surface_endpoint = os.environ.get("STREAMLIB_SURFACE_SOCKET", "")
        surface_endpoint_desc = "surface_socket"
    runtime_id = os.environ.get("STREAMLIB_RUNTIME_ID", "")
    if surface_endpoint:
        runtime_id_arg = runtime_id.encode("utf-8") if runtime_id else None
        handle_ptr = lib.slpn_surface_connect(
            surface_endpoint.encode("utf-8"), runtime_id_arg
        )
        if handle_ptr:
            log.info(
                "Connected to surface-share service",
                endpoint_kind=surface_endpoint_desc,
                endpoint=surface_endpoint,
                runtime_id=runtime_id,
            )
        else:
            log.warn(
                "Surface-share connect failed",
                endpoint_kind=surface_endpoint_desc,
                endpoint=surface_endpoint,
            )

    state = NativeProcessorState(
        lib, ctx_ptr, config, handle_ptr,
        escalate_channel=escalate_channel,
        read_buf_bytes=read_buf_bytes,
    )
    return lib, ctx_ptr, handle_ptr, state


def _cleanup_native(native_lib, native_ctx_ptr, native_handle_ptr, state=None):
    """Destroy native context and disconnect surface-share handle exactly once.

    The FFI calls (`slpn_surface_disconnect`, `slpn_context_destroy`) take the
    pointer by value and run `Box::from_raw` internally — calling them twice on
    the same pointer is a use-after-free that segfaults on the second call once
    the heap allocator's metadata is touched (#469). Callers must invoke this
    exactly once per (ctx_ptr, handle_ptr) pair; the `main()` loop guarantees
    this via a single `finally` block.
    """
    if state is not None:
        state.release_pool()
    if native_lib and native_handle_ptr:
        native_lib.slpn_surface_disconnect(native_handle_ptr)
    if native_lib and native_ctx_ptr:
        native_lib.slpn_context_destroy(native_ctx_ptr)


def _assert_capability(processor_id: str, cmd: str, msg: dict, expected: str) -> None:
    """Belt-and-braces check that the wire-format capability field matches
    what this lifecycle method expects.

    The Rust host is the source of truth; mismatches indicate a wire-format
    drift bug and are logged but not fatal — the subprocess still dispatches
    the matching typed ctx.
    """
    actual = msg.get("capability")
    if actual is not None and actual != expected:
        log.error(
            "capability mismatch",
            processor_id=processor_id,
            cmd=cmd,
            expected=expected,
            actual=actual,
        )


def _drain_lifecycle_during_run(
    lifecycle_queue, stdout, processor, full_ctx, limited_ctx, processor_id,
):
    """Non-blocking poll for lifecycle commands during the run loop.

    Drains all queued lifecycle messages (which the bridge reader thread
    deposited while we were busy processing a frame) and dispatches them.
    Handles on_pause / on_resume / update_config inline; returns ``"stop"``
    or ``"teardown"`` (or :data:`BridgeReaderThread.EOF_SENTINEL`'s
    sentinel form ``"eof"``) as soon as a terminal command is observed,
    re-queueing any messages we didn't process so a stop/teardown takes
    precedence in FIFO order.
    """
    while True:
        try:
            msg = lifecycle_queue.get_nowait()
        except queue.Empty:
            return None

        if msg.get("__bridge_eof__"):
            return "eof"

        result = _dispatch_lifecycle_msg(
            msg, stdout, processor, full_ctx, limited_ctx, processor_id,
        )
        if result is not None:
            return result


def _dispatch_lifecycle_msg(
    msg, stdout, processor, full_ctx, limited_ctx, processor_id,
):
    """Execute a lifecycle message and reply. Returns 'stop'/'teardown'/None."""
    cmd = msg.get("cmd", "")

    if cmd == "stop" or cmd == "teardown":
        _assert_capability(processor_id, cmd, msg, "full")
        return cmd

    if cmd == "on_pause":
        _assert_capability(processor_id, cmd, msg, "limited")
        if hasattr(processor, "on_pause"):
            try:
                processor.on_pause(limited_ctx)
            except Exception as e:
                log.error("on_pause() error", error=str(e))
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    if cmd == "on_resume":
        _assert_capability(processor_id, cmd, msg, "limited")
        if hasattr(processor, "on_resume"):
            try:
                processor.on_resume(limited_ctx)
            except Exception as e:
                log.error("on_resume() error", error=str(e))
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    if cmd == "update_config":
        if hasattr(processor, "update_config"):
            config = msg.get("config", {})
            try:
                processor.update_config(config)
            except Exception as e:
                log.error("update_config() error", error=str(e))
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    log.warn("Unknown command during run", cmd=cmd)
    return None


def _next_lifecycle_msg(lifecycle_queue):
    """Block until the next lifecycle message arrives, returning ``None`` on EOF.

    Used by the outer command loop. The ``__bridge_eof__`` sentinel maps to
    ``None`` so callers see a clean EOF signal.
    """
    msg = lifecycle_queue.get()
    if msg.get("__bridge_eof__"):
        return None
    return msg


def main():
    """Main entry point for the subprocess runner."""
    entrypoint = os.environ.get("STREAMLIB_ENTRYPOINT")
    if not entrypoint:
        # Pre-install fatal — escalate channel not up yet, so fall back to
        # raw stderr. The host's fd2 reader will still capture this line.
        sys.stderr.write("[streamlib] STREAMLIB_ENTRYPOINT not set\n")
        sys.stderr.flush()
        sys.exit(1)

    project_path = os.environ.get("STREAMLIB_PROJECT_PATH", "")
    native_lib_path = os.environ.get("STREAMLIB_PYTHON_NATIVE_LIB", "")
    processor_id = os.environ.get("STREAMLIB_PROCESSOR_ID", "unknown")

    if not native_lib_path:
        sys.stderr.write("[streamlib] STREAMLIB_PYTHON_NATIVE_LIB not set\n")
        sys.stderr.flush()
        sys.exit(1)

    # Bridge framing rides a dedicated socketpair, not stdin/stdout —
    # fd1/fd2 stay as log pipes (see #451). Capture the escalate fds
    # BEFORE installing the stdio interceptors so the bridge never
    # touches sys.stdin/sys.stdout.
    stdin, stdout, _escalate_sock = _open_escalate_fd_stream()

    # Install the escalate channel so processors can call ctx.escalate_*
    # during setup() and process(). The reader thread (started below)
    # demultiplexes responses by request_id so concurrent calls from
    # worker threads are safe (#604).
    escalate_channel = EscalateChannel(stdout)
    install_channel(escalate_channel)

    # Lifecycle commands flow through this queue; the bridge reader thread
    # is the sole producer, the runner's outer loop (and the run-phase
    # `_drain_lifecycle_during_run`) are the consumers.
    lifecycle_queue: "queue.Queue[dict]" = queue.Queue()
    bridge_reader = BridgeReaderThread(stdin, escalate_channel, lifecycle_queue)
    bridge_reader.start()

    # Install unified logging: writer thread + stdio / logging interceptors.
    # After this point, `print()` / `sys.stderr.write()` / `logging.*` all
    # route through streamlib.log with `intercepted=true`.
    log.set_processor_id(processor_id)
    log.install(escalate_channel)

    # Load processor class and instantiate
    processor_class = _load_processor_class(entrypoint, project_path)
    processor = processor_class()

    state: NativeProcessorState | None = None
    full_ctx: NativeRuntimeContextFullAccess | None = None
    limited_ctx: NativeRuntimeContextLimitedAccess | None = None
    native_lib = None
    native_ctx_ptr = None
    native_handle_ptr = None
    running = False

    log.info("Subprocess runner started", entrypoint=entrypoint)

    try:
        while True:
            msg = _next_lifecycle_msg(lifecycle_queue)
            if msg is None:
                log.info("escalate channel closed, shutting down")
                break
            cmd = msg.get("cmd", "")

            if cmd == "setup":
                _assert_capability(processor_id, cmd, msg, "full")
                log.info("Native mode: loading library", lib=native_lib_path)
                try:
                    native_lib, native_ctx_ptr, native_handle_ptr, state = _setup_native_state(
                        msg, native_lib_path, processor_id,
                        escalate_channel=escalate_channel,
                    )
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "error", "error": str(e)})
                    continue

                # Build both capability views once per lifecycle. Each view
                # wraps the same underlying FFI state, so input/output ports
                # and timing are shared; the capability split is enforced
                # purely by what each view exposes.
                full_ctx = NativeRuntimeContextFullAccess(state)
                limited_ctx = NativeRuntimeContextLimitedAccess(state)

                try:
                    if hasattr(processor, "setup"):
                        # setup() — privileged, receives full-access ctx
                        processor.setup(full_ctx)
                    bridge_send_message(stdout, {"rpc": "ready"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "error", "error": str(e)})

            elif cmd == "run":
                if not native_lib or state is None:
                    log.warn("run before setup")
                    continue

                execution_mode = msg.get("execution", "reactive")
                interval_ms = msg.get("interval_ms", 0)
                running = True
                data_count = 0

                log.info("Entering execution loop", mode=execution_mode)

                if execution_mode == "reactive":
                    listener_fd = native_lib.slpn_event_listener_fd(native_ctx_ptr)
                    # Cap the wait so a closed escalate fd / shutdown command
                    # latency stays bounded even if no notify ever arrives.
                    # Matches the previous select() timeout — teardown latency
                    # is bounded by this constant plus per-iteration overhead.
                    SELECT_TIMEOUT_S = 0.1
                    while running:
                        has_data = native_lib.slpn_input_poll(native_ctx_ptr)
                        if has_data == 1:
                            data_count += 1
                            if data_count <= 3 or data_count % 60 == 0:
                                log.debug(
                                    "poll: data received",
                                    frame_index=data_count,
                                )
                            try:
                                if hasattr(processor, "process"):
                                    # process() — hot loop, receives limited ctx
                                    processor.process(limited_ctx)
                            except Exception as e:
                                log.error("process() error", error=str(e))
                        else:
                            # No data: block on the input notify fd (when
                            # wired) or a coarse sleep otherwise. The
                            # lifecycle queue is drained unconditionally
                            # below, so the SELECT_TIMEOUT_S cap is what
                            # bounds teardown latency.
                            if listener_fd >= 0:
                                ready, _, _ = select.select(
                                    [listener_fd], [], [], SELECT_TIMEOUT_S,
                                )
                                if listener_fd in ready:
                                    native_lib.slpn_event_drain(native_ctx_ptr)
                            else:
                                time.sleep(SELECT_TIMEOUT_S)

                        lifecycle_cmd = _drain_lifecycle_during_run(
                            lifecycle_queue, stdout, processor, full_ctx, limited_ctx,
                            processor_id,
                        )
                        if lifecycle_cmd == "teardown":
                            running = False
                            if hasattr(processor, "teardown"):
                                try:
                                    processor.teardown(full_ctx)
                                except Exception as e:
                                    log.error("teardown() error", error=str(e))
                            bridge_send_message(stdout, {"rpc": "done"})
                            return
                        elif lifecycle_cmd == "stop":
                            running = False
                            bridge_send_message(stdout, {"rpc": "stopped"})
                        elif lifecycle_cmd == "eof":
                            running = False

                elif execution_mode == "continuous":
                    # Drift-free monotonic-clock dispatch via timerfd. Replaces
                    # the previous `time.sleep(interval_ms/1000)` loop, which
                    # accumulated drift and didn't match streamlib's pacing
                    # philosophy. interval_ms <= 0 falls through to a yielding
                    # busy loop matching the old "yield" semantics.
                    interval_ns = int(interval_ms) * 1_000_000 if interval_ms and interval_ms > 0 else 0
                    timer = None
                    if interval_ns > 0:
                        try:
                            timer = clock.MonotonicTimer(interval_ns)
                        except RuntimeError as e:
                            log.error(
                                "MonotonicTimer unavailable for continuous mode",
                                error=str(e),
                                interval_ms=interval_ms,
                            )
                            running = False

                    try:
                        while running:
                            native_lib.slpn_input_poll(native_ctx_ptr)
                            try:
                                if hasattr(processor, "process"):
                                    processor.process(limited_ctx)
                            except Exception as e:
                                log.error("process() error", error=str(e))

                            if timer is not None:
                                # Block until the next tick or 100ms timeout —
                                # the timeout bounds teardown latency without
                                # reaching for time.sleep.
                                expirations = timer.wait(100)
                                if expirations < 0:
                                    log.error("timer wait failed; exiting continuous loop")
                                    running = False
                                    break
                                # expirations == 0 -> timeout, fall through to
                                # the lifecycle drain. expirations >= 1 -> tick(s)
                                # consumed; the loop body already ran once for
                                # this iteration so missed-tick catch-up is
                                # naturally skipped (drift-free, not catch-up).
                            # interval_ms <= 0: no timer; loop runs as fast as
                            # process() returns.

                            lifecycle_cmd = _drain_lifecycle_during_run(
                                lifecycle_queue, stdout, processor, full_ctx, limited_ctx,
                                processor_id,
                            )
                            if lifecycle_cmd == "teardown":
                                running = False
                                if hasattr(processor, "teardown"):
                                    try:
                                        processor.teardown(full_ctx)
                                    except Exception as e:
                                        log.error("teardown() error", error=str(e))
                                bridge_send_message(stdout, {"rpc": "done"})
                                return
                            elif lifecycle_cmd == "stop":
                                running = False
                                bridge_send_message(stdout, {"rpc": "stopped"})
                            elif lifecycle_cmd == "eof":
                                running = False
                    finally:
                        if timer is not None:
                            timer.close()

                elif execution_mode == "manual":
                    # Manual mode: start() is a resource-lifecycle op → full access.
                    # The contract is that start() returns promptly (worker
                    # threads do the long-lived work). Worker threads are now
                    # safe to call ctx.escalate_* concurrently — the bridge
                    # reader thread demuxes responses by request_id.
                    if hasattr(processor, "start"):
                        try:
                            processor.start(full_ctx)
                        except Exception as e:
                            log.error("start() error", error=str(e))

            elif cmd == "teardown":
                _assert_capability(processor_id, cmd, msg, "full")
                try:
                    if hasattr(processor, "teardown") and full_ctx is not None:
                        processor.teardown(full_ctx)
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "done"})
                break

            elif cmd == "stop":
                _assert_capability(processor_id, cmd, msg, "full")
                running = False
                try:
                    if hasattr(processor, "stop") and full_ctx is not None:
                        processor.stop(full_ctx)
                    bridge_send_message(stdout, {"rpc": "stopped"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "stopped", "error": str(e)})

            elif cmd == "on_pause":
                _assert_capability(processor_id, cmd, msg, "limited")
                if hasattr(processor, "on_pause") and limited_ctx is not None:
                    try:
                        processor.on_pause(limited_ctx)
                    except Exception as e:
                        traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "ok"})

            elif cmd == "on_resume":
                _assert_capability(processor_id, cmd, msg, "limited")
                if hasattr(processor, "on_resume") and limited_ctx is not None:
                    try:
                        processor.on_resume(limited_ctx)
                    except Exception as e:
                        traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "ok"})

            elif cmd == "update_config":
                if hasattr(processor, "update_config"):
                    config = msg.get("config", {})
                    try:
                        processor.update_config(config)
                    except Exception as e:
                        traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "ok"})

            else:
                log.warn("Unknown command", cmd=cmd)

    except Exception as e:
        log.error("Fatal error", error=str(e), traceback=traceback.format_exc())
        # finally runs cleanup; exit code is set by sys.exit raising SystemExit
        sys.exit(1)
    finally:
        # Single cleanup site — the FFI free is not idempotent (#469).
        bridge_reader.stop()
        if native_lib and native_ctx_ptr:
            _cleanup_native(native_lib, native_ctx_ptr, native_handle_ptr, state)
        log.info("Subprocess runner exiting")
        log.shutdown()


if __name__ == "__main__":
    main()

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Subprocess runner for Python processors (native FFI mode).

Entry point for Python subprocess processors spawned by the Rust runtime.
Lifecycle commands (setup/run/stop/teardown) use length-prefixed JSON over
stdin/stdout pipes. Data I/O uses direct iceoryx2 FFI via
libstreamlib_python_native.

Usage:
    python -m streamlib.subprocess_runner

Environment variables:
    STREAMLIB_ENTRYPOINT: e.g., "passthrough_processor:PassthroughProcessor"
    STREAMLIB_PROJECT_PATH: Path to Python processor project
    STREAMLIB_PYTHON_NATIVE_LIB: Path to libstreamlib_python_native.dylib
    STREAMLIB_PROCESSOR_ID: Unique processor ID
    STREAMLIB_EXECUTION_MODE: "reactive", "continuous", or "manual"
"""

import importlib
import logging
import os
import select
import sys
import time
import traceback

from .escalate import EscalateChannel, install_channel
from .processor_context import (
    NativeProcessorState,
    NativeRuntimeContextFullAccess,
    NativeRuntimeContextLimitedAccess,
    bridge_read_message,
    bridge_send_message,
    load_native_lib,
)
from .telemetry import setup_subprocess_telemetry

# Module-level logger, initialized in main()
_logger: logging.Logger | None = None


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

    # Create native context
    ctx_ptr = lib.slpn_context_create(processor_id.encode("utf-8"))
    if not ctx_ptr:
        raise RuntimeError("Failed to create native context")

    # Subscribe to input iceoryx2 services
    for inp in ports.get("inputs", []):
        port_name = inp["name"]
        service_name = inp["service_name"]
        read_mode = inp.get("read_mode", "skip_to_latest")
        _logger.info(
            "Subscribing to input: port='%s', service='%s', read_mode='%s'",
            port_name, service_name, read_mode,
        )
        result = lib.slpn_input_subscribe(ctx_ptr, service_name.encode("utf-8"))
        if result != 0:
            _logger.error("Failed to subscribe to '%s'", service_name)
        # Configure per-port read mode (0 = skip_to_latest, 1 = read_next_in_order)
        mode_int = 0 if read_mode == "skip_to_latest" else 1
        lib.slpn_input_set_read_mode(ctx_ptr, port_name.encode("utf-8"), mode_int)

    # Create publishers for output iceoryx2 services
    for out in ports.get("outputs", []):
        port_name = out["name"]
        dest_port = out["dest_port"]
        dest_service = out["dest_service_name"]
        schema_name = out.get("schema_name", "")
        _logger.info(
            "Publishing to output: port='%s', dest='%s', service='%s'",
            port_name, dest_port, dest_service,
        )
        result = lib.slpn_output_publish(
            ctx_ptr,
            dest_service.encode("utf-8"),
            port_name.encode("utf-8"),
            dest_port.encode("utf-8"),
            schema_name.encode("utf-8"),
            out.get("max_payload_bytes", 65536),
        )
        if result != 0:
            _logger.error("Failed to create publisher for '%s'", dest_service)

    # Connect to broker for surface resolution (if XPC service name is set)
    broker_ptr = None
    xpc_service_name = os.environ.get("STREAMLIB_XPC_SERVICE_NAME", "")
    runtime_id = os.environ.get("STREAMLIB_RUNTIME_ID", "")
    if xpc_service_name:
        runtime_id_arg = runtime_id.encode("utf-8") if runtime_id else None
        broker_ptr = lib.slpn_broker_connect(
            xpc_service_name.encode("utf-8"), runtime_id_arg
        )
        if broker_ptr:
            _logger.info(
                "Connected to broker '%s' with runtime_id='%s'",
                xpc_service_name, runtime_id,
            )
        else:
            _logger.warning(
                "Broker connect failed for '%s'", xpc_service_name,
            )

    state = NativeProcessorState(
        lib, ctx_ptr, config, broker_ptr, escalate_channel=escalate_channel,
    )
    return lib, ctx_ptr, broker_ptr, state


def _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr, state=None):
    """Destroy native context and disconnect broker."""
    if state is not None:
        state.release_pool()
    if native_lib and native_broker_ptr:
        native_lib.slpn_broker_disconnect(native_broker_ptr)
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
        _logger.error(
            "[%s] capability mismatch for '%s': expected '%s', got '%s'",
            processor_id, cmd, expected, actual,
        )


def _handle_stdin_during_run(
    stdin, stdout, processor, full_ctx, limited_ctx, processor_id,
    escalate_channel=None,
):
    """Non-blocking check for lifecycle commands during the run loop.

    Also drains any lifecycle commands the escalate channel may have buffered
    while blocked on a correlated response. Handles on_pause, on_resume, and
    update_config inline. Returns "stop", "teardown", or None.
    """
    # Drain deferred lifecycle messages first — they arrived while a
    # process() call was blocked on ctx.escalate_*, so they're older than
    # anything still in the pipe.
    if escalate_channel and escalate_channel.has_deferred_lifecycle_messages():
        deferred = escalate_channel.take_deferred_lifecycle_messages()
        for dm in deferred:
            result = _dispatch_lifecycle_msg(
                dm, stdout, processor, full_ctx, limited_ctx, processor_id,
            )
            if result is not None:
                # Re-queue any messages we didn't process this turn so a
                # stop/teardown coming from a deferred slot takes precedence.
                remaining = deferred[deferred.index(dm) + 1 :]
                escalate_channel._deferred_lifecycle[:0] = remaining
                return result

    if not select.select([stdin], [], [], 0)[0]:
        return None

    msg = bridge_read_message(stdin)
    return _dispatch_lifecycle_msg(
        msg, stdout, processor, full_ctx, limited_ctx, processor_id,
    )


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
                _logger.error("on_pause() error: %s", e)
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    if cmd == "on_resume":
        _assert_capability(processor_id, cmd, msg, "limited")
        if hasattr(processor, "on_resume"):
            try:
                processor.on_resume(limited_ctx)
            except Exception as e:
                _logger.error("on_resume() error: %s", e)
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    if cmd == "update_config":
        if hasattr(processor, "update_config"):
            config = msg.get("config", {})
            try:
                processor.update_config(config)
            except Exception as e:
                _logger.error("update_config() error: %s", e)
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    _logger.warning("Unknown command during run: %s", cmd)
    return None


def main():
    """Main entry point for the subprocess runner."""
    global _logger

    entrypoint = os.environ.get("STREAMLIB_ENTRYPOINT")
    if not entrypoint:
        print("[streamlib] STREAMLIB_ENTRYPOINT not set", file=sys.stderr)
        sys.exit(1)

    project_path = os.environ.get("STREAMLIB_PROJECT_PATH", "")
    native_lib_path = os.environ.get("STREAMLIB_PYTHON_NATIVE_LIB", "")
    processor_id = os.environ.get("STREAMLIB_PROCESSOR_ID", "unknown")

    # Initialize telemetry logger (writes to SQLite + stderr)
    _logger = setup_subprocess_telemetry(processor_id)

    if not native_lib_path:
        _logger.error("STREAMLIB_PYTHON_NATIVE_LIB not set")
        sys.exit(1)

    # Use binary stdin/stdout for the lifecycle protocol
    stdin = sys.stdin.buffer
    stdout = sys.stdout.buffer

    # Install the escalate channel so processors can call ctx.escalate_*
    # during setup() and process(). The channel shares the same stdio pipes
    # as the lifecycle protocol and demultiplexes responses by request_id.
    escalate_channel = EscalateChannel(stdin, stdout)
    install_channel(escalate_channel)

    # Load processor class and instantiate
    processor_class = _load_processor_class(entrypoint, project_path)
    processor = processor_class()

    state: NativeProcessorState | None = None
    full_ctx: NativeRuntimeContextFullAccess | None = None
    limited_ctx: NativeRuntimeContextLimitedAccess | None = None
    native_lib = None
    native_ctx_ptr = None
    native_broker_ptr = None
    running = False

    _logger.info("Subprocess runner started: entrypoint=%s", entrypoint)

    try:
        while True:
            msg = bridge_read_message(stdin)
            cmd = msg.get("cmd", "")

            if cmd == "setup":
                _assert_capability(processor_id, cmd, msg, "full")
                _logger.info("Native mode: loading %s", native_lib_path)
                try:
                    native_lib, native_ctx_ptr, native_broker_ptr, state = _setup_native_state(
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
                    _logger.warning("run before setup")
                    continue

                execution_mode = msg.get("execution", "reactive")
                interval_ms = msg.get("interval_ms", 0)
                running = True
                data_count = 0

                _logger.info("Entering %s loop", execution_mode)

                if execution_mode == "reactive":
                    while running:
                        has_data = native_lib.slpn_input_poll(native_ctx_ptr)
                        if has_data == 1:
                            data_count += 1
                            if data_count <= 3 or data_count % 60 == 0:
                                _logger.debug(
                                    "poll: data received (frame #%d)", data_count,
                                )
                            try:
                                if hasattr(processor, "process"):
                                    # process() — hot loop, receives limited ctx
                                    processor.process(limited_ctx)
                            except Exception as e:
                                _logger.error("process() error: %s", e)
                        else:
                            time.sleep(0.001)  # 1ms yield

                        lifecycle_cmd = _handle_stdin_during_run(
                            stdin, stdout, processor, full_ctx, limited_ctx,
                            processor_id, escalate_channel=escalate_channel,
                        )
                        if lifecycle_cmd == "teardown":
                            running = False
                            if hasattr(processor, "teardown"):
                                try:
                                    processor.teardown(full_ctx)
                                except Exception as e:
                                    _logger.error("teardown() error: %s", e)
                            bridge_send_message(stdout, {"rpc": "done"})
                            _cleanup_native(
                                native_lib, native_ctx_ptr, native_broker_ptr, state,
                            )
                            sys.exit(0)
                        elif lifecycle_cmd == "stop":
                            running = False
                            bridge_send_message(stdout, {"rpc": "stopped"})

                elif execution_mode == "continuous":
                    while running:
                        native_lib.slpn_input_poll(native_ctx_ptr)
                        try:
                            if hasattr(processor, "process"):
                                processor.process(limited_ctx)
                        except Exception as e:
                            _logger.error("process() error: %s", e)
                        if interval_ms > 0:
                            time.sleep(interval_ms / 1000.0)
                        else:
                            time.sleep(0)  # yield

                        lifecycle_cmd = _handle_stdin_during_run(
                            stdin, stdout, processor, full_ctx, limited_ctx,
                            processor_id, escalate_channel=escalate_channel,
                        )
                        if lifecycle_cmd == "teardown":
                            running = False
                            if hasattr(processor, "teardown"):
                                try:
                                    processor.teardown(full_ctx)
                                except Exception as e:
                                    _logger.error("teardown() error: %s", e)
                            bridge_send_message(stdout, {"rpc": "done"})
                            _cleanup_native(
                                native_lib, native_ctx_ptr, native_broker_ptr, state,
                            )
                            sys.exit(0)
                        elif lifecycle_cmd == "stop":
                            running = False
                            bridge_send_message(stdout, {"rpc": "stopped"})

                elif execution_mode == "manual":
                    # Manual mode: start() is a resource-lifecycle op → full access
                    if hasattr(processor, "start"):
                        try:
                            processor.start(full_ctx)
                        except Exception as e:
                            _logger.error("start() error: %s", e)

            elif cmd == "teardown":
                _assert_capability(processor_id, cmd, msg, "full")
                try:
                    if hasattr(processor, "teardown") and full_ctx is not None:
                        processor.teardown(full_ctx)
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "done"})
                if native_lib and native_ctx_ptr:
                    _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr, state)
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
                _logger.warning("Unknown command: %s", cmd)

    except EOFError:
        _logger.info("stdin closed, shutting down")
    except Exception as e:
        _logger.error("Fatal error: %s", e, exc_info=True)
        if native_lib and native_ctx_ptr:
            _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr, state)
        sys.exit(1)

    _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr, state)

    _logger.info("Subprocess runner exiting")


if __name__ == "__main__":
    main()

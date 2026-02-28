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
import os
import select
import sys
import time
import traceback

from .processor_context import (
    NativeProcessorContext,
    bridge_read_message,
    bridge_send_message,
    load_native_lib,
)


def _load_processor_class(entrypoint: str, project_path: str):
    """Load a processor class from an entrypoint string."""
    if project_path and project_path not in sys.path:
        sys.path.insert(0, project_path)

    module_path, class_name = entrypoint.rsplit(":", 1)
    module = importlib.import_module(module_path)
    return getattr(module, class_name)


def _setup_native_context(msg, native_lib_path, processor_id):
    """Set up native FFI context with iceoryx2 subscriptions and publishers."""
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
        print(
            f"[streamlib:{processor_id}] Subscribing to input: port='{port_name}', service='{service_name}'",
            file=sys.stderr,
        )
        result = lib.slpn_input_subscribe(ctx_ptr, service_name.encode("utf-8"))
        if result != 0:
            print(
                f"[streamlib:{processor_id}] Failed to subscribe to '{service_name}'",
                file=sys.stderr,
            )

    # Create publishers for output iceoryx2 services
    for out in ports.get("outputs", []):
        port_name = out["name"]
        dest_port = out["dest_port"]
        dest_service = out["dest_service_name"]
        schema_name = out.get("schema_name", "")
        print(
            f"[streamlib:{processor_id}] Publishing to output: port='{port_name}', dest='{dest_port}', service='{dest_service}'",
            file=sys.stderr,
        )
        result = lib.slpn_output_publish(
            ctx_ptr,
            dest_service.encode("utf-8"),
            port_name.encode("utf-8"),
            dest_port.encode("utf-8"),
            schema_name.encode("utf-8"),
        )
        if result != 0:
            print(
                f"[streamlib:{processor_id}] Failed to create publisher for '{dest_service}'",
                file=sys.stderr,
            )

    # Connect to broker for surface resolution (if XPC service name is set)
    broker_ptr = None
    xpc_service_name = os.environ.get("STREAMLIB_XPC_SERVICE_NAME", "")
    if xpc_service_name:
        broker_ptr = lib.slpn_broker_connect(xpc_service_name.encode("utf-8"))
        if broker_ptr:
            print(
                f"[streamlib:{processor_id}] Connected to broker '{xpc_service_name}'",
                file=sys.stderr,
            )
        else:
            print(
                f"[streamlib:{processor_id}] Warning: broker connect failed for '{xpc_service_name}'",
                file=sys.stderr,
            )

    ctx = NativeProcessorContext(lib, ctx_ptr, config, broker_ptr)
    return lib, ctx_ptr, broker_ptr, ctx


def _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr):
    """Destroy native context and disconnect broker."""
    if native_lib and native_broker_ptr:
        native_lib.slpn_broker_disconnect(native_broker_ptr)
    if native_lib and native_ctx_ptr:
        native_lib.slpn_context_destroy(native_ctx_ptr)


def _handle_stdin_during_run(stdin, stdout, processor, ctx, processor_id):
    """Non-blocking check for lifecycle commands during the run loop.

    Handles on_pause, on_resume, and update_config inline.
    Returns "stop", "teardown", or None.
    """
    if not select.select([stdin], [], [], 0)[0]:
        return None

    msg = bridge_read_message(stdin)
    cmd = msg.get("cmd", "")

    if cmd == "stop" or cmd == "teardown":
        return cmd

    if cmd == "on_pause":
        if hasattr(processor, "on_pause"):
            try:
                processor.on_pause(ctx)
            except Exception as e:
                print(
                    f"[streamlib:{processor_id}] on_pause() error: {e}",
                    file=sys.stderr,
                )
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    if cmd == "on_resume":
        if hasattr(processor, "on_resume"):
            try:
                processor.on_resume(ctx)
            except Exception as e:
                print(
                    f"[streamlib:{processor_id}] on_resume() error: {e}",
                    file=sys.stderr,
                )
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    if cmd == "update_config":
        if hasattr(processor, "update_config"):
            config = msg.get("config", {})
            try:
                processor.update_config(config)
            except Exception as e:
                print(
                    f"[streamlib:{processor_id}] update_config() error: {e}",
                    file=sys.stderr,
                )
        bridge_send_message(stdout, {"rpc": "ok"})
        return None

    print(
        f"[streamlib:{processor_id}] Unknown command during run: {cmd}",
        file=sys.stderr,
    )
    return None


def main():
    """Main entry point for the subprocess runner."""
    entrypoint = os.environ.get("STREAMLIB_ENTRYPOINT")
    if not entrypoint:
        print("[streamlib] STREAMLIB_ENTRYPOINT not set", file=sys.stderr)
        sys.exit(1)

    project_path = os.environ.get("STREAMLIB_PROJECT_PATH", "")
    native_lib_path = os.environ.get("STREAMLIB_PYTHON_NATIVE_LIB", "")
    processor_id = os.environ.get("STREAMLIB_PROCESSOR_ID", "unknown")

    if not native_lib_path:
        print(
            f"[streamlib:{processor_id}] STREAMLIB_PYTHON_NATIVE_LIB not set",
            file=sys.stderr,
        )
        sys.exit(1)

    # Use binary stdin/stdout for the lifecycle protocol
    stdin = sys.stdin.buffer
    stdout = sys.stdout.buffer

    # Load processor class and instantiate
    processor_class = _load_processor_class(entrypoint, project_path)
    processor = processor_class()

    ctx = None
    native_lib = None
    native_ctx_ptr = None
    native_broker_ptr = None
    running = False

    print(
        f"[streamlib:{processor_id}] Subprocess runner started: entrypoint={entrypoint}",
        file=sys.stderr,
    )

    try:
        while True:
            msg = bridge_read_message(stdin)
            cmd = msg.get("cmd", "")

            if cmd == "setup":
                print(
                    f"[streamlib:{processor_id}] Native mode: loading {native_lib_path}",
                    file=sys.stderr,
                )
                try:
                    native_lib, native_ctx_ptr, native_broker_ptr, ctx = _setup_native_context(
                        msg, native_lib_path, processor_id
                    )
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "error", "error": str(e)})
                    continue

                try:
                    if hasattr(processor, "setup"):
                        processor.setup(ctx)
                    bridge_send_message(stdout, {"rpc": "ready"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "error", "error": str(e)})

            elif cmd == "run":
                if not native_lib or not ctx:
                    print(
                        f"[streamlib:{processor_id}] run before setup",
                        file=sys.stderr,
                    )
                    continue

                execution_mode = msg.get("execution", "reactive")
                interval_ms = msg.get("interval_ms", 0)
                running = True
                data_count = 0

                print(
                    f"[streamlib:{processor_id}] Entering {execution_mode} loop",
                    file=sys.stderr,
                )

                if execution_mode == "reactive":
                    while running:
                        has_data = native_lib.slpn_input_poll(native_ctx_ptr)
                        if has_data == 1:
                            data_count += 1
                            if data_count <= 3 or data_count % 60 == 0:
                                print(
                                    f"[streamlib:{processor_id}] poll: data received (frame #{data_count})",
                                    file=sys.stderr,
                                )
                            try:
                                if hasattr(processor, "process"):
                                    processor.process(ctx)
                            except Exception as e:
                                print(
                                    f"[streamlib:{processor_id}] process() error: {e}",
                                    file=sys.stderr,
                                )
                        else:
                            time.sleep(0.001)  # 1ms yield

                        lifecycle_cmd = _handle_stdin_during_run(
                            stdin, stdout, processor, ctx, processor_id
                        )
                        if lifecycle_cmd == "teardown":
                            running = False
                            if hasattr(processor, "teardown"):
                                try:
                                    processor.teardown(ctx)
                                except Exception as e:
                                    print(
                                        f"[streamlib:{processor_id}] teardown() error: {e}",
                                        file=sys.stderr,
                                    )
                            bridge_send_message(stdout, {"rpc": "done"})
                            _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr)
                            sys.exit(0)
                        elif lifecycle_cmd == "stop":
                            running = False
                            bridge_send_message(stdout, {"rpc": "stopped"})

                elif execution_mode == "continuous":
                    while running:
                        native_lib.slpn_input_poll(native_ctx_ptr)
                        try:
                            if hasattr(processor, "process"):
                                processor.process(ctx)
                        except Exception as e:
                            print(
                                f"[streamlib:{processor_id}] process() error: {e}",
                                file=sys.stderr,
                            )
                        if interval_ms > 0:
                            time.sleep(interval_ms / 1000.0)
                        else:
                            time.sleep(0)  # yield

                        lifecycle_cmd = _handle_stdin_during_run(
                            stdin, stdout, processor, ctx, processor_id
                        )
                        if lifecycle_cmd == "teardown":
                            running = False
                            if hasattr(processor, "teardown"):
                                try:
                                    processor.teardown(ctx)
                                except Exception as e:
                                    print(
                                        f"[streamlib:{processor_id}] teardown() error: {e}",
                                        file=sys.stderr,
                                    )
                            bridge_send_message(stdout, {"rpc": "done"})
                            _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr)
                            sys.exit(0)
                        elif lifecycle_cmd == "stop":
                            running = False
                            bridge_send_message(stdout, {"rpc": "stopped"})

                elif execution_mode == "manual":
                    # Manual mode: start() returns, outer loop handles stop/teardown
                    if hasattr(processor, "start"):
                        try:
                            processor.start(ctx)
                        except Exception as e:
                            print(
                                f"[streamlib:{processor_id}] start() error: {e}",
                                file=sys.stderr,
                            )

            elif cmd == "teardown":
                try:
                    if hasattr(processor, "teardown"):
                        processor.teardown(ctx)
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "done"})
                if native_lib and native_ctx_ptr:
                    _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr)
                break

            elif cmd == "stop":
                running = False
                try:
                    if hasattr(processor, "stop"):
                        processor.stop(ctx)
                    bridge_send_message(stdout, {"rpc": "stopped"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "stopped", "error": str(e)})

            elif cmd == "on_pause":
                if hasattr(processor, "on_pause"):
                    try:
                        processor.on_pause(ctx)
                    except Exception as e:
                        traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "ok"})

            elif cmd == "on_resume":
                if hasattr(processor, "on_resume"):
                    try:
                        processor.on_resume(ctx)
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
                print(
                    f"[streamlib:{processor_id}] Unknown command: {cmd}",
                    file=sys.stderr,
                )

    except EOFError:
        print(f"[streamlib:{processor_id}] stdin closed, shutting down", file=sys.stderr)
    except Exception as e:
        print(f"[streamlib:{processor_id}] Fatal error: {e}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        if native_lib and native_ctx_ptr:
            _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr)
        sys.exit(1)

    _cleanup_native(native_lib, native_ctx_ptr, native_broker_ptr)

    print(f"[streamlib:{processor_id}] Subprocess runner exiting", file=sys.stderr)


if __name__ == "__main__":
    main()

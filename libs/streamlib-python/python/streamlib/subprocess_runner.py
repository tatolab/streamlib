# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Subprocess runner for Python processors (bridge protocol).

Entry point for Python subprocess processors spawned by the Rust runtime.
Communicates with the Rust SubprocessHostProcessor via a length-prefixed
JSON protocol over stdin/stdout pipes.

Protocol:
  - All messages: [4 bytes u32 BE length][JSON bytes]
  - Binary data follows JSON headers when data_len > 0
  - Rust sends commands (setup, process, teardown, start, stop)
  - Python sends RPCs (read, write, done, ready) during processor methods

Usage:
    python -m streamlib.subprocess_runner

Environment variables:
    STREAMLIB_ENTRYPOINT: e.g., "passthrough_processor:PassthroughProcessor"
    STREAMLIB_PROJECT_PATH: Path to Python processor project
"""

import importlib
import os
import sys
import traceback

from .processor_context import (
    BridgeGpu,
    BridgeInputs,
    BridgeOutputs,
    SubprocessProcessorContext,
    bridge_read_message,
    bridge_send_message,
)


def _load_processor_class(entrypoint: str, project_path: str):
    """Load a processor class from an entrypoint string.

    Args:
        entrypoint: Module and class in format "module_name:ClassName"
        project_path: Path to add to sys.path for module resolution
    """
    if project_path and project_path not in sys.path:
        sys.path.insert(0, project_path)

    module_path, class_name = entrypoint.rsplit(":", 1)
    module = importlib.import_module(module_path)
    return getattr(module, class_name)


def main():
    """Main entry point for the subprocess runner."""
    entrypoint = os.environ.get("STREAMLIB_ENTRYPOINT")
    if not entrypoint:
        print("[streamlib] STREAMLIB_ENTRYPOINT not set", file=sys.stderr)
        sys.exit(1)

    project_path = os.environ.get("STREAMLIB_PROJECT_PATH", "")

    # Use binary stdin/stdout for the bridge protocol
    stdin = sys.stdin.buffer
    stdout = sys.stdout.buffer

    # Load processor class and instantiate
    processor_class = _load_processor_class(entrypoint, project_path)
    processor = processor_class()

    # Context is created during setup
    ctx = None

    print(
        f"[streamlib] Subprocess runner started: entrypoint={entrypoint}",
        file=sys.stderr,
    )

    try:
        while True:
            msg = bridge_read_message(stdin)
            cmd = msg.get("cmd", "")

            if cmd == "setup":
                config = msg.get("config")
                inputs = BridgeInputs(stdin, stdout)
                outputs = BridgeOutputs(stdin, stdout)
                gpu = BridgeGpu(stdin, stdout)
                ctx = SubprocessProcessorContext(
                    config=config,
                    inputs=inputs,
                    outputs=outputs,
                    gpu=gpu,
                )
                try:
                    if hasattr(processor, "setup"):
                        processor.setup(ctx)
                    bridge_send_message(stdout, {"rpc": "ready"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "error", "error": str(e)})

            elif cmd == "process":
                try:
                    if hasattr(processor, "process"):
                        processor.process(ctx)
                    bridge_send_message(stdout, {"rpc": "done"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "done", "error": str(e)})

            elif cmd == "teardown":
                try:
                    if hasattr(processor, "teardown"):
                        processor.teardown(ctx)
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                bridge_send_message(stdout, {"rpc": "done"})
                break

            elif cmd == "start":
                try:
                    if hasattr(processor, "start"):
                        processor.start(ctx)
                    bridge_send_message(stdout, {"rpc": "ready"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "error", "error": str(e)})

            elif cmd == "stop":
                try:
                    if hasattr(processor, "stop"):
                        processor.stop(ctx)
                    bridge_send_message(stdout, {"rpc": "done"})
                except Exception as e:
                    traceback.print_exc(file=sys.stderr)
                    bridge_send_message(stdout, {"rpc": "done", "error": str(e)})

            else:
                print(
                    f"[streamlib] Unknown command: {cmd}",
                    file=sys.stderr,
                )

    except EOFError:
        print("[streamlib] stdin closed, shutting down", file=sys.stderr)
    except Exception as e:
        print(f"[streamlib] Fatal error: {e}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)

    print("[streamlib] Subprocess runner exiting", file=sys.stderr)


if __name__ == "__main__":
    main()

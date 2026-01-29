# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Subprocess processor context — bridge protocol over stdin/stdout pipes.

Provides the `ctx` object passed to Python processor methods:
  - ctx.inputs.read("port_name") -> deserialized msgpack data or None
  - ctx.outputs.write("port_name", data) -> serializes and sends via bridge
  - ctx.time -> monotonic clock (nanoseconds)
  - ctx.config -> processor config dict

All I/O goes through the Rust SubprocessHostProcessor via RPC over pipes.
No iceoryx2 usage on the Python side.

Protocol:
  All messages are length-prefixed: [4 bytes u32 BE length][JSON bytes]
  Binary data follows JSON headers when data_len > 0.
"""

import json
import struct
import time

import msgpack


# ============================================================================
# Bridge protocol I/O
# ============================================================================


def bridge_read_message(stdin):
    """Read a length-prefixed JSON message from stdin."""
    len_buf = stdin.read(4)
    if len(len_buf) < 4:
        raise EOFError("stdin closed")
    length = struct.unpack(">I", len_buf)[0]
    msg_buf = stdin.read(length)
    if len(msg_buf) < length:
        raise EOFError("stdin closed mid-message")
    return json.loads(msg_buf)


def bridge_send_message(stdout, msg):
    """Send a length-prefixed JSON message to stdout."""
    json_bytes = json.dumps(msg, separators=(",", ":")).encode("utf-8")
    stdout.write(struct.pack(">I", len(json_bytes)))
    stdout.write(json_bytes)
    stdout.flush()


def bridge_read_binary(stdin, length):
    """Read raw binary data of a specific length from stdin."""
    data = stdin.read(length)
    if len(data) < length:
        raise EOFError("stdin closed mid-binary")
    return data


def bridge_send_binary(stdout, data):
    """Send raw binary data to stdout."""
    stdout.write(data)
    stdout.flush()


# ============================================================================
# RPC proxy classes
# ============================================================================


class BridgeInputs:
    """RPC proxy for reading input ports via the Rust bridge."""

    def __init__(self, stdin, stdout):
        self._stdin = stdin
        self._stdout = stdout

    def read(self, port_name):
        """Read latest data from a port. Returns deserialized msgpack data or None.

        Sends a read RPC to Rust, which reads from the real InputMailboxes.
        """
        bridge_send_message(self._stdout, {"rpc": "read", "port": port_name})
        response = bridge_read_message(self._stdin)
        data_len = response.get("data_len", 0)
        if data_len == 0:
            return None
        raw_data = bridge_read_binary(self._stdin, data_len)
        return msgpack.unpackb(raw_data, raw=False)

    def read_with_timestamp(self, port_name):
        """Read latest data and timestamp. Returns (data, timestamp_ns) or (None, None)."""
        bridge_send_message(self._stdout, {"rpc": "read", "port": port_name})
        response = bridge_read_message(self._stdin)
        data_len = response.get("data_len", 0)
        if data_len == 0:
            return None, None
        timestamp_ns = response.get("ts", 0)
        raw_data = bridge_read_binary(self._stdin, data_len)
        return msgpack.unpackb(raw_data, raw=False), timestamp_ns


class BridgeOutputs:
    """RPC proxy for writing output ports via the Rust bridge."""

    def __init__(self, stdin, stdout):
        self._stdin = stdin
        self._stdout = stdout

    def write(self, port_name, data, timestamp_ns=None):
        """Write data to a port. Serializes via msgpack and sends via bridge.

        Sends a write RPC to Rust, which writes to the real OutputWriter.
        """
        if timestamp_ns is None:
            timestamp_ns = time.monotonic_ns()

        packed = msgpack.packb(data, use_bin_type=True)

        bridge_send_message(self._stdout, {
            "rpc": "write",
            "port": port_name,
            "ts": timestamp_ns,
            "data_len": len(packed),
        })
        bridge_send_binary(self._stdout, packed)

        # Read acknowledgment from Rust
        response = bridge_read_message(self._stdin)
        if not response.get("ok", False):
            error = response.get("error", "unknown")
            raise RuntimeError(f"Write to port '{port_name}' failed: {error}")


class BridgeGpu:
    """RPC proxy for GPU surface operations via the Rust bridge."""

    def __init__(self, stdin, stdout):
        self._stdin = stdin
        self._stdout = stdout

    def resolve_surface(self, surface_id):
        """Resolve a broker surface_id UUID to a GPU surface handle.

        Returns GpuSurfaceHandle with .lock(), .unlock(), .as_numpy().
        Raises RuntimeError on unsupported platforms.
        """
        from .gpu_surface import GpuSurfaceHandle

        bridge_send_message(self._stdout, {
            "rpc": "resolve_surface",
            "surface_id": surface_id,
        })
        response = bridge_read_message(self._stdin)
        if "error" in response:
            raise RuntimeError(f"resolve_surface failed: {response['error']}")
        iosurface_id = response["iosurface_id"]
        return GpuSurfaceHandle(iosurface_id)

    def acquire_surface(self, width, height, format="bgra"):
        """Acquire a new pixel buffer from the Rust-managed pool.

        Returns (surface_id_for_metadata, GpuSurfaceHandle_for_pixel_access).
        Raises RuntimeError on unsupported platforms.
        """
        from .gpu_surface import GpuSurfaceHandle

        bridge_send_message(self._stdout, {
            "rpc": "acquire_surface",
            "width": width,
            "height": height,
            "format": format,
        })
        response = bridge_read_message(self._stdin)
        if "error" in response:
            raise RuntimeError(f"acquire_surface failed: {response['error']}")
        broker_uuid = response["surface_id"]
        iosurface_id = response["iosurface_id"]
        handle = GpuSurfaceHandle(iosurface_id)
        return broker_uuid, handle


# ============================================================================
# Processor context
# ============================================================================


class SubprocessProcessorContext:
    """Context object passed to Python subprocess processors."""

    def __init__(self, config, inputs, outputs, gpu):
        self._config = config or {}
        self.inputs = inputs
        self.outputs = outputs
        self.gpu = gpu

    @property
    def config(self):
        """Processor configuration dictionary."""
        return self._config

    @property
    def time(self):
        """Current monotonic time in nanoseconds."""
        return time.monotonic_ns()

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""FramePayload ctypes definition matching the Rust iceoryx2 FramePayload layout.

This must match the exact memory layout of the Rust struct:
  - PortKey: 1 byte len + 63 bytes name = 64 bytes
  - SchemaName: 1 byte len + 127 bytes name = 128 bytes
  - timestamp_ns: i64 (8 bytes)
  - len: u32 (4 bytes)
  - data: [u8; 32768]
"""

import ctypes

MAX_PAYLOAD_SIZE = 32768
MAX_SCHEMA_NAME_SIZE = 128
MAX_PORT_KEY_SIZE = 64


class PortKey(ctypes.Structure):
    """Fixed-size port name for zero-copy IPC. Matches Rust PortKey layout."""

    _fields_ = [
        ("len", ctypes.c_uint8),
        ("name", ctypes.c_uint8 * (MAX_PORT_KEY_SIZE - 1)),
    ]

    @staticmethod
    def from_str(s: str) -> "PortKey":
        key = PortKey()
        data = s.encode("utf-8")
        key.len = min(len(data), MAX_PORT_KEY_SIZE - 1)
        ctypes.memmove(key.name, data, key.len)
        return key

    def as_str(self) -> str:
        return bytes(self.name[: self.len]).decode("utf-8", errors="replace")


class SchemaName(ctypes.Structure):
    """Fixed-size schema name for zero-copy IPC. Matches Rust SchemaName layout."""

    _fields_ = [
        ("len", ctypes.c_uint8),
        ("name", ctypes.c_uint8 * (MAX_SCHEMA_NAME_SIZE - 1)),
    ]

    @staticmethod
    def from_str(s: str) -> "SchemaName":
        schema = SchemaName()
        data = s.encode("utf-8")
        schema.len = min(len(data), MAX_SCHEMA_NAME_SIZE - 1)
        ctypes.memmove(schema.name, data, schema.len)
        return schema

    def as_str(self) -> str:
        return bytes(self.name[: self.len]).decode("utf-8", errors="replace")


class FramePayload(ctypes.Structure):
    """Zero-copy frame payload for iceoryx2 pub/sub. Matches Rust FramePayload layout."""

    _fields_ = [
        ("port_key", PortKey),
        ("schema_name", SchemaName),
        ("timestamp_ns", ctypes.c_int64),
        ("len", ctypes.c_uint32),
        ("data", ctypes.c_uint8 * MAX_PAYLOAD_SIZE),
    ]

    @staticmethod
    def type_name() -> str:
        """Returns the iceoryx2 type name. Must match Rust #[type_name("FramePayload")]."""
        return "FramePayload"

    def get_data(self) -> bytes:
        """Get the actual payload data (excluding padding)."""
        return bytes(self.data[: self.len])

    def set_data(
        self, port: str, schema: str, timestamp_ns: int, data: bytes
    ) -> None:
        """Set all fields of the payload."""
        self.port_key = PortKey.from_str(port)
        self.schema_name = SchemaName.from_str(schema)
        self.timestamp_ns = timestamp_ns
        data_len = min(len(data), MAX_PAYLOAD_SIZE)
        self.len = data_len
        ctypes.memmove(self.data, data, data_len)

    def get_port(self) -> str:
        """Get the port key as a string."""
        return self.port_key.as_str()

    def get_schema(self) -> str:
        """Get the schema name as a string."""
        return self.schema_name.as_str()

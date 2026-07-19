# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""FramePayload ctypes definition matching the Rust iceoryx2 FramePayload layout.

This must match the exact memory layout of the Rust struct defined in
`runtime/streamlib-ipc-types/src/lib.rs`:
  - PortKey: 1 byte len + 63 bytes name = 64 bytes
  - SchemaIdentWire: 128 bytes structured record (org/package/type/version)
  - timestamp_ns: i64 (8 bytes)
  - len: u32 (4 bytes)
  - data: [u8; 32768]

Wire format (#401 phase 2, structured-everywhere):
the schema identifier is a 4-tuple of typed segments rather than a joined
string subject to per-runtime parsing drift.
"""

import ctypes

MAX_PAYLOAD_SIZE = 32768
MAX_PORT_KEY_SIZE = 64
SCHEMA_IDENT_WIRE_SIZE = 128
SCHEMA_IDENT_WIRE_MAX_ORG_LEN = 31
SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN = 31
SCHEMA_IDENT_WIRE_MAX_TYPE_LEN = 51


class PortKey(ctypes.Structure):
    """Fixed-size port name for zero-copy IPC. Matches Rust PortKey layout."""

    _fields_ = [
        ("len", ctypes.c_uint8),
        ("name", ctypes.c_uint8 * (MAX_PORT_KEY_SIZE - 1)),
    ]

    @staticmethod
    def from_str(s: str) -> "PortKey":
        # Mirrors Rust `PortKey::new` (#1416): an over-length name is a hard
        # error, never a silent truncation that would route frames to a
        # different (clipped) port than the one the author named.
        key = PortKey()
        data = s.encode("utf-8")
        if len(data) > MAX_PORT_KEY_SIZE - 1:
            raise ValueError(
                f"port key name is {len(data)} bytes, exceeding the fixed wire "
                f"capacity of {MAX_PORT_KEY_SIZE - 1} bytes"
            )
        key.len = len(data)
        ctypes.memmove(key.name, data, key.len)
        return key

    def as_str(self) -> str:
        return bytes(self.name[: self.len]).decode("utf-8", errors="replace")


class SchemaIdentWire(ctypes.Structure):
    """Structured schema identifier on the iceoryx2 wire — `@org/package/Type@version`.

    Layout matches Rust `streamlib_ipc_types::SchemaIdentWire` byte-for-byte:
    128 bytes total, alignment 4, little-endian for the version u32 fields.

    Offset map:
      0      : org_len: u8
      1..=31 : org bytes (length=org_len)
      32     : package_len: u8
      33..=63: package bytes
      64     : type_len: u8
      65..=115: type bytes
      116..=119: version_major: u32 LE
      120..=123: version_minor: u32 LE
      124..=127: version_patch: u32 LE
    """

    _fields_ = [
        ("org_len", ctypes.c_uint8),
        ("org", ctypes.c_uint8 * SCHEMA_IDENT_WIRE_MAX_ORG_LEN),
        ("package_len", ctypes.c_uint8),
        ("package", ctypes.c_uint8 * SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN),
        ("type_len", ctypes.c_uint8),
        ("type_name", ctypes.c_uint8 * SCHEMA_IDENT_WIRE_MAX_TYPE_LEN),
        ("version_major", ctypes.c_uint32),
        ("version_minor", ctypes.c_uint32),
        ("version_patch", ctypes.c_uint32),
    ]

    @staticmethod
    def from_segments(
        org: str,
        package: str,
        type_name: str,
        version_major: int,
        version_minor: int,
        version_patch: int,
    ) -> "SchemaIdentWire":
        wire = SchemaIdentWire()
        org_b = org.encode("utf-8")
        pkg_b = package.encode("utf-8")
        type_b = type_name.encode("utf-8")
        if len(org_b) > SCHEMA_IDENT_WIRE_MAX_ORG_LEN:
            raise ValueError(
                f"schema ident org segment is {len(org_b)} bytes (max {SCHEMA_IDENT_WIRE_MAX_ORG_LEN})"
            )
        if len(pkg_b) > SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN:
            raise ValueError(
                f"schema ident package segment is {len(pkg_b)} bytes (max {SCHEMA_IDENT_WIRE_MAX_PACKAGE_LEN})"
            )
        if len(type_b) > SCHEMA_IDENT_WIRE_MAX_TYPE_LEN:
            raise ValueError(
                f"schema ident type segment is {len(type_b)} bytes (max {SCHEMA_IDENT_WIRE_MAX_TYPE_LEN})"
            )
        wire.org_len = len(org_b)
        ctypes.memmove(wire.org, org_b, len(org_b))
        wire.package_len = len(pkg_b)
        ctypes.memmove(wire.package, pkg_b, len(pkg_b))
        wire.type_len = len(type_b)
        ctypes.memmove(wire.type_name, type_b, len(type_b))
        wire.version_major = version_major
        wire.version_minor = version_minor
        wire.version_patch = version_patch
        return wire

    def org_str(self) -> str:
        return bytes(self.org[: self.org_len]).decode("utf-8", errors="replace")

    def package_str(self) -> str:
        return bytes(self.package[: self.package_len]).decode("utf-8", errors="replace")

    def type_str(self) -> str:
        return bytes(self.type_name[: self.type_len]).decode("utf-8", errors="replace")

    def render_joined(self) -> str:
        """Human-facing render only — never round-trip back through a parser."""
        return (
            f"@{self.org_str()}/{self.package_str()}/{self.type_str()}@"
            f"{self.version_major}.{self.version_minor}.{self.version_patch}"
        )


# ABI lock — matches the Rust `const _: () = { assert!(size_of::<SchemaIdentWire>() == 128); ... };`
# in `runtime/streamlib-ipc-types/src/lib.rs`. Drift between Rust + Python +
# Deno layouts trips at import time. Same gate as the Deno-side
# `Deno.UnsafePointerView.getUint8` byte offsets in the cross-language test.
assert ctypes.sizeof(SchemaIdentWire) == SCHEMA_IDENT_WIRE_SIZE, (
    f"Python SchemaIdentWire layout drifted: ctypes.sizeof = "
    f"{ctypes.sizeof(SchemaIdentWire)}, expected {SCHEMA_IDENT_WIRE_SIZE}"
)


class FramePayload(ctypes.Structure):
    """Zero-copy frame payload for iceoryx2 pub/sub. Matches Rust FramePayload layout."""

    _fields_ = [
        ("port_key", PortKey),
        ("schema_ident", SchemaIdentWire),
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
        self,
        port: str,
        schema_ident: "SchemaIdentWire",
        timestamp_ns: int,
        data: bytes,
    ) -> None:
        """Set all fields of the payload. `schema_ident` is a structured
        `SchemaIdentWire` (built via `SchemaIdentWire.from_segments(...)`),
        not a joined string."""
        self.port_key = PortKey.from_str(port)
        self.schema_ident = schema_ident
        self.timestamp_ns = timestamp_ns
        data_len = min(len(data), MAX_PAYLOAD_SIZE)
        self.len = data_len
        ctypes.memmove(self.data, data, data_len)

    def get_port(self) -> str:
        """Get the port key as a string."""
        return self.port_key.as_str()

    def get_schema(self) -> SchemaIdentWire:
        """Get the structured schema identifier."""
        return self.schema_ident

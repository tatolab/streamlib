# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What this extension's native module is allowed to link against.

An extension wheel is installed beside the engine wheel on machines this
project never sees, so it owes the same promise: everything it needs is inside
the artifact, bar the handful of libraries manylinux lets a wheel leave to the
host. A `libssl.so` on the `NEEDED` list would turn `pip install
streamlib-moq` into an import error on whichever machine happens not to have
the version it was built against.

On macOS the promise is the one the engine's macOS wheel keeps: every library
the Mach-O loads is one macOS itself supplies, under `/usr/lib/` or `/System/`.

Binaries are parsed here rather than shelled out to `readelf` or `otool`,
because a test asserting the wheel needs no build tools should not itself need
them. The engine wheel has its own copy of these walks: two standalone projects
cannot import each other's test helpers, which is the cost of an extension being
built exactly as a third party would build one.
"""

import struct
from pathlib import Path

import pytest

import streamlib_moq

# manylinux's policy list: the libraries a conforming wheel may leave to the
# host. Everything else must be inside the artifact.
LIBRARIES_THE_HOST_MAY_SUPPLY = frozenset(
    {
        "libc.so.6",
        "libdl.so.2",
        "libgcc_s.so.1",
        "libm.so.6",
        "libpthread.so.0",
        "librt.so.1",
        "libstdc++.so.6",
        "ld-linux-x86-64.so.2",
        "ld-linux-aarch64.so.1",
    }
)

# The prefix policy maturin and delocate both apply. On macOS 11+ these paths
# do not exist on disk — the libraries live in the dyld shared cache — so the
# load-command strings are matched and never `stat`ed.
MACOS_PATH_PREFIXES_THE_HOST_MAY_SUPPLY = ("/usr/lib/", "/System/")

ELF_MAGIC = b"\x7fELF"
MACH_O_64_LITTLE_ENDIAN_MAGIC = 0xFEEDFACF
LC_REQ_DYLD = 0x80000000
# Every command that makes dyld load another image. `LC_ID_DYLIB` is a dylib's
# own install name and is deliberately not among them.
MACH_O_LINKING_LOAD_COMMANDS = frozenset(
    {
        0x0C,  # LC_LOAD_DYLIB
        0x18 | LC_REQ_DYLD,  # LC_LOAD_WEAK_DYLIB
        0x1F | LC_REQ_DYLD,  # LC_REEXPORT_DYLIB
        0x20,  # LC_LAZY_LOAD_DYLIB
        0x23 | LC_REQ_DYLD,  # LC_LOAD_UPWARD_DYLIB
    }
)


def the_native_extension() -> Path:
    package_directory = Path(streamlib_moq.__file__).parent
    built = sorted(package_directory.glob("_native*.so"))
    if not built:
        pytest.skip("the native module is not built beside the package")
    return built[0]


def dynamic_libraries_needed_by(native_extension: Path) -> "list[str]":
    """What the loader must find for the native module to load, ELF or Mach-O."""
    data = native_extension.read_bytes()
    if data[:4] == ELF_MAGIC:
        return _dynamic_libraries_needed_by_elf(data, native_extension)
    if struct.unpack_from("<I", data, 0)[0] == MACH_O_64_LITTLE_ENDIAN_MAGIC:
        return _dynamic_libraries_linked_by_mach_o(data)
    pytest.fail(f"{native_extension} is neither ELF nor a thin 64-bit Mach-O")


def _dynamic_libraries_linked_by_mach_o(data: bytes) -> "list[str]":
    """The library paths a thin 64-bit Mach-O's load commands ask dyld for."""
    load_command_count, = struct.unpack_from("<I", data, 16)
    linked_library_paths = []
    command_offset = 32  # sizeof(mach_header_64)
    for _ in range(load_command_count):
        command, command_size = struct.unpack_from("<II", data, command_offset)
        if command in MACH_O_LINKING_LOAD_COMMANDS:
            # `dylib_command.dylib.name` is an offset from the command's start.
            name_offset, = struct.unpack_from("<I", data, command_offset + 8)
            start = command_offset + name_offset
            linked_library_paths.append(data[start : data.index(b"\0", start)].decode())
        command_offset += command_size
    return linked_library_paths


def _dynamic_libraries_needed_by_elf(data: bytes, elf_path: Path) -> "list[str]":
    """The `DT_NEEDED` names in an ELF64 little-endian shared object.

    Walks program headers rather than sections: `PT_DYNAMIC` is what the loader
    itself reads, and a stripped object can lose its section table while still
    loading fine.
    """
    if data[4] != 2 or data[5] != 1:
        pytest.fail(f"{elf_path} is not ELF64 little-endian")

    program_header_offset, = struct.unpack_from("<Q", data, 0x20)
    entry_size, entry_count = struct.unpack_from("<HH", data, 0x36)

    dynamic_segment = None
    loadable_segments = []
    for index in range(entry_count):
        header = program_header_offset + index * entry_size
        segment_type, = struct.unpack_from("<I", data, header)
        offset, virtual_address = struct.unpack_from("<QQ", data, header + 0x08)
        size_in_file, = struct.unpack_from("<Q", data, header + 0x20)
        if segment_type == 2:  # PT_DYNAMIC
            dynamic_segment = (offset, size_in_file)
        elif segment_type == 1:  # PT_LOAD
            loadable_segments.append((virtual_address, offset, size_in_file))
    assert dynamic_segment is not None, f"{elf_path} has no PT_DYNAMIC segment"

    def file_offset_of(virtual_address: int) -> int:
        for segment_address, offset, size in loadable_segments:
            if segment_address <= virtual_address < segment_address + size:
                return offset + (virtual_address - segment_address)
        raise AssertionError(f"address {virtual_address:#x} is in no PT_LOAD segment")

    dynamic_offset, dynamic_size = dynamic_segment
    entries = []
    for entry in range(dynamic_offset, dynamic_offset + dynamic_size, 16):
        tag, value = struct.unpack_from("<qQ", data, entry)
        if tag == 0:  # DT_NULL
            break
        entries.append((tag, value))

    string_table = file_offset_of(
        next(value for tag, value in entries if tag == 5)  # DT_STRTAB
    )

    needed = []
    for tag, value in entries:
        if tag != 1:  # DT_NEEDED
            continue
        start = string_table + value
        needed.append(data[start : data.index(b"\0", start)].decode())
    return needed


def test_the_native_extension_links_nothing_the_host_may_not_supply():
    native_extension = the_native_extension()
    needed = dynamic_libraries_needed_by(native_extension)

    assert needed, "an extension module that links nothing did not parse"
    if native_extension.read_bytes()[:4] == ELF_MAGIC:
        beyond_the_host = sorted(set(needed) - LIBRARIES_THE_HOST_MAY_SUPPLY)
    else:
        beyond_the_host = sorted(
            library
            for library in needed
            if not library.startswith(MACOS_PATH_PREFIXES_THE_HOST_MAY_SUPPLY)
        )
    assert not beyond_the_host, (
        f"{beyond_the_host} would have to be installed on the user's machine "
        "for `import streamlib_moq` to work"
    )


def test_the_tls_stack_is_inside_the_artifact():
    """rustls, its `ring` backend and the `aws-lc-rs` that `web-transport-quinn`
    drags in are the reason to check: both are meant to be statically linked, and
    an OpenSSL-backed build of either would put `libssl` and `libcrypto` on this
    list, whose ABI changes under a wheel built against another version."""
    needed = dynamic_libraries_needed_by(the_native_extension())

    assert not [
        library for library in needed if "ssl" in library or "crypto" in library
    ]

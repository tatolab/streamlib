# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What the native extension is allowed to link against.

The wheel carries a C++ GLSL compiler so a kernel author needs no shader
toolchain. "Carries" has to mean statically linked: a `libshaderc.so` on the
`NEEDED` list would make the wheel depend on a system library that manylinux
does not ship and most machines do not have, turning the toolchain-free
promise into a toolchain requirement discovered at import.

The ELF is parsed here rather than shelled out to `readelf`, because a test
asserting the wheel needs no build tools should not itself need binutils.
"""

import importlib
import struct
from pathlib import Path

import pytest

# manylinux's policy list: the libraries a conforming wheel may leave to the
# host. Everything else must be inside the artifact. `libvulkan` is absent
# deliberately — the Vulkan loader is dlopen'd at runtime, which is not a
# `NEEDED` entry and not what this checks.
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

# Every DSO name any of the vendored C++ shader compiler's pieces could take.
SHADER_COMPILER_LIBRARY_STEMS = ("shaderc", "glslang", "SPIRV", "spirv")


def _dynamic_libraries_needed_by(elf_path: Path) -> list[str]:
    """The `DT_NEEDED` names in an ELF64 little-endian shared object.

    Walks program headers rather than sections: `PT_DYNAMIC` is what the
    loader itself reads, and a stripped object can lose its section table
    while still loading fine.
    """
    data = elf_path.read_bytes()
    if data[:4] != b"\x7fELF":
        pytest.skip(f"{elf_path} is not an ELF object")
    if data[4] != 2 or data[5] != 1:
        pytest.skip("this check reads ELF64 little-endian only")

    e_phoff, = struct.unpack_from("<Q", data, 0x20)
    e_phentsize, e_phnum = struct.unpack_from("<HH", data, 0x36)

    dynamic_segment = None
    loadable_segments = []
    for index in range(e_phnum):
        header = e_phoff + index * e_phentsize
        p_type, = struct.unpack_from("<I", data, header)
        p_offset, p_vaddr = struct.unpack_from("<QQ", data, header + 0x08)
        p_filesz, = struct.unpack_from("<Q", data, header + 0x20)
        if p_type == 2:  # PT_DYNAMIC
            dynamic_segment = (p_offset, p_filesz)
        elif p_type == 1:  # PT_LOAD
            loadable_segments.append((p_vaddr, p_offset, p_filesz))
    assert dynamic_segment is not None, f"{elf_path} has no PT_DYNAMIC segment"

    def file_offset_of(virtual_address: int) -> int:
        for p_vaddr, p_offset, p_filesz in loadable_segments:
            if p_vaddr <= virtual_address < p_vaddr + p_filesz:
                return p_offset + (virtual_address - p_vaddr)
        raise AssertionError(f"address {virtual_address:#x} is in no PT_LOAD segment")

    # DT_STRTAB holds the names DT_NEEDED indexes into, so both passes of the
    # dynamic array are needed before any name can be read.
    dynamic_offset, dynamic_size = dynamic_segment
    entries = []
    for entry in range(dynamic_offset, dynamic_offset + dynamic_size, 16):
        d_tag, d_val = struct.unpack_from("<qQ", data, entry)
        if d_tag == 0:  # DT_NULL
            break
        entries.append((d_tag, d_val))

    string_table = next(d_val for d_tag, d_val in entries if d_tag == 5)  # DT_STRTAB
    string_table_offset = file_offset_of(string_table)

    needed = []
    for d_tag, d_val in entries:
        if d_tag != 1:  # DT_NEEDED
            continue
        start = string_table_offset + d_val
        needed.append(data[start : data.index(b"\x00", start)].decode())
    return needed


@pytest.fixture(scope="module")
def native_extension_needed_libraries() -> list[str]:
    engine = importlib.import_module("streamlib._engine")
    assert engine.__file__ is not None, "the native extension has no file on disk"
    return _dynamic_libraries_needed_by(Path(engine.__file__))


def test_the_glsl_compiler_is_linked_statically(native_extension_needed_libraries):
    """The compiler is in the artifact, not on the host."""
    linked_compilers = [
        library
        for library in native_extension_needed_libraries
        if any(stem in library for stem in SHADER_COMPILER_LIBRARY_STEMS)
    ]
    assert not linked_compilers, (
        f"the wheel links the shader compiler dynamically: {linked_compilers}. "
        "Its `build-from-source` feature exists to stop the build script finding "
        "a system libshaderc; a hit here means that probe won"
    )


def test_the_native_extension_links_nothing_the_host_may_not_supply(
    native_extension_needed_libraries,
):
    """Every other portability regression the static link could have caused,
    caught by the same read of the same list."""
    outside_the_policy = sorted(
        set(native_extension_needed_libraries) - LIBRARIES_THE_HOST_MAY_SUPPLY
    )
    assert not outside_the_policy, (
        f"the wheel needs {outside_the_policy}, which manylinux does not promise the "
        f"host has. Full NEEDED list: {sorted(native_extension_needed_libraries)}"
    )

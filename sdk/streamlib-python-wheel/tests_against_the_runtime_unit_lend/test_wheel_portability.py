# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What the runtime unit's native binaries are allowed to link against.

Read from the lend `cargo xtask build-runtime` lays out at
`target/tatolab-runtime/lib/tatolab/lend`, with that directory first on
`PYTHONPATH` — the same files a processor interpreter borrows and the wheel
carries — and from `tatolabd` and `tatolab` in the unit's `bin/` beside it.
Imported from anywhere else, the tests that read binaries fail.

With `TATOLAB_RUNTIME_UNIT_GLIBC_FLOOR` set, as the release's Linux tarball
build sets it, no ELF in the unit may need a newer glibc than that floor.

The wheel carries a C++ GLSL compiler so a kernel author needs no shader
toolchain. "Carries" has to mean statically linked: a `libshaderc.so` on the
`NEEDED` list would make the wheel depend on a system library that manylinux
does not ship and most machines do not have, turning the toolchain-free
promise into a toolchain requirement discovered at import.

The wheel also carries the Vulkan loader and a Vulkan driver — MoltenVK on
macOS, Mesa's lavapipe on Linux — because a stock Mac has neither, and neither
has a Linux machine with no GPU driver installed. Every ELF the lend carries
must still link only what any Linux host supplies, and every Mach-O only what
macOS itself supplies, must be code signed — an arm64 binary whose signature a post-link
rewrite broke loads on the machine that built it and fails on every other, so
on macOS the signature is verified, not merely found — and
must not ask for a newer macOS than the wheel's tag admits.

Binaries are parsed here rather than shelled out to `readelf` or `otool`,
because a test asserting the wheel needs no build tools should not itself need
them. A binary this cannot parse fails the test; it is never skipped.
"""

import importlib
import os
import re
import shutil
import struct
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import pytest

# manylinux's policy list: the libraries a conforming wheel may leave to the
# host. Everything else must be inside the artifact. `libvulkan` is absent
# deliberately — the engine dlopens the Vulkan loader at runtime and the loader
# dlopens the driver, neither of which is a `NEEDED` entry.
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

# Every DSO name any of the vendored C++ shader compiler's pieces could take.
SHADER_COMPILER_LIBRARY_STEMS = ("shaderc", "glslang", "SPIRV", "spirv")

ELF_MAGIC = b"\x7fELF"
DT_VERNEED = 0x6FFFFFFE
DT_VERNEEDNUM = 0x6FFFFFFF
MACH_O_64_LITTLE_ENDIAN_MAGIC = 0xFEEDFACF
MACH_O_FAT_MAGICS = (0xCAFEBABE, 0xCAFEBABF)
MACH_O_CPU_TYPE_ARM64 = 0x0100000C

LC_REQ_DYLD = 0x80000000
LC_LOAD_DYLIB = 0x0C
LC_ID_DYLIB = 0x0D
LC_LOAD_WEAK_DYLIB = 0x18 | LC_REQ_DYLD
LC_REEXPORT_DYLIB = 0x1F | LC_REQ_DYLD
LC_LAZY_LOAD_DYLIB = 0x20
LC_LOAD_UPWARD_DYLIB = 0x23 | LC_REQ_DYLD
LC_CODE_SIGNATURE = 0x1D
LC_VERSION_MIN_MACOSX = 0x24
LC_BUILD_VERSION = 0x32
PLATFORM_MACOS = 1
# Every command that makes dyld load another image. `LC_ID_DYLIB` is a dylib's
# own install name and is deliberately not among them.
MACH_O_LINKING_LOAD_COMMANDS = frozenset(
    {
        LC_LOAD_DYLIB,
        LC_LOAD_WEAK_DYLIB,
        LC_REEXPORT_DYLIB,
        LC_LAZY_LOAD_DYLIB,
        LC_LOAD_UPWARD_DYLIB,
    }
)

MacOSVersion = tuple[int, int, int]

# The oldest glibc every ELF in the runtime unit must run on; unset, the floor
# goes unchecked, because only a manylinux build can meet one.
RUNTIME_UNIT_GLIBC_FLOOR_ENVIRONMENT_VARIABLE = "TATOLAB_RUNTIME_UNIT_GLIBC_FLOOR"

# The runtime unit's own executables, in its `bin/`.
RUNTIME_UNIT_EXECUTABLE_NAMES = ("tatolabd", "tatolab")

GlibcVersion = tuple[int, ...]

# Where `cargo xtask build-runtime` lays the lend out, below the workspace's `target/`.
RUNTIME_UNIT_LEND_DIRECTORY_TRAILING_PARTS = ("tatolab-runtime", "lib", "tatolab", "lend")


def runtime_unit_lend_directory() -> Path:
    """The lend the imported `tatolab.runtime` came from, refusing any other origin."""
    runtime_package = importlib.import_module("tatolab.runtime")
    assert runtime_package.__file__ is not None, "tatolab.runtime has no file on disk"
    lend_directory = Path(runtime_package.__file__).resolve().parents[2]
    if lend_directory.parts[-len(RUNTIME_UNIT_LEND_DIRECTORY_TRAILING_PARTS) :] != (
        RUNTIME_UNIT_LEND_DIRECTORY_TRAILING_PARTS
    ):
        raise AssertionError(
            f"tatolab.runtime was imported from {lend_directory}, not the runtime unit's lend: "
            "run `cargo xtask build-runtime`, then these tests with "
            "<workspace>/target/tatolab-runtime/lib/tatolab/lend first on PYTHONPATH"
        )
    return lend_directory


def runtime_unit_dist_info_directory(lend_directory: Path) -> Path:
    """The one `.dist-info` the wheel left beside `tatolab/` in the lend."""
    dist_info_directories = sorted(lend_directory.glob("*.dist-info"))
    assert len(dist_info_directories) == 1, (lend_directory, dist_info_directories)
    return dist_info_directories[0]


class NativeBinaryUnreadable(Exception):
    """A file shaped like a native binary that this module cannot parse."""


@dataclass(frozen=True)
class ElfDynamicSectionSummary:
    """What the portability policy reads out of one ELF64 little-endian object."""

    needed_libraries: list[str]
    # Every `GLIBC_x.y` its version-needs entries name, whichever library they
    # are against — what decides the oldest glibc it loads on.
    glibc_versions_needed: list[GlibcVersion]


def _read_elf_dynamic_section(data: bytes, elf_path: Path) -> ElfDynamicSectionSummary:
    """The `DT_NEEDED` names and the glibc versions an ELF64 little-endian object needs.

    Walks program headers rather than sections: `PT_DYNAMIC` is what the
    loader itself reads, and a stripped object can lose its section table
    while still loading fine.
    """
    if data[4] != 2 or data[5] != 1:
        raise NativeBinaryUnreadable(f"{elf_path} is not ELF64 little-endian")

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

    # DT_STRTAB holds the names DT_NEEDED and the version-needs entries index
    # into, so both passes of the dynamic array are needed before any name can
    # be read.
    dynamic_offset, dynamic_size = dynamic_segment
    entries = []
    for entry in range(dynamic_offset, dynamic_offset + dynamic_size, 16):
        d_tag, d_val = struct.unpack_from("<qQ", data, entry)
        if d_tag == 0:  # DT_NULL
            break
        entries.append((d_tag, d_val))

    string_table = next(d_val for d_tag, d_val in entries if d_tag == 5)  # DT_STRTAB
    string_table_offset = file_offset_of(string_table)

    def string_at(string_table_index: int) -> str:
        start = string_table_offset + string_table_index
        return data[start : data.index(b"\x00", start)].decode()

    needed = [string_at(d_val) for d_tag, d_val in entries if d_tag == 1]  # DT_NEEDED

    glibc_versions_needed = []
    version_needs = dict(
        (d_tag, d_val) for d_tag, d_val in entries if d_tag in (DT_VERNEED, DT_VERNEEDNUM)
    )
    if DT_VERNEED in version_needs:
        version_need_offset = file_offset_of(version_needs[DT_VERNEED])
        for _ in range(version_needs.get(DT_VERNEEDNUM, 0)):
            # Elf64_Verneed: vn_version, vn_cnt, vn_file, vn_aux, vn_next.
            _vn_version, vn_cnt, _vn_file, vn_aux, vn_next = struct.unpack_from(
                "<HHIII", data, version_need_offset
            )
            version_need_auxiliary_offset = version_need_offset + vn_aux
            for _ in range(vn_cnt):
                # Elf64_Vernaux: vna_hash, vna_flags, vna_other, vna_name, vna_next.
                _vna_hash, _vna_flags, _vna_other, vna_name, vna_next = struct.unpack_from(
                    "<IHHII", data, version_need_auxiliary_offset
                )
                if glibc_version := re.fullmatch(r"GLIBC_(\d+(?:\.\d+)*)", string_at(vna_name)):
                    glibc_versions_needed.append(
                        tuple(int(part) for part in glibc_version.group(1).split("."))
                    )
                version_need_auxiliary_offset += vna_next
            version_need_offset += vn_next
    return ElfDynamicSectionSummary(
        needed_libraries=needed, glibc_versions_needed=glibc_versions_needed
    )


def _dynamic_libraries_needed_by_elf(data: bytes, elf_path: Path) -> list[str]:
    """The `DT_NEEDED` names in an ELF64 little-endian shared object."""
    return _read_elf_dynamic_section(data, elf_path).needed_libraries


@dataclass(frozen=True)
class MachOLoadCommandSummary:
    """What the portability policy reads out of one thin arm64 Mach-O."""

    linked_library_paths: list[str]
    is_code_signed: bool
    minimum_macos_version: Optional[MacOSVersion]
    # `LC_BUILD_VERSION.platform`; absent for a binary that states its floor
    # through the older `LC_VERSION_MIN_MACOSX`, which is macOS by definition.
    build_platform: Optional[int] = None


def _macos_version_from_nibbles(encoded_version: int) -> MacOSVersion:
    """Decode the `xxxx.yy.zz` packing `LC_BUILD_VERSION.minos` uses."""
    return (encoded_version >> 16, (encoded_version >> 8) & 0xFF, encoded_version & 0xFF)


def _read_mach_o_load_commands(data: bytes, mach_o_path: Path) -> MachOLoadCommandSummary:
    """Walk a thin 64-bit Mach-O's load commands."""
    if len(data) < 32:
        raise NativeBinaryUnreadable(f"{mach_o_path} is too short for a Mach-O header")
    magic_big_endian, = struct.unpack_from(">I", data, 0)
    if magic_big_endian in MACH_O_FAT_MAGICS:
        raise NativeBinaryUnreadable(
            f"{mach_o_path} is a fat Mach-O; the wheel is arm64 only, so every binary "
            "it carries must be thinned (and re-signed after thinning)"
        )
    magic, cpu_type, _cpu_subtype, _file_type, load_command_count, load_commands_size = (
        struct.unpack_from("<IiiIII", data, 0)
    )
    if magic != MACH_O_64_LITTLE_ENDIAN_MAGIC:
        raise NativeBinaryUnreadable(f"{mach_o_path} is not a 64-bit little-endian Mach-O")
    if cpu_type != MACH_O_CPU_TYPE_ARM64:
        raise NativeBinaryUnreadable(f"{mach_o_path} is not arm64 (cputype {cpu_type:#x})")
    load_commands_end = 32 + load_commands_size
    if load_commands_end > len(data):
        raise NativeBinaryUnreadable(f"{mach_o_path} declares load commands past its end")

    linked_library_paths = []
    is_code_signed = False
    minimum_macos_version = None
    build_platform = None
    command_offset = 32  # sizeof(mach_header_64)
    for _ in range(load_command_count):
        if command_offset + 8 > load_commands_end:
            raise NativeBinaryUnreadable(f"{mach_o_path} has a truncated load command")
        command, command_size = struct.unpack_from("<II", data, command_offset)
        if command_size < 8 or command_offset + command_size > load_commands_end:
            raise NativeBinaryUnreadable(f"{mach_o_path} has a malformed load command size")
        if command in MACH_O_LINKING_LOAD_COMMANDS:
            name_offset, = struct.unpack_from("<I", data, command_offset + 8)
            name_start = command_offset + name_offset
            name_end = data.index(b"\x00", name_start, command_offset + command_size)
            linked_library_paths.append(data[name_start:name_end].decode())
        elif command == LC_CODE_SIGNATURE:
            is_code_signed = True
        elif command == LC_BUILD_VERSION:
            build_platform, encoded_minimum = struct.unpack_from("<II", data, command_offset + 8)
            minimum_macos_version = _macos_version_from_nibbles(encoded_minimum)
        elif command == LC_VERSION_MIN_MACOSX:
            encoded_minimum, = struct.unpack_from("<I", data, command_offset + 8)
            minimum_macos_version = _macos_version_from_nibbles(encoded_minimum)
        command_offset += command_size

    return MachOLoadCommandSummary(
        linked_library_paths=linked_library_paths,
        is_code_signed=is_code_signed,
        minimum_macos_version=minimum_macos_version,
        build_platform=build_platform,
    )


def _is_mach_o(data: bytes) -> bool:
    if len(data) < 4:
        return False
    magic_big_endian, = struct.unpack_from(">I", data, 0)
    magic_little_endian, = struct.unpack_from("<I", data, 0)
    return (
        magic_big_endian in MACH_O_FAT_MAGICS
        or magic_little_endian == MACH_O_64_LITTLE_ENDIAN_MAGIC
    )


def _libraries_linked_by(binary_path: Path) -> list[str]:
    """What a native binary makes the dynamic loader load, ELF or Mach-O."""
    data = binary_path.read_bytes()
    if data[:4] == ELF_MAGIC:
        return _dynamic_libraries_needed_by_elf(data, binary_path)
    if _is_mach_o(data):
        return _read_mach_o_load_commands(data, binary_path).linked_library_paths
    raise NativeBinaryUnreadable(f"{binary_path} is neither ELF nor Mach-O")


def glibc_versions_newer_than_the_floor(
    elf_summary: ElfDynamicSectionSummary, glibc_floor: GlibcVersion
) -> list[GlibcVersion]:
    """Each glibc version an ELF needs that a machine at the floor does not have."""
    return sorted({version for version in elf_summary.glibc_versions_needed if version > glibc_floor})


def _glibc_version_from_text(glibc_version_text: str) -> GlibcVersion:
    return tuple(int(part) for part in glibc_version_text.split("."))


def mach_o_portability_violations(
    load_commands: MachOLoadCommandSummary,
    wheel_minimum_macos_version: MacOSVersion,
) -> list[str]:
    """Every way one Mach-O breaks the wheel's portability policy, in words."""
    violations = [
        f"links {linked_library_path}, which macOS does not supply"
        for linked_library_path in load_commands.linked_library_paths
        if not linked_library_path.startswith(MACOS_PATH_PREFIXES_THE_HOST_MAY_SUPPLY)
    ]
    if not load_commands.is_code_signed:
        violations.append(
            "carries no LC_CODE_SIGNATURE — arm64 macOS refuses to load it; a post-link "
            "rewrite needs `codesign -f -s -` after it"
        )
    if load_commands.build_platform not in (None, PLATFORM_MACOS):
        violations.append(
            f"is built for platform {load_commands.build_platform}, not macOS — its minimum "
            "version says nothing about the macOS floor"
        )
    if load_commands.minimum_macos_version is None:
        violations.append("declares no minimum macOS version")
    elif load_commands.minimum_macos_version > wheel_minimum_macos_version:
        violations.append(
            f"needs macOS {'.'.join(map(str, load_commands.minimum_macos_version))}, newer "
            f"than the wheel's tag admits ({'.'.join(map(str, wheel_minimum_macos_version))})"
        )
    return violations


def code_signature_verification_failure(mach_o_path: Path) -> Optional[str]:
    """What `codesign --verify --strict` says is wrong with a signature, if anything.

    Presence of `LC_CODE_SIGNATURE` is not validity: a byte rewritten after
    signing leaves the command in place and the signature broken, and only
    macOS's own verifier can say so.
    """
    verification = subprocess.run(
        ["codesign", "--verify", "--strict", str(mach_o_path)],
        capture_output=True,
        text=True,
    )
    if verification.returncode == 0:
        return None
    return f"fails `codesign --verify --strict`: {verification.stderr.strip()}"


# A shared object's file name: `.so`, a versioned soname such as `.so.1`, or
# `.dylib`.
NATIVE_LIBRARY_FILE_NAME_PATTERN = re.compile(r".*(\.so(\.\d+)*|\.dylib)")


def _native_binaries_in(directory: Path) -> list[Path]:
    """Every ELF or Mach-O file under a directory, plus anything named like one —
    which then has to parse, or the test fails."""
    native_binaries = []
    for candidate in sorted(directory.rglob("*")):
        if not candidate.is_file():
            continue
        with candidate.open("rb") as candidate_file:
            leading_bytes = candidate_file.read(4)
        if (
            leading_bytes == ELF_MAGIC
            or _is_mach_o(leading_bytes)
            or NATIVE_LIBRARY_FILE_NAME_PATTERN.fullmatch(candidate.name)
        ):
            native_binaries.append(candidate)
    return native_binaries


def elf_libraries_the_host_is_not_promised(elf_needed_libraries: list[str]) -> list[str]:
    """Each `NEEDED` library of an ELF outside what any Linux host supplies."""
    return sorted(set(elf_needed_libraries) - LIBRARIES_THE_HOST_MAY_SUPPLY)


@pytest.fixture(scope="module")
def lend_directory() -> Path:
    return runtime_unit_lend_directory()


@pytest.fixture(scope="module")
def native_extension_path(lend_directory: Path) -> Path:
    engine = importlib.import_module("tatolab.runtime._engine")
    assert engine.__file__ is not None, "the native extension has no file on disk"
    native_extension_path = Path(engine.__file__).resolve()
    assert native_extension_path.parent == lend_directory / "tatolab" / "runtime", (
        f"tatolab.runtime._engine was imported from {native_extension_path}, outside the lend "
        f"at {lend_directory}"
    )
    return native_extension_path


@pytest.fixture(scope="module")
def native_extension_needed_libraries(native_extension_path: Path) -> list[str]:
    return _libraries_linked_by(native_extension_path)


@pytest.fixture(scope="module")
def native_binaries_the_lend_carries(lend_directory: Path) -> list[Path]:
    return _native_binaries_in(lend_directory)


@pytest.fixture(scope="module")
def mach_o_binaries_the_lend_carries(native_binaries_the_lend_carries) -> list[Path]:
    return [
        binary
        for binary in native_binaries_the_lend_carries
        if binary.read_bytes()[:4] != ELF_MAGIC
    ]


@pytest.fixture(scope="module")
def runtime_unit_executables(lend_directory: Path) -> list[Path]:
    """`tatolabd` and `tatolab`, from the `bin/` beside the lend's `lib/`."""
    runtime_unit_root = lend_directory.parents[2]
    executables = [
        runtime_unit_root / "bin" / executable_name for executable_name in RUNTIME_UNIT_EXECUTABLE_NAMES
    ]
    missing_executables = [str(executable) for executable in executables if not executable.is_file()]
    assert not missing_executables, f"the runtime unit carries no {missing_executables}"
    return executables


@pytest.fixture(scope="module")
def wheel_minimum_macos_version(lend_directory: Path) -> Optional[MacOSVersion]:
    """The macOS floor the lend's wheel tag admits, if it has one."""
    wheel_metadata = (runtime_unit_dist_info_directory(lend_directory) / "WHEEL").read_text()
    macos_platform_tags = re.findall(r"^Tag: .*-macosx_(\d+)_(\d+)_\w+$", wheel_metadata, re.M)
    if not macos_platform_tags:
        return None
    return min((int(major), int(minor), 0) for major, minor in macos_platform_tags)


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


def test_every_elf_the_lend_carries_links_only_what_the_host_may_supply(
    native_binaries_the_lend_carries, native_extension_path
):
    """`_engine`, the Vulkan loader and lavapipe alike: a library on any of their
    `NEEDED` lists that the host is not promised fails the unit on every machine
    that lacks it. Vacuous on macOS, which carries no ELF."""
    elf_binaries = [
        binary for binary in native_binaries_the_lend_carries if binary.read_bytes()[:4] == ELF_MAGIC
    ]
    if native_extension_path.read_bytes()[:4] == ELF_MAGIC:
        assert native_extension_path in elf_binaries, elf_binaries
    outside_the_policy_per_binary = {}
    for binary in elf_binaries:
        needed_libraries = _dynamic_libraries_needed_by_elf(binary.read_bytes(), binary)
        if outside_the_policy := elf_libraries_the_host_is_not_promised(needed_libraries):
            outside_the_policy_per_binary[str(binary)] = {
                "outside the policy": outside_the_policy,
                "needed": sorted(needed_libraries),
            }
    assert not outside_the_policy_per_binary, (
        f"ELFs in the lend need libraries the host is not promised to have: "
        f"{outside_the_policy_per_binary}"
    )


def test_every_mach_o_the_lend_carries_is_portable(
    mach_o_binaries_the_lend_carries, wheel_minimum_macos_version
):
    """The extension and the bundled Vulkan driver alike: system links only,
    signed, and no newer than the tag. Vacuous on Linux, which carries none."""
    if not mach_o_binaries_the_lend_carries:
        return
    assert wheel_minimum_macos_version is not None, (
        "the lend carries Mach-O but its wheel tag names no macOS version"
    )
    # A macOS host carries `codesign`, so there the signature is verified as
    # well as found. A Linux host carries no Mach-O to verify.
    signatures_are_verifiable = shutil.which("codesign") is not None
    violations_per_binary = {}
    for binary in mach_o_binaries_the_lend_carries:
        violations = mach_o_portability_violations(
            _read_mach_o_load_commands(binary.read_bytes(), binary),
            wheel_minimum_macos_version,
        )
        if signatures_are_verifiable and (
            verification_failure := code_signature_verification_failure(binary)
        ):
            violations.append(verification_failure)
        if violations:
            violations_per_binary[str(binary)] = violations
    assert not violations_per_binary, f"non-portable Mach-O in the lend: {violations_per_binary}"


def test_every_native_binary_the_lend_carries_parses(
    native_binaries_the_lend_carries, native_extension_path
):
    """A binary the proof cannot read is a failure, never a skipped check."""
    assert native_extension_path in native_binaries_the_lend_carries
    for binary in native_binaries_the_lend_carries:
        _libraries_linked_by(binary)


def test_the_runtime_units_executables_link_nothing_the_host_may_not_supply(
    runtime_unit_executables,
):
    """`tatolabd` and `tatolab` reach machines this project never sees, as the wheel does."""
    outside_the_policy_per_executable = {}
    for executable in runtime_unit_executables:
        linked_libraries = _libraries_linked_by(executable)
        if executable.read_bytes()[:4] == ELF_MAGIC:
            outside_the_policy = elf_libraries_the_host_is_not_promised(linked_libraries)
        else:
            outside_the_policy = sorted(
                library
                for library in linked_libraries
                if not library.startswith(MACOS_PATH_PREFIXES_THE_HOST_MAY_SUPPLY)
            )
        if outside_the_policy:
            outside_the_policy_per_executable[str(executable)] = outside_the_policy
    assert not outside_the_policy_per_executable, (
        f"the runtime unit's executables need libraries the host is not promised to have: "
        f"{outside_the_policy_per_executable}"
    )


def test_no_elf_in_the_runtime_unit_needs_a_newer_glibc_than_its_floor(
    runtime_unit_executables, native_binaries_the_lend_carries
):
    """What a manylinux build promises, held over the executables too, which no wheel audit reads."""
    glibc_floor_text = os.environ.get(RUNTIME_UNIT_GLIBC_FLOOR_ENVIRONMENT_VARIABLE, "")
    if not glibc_floor_text:
        pytest.skip(
            f"{RUNTIME_UNIT_GLIBC_FLOOR_ENVIRONMENT_VARIABLE} is unset: only the release's "
            "manylinux build promises a glibc floor"
        )
    glibc_floor = _glibc_version_from_text(glibc_floor_text)
    elf_binaries = [
        binary
        for binary in [*runtime_unit_executables, *native_binaries_the_lend_carries]
        if binary.read_bytes()[:4] == ELF_MAGIC
    ]
    assert elf_binaries, "the runtime unit carries no ELF to hold to a glibc floor"
    newer_than_the_floor_per_binary = {}
    for binary in elf_binaries:
        newer_than_the_floor = glibc_versions_newer_than_the_floor(
            _read_elf_dynamic_section(binary.read_bytes(), binary), glibc_floor
        )
        if newer_than_the_floor:
            newer_than_the_floor_per_binary[str(binary)] = [
                ".".join(map(str, version)) for version in newer_than_the_floor
            ]
    assert not newer_than_the_floor_per_binary, (
        f"binaries needing a newer glibc than the {glibc_floor_text} floor: "
        f"{newer_than_the_floor_per_binary}"
    )


def _synthetic_elf(needed_libraries: list[str], glibc_versions_needed: list[str]) -> bytes:
    """A shared object with one PT_LOAD mapping it whole, at address 0, and a PT_DYNAMIC."""
    string_table = bytearray(b"\x00")

    def add_string(text: str) -> int:
        index = len(string_table)
        string_table.extend(text.encode() + b"\x00")
        return index

    needed_library_names = [add_string(library) for library in needed_libraries]
    version_need_file_name = add_string("libc.so.6")
    glibc_version_names = [add_string(f"GLIBC_{version}") for version in glibc_versions_needed]

    elf_header_size, program_header_size = 64, 56
    string_table_offset = elf_header_size + 2 * program_header_size
    version_need_offset = (string_table_offset + len(string_table) + 7) // 8 * 8
    version_need_auxiliaries = b"".join(
        struct.pack("<IHHII", 0, 0, 0, name, 16 if position + 1 < len(glibc_version_names) else 0)
        for position, name in enumerate(glibc_version_names)
    )
    version_need = (
        struct.pack("<HHIII", 1, len(glibc_version_names), version_need_file_name, 16, 0)
        + version_need_auxiliaries
    )
    dynamic_offset = version_need_offset + len(version_need)
    dynamic_entries = [(1, name) for name in needed_library_names] + [
        (5, string_table_offset),
        (DT_VERNEED, version_need_offset),
        (DT_VERNEEDNUM, 1),
        (0, 0),
    ]
    dynamic = b"".join(struct.pack("<qQ", d_tag, d_val) for d_tag, d_val in dynamic_entries)
    total_size = dynamic_offset + len(dynamic)

    elf_header = struct.pack(
        "<16sHHIQQQIHHHHHH",
        ELF_MAGIC + bytes([2, 1, 1]) + bytes(9),
        3,  # ET_DYN
        0x3E,  # EM_X86_64
        1,
        0,
        elf_header_size,
        0,
        0,
        elf_header_size,
        program_header_size,
        2,
        0,
        0,
        0,
    )
    loadable_segment = struct.pack("<IIQQQQQQ", 1, 5, 0, 0, 0, total_size, total_size, 0x1000)
    dynamic_segment = struct.pack(
        "<IIQQQQQQ", 2, 6, dynamic_offset, dynamic_offset, dynamic_offset, len(dynamic), len(dynamic), 8
    )
    synthetic_elf = bytearray(elf_header + loadable_segment + dynamic_segment + string_table)
    synthetic_elf.extend(bytes(version_need_offset - len(synthetic_elf)))
    synthetic_elf.extend(version_need + dynamic)
    return bytes(synthetic_elf)


def test_an_elf_needing_a_newer_glibc_than_the_floor_is_caught():
    summary = _read_elf_dynamic_section(
        _synthetic_elf(["libc.so.6"], ["2.2.5", "2.28", "2.34"]), Path("synthetic.so")
    )
    assert summary.needed_libraries == ["libc.so.6"]
    assert glibc_versions_newer_than_the_floor(summary, (2, 28)) == [(2, 34)]


def test_an_elf_at_the_glibc_floor_passes():
    summary = _read_elf_dynamic_section(
        _synthetic_elf(["libc.so.6", "libm.so.6"], ["2.2.5", "2.17", "2.28"]), Path("synthetic.so")
    )
    assert summary.glibc_versions_needed == [(2, 2, 5), (2, 17), (2, 28)]
    assert glibc_versions_newer_than_the_floor(summary, (2, 28)) == []


def test_an_elf_linking_a_library_the_host_may_not_supply_is_caught():
    needed_libraries = _read_elf_dynamic_section(
        _synthetic_elf(["libdrm.so.2", "libc.so.6", "libxcb.so.1"], ["2.2.5"]), Path("libvulkan_lvp.so")
    ).needed_libraries
    assert elf_libraries_the_host_is_not_promised(needed_libraries) == ["libdrm.so.2", "libxcb.so.1"]


def test_an_elf_linking_only_what_the_host_supplies_passes():
    needed_libraries = _read_elf_dynamic_section(
        _synthetic_elf(
            ["libc.so.6", "libm.so.6", "libstdc++.so.6", "libgcc_s.so.1", "libdl.so.2", "libpthread.so.0"],
            ["2.2.5"],
        ),
        Path("libvulkan_lvp.so"),
    ).needed_libraries
    assert elf_libraries_the_host_is_not_promised(needed_libraries) == []


def test_a_versioned_soname_that_does_not_parse_fails_rather_than_skips(tmp_path: Path):
    named_like_the_vulkan_loader = tmp_path / "libvulkan.so.1"
    named_like_the_vulkan_loader.write_bytes(b"not a binary")
    (tmp_path / "lvp_icd.x86_64.json").write_text("{}")

    assert _native_binaries_in(tmp_path) == [named_like_the_vulkan_loader]
    with pytest.raises(NativeBinaryUnreadable):
        _libraries_linked_by(named_like_the_vulkan_loader)


def _synthetic_dylib_load_command(command: int, library_path: str) -> bytes:
    name = library_path.encode() + b"\x00"
    command_size = (24 + len(name) + 7) // 8 * 8
    return struct.pack("<IIIIII", command, command_size, 24, 2, 0x10000, 0x10000) + name.ljust(
        command_size - 24, b"\x00"
    )


def _synthetic_build_version_load_command(minimum_macos_version: MacOSVersion) -> bytes:
    major, minor, patch = minimum_macos_version
    encoded_minimum = (major << 16) | (minor << 8) | patch
    platform_macos = 1
    return struct.pack("<IIIIII", LC_BUILD_VERSION, 24, platform_macos, encoded_minimum, encoded_minimum, 0)


def _synthetic_code_signature_load_command() -> bytes:
    return struct.pack("<IIII", LC_CODE_SIGNATURE, 16, 0, 0)


def _synthetic_arm64_dylib(load_commands: list[bytes]) -> bytes:
    mh_dylib = 6
    header = struct.pack(
        "<IiiIIIII",
        MACH_O_64_LITTLE_ENDIAN_MAGIC,
        MACH_O_CPU_TYPE_ARM64,
        0,
        mh_dylib,
        len(load_commands),
        sum(map(len, load_commands)),
        0,
        0,
    )
    return header + b"".join(load_commands)


WHEEL_TAG_FLOOR_FOR_SYNTHETIC_BINARIES: MacOSVersion = (15, 0, 0)


def _violations_of_synthetic(load_commands: list[bytes]) -> list[str]:
    return mach_o_portability_violations(
        _read_mach_o_load_commands(_synthetic_arm64_dylib(load_commands), Path("synthetic.dylib")),
        WHEEL_TAG_FLOOR_FOR_SYNTHETIC_BINARIES,
    )


def test_a_signed_system_only_dylib_at_the_floor_passes():
    """The positive control the negative cases below are measured against."""
    assert (
        _violations_of_synthetic(
            [
                _synthetic_dylib_load_command(LC_ID_DYLIB, "@rpath/libvulkan.1.dylib"),
                _synthetic_dylib_load_command(LC_LOAD_DYLIB, "/usr/lib/libSystem.B.dylib"),
                _synthetic_dylib_load_command(
                    LC_LOAD_DYLIB,
                    "/System/Library/Frameworks/Metal.framework/Versions/A/Metal",
                ),
                _synthetic_build_version_load_command((15, 0, 0)),
                _synthetic_code_signature_load_command(),
            ]
        )
        == []
    ), "a dylib's own install name is not a link, and system paths are allowed"


def test_an_unsigned_binary_is_caught():
    violations = _violations_of_synthetic(
        [
            _synthetic_dylib_load_command(LC_LOAD_DYLIB, "/usr/lib/libSystem.B.dylib"),
            _synthetic_build_version_load_command((15, 0, 0)),
        ]
    )
    assert any("LC_CODE_SIGNATURE" in violation for violation in violations), violations


@pytest.mark.parametrize("linking_command", sorted(MACH_O_LINKING_LOAD_COMMANDS))
def test_a_binary_linking_outside_the_system_is_caught(linking_command: int):
    violations = _violations_of_synthetic(
        [
            _synthetic_dylib_load_command(linking_command, "/opt/homebrew/lib/libvulkan.1.dylib"),
            _synthetic_build_version_load_command((15, 0, 0)),
            _synthetic_code_signature_load_command(),
        ]
    )
    assert violations == [
        "links /opt/homebrew/lib/libvulkan.1.dylib, which macOS does not supply"
    ]


def test_a_binary_needing_a_newer_macos_than_the_tag_is_caught():
    violations = _violations_of_synthetic(
        [_synthetic_build_version_load_command((26, 0, 0)), _synthetic_code_signature_load_command()]
    )
    assert any("needs macOS 26.0.0" in violation for violation in violations), violations


def test_a_binary_built_for_another_apple_platform_is_caught():
    ios_platform = 2
    build_version_for_ios = struct.pack("<IIIIII", LC_BUILD_VERSION, 24, ios_platform, 12 << 16, 12 << 16, 0)
    violations = _violations_of_synthetic(
        [build_version_for_ios, _synthetic_code_signature_load_command()]
    )
    assert any("not macOS" in violation for violation in violations), violations


@pytest.mark.skipif(sys.platform != "darwin", reason="`codesign` is macOS's own verifier")
def test_a_signed_binary_rewritten_after_signing_is_caught(
    mach_o_binaries_the_lend_carries, tmp_path: Path
):
    """The failure a presence check cannot see: the command survives, the
    signature does not."""
    smallest_signed_binary = min(mach_o_binaries_the_lend_carries, key=lambda binary: binary.stat().st_size)
    rewritten_copy = tmp_path / smallest_signed_binary.name
    rewritten_bytes = bytearray(smallest_signed_binary.read_bytes())
    rewritten_bytes[len(rewritten_bytes) // 2] ^= 0xFF
    rewritten_copy.write_bytes(bytes(rewritten_bytes))

    assert code_signature_verification_failure(smallest_signed_binary) is None
    assert _read_mach_o_load_commands(bytes(rewritten_bytes), rewritten_copy).is_code_signed
    assert code_signature_verification_failure(rewritten_copy) is not None


def test_a_fat_binary_fails_rather_than_skips():
    fat_header = struct.pack(">II", 0xCAFEBABE, 2) + b"\x00" * 40
    with pytest.raises(NativeBinaryUnreadable, match="fat Mach-O"):
        _read_mach_o_load_commands(fat_header, Path("fat.dylib"))


def test_an_unreadable_binary_fails_rather_than_skips(tmp_path: Path):
    named_like_a_library = tmp_path / "libtruncated.dylib"
    named_like_a_library.write_bytes(b"not a binary")

    assert _native_binaries_in(tmp_path) == [named_like_a_library]
    with pytest.raises(NativeBinaryUnreadable):
        _libraries_linked_by(named_like_a_library)

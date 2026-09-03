# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The installed wheel carries the notices it owes, with real text in them.

Shipping a wheel is binary distribution, so the BSD-3 and MIT reproduce-the-
notice terms and Apache-2.0 §4(d) both bite. PEP 639 `license-files` is how a
wheel discharges that: the files land in `.dist-info/licenses/` and are named
by `License-File:` in `METADATA`.

Read through `importlib.metadata` rather than off disk, because the failure
this guards against is not a missing file in the repo — it is a file that is
present in the repo and absent from the artifact. Only the installed
distribution can tell those apart.
"""

from importlib.metadata import distribution

import pytest

# The C++ projects compiled into the engine from vendored sources. Four arrive
# through `shaderc-sys` as `libshaderc_combined.a`; libopus arrives through
# `opusic-sys`, statically linked as `libopus.a` so the wheel carries the Opus
# codec without a `DT_NEEDED` entry; VulkanMemoryAllocator and
# Vulkan-Headers are checked into `vendor/tatolab-vulkanalia-vma/` and compiled
# by its build script; PipeWire's SPA layer is checked into
# `vendor/pipewire-headers/` and compiled by the engine's own build script — no
# PipeWire library is linked, but its header-only inline code ships in the
# binary. `cargo about` reads `cargo metadata`, and none of them is a package in
# that graph, so tooling cannot find them — they reach the notices only because
# the generator appends them by hand.
VENDORED_CPP_PROJECT_NAMES = (
    "shaderc",
    "glslang",
    "SPIRV-Tools",
    "SPIRV-Headers",
    "libopus",
    "VulkanMemoryAllocator",
    "Vulkan-Headers",
    "PipeWire",
)

# A thin sample of the Rust closure, one per link shape: the IPC transport, the
# binding layer this wheel is built on, and the GLSL compiler that pulled the
# vendored C++ in. Enough to catch a notices file generated against the wrong
# manifest; not so many that a dependency swap fails an unrelated test.
SAMPLED_RUST_DEPENDENCY_NAMES = ("iceoryx2", "pyo3", "shaderc")

NOTICES_LICENSE_FILE_NAME = "THIRD-PARTY-NOTICES.md"
BUSL_LICENSE_FILE_NAME = "LICENSE"


@pytest.fixture(scope="module")
def installed_streamlib_distribution():
    return distribution("streamlib")


def read_shipped_license_file(installed_streamlib_distribution, license_file_name):
    """Read one `.dist-info/licenses/` entry out of the installed distribution.

    `Distribution.read_text` resolves relative to the `.dist-info` directory,
    which is where PEP 639 puts `licenses/` — so this reads the artifact's own
    copy, never the repo's.
    """
    contents = installed_streamlib_distribution.read_text(
        f"licenses/{license_file_name}"
    )
    assert contents is not None, (
        f"{license_file_name} is not in the installed .dist-info/licenses/ — "
        "the wheel ships third-party code and discharges its notice obligation nowhere"
    )
    return contents


def test_metadata_names_both_license_files(installed_streamlib_distribution):
    declared = installed_streamlib_distribution.metadata.get_all("License-File") or []
    assert BUSL_LICENSE_FILE_NAME in declared
    assert NOTICES_LICENSE_FILE_NAME in declared


def test_the_shipped_busl_license_is_the_parameterized_text(
    installed_streamlib_distribution,
):
    """The SPDX id alone does not say who the Licensor is or when the Change Date falls.

    BUSL-1.1 is a template; the parameters are the licence. A wheel carrying
    only `License: BUSL-1.1` tells a recipient nothing they can act on.
    """
    contents = read_shipped_license_file(
        installed_streamlib_distribution, BUSL_LICENSE_FILE_NAME
    )
    assert "Business Source License 1.1" in contents
    assert "Change Date:" in contents
    assert "Jonathan Fontanez" in contents


def test_the_shipped_notices_carry_the_vendored_cpp_projects(
    installed_streamlib_distribution,
):
    contents = read_shipped_license_file(
        installed_streamlib_distribution, NOTICES_LICENSE_FILE_NAME
    )
    missing = [name for name in VENDORED_CPP_PROJECT_NAMES if name not in contents]
    assert not missing, f"vendored C++ projects absent from the notices: {missing}"
    # Naming the project is not reproducing its notice — these two carry no
    # licence file at all, so the copyright line is the only thing that proves
    # the generator read the header rather than the directory name.
    assert "Advanced Micro Devices" in contents
    assert "The Khronos Group Inc" in contents


def test_the_shipped_notices_carry_the_rust_closure(installed_streamlib_distribution):
    contents = read_shipped_license_file(
        installed_streamlib_distribution, NOTICES_LICENSE_FILE_NAME
    )
    missing = [name for name in SAMPLED_RUST_DEPENDENCY_NAMES if name not in contents]
    assert not missing, f"Rust dependencies absent from the notices: {missing}"


def test_the_shipped_notices_reproduce_licence_text_not_just_identifiers(
    installed_streamlib_distribution,
):
    """A list of SPDX ids discharges nothing — the terms require the text.

    Checks a distinctive clause from each of the two families that carry the
    obligation: Apache-2.0's redistribution section and the MIT permission
    grant.
    """
    contents = read_shipped_license_file(
        installed_streamlib_distribution, NOTICES_LICENSE_FILE_NAME
    )
    assert "You must retain, in the Source form" in contents
    assert "Permission is hereby granted, free of charge" in contents

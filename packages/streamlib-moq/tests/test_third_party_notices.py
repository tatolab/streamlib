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

The engine wheel has its own copy of this check, over its own closure. Two
standalone projects cannot import each other's test helpers, which is the cost
of an extension being built exactly as a third party would build one — and the
two closures share nothing, since this wheel links no engine crate.
"""

from importlib.metadata import distribution

import pytest

# A thin sample of the Rust closure this wheel compiles in, one per link
# shape: the MoQ session layer, the QUIC transport under it, the TLS layer and
# the C cryptography it links, the CMAF box layer, and the binding layer this
# wheel is built on. Enough to catch a notices file generated against the wrong
# manifest; not so many that a dependency swap fails an unrelated test. Matched
# in the bracketed link form the generator writes, so a name cannot be satisfied
# by a substring of some other crate's.
SAMPLED_DEPENDENCY_NAMES = ("moq-transport", "quinn", "rustls", "aws-lc-sys", "mp4-atom", "pyo3")

DISTRIBUTION_NAME = "streamlib-moq"
NOTICES_LICENSE_FILE_NAME = "THIRD-PARTY-NOTICES.md"
BUSL_LICENSE_FILE_NAME = "LICENSE"


@pytest.fixture(scope="module")
def installed_extension_distribution():
    return distribution(DISTRIBUTION_NAME)


def read_shipped_license_file(installed_extension_distribution, license_file_name):
    """Read one `.dist-info/licenses/` entry out of the installed distribution.

    `Distribution.read_text` resolves relative to the `.dist-info` directory,
    which is where PEP 639 puts `licenses/` — so this reads the artifact's own
    copy, never the repo's.
    """
    contents = installed_extension_distribution.read_text(
        f"licenses/{license_file_name}"
    )
    assert contents is not None, (
        f"{license_file_name} is not in the installed .dist-info/licenses/ — "
        "the wheel ships third-party code and discharges its notice obligation nowhere"
    )
    return contents


def test_metadata_names_both_license_files(installed_extension_distribution):
    declared = installed_extension_distribution.metadata.get_all("License-File") or []
    assert BUSL_LICENSE_FILE_NAME in declared
    assert NOTICES_LICENSE_FILE_NAME in declared


def test_the_shipped_busl_license_is_the_parameterized_text(
    installed_extension_distribution,
):
    """The SPDX id alone does not say who the Licensor is or when the Change Date falls.

    BUSL-1.1 is a template; the parameters are the licence. A wheel carrying
    only `License: BUSL-1.1` tells a recipient nothing they can act on.
    """
    contents = read_shipped_license_file(
        installed_extension_distribution, BUSL_LICENSE_FILE_NAME
    )
    assert "Business Source License 1.1" in contents
    assert "Change Date:" in contents
    assert "Jonathan Fontanez" in contents


def test_the_shipped_notices_carry_this_wheels_own_closure(
    installed_extension_distribution,
):
    contents = read_shipped_license_file(
        installed_extension_distribution, NOTICES_LICENSE_FILE_NAME
    )
    missing = [
        name for name in SAMPLED_DEPENDENCY_NAMES if f"[{name} " not in contents
    ]
    assert not missing, f"dependencies absent from the notices: {missing}"


def test_the_notices_are_this_wheels_own_rather_than_the_engines(
    installed_extension_distribution,
):
    """An extension links no engine crate, so copying the engine's file here
    would over-attribute wildly and say nothing about what this wheel ships."""
    contents = read_shipped_license_file(
        installed_extension_distribution, NOTICES_LICENSE_FILE_NAME
    )
    assert "Vendored C++ sources" not in contents
    assert "VulkanMemoryAllocator" not in contents


def test_the_shipped_notices_reproduce_licence_text_not_just_identifiers(
    installed_extension_distribution,
):
    """A list of SPDX ids discharges nothing — the terms require the text.

    Checks a distinctive clause from each of the two families that carry the
    obligation: Apache-2.0's redistribution section and the MIT permission
    grant.
    """
    contents = read_shipped_license_file(
        installed_extension_distribution, NOTICES_LICENSE_FILE_NAME
    )
    assert "You must retain, in the Source form" in contents
    assert "Permission is hereby granted, free of charge" in contents

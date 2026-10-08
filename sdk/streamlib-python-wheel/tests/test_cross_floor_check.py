# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cross-floor check over the runtime's own Python.

Each finding kind and the block's rendering are the stream suite's
(`sdk/tatolab-stream/tests/test_cross_floor_check.py`).
"""

from pathlib import Path

import tatolab.runtime
from tatolab.stream._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    render_cross_floor_warning_block,
)

RUNTIME_PACKAGE_DIRECTORY = Path(tatolab.runtime.__file__).resolve().parent


def test_the_runtimes_own_python_binds_to_no_floor():
    assert (RUNTIME_PACKAGE_DIRECTORY / "__init__.py").is_file(), (
        f"the gate must read the runtime's own Python, not {RUNTIME_PACKAGE_DIRECTORY}"
    )
    report = check_app_directory_for_floor_bindings(RUNTIME_PACKAGE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, RUNTIME_PACKAGE_DIRECTORY
    )

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Per-package venv isolation proof — package A's version reporter.

Package A's pyproject pins ``numpy==1.26.4``. Its sibling package B pins
``numpy==2.1.3`` — a NumPy 1.x vs 2.x ABI conflict that could never
co-resolve in a single shared environment. This processor imports numpy
and reports ``numpy.__version__`` so the host can assert that package A's
processor ran inside its own per-package venv with its own pinned numpy.

Each ``process()`` call is a pure in-memory tick; on ``setup()`` and
``teardown()`` the observed numpy version is written to a host-visible
output file as JSON. The host reads it after the run and asserts it is
exactly the version this package pinned.

Config keys:
    output_file (str, required): host-visible file path to write the
        observed numpy version into.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import numpy

import streamlib
from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess

PACKAGE_LABEL = "pkg-a"


class NumpyVersionReporter:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._output_file = Path(str(cfg["output_file"]))
        self._numpy_version = str(numpy.__version__)
        self._tick_count = 0
        self._write_report()
        streamlib.log.info(
            "NumpyVersionReporter setup",
            package=PACKAGE_LABEL,
            numpy_version=self._numpy_version,
            output_file=str(self._output_file),
        )

    def process(self, _ctx: RuntimeContextLimitedAccess) -> None:
        self._tick_count += 1

    def teardown(self, _ctx: RuntimeContextFullAccess) -> None:
        self._write_report()
        streamlib.log.info(
            "NumpyVersionReporter teardown",
            package=PACKAGE_LABEL,
            numpy_version=self._numpy_version,
            ticks=self._tick_count,
        )

    def _write_report(self) -> None:
        """Atomically write the observed numpy version (write tmp + rename)."""
        try:
            payload = json.dumps({
                "package": PACKAGE_LABEL,
                "numpy_version": self._numpy_version,
                "tick_count": self._tick_count,
            })
            tmp = self._output_file.with_suffix(self._output_file.suffix + ".tmp")
            tmp.write_text(payload)
            os.replace(tmp, self._output_file)
        except Exception as e:
            streamlib.log.warn(
                "NumpyVersionReporter write failed",
                package=PACKAGE_LABEL,
                error=str(e),
            )

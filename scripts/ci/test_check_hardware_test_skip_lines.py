# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The hardware-test skip check `ci.yml` runs over cargo-nextest's JUnit reports."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_hardware_test_skip_lines import check  # noqa: E402

SKIP_LINE_PREFIX = "hardware test skipped, needing "
SKIP_LINE_REASONS = ["a camera", "Vulkan Video"]


def junit_report_with(binary_id: str, output: str) -> Path:
    report = tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False)
    report.write(
        '<?xml version="1.0" encoding="UTF-8"?><testsuites><testsuite name="s">'
        f'<testcase name="the_test" classname="{binary_id}"><system-out>'
        "Selected Vulkan device: llvmpipe (LLVM 20)\n"
        f"{output}</system-out></testcase></testsuite></testsuites>"
    )
    report.close()
    return Path(report.name)


def failures_for(binary_id: str, output: str, expected_device: str = "llvmpipe") -> list[str]:
    return check([junit_report_with(binary_id, output)], SKIP_LINE_PREFIX, SKIP_LINE_REASONS, expected_device)


class HardwareTestSkipLineCheckTests(unittest.TestCase):
    def test_a_skip_naming_a_listed_reason_passes(self) -> None:
        self.assertEqual(failures_for("streamlib-engine", "hardware test skipped, needing a camera: no /dev/video0"), [])

    def test_a_skip_naming_no_listed_reason_fails(self) -> None:
        self.assertEqual(len(failures_for("streamlib-engine", "skipped: the device is busy")), 1)

    def test_a_reason_off_the_list_fails(self) -> None:
        self.assertEqual(len(failures_for("tatolabd::bin/tatolabd", "hardware test skipped, needing a unicorn: x")), 1)

    def test_a_test_name_saying_skip_is_not_a_skip(self) -> None:
        self.assertEqual(failures_for("streamlib-engine", "test a_skipped_frame_is_counted ... ok"), [])

    def test_the_exempt_validation_line_passes(self) -> None:
        self.assertEqual(failures_for("streamlib-engine", "Skipping — no validation messenger installed."), [])

    def test_a_package_outside_the_closed_list_is_not_checked(self) -> None:
        self.assertEqual(failures_for("tatolab-cli::bin/tatolab", "skipped: v4l2loopback is not loaded here"), [])

    def test_a_device_other_than_the_expected_one_fails(self) -> None:
        self.assertEqual(len(failures_for("streamlib-engine", "", expected_device="NVIDIA")), 2)


if __name__ == "__main__":
    unittest.main()

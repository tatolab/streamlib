# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Fail on a hardware test that skipped without naming a closed-list reason.

A skipped test passes, so the line it prints is the only trace it leaves.
Reads cargo-nextest JUnit reports and, for the packages whose `hardware-tests`
suites skip through `rig_only_capability_skip`, fails on any line saying it
skipped that is not that helper's line naming a reason from the closed list.
It also fails unless the engine selected the expected Vulkan device and no
other.
"""

from __future__ import annotations

import argparse
import re
import sys
import xml.etree.ElementTree as ElementTree
from pathlib import Path

PACKAGES_WHOSE_SKIPS_NAME_A_CLOSED_LIST_REASON = ("streamlib-engine", "streamlib-media-builtins", "tatolabd")

# Lines that say "skip" and skip no capability: the validation tests need the
# layer a run turns on, and two engine log lines skip no test.
EXEMPT_SKIP_LINES = re.compile(
    r"Skipping — no validation messenger installed\."
    r"|Skipping — only synchronization validation can see this hazard\."
    r"|Manual mode: skipping thread priority"
    r"|; it is skipped and left in place$"
)
SKIP_WORD = re.compile(r"\bskip(s|ped|ping)?\b", re.IGNORECASE)
# libtest opens a line with `test <name> ... `, and a name may say "skip".
LIBTEST_STATUS_PREFIX = re.compile(r"^test \S+ \.\.\. ")
SELECTED_DEVICE = re.compile(r"Selected Vulkan device: (.+)$")


def binary_belongs_to_a_checked_package(binary_id: str) -> bool:
    package_name = re.split(r"::|/", binary_id, maxsplit=1)[0]
    return package_name in PACKAGES_WHOSE_SKIPS_NAME_A_CLOSED_LIST_REASON


def output_lines_of_each_test_case(junit_report: Path):
    """Yields `(binary id, test name, output line)` for every test case."""
    for test_case in ElementTree.parse(junit_report).getroot().iter("testcase"):
        binary_id = test_case.get("classname", "")
        for output_element_name in ("system-out", "system-err"):
            for output_element in test_case.iter(output_element_name):
                for line in (output_element.text or "").splitlines():
                    yield binary_id, test_case.get("name", ""), line


def check(
    junit_reports: list[Path], skip_line_prefix: str, skip_line_reasons: list[str], expected_device: str
) -> list[str]:
    listed_skip_line = re.compile(
        re.escape(skip_line_prefix) + "(" + "|".join(re.escape(reason) for reason in skip_line_reasons) + "): "
    )
    failures: list[str] = []
    devices_selected: set[str] = set()
    for junit_report in junit_reports:
        for binary_id, test_name, line in output_lines_of_each_test_case(junit_report):
            device = SELECTED_DEVICE.search(line)
            if device:
                devices_selected.add(device.group(1).strip())
            if not binary_belongs_to_a_checked_package(binary_id):
                continue
            line = LIBTEST_STATUS_PREFIX.sub("", line)
            if not SKIP_WORD.search(line) or listed_skip_line.search(line) or EXEMPT_SKIP_LINES.search(line):
                continue
            failures.append(f"{binary_id} {test_name}: {line.strip()}")
    if not any(device.startswith(expected_device) for device in devices_selected):
        failures.append(f"the engine never selected {expected_device}; it selected {sorted(devices_selected)}")
    for device in sorted(devices_selected):
        if not device.startswith(expected_device):
            failures.append(f"the engine selected {device}, not {expected_device}")
    return failures


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-line-prefix", required=True)
    parser.add_argument("--skip-line-reasons", required=True, help="The closed list, `|`-separated")
    parser.add_argument("--expected-device", required=True)
    parser.add_argument("junit_reports", nargs="+", type=Path)
    arguments = parser.parse_args(argv)

    failures = check(
        arguments.junit_reports,
        arguments.skip_line_prefix,
        arguments.skip_line_reasons.split("|"),
        arguments.expected_device,
    )
    for failure in failures:
        print(f"::error::{failure}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

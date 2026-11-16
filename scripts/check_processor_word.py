#!/usr/bin/env python3
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
"""Refuses the word "processor" in any tracked file outside vendor/, in its text
or its path: the unit in the graph is a node.

The word counts in any case and inside any identifier (`ManualProcessor`,
`processor_id`, `processors/`); "preprocessor" is a different word and passes.
The files still holding it are listed in lint-exceptions/processor-word.txt as
`<count> <path>`, an optional `# reason` after it. With `--compare-to <git-ref>`,
the list may only shrink from the one at that ref: no new path, no higher count.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

EXCEPTION_LIST = Path("lint-exceptions/processor-word.txt")
EXEMPT_PREFIXES = (
    "vendor/",
    "scripts/check_processor_word.py",
    "scripts/test_check_processor_word.py",
    str(EXCEPTION_LIST),
)
PROCESSOR_WORD = re.compile(r"(?<!pre)processor", re.IGNORECASE)
EXCEPTION_LINE = re.compile(r"^(?P<count>\d+) (?P<path>\S+)(?:\s+#.*)?$")


def processor_word_count(path: str, text: str) -> int:
    """How many times "processor" appears in `path` and `text` together."""
    return len(PROCESSOR_WORD.findall(path)) + len(PROCESSOR_WORD.findall(text))


def parse_exception_list(text: str) -> dict[str, int]:
    """Each listed path and its count; raises ValueError on a malformed line."""
    counts: dict[str, int] = {}
    for line_number, line in enumerate(text.splitlines(), start=1):
        if not line.strip() or line.startswith("#"):
            continue
        entry = EXCEPTION_LINE.match(line)
        if entry is None:
            raise ValueError(f"line {line_number} is not `<count> <path> [# reason]`: {line}")
        counts[entry.group("path")] = int(entry.group("count"))
    return counts


def findings(
    found_counts: dict[str, int],
    listed_counts: dict[str, int],
    compared_counts: dict[str, int] | None,
) -> list[str]:
    """Every way the tree and the list disagree, or the list grew from the compared one."""
    failures = []
    for path, found in sorted(found_counts.items()):
        listed = listed_counts.get(path, 0)
        if found > listed:
            failures.append(
                f'{path}: says "processor" {found} times, {listed} listed; say node'
            )
        elif found < listed:
            failures.append(
                f"{path}: says \"processor\" {found} times, {listed} listed; "
                f"lower its entry in {EXCEPTION_LIST}"
            )
    for path in sorted(listed_counts.keys() - found_counts.keys()):
        failures.append(
            f'{path}: listed, but no longer says "processor"; drop its entry from {EXCEPTION_LIST}'
        )
    if compared_counts is not None:
        for path, listed in sorted(listed_counts.items()):
            compared = compared_counts.get(path, 0)
            if listed > compared:
                failures.append(
                    f"{path}: listed at {listed}, up from {compared}; "
                    f"{EXCEPTION_LIST} only shrinks, so say node"
                )
    return failures


def tracked_processor_word_counts() -> dict[str, int]:
    tracked_paths = subprocess.run(
        ["git", "ls-files", "-z"], capture_output=True, text=True, check=True
    ).stdout.split("\0")
    counts = {}
    for tracked_path in tracked_paths:
        if not tracked_path or tracked_path.startswith(EXEMPT_PREFIXES):
            continue
        try:
            text = Path(tracked_path).read_text()
        except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
            text = ""
        count = processor_word_count(tracked_path, text)
        if count:
            counts[tracked_path] = count
    return counts


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compare-to", metavar="GIT_REF")
    arguments = parser.parse_args()
    try:
        listed_counts = parse_exception_list(EXCEPTION_LIST.read_text())
        compared_counts = None
        if arguments.compare_to:
            compared_text = subprocess.run(
                ["git", "show", f"{arguments.compare_to}:{EXCEPTION_LIST}"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            compared_counts = parse_exception_list(compared_text)
    except (OSError, ValueError, subprocess.CalledProcessError) as unreadable:
        print(f"{EXCEPTION_LIST}: {unreadable}", file=sys.stderr)
        return 1
    failures = findings(tracked_processor_word_counts(), listed_counts, compared_counts)
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

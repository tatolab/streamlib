#!/usr/bin/env python3
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
"""Refuses the word "processor" in any tracked file outside vendor/, in its text
or its path: the unit in the graph is a node.

The word counts in any case and inside any identifier (`ManualProcessor`,
`processor_id`, `processors/`); "preprocessor" is a different word and passes.
The files still holding it are listed in lint-exceptions/processor-word.txt as
`<count> <path>`, an optional `# reason` after it. With `--compare-to <git-ref>`,
the list may only shrink from the one at that ref: no new path, no higher count,
an entry following its file across a rename.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

EXCEPTION_LIST = Path("lint-exceptions/processor-word.txt")
EXEMPT_PREFIXES = ("vendor/", str(EXCEPTION_LIST))
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


def times(count: int) -> str:
    return "once" if count == 1 else f"{count} times"


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
                f'{path}: says "processor" {times(found)}, {listed} listed; say node'
            )
        elif found < listed:
            failures.append(
                f"{path}: says \"processor\" {times(found)}, {listed} listed; "
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


def paths_renamed_since(git_ref: str) -> dict[str, str]:
    """Each tracked file git sees as renamed since `git_ref`, old path to new."""
    fields = subprocess.run(
        ["git", "diff", "--name-status", "-M", "-z", git_ref],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split("\0")
    renamed = {}
    index = 0
    while index < len(fields) and fields[index]:
        status = fields[index]
        if status.startswith(("R", "C")):
            if status.startswith("R"):
                renamed[fields[index + 1]] = fields[index + 2]
            index += 3
        else:
            index += 2
    return renamed


def counts_after_renames(counts: dict[str, int], renamed: dict[str, str]) -> dict[str, int]:
    """`counts` with each renamed file's entry under its new path."""
    moved: dict[str, int] = {}
    for path, count in counts.items():
        new_path = renamed.get(path, path)
        moved[new_path] = max(count, moved.get(new_path, 0))
    return moved


def tracked_processor_word_counts() -> dict[str, int]:
    tracked_paths = subprocess.run(
        ["git", "ls-files", "-z"], capture_output=True, text=True, check=True
    ).stdout.split("\0")
    counts = {}
    for tracked_path in tracked_paths:
        if not tracked_path or tracked_path.startswith(EXEMPT_PREFIXES):
            continue
        try:
            text = Path(tracked_path).read_bytes().decode("utf-8", errors="replace")
        except (FileNotFoundError, IsADirectoryError):
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
        listed_counts = parse_exception_list(EXCEPTION_LIST.read_text(encoding="utf-8"))
        compared_counts = None
        if arguments.compare_to:
            compared_text = subprocess.run(
                ["git", "show", f"{arguments.compare_to}:{EXCEPTION_LIST}"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            compared_counts = counts_after_renames(
                parse_exception_list(compared_text), paths_renamed_since(arguments.compare_to)
            )
    except (OSError, ValueError, subprocess.CalledProcessError) as unreadable:
        print(f"{EXCEPTION_LIST}: {unreadable}", file=sys.stderr)
        return 1
    failures = findings(tracked_processor_word_counts(), listed_counts, compared_counts)
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

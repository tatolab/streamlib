#!/usr/bin/env python3
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
"""Requires a finding in every file a `severity: hint` rule in ast-grep-rules/rules
lists under `files:`, and requires that list to equal the `ignores:` of the
refusing rule its `# required-by: <rule-id>` line names.

A hint rule states what a permitted file must still do, so a refusing rule's
ignore list can neither grow past the files that need it nor keep a file that
stopped needing it. A hint rule with `# keeps-wired: <rule-id>` instead only needs
its findings: it proves the shape that rule keys on still exists, so a rename
cannot leave the rule matching nothing.
"""

import json
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

RULE_DIRECTORY = Path("ast-grep-rules/rules")


def listed_paths(lines: list[str], key: str) -> list[str]:
    """The entries of the top-level `key:` list in a rule file's lines."""
    entries = []
    in_list = False
    for line in lines:
        if line == f"{key}:":
            in_list = True
        elif in_list and line.startswith("  - "):
            entries.append(line.removeprefix("  - ").strip().strip('"'))
        elif in_list and line.strip() and not line.startswith("  #"):
            in_list = False
    return entries


def rule_lines_by_id(rule_directory: Path = RULE_DIRECTORY) -> dict[str, list[str]]:
    """Each rule file's lines, keyed by its `id:`."""
    rules = {}
    for rule_path in sorted(rule_directory.glob("*.yml")):
        lines = rule_path.read_text().splitlines()
        rule_id = next(
            line.removeprefix("id:").strip() for line in lines if line.startswith("id:")
        )
        rules[rule_id] = lines
    return rules


def rule_named_on(lines: list[str], marker: str) -> str | None:
    """The rule a hint rule's `# <marker>:` line names."""
    return next(
        (
            line.removeprefix(f"# {marker}:").strip()
            for line in lines
            if line.startswith(f"# {marker}:")
        ),
        None,
    )


def files_with_a_finding_of(rule_id: str) -> set[str] | str:
    """The files `ast-grep scan` reports `rule_id` in, or why the scan failed."""
    scan = subprocess.run(
        # ast-grep skips the hidden directories it walks into, so .github is named.
        [
            "ast-grep",
            "scan",
            "--json=compact",
            "--filter",
            f"^{rule_id}$",
            ".",
            ".github",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if not scan.stdout.strip().startswith("["):
        return f"ast-grep scan for {rule_id} failed ({scan.returncode}): {scan.stderr}"
    return {finding["file"] for finding in json.loads(scan.stdout)}


def required_match_failures(
    all_rule_lines: dict[str, list[str]],
    files_with_a_finding: Callable[[str], set[str] | str],
    file_exists: Callable[[str], bool],
) -> list[str]:
    """Every way a hint rule's list, its required-by rule's ignores and the findings disagree."""
    failures = []
    for rule_id, lines in all_rule_lines.items():
        if "severity: hint" not in lines:
            continue
        listed_files = listed_paths(lines, "files")
        required_by = rule_named_on(lines, "required-by")
        keeps_wired = rule_named_on(lines, "keeps-wired")
        if not listed_files:
            failures.append(f"{rule_id} lists no files")
            continue
        if (required_by or keeps_wired) not in all_rule_lines:
            failures.append(
                f"{rule_id} names no `# required-by:` or `# keeps-wired:` rule that exists"
            )
            continue
        permitted_files = (
            listed_paths(all_rule_lines[required_by], "ignores")
            if required_by
            else listed_files
        )
        if sorted(permitted_files) != sorted(listed_files):
            failures.append(
                f"{rule_id} lists {sorted(listed_files)}, but {required_by} ignores "
                f"{sorted(permitted_files)}; the two lists change together"
            )
        found_in = files_with_a_finding(rule_id)
        if isinstance(found_in, str):
            failures.append(found_in)
            continue
        for listed_file in listed_files:
            if not file_exists(listed_file):
                failures.append(f"{rule_id} lists {listed_file}, which does not exist")
            elif listed_file not in found_in:
                remedy = (
                    "drop the file from this rule and from the ignores of the rule that permits it"
                    if required_by
                    else f"if the shape {keeps_wired} keys on moved, move both rules with it"
                )
                failures.append(
                    f"{rule_id} requires a finding in {listed_file}, and it has none: {remedy}"
                )
    return failures


def main() -> int:
    failures = required_match_failures(
        rule_lines_by_id(),
        files_with_a_finding_of,
        lambda listed_file: Path(listed_file).is_file(),
    )
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

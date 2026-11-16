#!/usr/bin/env python3
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
"""Requires a finding in every file a `severity: hint` rule in ast-grep-rules/rules
lists under `files:`, and requires that list to equal the `ignores:` of the
refusing rule its `# required-by: <rule-id>` line names.

A hint rule states what a permitted file must still do, so a refusing rule's
ignore list can neither grow past the files that need it nor keep a file that
stopped needing it.
"""

import json
import subprocess
import sys
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
        elif in_list and not line.startswith("  #"):
            in_list = False
    return entries


def rule_lines_by_id() -> dict[str, list[str]]:
    rules = {}
    for rule_path in sorted(RULE_DIRECTORY.glob("*.yml")):
        lines = rule_path.read_text().splitlines()
        rule_id = next(
            line.removeprefix("id:").strip() for line in lines if line.startswith("id:")
        )
        rules[rule_id] = lines
    return rules


def required_match_rules() -> dict[str, tuple[list[str], str | None]]:
    """Each hint rule's id, mapped to the files it lists and the rule requiring it."""
    rules = {}
    for rule_id, lines in rule_lines_by_id().items():
        if "severity: hint" not in lines:
            continue
        required_by = next(
            (
                line.removeprefix("# required-by:").strip()
                for line in lines
                if line.startswith("# required-by:")
            ),
            None,
        )
        rules[rule_id] = (listed_paths(lines, "files"), required_by)
    return rules


def main() -> int:
    failures = []
    all_rule_lines = rule_lines_by_id()
    for rule_id, (listed_files, required_by) in required_match_rules().items():
        if not listed_files:
            failures.append(f"{rule_id} lists no files")
            continue
        if required_by not in all_rule_lines:
            failures.append(f"{rule_id} names no `# required-by:` rule that exists")
            continue
        permitted_files = listed_paths(all_rule_lines[required_by], "ignores")
        if sorted(permitted_files) != sorted(listed_files):
            failures.append(
                f"{rule_id} lists {sorted(listed_files)}, but {required_by} ignores "
                f"{sorted(permitted_files)}; the two lists change together"
            )
        scan = subprocess.run(
            ["ast-grep", "scan", "--json=compact", "--filter", f"^{rule_id}$"],
            capture_output=True,
            text=True,
            check=False,
        )
        if not scan.stdout.strip().startswith("["):
            failures.append(
                f"ast-grep scan for {rule_id} failed ({scan.returncode}): {scan.stderr}"
            )
            continue
        files_with_a_finding = {finding["file"] for finding in json.loads(scan.stdout)}
        for listed_file in listed_files:
            if not Path(listed_file).is_file():
                failures.append(f"{rule_id} lists {listed_file}, which does not exist")
            elif listed_file not in files_with_a_finding:
                failures.append(
                    f"{rule_id} requires a finding in {listed_file}, and it has none: drop the "
                    "file from this rule and from the ignores of the rule that permits it"
                )
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

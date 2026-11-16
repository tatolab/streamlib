#!/usr/bin/env python3
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
"""Refuses an `ast-grep-ignore` comment that names no rule, or that names a rule
listed in ast-grep-rules/unsuppressible-rules.txt.

ast-grep can't police these itself: a suppression comment silences findings on
its own line as well as the next, the finding on the comment included.
"""

import re
import subprocess
import sys
from pathlib import Path

UNSUPPRESSIBLE_RULE_LIST = Path("ast-grep-rules/unsuppressible-rules.txt")
EXEMPT_PREFIXES = (
    "ast-grep-rules/",
    "vendor/",
    "scripts/check_ast_grep_suppressions.py",
    "scripts/test_check_ast_grep_suppressions.py",
)
SUPPRESSION_COMMENT = re.compile(
    r"(?://|#|/\*)[^\w\n]*ast-grep-ignore(?P<rules>:[^\n]*)?"
)
# ast-grep reads a block comment whole, so a directive that opens a continuation
# line of one counts though the line carries no comment marker of its own.
SUPPRESSION_ON_A_BLOCK_COMMENT_LINE = re.compile(
    r"^[^\w\n]*ast-grep-ignore(?P<rules>:[^\n]*)?"
)
BLOCK_COMMENT_DELIMITER = re.compile(r"/\*|\*/")


def unsuppressible_rule_ids() -> set[str]:
    lines = UNSUPPRESSIBLE_RULE_LIST.read_text().splitlines()
    return {line.strip() for line in lines if line.strip() and not line.startswith("#")}


def refused_suppressions(
    text: str, refused_rule_ids: set[str]
) -> list[tuple[int, str]]:
    """Each refused suppression in `text`, as its 1-based line and the reason."""
    refusals = []
    inside_a_block_comment = False
    for line_number, line in enumerate(text.splitlines(), start=1):
        directives = {
            match.start("rules"): match for match in SUPPRESSION_COMMENT.finditer(line)
        }
        if inside_a_block_comment:
            continuation = SUPPRESSION_ON_A_BLOCK_COMMENT_LINE.match(line)
            if continuation:
                directives.setdefault(continuation.start("rules"), continuation)
        for delimiter in BLOCK_COMMENT_DELIMITER.finditer(line):
            inside_a_block_comment = delimiter.group() == "/*"
        for comment in directives.values():
            named_rules = set(
                re.findall(r"[a-z0-9][a-z0-9-]*", comment.group("rules") or "")
            )
            if not named_rules:
                refusals.append(
                    (line_number, "names no rule, so it silences every rule")
                )
            for rule_id in sorted(named_rules & refused_rule_ids):
                refusals.append(
                    (line_number, f"names {rule_id}, which takes no ast-grep-ignore")
                )
    return refusals


def main() -> int:
    refused_rule_ids = unsuppressible_rule_ids()
    tracked_paths = subprocess.run(
        ["git", "ls-files", "-z"], capture_output=True, text=True, check=True
    ).stdout.split("\0")
    failures = []
    for tracked_path in tracked_paths:
        if not tracked_path or tracked_path.startswith(EXEMPT_PREFIXES):
            continue
        try:
            text = Path(tracked_path).read_text()
        except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
            continue
        if "ast-grep-ignore" not in text:
            continue
        for line_number, reason in refused_suppressions(text, refused_rule_ids):
            failures.append(
                f"{tracked_path}:{line_number}: this ast-grep-ignore {reason}"
            )
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

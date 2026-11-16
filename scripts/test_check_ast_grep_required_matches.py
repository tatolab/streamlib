# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""check_ast_grep_required_matches.py holds each hint rule's files to a finding
apiece and to the ignores of the rule its `# required-by:` line names."""

import unittest

from check_ast_grep_required_matches import required_match_failures

REFUSING_RULE = """id: no-wall-clock-read
severity: error
ignores:
  # log file naming
  - runtime/a.rs
  - runtime/b.rs
rule:
  matches: wall-clock-read-in-rust
""".splitlines()

HINT_RULE = """# required-by: no-wall-clock-read
id: wall-clock-read-on-a-permitted-surface
severity: hint
files:
  - runtime/a.rs
  - runtime/b.rs
rule:
  matches: wall-clock-read-in-rust
""".splitlines()


def failures_for(
    hint_rule: list[str],
    found_in: set[str] | str = frozenset({"runtime/a.rs", "runtime/b.rs"}),
    refusing_rule: list[str] = REFUSING_RULE,
) -> list[str]:
    return required_match_failures(
        {
            "no-wall-clock-read": refusing_rule,
            "wall-clock-read-on-a-permitted-surface": hint_rule,
        },
        lambda _rule_id: found_in,
        lambda _listed_file: True,
    )


class RequiredMatchFailuresTest(unittest.TestCase):
    def test_matching_lists_with_a_finding_in_each_file_pass(self) -> None:
        self.assertEqual(failures_for(HINT_RULE), [])

    def test_a_listed_file_without_a_finding_fails(self) -> None:
        failures = failures_for(HINT_RULE, found_in={"runtime/a.rs"})
        self.assertEqual(len(failures), 1)
        self.assertIn("requires a finding in runtime/b.rs", failures[0])

    def test_an_ignore_the_hint_rule_does_not_list_fails(self) -> None:
        refusing_rule = [*REFUSING_RULE[:5], "  - runtime/extra.rs", *REFUSING_RULE[5:]]
        failures = failures_for(HINT_RULE, refusing_rule=refusing_rule)
        self.assertEqual(len(failures), 1)
        self.assertIn("the two lists change together", failures[0])

    def test_an_ignore_after_a_blank_line_in_the_list_still_counts(self) -> None:
        refusing_rule = [
            *REFUSING_RULE[:5],
            "",
            "  - runtime/extra.rs",
            *REFUSING_RULE[5:],
        ]
        failures = failures_for(HINT_RULE, refusing_rule=refusing_rule)
        self.assertEqual(len(failures), 1)
        self.assertIn("the two lists change together", failures[0])

    def test_a_hint_rule_without_a_required_by_line_fails(self) -> None:
        failures = failures_for(HINT_RULE[1:])
        self.assertEqual(
            failures,
            [
                "wall-clock-read-on-a-permitted-surface names no `# required-by:` rule that exists"
            ],
        )

    def test_a_failed_scan_fails(self) -> None:
        self.assertEqual(
            failures_for(HINT_RULE, found_in="scan failed"), ["scan failed"]
        )


if __name__ == "__main__":
    unittest.main()

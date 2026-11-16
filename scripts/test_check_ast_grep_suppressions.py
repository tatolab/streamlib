# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""check_ast_grep_suppressions.py refuses every suppression comment ast-grep
honours for a rule that takes none, and leaves prose and strings alone."""

import unittest

from check_ast_grep_suppressions import refused_suppressions

UNSUPPRESSIBLE = {"no-wall-clock-read", "no-wall-clock-read-in-python"}


class RefusedSuppressionsTest(unittest.TestCase):
    def test_every_spelling_ast_grep_honours_is_refused(self) -> None:
        for spelling in [
            "// ast-grep-ignore: no-wall-clock-read",
            "let s = now(); // ast-grep-ignore: no-wall-clock-read",
            "//! ast-grep-ignore: no-wall-clock-read",
            "/** ast-grep-ignore: no-wall-clock-read */",
            "# ast-grep-ignore: no-wall-clock-read-in-python",
            "#!ast-grep-ignore: no-wall-clock-read-in-python",
            "#:ast-grep-ignore: no-wall-clock-read-in-python",
            "# ast-grep-ignore: no-device-api-version-read, no-wall-clock-read",
        ]:
            with self.subTest(spelling=spelling):
                self.assertEqual(len(refused_suppressions(spelling, UNSUPPRESSIBLE)), 1)

    def test_a_bare_suppression_is_refused(self) -> None:
        for spelling in [
            "// ast-grep-ignore",
            "x = 1  # ast-grep-ignore",
            "/* ast-grep-ignore */",
        ]:
            with self.subTest(spelling=spelling):
                reasons = refused_suppressions(spelling, UNSUPPRESSIBLE)
                self.assertEqual(len(reasons), 1)
                self.assertIn("names no rule", reasons[0][1])

    def test_a_suppression_of_an_ordinary_rule_passes(self) -> None:
        self.assertEqual(
            refused_suppressions(
                "// ast-grep-ignore: no-device-api-version-read", UNSUPPRESSIBLE
            ),
            [],
        )

    def test_prose_and_strings_naming_the_directive_pass(self) -> None:
        for text in [
            'let directive = "ast-grep-ignore";',
            "// see the ast-grep-ignore docs",
            "# note: ast-grep-ignore takes a rule id",
        ]:
            with self.subTest(text=text):
                self.assertEqual(refused_suppressions(text, UNSUPPRESSIBLE), [])

    def test_the_refused_line_is_reported(self) -> None:
        text = "fn a() {}\nfn b() {} // ast-grep-ignore: no-wall-clock-read\n"
        self.assertEqual(refused_suppressions(text, UNSUPPRESSIBLE)[0][0], 2)


if __name__ == "__main__":
    unittest.main()

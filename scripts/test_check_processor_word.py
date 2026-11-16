# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""check_processor_word.py counts "processor" in every spelling but "preprocessor",
and holds its exception list to the tree and to the list it shrinks from."""

import unittest

from check_processor_word import findings, parse_exception_list, processor_word_count


class ProcessorWordCountTest(unittest.TestCase):
    def test_every_spelling_counts(self) -> None:
        for text in [
            "#[processor]",
            "impl ManualProcessor for X",
            "processor_id",
            "PROCESSOR_REGISTRY",
            "the processors key",
            "a Processor.",
        ]:
            with self.subTest(text=text):
                self.assertEqual(processor_word_count("a.rs", text), 1)

    def test_preprocessor_passes(self) -> None:
        self.assertEqual(processor_word_count("a.rs", "preprocessor_macros, Preprocessor"), 0)

    def test_the_path_counts(self) -> None:
        self.assertEqual(processor_word_count("core/processors/mod.rs", "processor"), 2)


class ExceptionListTest(unittest.TestCase):
    def test_entries_parse_with_or_without_a_reason(self) -> None:
        self.assertEqual(
            parse_exception_list("# header\n\n3 a.rs\n1 b.rs  # the CLAP API's own name\n"),
            {"a.rs": 3, "b.rs": 1},
        )

    def test_a_malformed_entry_is_refused(self) -> None:
        with self.assertRaises(ValueError):
            parse_exception_list("a.rs 3\n")


class FindingsTest(unittest.TestCase):
    def test_a_tree_matching_its_list_passes(self) -> None:
        self.assertEqual(findings({"a.rs": 2}, {"a.rs": 2}, {"a.rs": 2}), [])

    def test_an_unlisted_file_says_node(self) -> None:
        [failure] = findings({"a.rs": 1}, {}, None)
        self.assertIn("say node", failure)

    def test_a_count_above_its_entry_fails(self) -> None:
        self.assertEqual(len(findings({"a.rs": 3}, {"a.rs": 2}, None)), 1)

    def test_a_count_below_its_entry_asks_for_the_entry_to_drop(self) -> None:
        [failure] = findings({"a.rs": 1}, {"a.rs": 2}, None)
        self.assertIn("lower its entry", failure)

    def test_an_entry_for_a_clean_file_asks_to_be_dropped(self) -> None:
        [failure] = findings({}, {"a.rs": 2}, None)
        self.assertIn("drop its entry", failure)

    def test_the_list_only_shrinks_from_the_compared_one(self) -> None:
        self.assertEqual(len(findings({"a.rs": 3}, {"a.rs": 3}, {"a.rs": 2})), 1)
        self.assertEqual(len(findings({"b.rs": 1}, {"b.rs": 1}, {})), 1)
        self.assertEqual(findings({"a.rs": 1}, {"a.rs": 1}, {"a.rs": 2}), [])


if __name__ == "__main__":
    unittest.main()

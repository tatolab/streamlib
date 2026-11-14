# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The version a merge releases as, and the manifests `apply` writes it into."""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import release_version  # noqa: E402


class NextReleaseVersion(unittest.TestCase):
    def test_a_merge_moves_the_patch(self):
        self.assertEqual(release_version.next_release_version("0.68.82", "0.68.82", "patch"), "0.68.83")

    def test_a_contract_change_or_a_minor_dispatch_moves_the_minor(self):
        self.assertEqual(release_version.next_release_version("0.68.82", "0.68.82", "minor"), "0.69.0")

    def test_a_major_dispatch_moves_the_major(self):
        self.assertEqual(release_version.next_release_version("0.68.82", "0.68.82", "major"), "1.0.0")

    def test_a_tree_version_newer_than_the_last_release_is_taken_as_it_stands(self):
        self.assertEqual(release_version.next_release_version("0.68.82", "0.70.0", "patch"), "0.70.0")

    def test_a_tree_version_older_than_the_last_release_never_moves_it_back(self):
        self.assertEqual(release_version.next_release_version("0.68.82", "0.68.10", "patch"), "0.68.83")

    def test_the_first_release_takes_the_tree_version(self):
        self.assertEqual(release_version.next_release_version(None, "0.1.0", "patch"), "0.1.0")

    def test_versions_compare_as_numbers_not_text(self):
        self.assertEqual(release_version.next_release_version("0.68.9", "0.68.10", "patch"), "0.68.10")


class ApplyReleaseVersion(unittest.TestCase):
    def setUp(self):
        self.repository_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.repository_directory.cleanup)
        self.root = Path(self.repository_directory.name)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = []\n\n[workspace.package]\nedition = "2021"\nversion = "0.1.0"\n\n'
            '[workspace.dependencies]\nserde = { version = "1.0.0" }\n',
            encoding="utf-8",
        )
        (self.root / "package.json").write_text('{\n  "name": "client",\n  "version": "0.1.0"\n}\n', encoding="utf-8")
        (self.root / "pyproject.toml").write_text(
            '[project]\nname = "tatolab-x"\nversion = "0.1.0"\n'
            'dependencies = ["tatolab-stream>=0.1.0", "numpy>=1.26"]  # x-release-version\n',
            encoding="utf-8",
        )
        self.original_root = release_version.REPOSITORY_ROOT
        release_version.REPOSITORY_ROOT = self.root
        self.addCleanup(setattr, release_version, "REPOSITORY_ROOT", self.original_root)

    def test_each_kind_of_manifest_takes_the_version_and_nothing_else_moves(self):
        for version_file in (
            {"path": "Cargo.toml", "table": "workspace.package"},
            {"path": "package.json", "key": "version"},
            {"path": "pyproject.toml", "table": "project"},
        ):
            release_version.write_version_into(version_file, "0.2.3")

        cargo_manifest = (self.root / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn('[workspace.package]\nedition = "2021"\nversion = "0.2.3"', cargo_manifest)
        self.assertIn('serde = { version = "1.0.0" }', cargo_manifest)
        self.assertIn('"version": "0.2.3"', (self.root / "package.json").read_text(encoding="utf-8"))
        self.assertIn('version = "0.2.3"', (self.root / "pyproject.toml").read_text(encoding="utf-8"))
        self.assertEqual(release_version.tree_version(), "0.2.3")

    def test_a_marked_line_takes_the_version_in_place_of_its_x_y_z(self):
        release_version.write_version_into({"path": "pyproject.toml", "marked_lines": "x-release-version"}, "0.2.3")

        self.assertIn(
            'dependencies = ["tatolab-stream>=0.2.3", "numpy>=1.26"]',
            (self.root / "pyproject.toml").read_text(encoding="utf-8"),
        )

    def test_a_manifest_with_no_version_where_the_list_says_is_refused(self):
        with self.assertRaises(ValueError):
            release_version.write_version_into({"path": "Cargo.toml", "table": "package"}, "0.2.3")


if __name__ == "__main__":
    unittest.main()

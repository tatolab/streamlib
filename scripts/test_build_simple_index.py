# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The simple index is what `pip install streamlib --index-url …` resolves.

Stdlib `unittest` on purpose: this runs in the release workflow before anything
is published, on a runner with no test dependencies installed.
"""

import tempfile
import unittest
from pathlib import Path

from build_simple_index import (
    WheelAsset,
    collect_wheel_assets,
    normalize_project_name,
    project_name_of_wheel,
    render_project_page,
    write_simple_index,
)

WHEEL_FILE_NAME = "streamlib-0.12.0-cp310-abi3-manylinux_2_28_x86_64.whl"
WHEEL_DOWNLOAD_URL = (
    f"https://github.com/tatolab/streamlib/releases/download/v0.12.0/{WHEEL_FILE_NAME}"
)


def release(*, assets, draft=False):
    return {"draft": draft, "assets": list(assets)}


def asset(name, url=WHEEL_DOWNLOAD_URL):
    return {"name": name, "browser_download_url": url}


class CollectingWheelAssets(unittest.TestCase):
    def test_a_published_wheel_is_collected(self):
        collected = collect_wheel_assets([release(assets=[asset(WHEEL_FILE_NAME)])])

        self.assertEqual(
            collected, [WheelAsset(WHEEL_FILE_NAME, WHEEL_DOWNLOAD_URL)]
        )

    def test_a_draft_releases_assets_are_skipped(self):
        """A draft's assets are not publicly fetchable — an entry pip cannot
        download is worse than one that is missing."""
        collected = collect_wheel_assets(
            [release(assets=[asset(WHEEL_FILE_NAME)], draft=True)]
        )

        self.assertEqual(collected, [])

    def test_non_wheel_release_assets_are_ignored(self):
        collected = collect_wheel_assets(
            [
                release(
                    assets=[
                        asset("streamlib-0.12.0.tar.gz"),
                        asset("checksums.txt"),
                        asset(WHEEL_FILE_NAME),
                    ]
                )
            ]
        )

        self.assertEqual([found.file_name for found in collected], [WHEEL_FILE_NAME])

    def test_a_wheel_for_another_project_is_not_published_here(self):
        """The index serves one project; anything else is a packaging mistake."""
        collected = collect_wheel_assets(
            [release(assets=[asset("numpy-2.1.0-cp312-cp312-linux_x86_64.whl")])]
        )

        self.assertEqual(collected, [])

    def test_an_asset_without_a_download_url_is_skipped(self):
        collected = collect_wheel_assets(
            [release(assets=[{"name": WHEEL_FILE_NAME}])]
        )

        self.assertEqual(collected, [])

    def test_a_release_without_assets_does_not_raise(self):
        self.assertEqual(collect_wheel_assets([{"draft": False}]), [])

    def test_every_release_contributes_its_wheels(self):
        older_wheel = "streamlib-0.11.1-cp310-abi3-manylinux_2_28_x86_64.whl"
        collected = collect_wheel_assets(
            [
                release(assets=[asset(WHEEL_FILE_NAME)]),
                release(assets=[asset(older_wheel)]),
            ]
        )

        self.assertEqual(
            [found.file_name for found in collected], [WHEEL_FILE_NAME, older_wheel]
        )


class NormalizingNames(unittest.TestCase):
    def test_pep_503_normalization(self):
        self.assertEqual(normalize_project_name("Stream_Lib.Tools"), "stream-lib-tools")

    def test_a_wheels_project_comes_from_its_first_field(self):
        self.assertEqual(project_name_of_wheel(WHEEL_FILE_NAME), "streamlib")


class RenderingTheIndex(unittest.TestCase):
    def test_the_project_page_links_each_wheel_by_name(self):
        page = render_project_page([WheelAsset(WHEEL_FILE_NAME, WHEEL_DOWNLOAD_URL)])

        self.assertIn(f'href="{WHEEL_DOWNLOAD_URL}"', page)
        self.assertIn(WHEEL_FILE_NAME, page)

    def test_a_url_with_html_metacharacters_is_escaped(self):
        """The URL comes from an API response, and lands inside an attribute."""
        page = render_project_page(
            [WheelAsset(WHEEL_FILE_NAME, "https://example.test/a?x=1&y=2")]
        )

        self.assertIn("x=1&amp;y=2", page)
        self.assertNotIn("x=1&y=2", page)

    def test_an_empty_index_is_still_valid_html(self):
        page = render_project_page([])

        self.assertIn("</html>", page)

    def test_the_written_tree_is_where_pip_looks(self):
        with tempfile.TemporaryDirectory() as output_directory:
            simple_root = write_simple_index(
                Path(output_directory), [WheelAsset(WHEEL_FILE_NAME, WHEEL_DOWNLOAD_URL)]
            )

            self.assertTrue((simple_root / "index.html").is_file())
            project_page = simple_root / "streamlib" / "index.html"
            self.assertTrue(project_page.is_file())
            self.assertIn(WHEEL_FILE_NAME, project_page.read_text())


if __name__ == "__main__":
    unittest.main()

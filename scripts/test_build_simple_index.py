# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The simple index is what `pip install streamlib --index-url …` resolves.

Stdlib on purpose: this runs in the release workflow before anything is
published, on a runner with no test dependencies installed. `tomllib` is the one
import beyond `unittest`, and it is stdlib from 3.11 — both workflows that run
this file are on `ubuntu-latest`, whose `python3` is well past that.
"""

import tempfile
import tomllib
import unittest
from pathlib import Path

from build_simple_index import (
    EXTENSION_ENTRY_POINT_GROUP,
    PUBLISHED_PROJECT_NAMES,
    WheelAsset,
    collect_wheel_assets,
    normalize_project_name,
    project_name_of_wheel,
    render_project_page,
    render_root_page,
    write_simple_index,
)

WHEEL_FILE_NAME = "streamlib-0.12.0-cp310-abi3-manylinux_2_28_x86_64.whl"
WHEEL_DOWNLOAD_URL = (
    f"https://github.com/tatolab/streamlib/releases/download/v0.12.0/{WHEEL_FILE_NAME}"
)
EXTENSION_WHEEL_FILE_NAME = (
    "streamlib_webrtc-0.1.1-cp310-abi3-manylinux_2_28_x86_64.whl"
)
EXTENSION_WHEEL_DOWNLOAD_URL = (
    "https://github.com/tatolab/streamlib/releases/download/"
    f"streamlib-webrtc-v0.1.1/{EXTENSION_WHEEL_FILE_NAME}"
)


def release(*, assets, draft=False):
    return {"draft": draft, "assets": list(assets)}


def asset(name, url=WHEEL_DOWNLOAD_URL):
    return {"name": name, "browser_download_url": url}


class CollectingWheelAssets(unittest.TestCase):
    def test_a_published_wheel_is_collected(self):
        collected = collect_wheel_assets([release(assets=[asset(WHEEL_FILE_NAME)])])

        self.assertEqual(
            collected["streamlib"], [WheelAsset(WHEEL_FILE_NAME, WHEEL_DOWNLOAD_URL)]
        )

    def test_a_draft_releases_assets_are_skipped(self):
        """A draft's assets are not publicly fetchable — an entry pip cannot
        download is worse than one that is missing."""
        collected = collect_wheel_assets(
            [release(assets=[asset(WHEEL_FILE_NAME)], draft=True)]
        )

        self.assertEqual(collected["streamlib"], [])

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

        self.assertEqual(
            [found.file_name for found in collected["streamlib"]], [WHEEL_FILE_NAME]
        )

    def test_a_wheel_for_an_unlisted_project_is_not_published_here(self):
        """The index serves the projects this repo releases; anything else is a
        packaging mistake, not a fourth project to publish."""
        collected = collect_wheel_assets(
            [release(assets=[asset("numpy-2.1.0-cp312-cp312-linux_x86_64.whl")])]
        )

        self.assertEqual(collected, {name: [] for name in PUBLISHED_PROJECT_NAMES})

    def test_an_asset_without_a_download_url_is_skipped(self):
        collected = collect_wheel_assets([release(assets=[{"name": WHEEL_FILE_NAME}])])

        self.assertEqual(collected["streamlib"], [])

    def test_a_release_without_assets_does_not_raise(self):
        self.assertEqual(collect_wheel_assets([{"draft": False}])["streamlib"], [])

    def test_every_release_contributes_its_wheels(self):
        older_wheel = "streamlib-0.11.1-cp310-abi3-manylinux_2_28_x86_64.whl"
        collected = collect_wheel_assets(
            [
                release(assets=[asset(WHEEL_FILE_NAME)]),
                release(assets=[asset(older_wheel)]),
            ]
        )

        self.assertEqual(
            [found.file_name for found in collected["streamlib"]],
            [WHEEL_FILE_NAME, older_wheel],
        )

    def test_each_projects_wheels_are_grouped_under_its_own_name(self):
        """An extension is released on its own tag, so the two arrive from
        different releases and must not land on one page."""
        collected = collect_wheel_assets(
            [
                release(
                    assets=[
                        asset(
                            EXTENSION_WHEEL_FILE_NAME,
                            EXTENSION_WHEEL_DOWNLOAD_URL,
                        )
                    ]
                ),
                release(assets=[asset(WHEEL_FILE_NAME)]),
            ]
        )

        self.assertEqual(
            [found.file_name for found in collected["streamlib"]], [WHEEL_FILE_NAME]
        )
        self.assertEqual(
            [found.file_name for found in collected["streamlib-webrtc"]],
            [EXTENSION_WHEEL_FILE_NAME],
        )

    def test_a_wheel_whose_name_underscores_the_project_lands_on_its_page(self):
        """The wheel spec spells `streamlib-webrtc` as `streamlib_webrtc`, so
        the grouping key has to be the normalized form, not the file's."""
        collected = collect_wheel_assets(
            [release(assets=[asset(EXTENSION_WHEEL_FILE_NAME)])]
        )

        self.assertEqual(len(collected["streamlib-webrtc"]), 1)

    def test_every_published_project_has_an_entry_before_its_first_release(self):
        """A project directory that exists and resolves nothing is one pip can
        install from the moment its first wheel lands; a missing one is a 404."""
        collected = collect_wheel_assets([])

        self.assertEqual(sorted(collected), sorted(PUBLISHED_PROJECT_NAMES))


class NormalizingNames(unittest.TestCase):
    def test_pep_503_normalization(self):
        self.assertEqual(normalize_project_name("Stream_Lib.Tools"), "stream-lib-tools")

    def test_a_wheels_project_comes_from_its_first_field(self):
        self.assertEqual(project_name_of_wheel(WHEEL_FILE_NAME), "streamlib")

    def test_the_published_names_are_already_normalized(self):
        """They are matched against a normalized wheel name, so a name that
        normalizes to something else would silently match nothing."""
        for project_name in PUBLISHED_PROJECT_NAMES:
            self.assertEqual(normalize_project_name(project_name), project_name)


class RenderingTheIndex(unittest.TestCase):
    def test_the_project_page_links_each_wheel_by_name(self):
        page = render_project_page(
            "streamlib", [WheelAsset(WHEEL_FILE_NAME, WHEEL_DOWNLOAD_URL)]
        )

        self.assertIn(f'href="{WHEEL_DOWNLOAD_URL}"', page)
        self.assertIn(WHEEL_FILE_NAME, page)

    def test_the_project_page_is_titled_for_its_own_project(self):
        page = render_project_page("streamlib-webrtc", [])

        self.assertIn("Links for streamlib-webrtc", page)
        self.assertNotIn("Links for streamlib<", page)

    def test_a_url_with_html_metacharacters_is_escaped(self):
        """The URL comes from an API response, and lands inside an attribute."""
        page = render_project_page(
            "streamlib",
            [WheelAsset(WHEEL_FILE_NAME, "https://example.test/a?x=1&y=2")],
        )

        self.assertIn("x=1&amp;y=2", page)
        self.assertNotIn("x=1&y=2", page)

    def test_an_empty_index_is_still_valid_html(self):
        page = render_project_page("streamlib", [])

        self.assertIn("</html>", page)

    def test_the_root_page_links_every_published_project(self):
        page = render_root_page(PUBLISHED_PROJECT_NAMES)

        for project_name in PUBLISHED_PROJECT_NAMES:
            self.assertIn(f'href="{project_name}/"', page)

    def test_the_written_tree_is_where_pip_looks(self):
        with tempfile.TemporaryDirectory() as output_directory:
            simple_root = write_simple_index(
                Path(output_directory),
                {"streamlib": [WheelAsset(WHEEL_FILE_NAME, WHEEL_DOWNLOAD_URL)]},
            )

            self.assertTrue((simple_root / "index.html").is_file())
            project_page = simple_root / "streamlib" / "index.html"
            self.assertTrue(project_page.is_file())
            self.assertIn(WHEEL_FILE_NAME, project_page.read_text())

    def test_each_project_gets_its_own_pep_503_directory(self):
        with tempfile.TemporaryDirectory() as output_directory:
            simple_root = write_simple_index(
                Path(output_directory),
                {
                    "streamlib": [WheelAsset(WHEEL_FILE_NAME, WHEEL_DOWNLOAD_URL)],
                    "streamlib-webrtc": [
                        WheelAsset(
                            EXTENSION_WHEEL_FILE_NAME, EXTENSION_WHEEL_DOWNLOAD_URL
                        )
                    ],
                },
            )

            root_page = (simple_root / "index.html").read_text()
            self.assertIn('href="streamlib/"', root_page)
            self.assertIn('href="streamlib-webrtc/"', root_page)
            self.assertIn(
                EXTENSION_WHEEL_FILE_NAME,
                (simple_root / "streamlib-webrtc" / "index.html").read_text(),
            )
            self.assertNotIn(
                EXTENSION_WHEEL_FILE_NAME,
                (simple_root / "streamlib" / "index.html").read_text(),
            )


class PublishingEveryProjectThisRepoReleases(unittest.TestCase):
    """`PUBLISHED_PROJECT_NAMES` is the whole index, so it is the whole answer to
    "can pip resolve this?" — a released distribution missing from it is built,
    attached to its release, and unreachable."""

    def released_distribution_names(self):
        """The engine wheel, plus every extension wheel under `packages/`.

        An extension is one whose `pyproject.toml` declares the entry-point
        group pip records at install — discovered rather than listed, so a third
        extension is covered the day its `pyproject.toml` lands.
        """
        repository_root = Path(__file__).resolve().parent.parent
        released = {"streamlib"}
        for pyproject_path in sorted(
            (repository_root / "packages").glob("*/pyproject.toml")
        ):
            project = tomllib.loads(pyproject_path.read_text(encoding="utf-8")).get(
                "project", {}
            )
            if EXTENSION_ENTRY_POINT_GROUP in project.get("entry-points", {}):
                released.add(normalize_project_name(project["name"]))
        return released

    def test_the_discovery_rule_still_finds_the_extension_wheels(self):
        """A rule that matches nothing would make the check below vacuous."""
        self.assertGreater(len(self.released_distribution_names()), 1)

    def test_the_index_serves_exactly_what_this_repo_releases(self):
        self.assertEqual(
            set(PUBLISHED_PROJECT_NAMES), self.released_distribution_names()
        )


if __name__ == "__main__":
    unittest.main()

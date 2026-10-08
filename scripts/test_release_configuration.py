# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Every distribution the release publishes to PyPI moves with the one version.

Several files have to agree, and each disagreement fails too late to be
obvious: a manifest release-please does not bump publishes the previous
version again, which PyPI refuses, and an extension `release-wheel.yml` does not
name is never built or published at all.

Stdlib on purpose: this runs on a runner with no test dependencies installed.
`tomllib` is stdlib from 3.11, and the job that runs this file is on
`ubuntu-latest`, whose `python3` is well past that.
"""

import json
import tomllib
import unittest
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent

# What makes a `packages/` directory a first-party extension: a Python project
# published under the `tatolab` namespace.
EXTENSION_DISTRIBUTION_NAME_PREFIX = "tatolab-"

TATOLAB_STREAM_PYPROJECT_RELATIVE_PATH = "sdk/tatolab-stream/pyproject.toml"
ENGINE_WHEEL_PYPROJECT_RELATIVE_PATH = "sdk/streamlib-python-wheel/pyproject.toml"
RELEASE_TO_PYPI_WORKFLOW_RELATIVE_PATH = ".github/workflows/release-wheel.yml"


def extension_package_directory_and_distribution_names():
    """Every directory under `packages/` that is a first-party extension.

    Discovered rather than listed, so a second extension is covered the day its
    `pyproject.toml` lands.
    """
    names = []
    for pyproject_path in sorted(
        (REPOSITORY_ROOT / "packages").glob("*/pyproject.toml")
    ):
        project = tomllib.loads(pyproject_path.read_text(encoding="utf-8")).get(
            "project", {}
        )
        if project.get("name", "").startswith(EXTENSION_DISTRIBUTION_NAME_PREFIX):
            names.append((pyproject_path.parent.name, project["name"]))
    return names


def release_please_extra_files():
    return json.loads(
        (REPOSITORY_ROOT / "release-please-config.json").read_text(encoding="utf-8")
    )["packages"]["."]["extra-files"]


def seeded_release_version():
    return json.loads(
        (REPOSITORY_ROOT / ".release-please-manifest.json").read_text(encoding="utf-8")
    )["."]


class ReleasingEveryDistributionOnTheOneVersion(unittest.TestCase):
    def test_the_discovery_rule_still_finds_the_extension_packages(self):
        """A rule that matches nothing would make every check below vacuous."""
        self.assertTrue(extension_package_directory_and_distribution_names())

    def test_tatolab_stream_moves_with_the_one_version(self):
        self.assertIn(
            {
                "type": "toml",
                "path": TATOLAB_STREAM_PYPROJECT_RELATIVE_PATH,
                "jsonpath": "$.project.version",
            },
            release_please_extra_files(),
        )
        stream_project = tomllib.loads(
            (REPOSITORY_ROOT / TATOLAB_STREAM_PYPROJECT_RELATIVE_PATH).read_text(
                encoding="utf-8"
            )
        )["project"]
        self.assertEqual(stream_project["version"], seeded_release_version())

    def test_every_extension_moves_with_the_one_version(self):
        """An extension carries the repository's one version and releases on its
        tag, so it is no release-please package of its own and both its manifests
        are the root package's extra files."""
        configured_packages = json.loads(
            (REPOSITORY_ROOT / "release-please-config.json").read_text(encoding="utf-8")
        )["packages"]
        seeded_packages = json.loads(
            (REPOSITORY_ROOT / ".release-please-manifest.json").read_text(
                encoding="utf-8"
            )
        )
        extra_files = release_please_extra_files()

        for (
            directory_name,
            distribution_name,
        ) in extension_package_directory_and_distribution_names():
            package_path = f"packages/{directory_name}"
            for manifest_name, version_table in (
                ("Cargo.toml", "package"),
                ("pyproject.toml", "project"),
            ):
                manifest = tomllib.loads(
                    (REPOSITORY_ROOT / package_path / manifest_name).read_text(
                        encoding="utf-8"
                    )
                )
                self.assertEqual(
                    manifest[version_table]["version"],
                    seeded_release_version(),
                    f"{distribution_name} {manifest_name}",
                )
            self.assertNotIn(package_path, configured_packages, distribution_name)
            self.assertNotIn(package_path, seeded_packages, distribution_name)
            self.assertIn(
                {
                    "type": "toml",
                    "path": f"{package_path}/Cargo.toml",
                    "jsonpath": "$.package.version",
                },
                extra_files,
                distribution_name,
            )
            self.assertIn(
                {
                    "type": "toml",
                    "path": f"{package_path}/pyproject.toml",
                    "jsonpath": "$.project.version",
                },
                extra_files,
                distribution_name,
            )

    def test_the_release_builds_and_publishes_every_extension(self):
        release_workflow = (
            REPOSITORY_ROOT / RELEASE_TO_PYPI_WORKFLOW_RELATIVE_PATH
        ).read_text(encoding="utf-8")

        for (
            _,
            distribution_name,
        ) in extension_package_directory_and_distribution_names():
            self.assertIn(
                f"extension_distribution_name: {distribution_name}\n",
                release_workflow,
                f"{RELEASE_TO_PYPI_WORKFLOW_RELATIVE_PATH} never builds {distribution_name}",
            )

    def test_the_unreleased_engine_wheel_takes_its_version_from_its_crate(self):
        """No release bumps the engine wheel's manifest, so a version written
        there would fall behind the crate the lend is built from."""
        engine_project = tomllib.loads(
            (REPOSITORY_ROOT / ENGINE_WHEEL_PYPROJECT_RELATIVE_PATH).read_text(
                encoding="utf-8"
            )
        )["project"]

        self.assertNotIn("version", engine_project)
        self.assertIn("version", engine_project.get("dynamic", []))
        self.assertNotIn(
            ENGINE_WHEEL_PYPROJECT_RELATIVE_PATH,
            [extra_file["path"] for extra_file in release_please_extra_files()],
        )


if __name__ == "__main__":
    unittest.main()

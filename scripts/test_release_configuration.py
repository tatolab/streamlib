# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Every distribution the release publishes to PyPI moves with the one version.

Several files have to agree, and each disagreement fails too late to be
obvious: a manifest the release does not bump publishes the previous
version again, which PyPI refuses, and an extension `reusable-distributions.yml` does not
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
RELEASE_TO_PYPI_WORKFLOW_RELATIVE_PATH = ".github/workflows/reusable-distributions.yml"


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


def release_version_files():
    return json.loads(
        (REPOSITORY_ROOT / "release-version-files.json").read_text(encoding="utf-8")
    )


def tree_release_version():
    return tomllib.loads((REPOSITORY_ROOT / "Cargo.toml").read_text(encoding="utf-8"))[
        "workspace"
    ]["package"]["version"]


class ReleasingEveryDistributionOnTheOneVersion(unittest.TestCase):
    def test_the_discovery_rule_still_finds_the_extension_packages(self):
        """A rule that matches nothing would make every check below vacuous."""
        self.assertTrue(extension_package_directory_and_distribution_names())

    def test_tatolab_stream_moves_with_the_one_version(self):
        self.assertIn(
            {"path": TATOLAB_STREAM_PYPROJECT_RELATIVE_PATH, "table": "project"},
            release_version_files(),
        )
        stream_project = tomllib.loads(
            (REPOSITORY_ROOT / TATOLAB_STREAM_PYPROJECT_RELATIVE_PATH).read_text(
                encoding="utf-8"
            )
        )["project"]
        self.assertEqual(stream_project["version"], tree_release_version())

    def test_every_extension_moves_with_the_one_version(self):
        """An extension carries the repository's one version and releases on its
        tag, so both its manifests are release version files."""
        version_files = release_version_files()

        for (
            directory_name,
            distribution_name,
        ) in extension_package_directory_and_distribution_names():
            package_path = f"packages/{directory_name}"
            # A pure-Python extension has no crate manifest to move.
            versioned_manifests = [("pyproject.toml", "project")]
            if (REPOSITORY_ROOT / package_path / "Cargo.toml").is_file():
                versioned_manifests.append(("Cargo.toml", "package"))
            for manifest_name, version_table in versioned_manifests:
                manifest = tomllib.loads(
                    (REPOSITORY_ROOT / package_path / manifest_name).read_text(
                        encoding="utf-8"
                    )
                )
                self.assertEqual(
                    manifest[version_table]["version"],
                    tree_release_version(),
                    f"{distribution_name} {manifest_name}",
                )
                self.assertIn(
                    {"path": f"{package_path}/{manifest_name}", "table": version_table},
                    version_files,
                    f"{distribution_name} {manifest_name}",
                )

    def test_the_release_builds_and_publishes_every_extension(self):
        release_workflow = (
            REPOSITORY_ROOT / RELEASE_TO_PYPI_WORKFLOW_RELATIVE_PATH
        ).read_text(encoding="utf-8")

        for (
            _,
            distribution_name,
        ) in extension_package_directory_and_distribution_names():
            # A compiled extension is built by reusable-extension-wheel.yml; a
            # pure-Python one keeps its distributions as its own artifact.
            self.assertTrue(
                f"extension_distribution_name: {distribution_name}\n" in release_workflow
                or f"name: release-distribution-{distribution_name}\n" in release_workflow,
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
            [version_file["path"] for version_file in release_version_files()],
        )


if __name__ == "__main__":
    unittest.main()

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The loader is pointed at the wheel's driver additively, and only once.

A stock Mac has no Vulkan driver, so the macOS wheel carries MoltenVK. It must
reach the loader without displacing a driver the user installed, and without
overriding a user who has chosen their drivers explicitly.

The lend `cargo xtask build-runtime` lays out carries that driver beside
`_engine` on macOS and nothing of it elsewhere; those tests read the lend first
on `PYTHONPATH` and fail when `tatolab.runtime` came from anywhere else.
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

from tatolab.runtime._bundled_vulkan_driver import (
    BUNDLED_ICD_MANIFEST_FILE_NAME,
    point_the_vulkan_loader_at_the_bundled_driver,
)
from test_wheel_portability import runtime_unit_lend_directory

# What `scripts/stage_macos_bundled_vulkan_driver.sh` writes.
BUNDLED_VULKAN_DRIVER_FILE_NAMES = ("libvulkan.1.dylib", "libMoltenVK.dylib", BUNDLED_ICD_MANIFEST_FILE_NAME)


@pytest.fixture
def staged_driver_directory(tmp_path: Path) -> Path:
    (tmp_path / BUNDLED_ICD_MANIFEST_FILE_NAME).write_text("{}")
    return tmp_path


def test_the_bundled_manifest_is_added_when_nothing_is_set(staged_driver_directory: Path):
    environment: dict[str, str] = {}

    point_the_vulkan_loader_at_the_bundled_driver(environment, staged_driver_directory)

    assert environment == {
        "VK_ADD_DRIVER_FILES": str(staged_driver_directory / BUNDLED_ICD_MANIFEST_FILE_NAME)
    }


def test_a_driver_the_user_already_added_stays_listed(staged_driver_directory: Path):
    environment = {"VK_ADD_DRIVER_FILES": "/opt/own/icd.json"}

    point_the_vulkan_loader_at_the_bundled_driver(environment, staged_driver_directory)

    assert environment["VK_ADD_DRIVER_FILES"].split(os.pathsep) == [
        "/opt/own/icd.json",
        str(staged_driver_directory / BUNDLED_ICD_MANIFEST_FILE_NAME),
    ]


@pytest.mark.parametrize("driver_search_replacing_variable", ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"])
def test_a_user_who_chose_their_drivers_is_left_alone(
    staged_driver_directory: Path, driver_search_replacing_variable: str
):
    environment = {driver_search_replacing_variable: "/opt/own/icd.json"}

    point_the_vulkan_loader_at_the_bundled_driver(environment, staged_driver_directory)

    assert environment == {driver_search_replacing_variable: "/opt/own/icd.json"}


def test_a_helper_reimporting_the_package_does_not_list_the_driver_twice(
    staged_driver_directory: Path,
):
    environment: dict[str, str] = {}

    point_the_vulkan_loader_at_the_bundled_driver(environment, staged_driver_directory)
    point_the_vulkan_loader_at_the_bundled_driver(environment, staged_driver_directory)

    assert environment["VK_ADD_DRIVER_FILES"].count(BUNDLED_ICD_MANIFEST_FILE_NAME) == 1


def test_a_wheel_that_carries_no_driver_changes_nothing(tmp_path: Path):
    environment: dict[str, str] = {}

    point_the_vulkan_loader_at_the_bundled_driver(environment, tmp_path)

    assert environment == {}


@pytest.fixture(scope="module")
def lent_runtime_package_directory() -> Path:
    return runtime_unit_lend_directory() / "tatolab" / "runtime"


def test_the_lend_carries_the_driver_beside_the_engine_on_macos_and_none_elsewhere(
    lent_runtime_package_directory: Path,
):
    bundled_vulkan_driver_directory = lent_runtime_package_directory / "_vulkan_driver"

    assert list(lent_runtime_package_directory.glob("_engine*.so")), lent_runtime_package_directory
    if sys.platform == "darwin":
        missing_driver_files = [
            driver_file_name
            for driver_file_name in BUNDLED_VULKAN_DRIVER_FILE_NAMES
            if not (bundled_vulkan_driver_directory / driver_file_name).is_file()
        ]
        assert missing_driver_files == [], bundled_vulkan_driver_directory
    else:
        assert not bundled_vulkan_driver_directory.exists(), bundled_vulkan_driver_directory


def test_importing_the_lent_runtime_points_the_loader_at_the_lends_driver(
    lent_runtime_package_directory: Path,
):
    environment_without_a_driver_choice = {
        name: value for name, value in os.environ.items() if not name.startswith("VK_")
    }

    reported = subprocess.run(
        [
            sys.executable,
            "-c",
            "import os, tatolab.runtime; print(os.environ.get('VK_ADD_DRIVER_FILES', ''))",
        ],
        env=environment_without_a_driver_choice,
        capture_output=True,
        text=True,
        timeout=120,
        check=True,
    )

    expected_driver_files = (
        str(lent_runtime_package_directory / "_vulkan_driver" / BUNDLED_ICD_MANIFEST_FILE_NAME)
        if sys.platform == "darwin"
        else ""
    )
    assert reported.stdout.strip() == expected_driver_files, reported.stderr

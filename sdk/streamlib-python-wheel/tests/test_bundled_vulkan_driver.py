# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The loader is pointed at the wheel's driver additively, and only once.

A stock Mac has no Vulkan driver, so the macOS wheel carries MoltenVK. It must
reach the loader without displacing a driver the user installed, and without
overriding a user who has chosen their drivers explicitly.
"""

import os
from pathlib import Path

import pytest

from streamlib._bundled_vulkan_driver import (
    BUNDLED_ICD_MANIFEST_FILE_NAME,
    point_the_vulkan_loader_at_the_bundled_driver,
)


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

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Points the Vulkan loader at the driver the macOS wheel carries.

A stock Mac has no Vulkan driver, so the wheel ships MoltenVK beside the loader
in `_vulkan_driver/`. The engine opens that loader by path; this names the ICD
manifest to it. Only the macOS wheel stages the directory, so everywhere else
this is a no-op.
"""

import os
from collections.abc import MutableMapping
from pathlib import Path

# Staged by `scripts/stage_macos_bundled_vulkan_driver.sh`; the engine's loader
# search (`vulkan_loader_library.rs`) looks in the same directory.
BUNDLED_VULKAN_DRIVER_DIRECTORY = Path(__file__).resolve().parent / "_vulkan_driver"
BUNDLED_ICD_MANIFEST_FILE_NAME = "MoltenVK_icd.json"

# Either one replaces the loader's driver search outright; a user who set one
# has chosen their drivers, and adding ours would override that choice.
DRIVER_SEARCH_REPLACING_ENVIRONMENT_VARIABLES = ("VK_DRIVER_FILES", "VK_ICD_FILENAMES")
# Additive: the loader enumerates these beside every driver it finds itself, so
# a user's own Vulkan install stays discoverable.
DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE = "VK_ADD_DRIVER_FILES"


def point_the_vulkan_loader_at_the_bundled_driver(
    environment: MutableMapping[str, str],
    bundled_vulkan_driver_directory: Path = BUNDLED_VULKAN_DRIVER_DIRECTORY,
) -> None:
    """Add the bundled ICD manifest to `VK_ADD_DRIVER_FILES`, once.

    Idempotent, because a helper process inherits the app's environment and
    then imports this package itself.
    """
    bundled_icd_manifest = bundled_vulkan_driver_directory / BUNDLED_ICD_MANIFEST_FILE_NAME
    if not bundled_icd_manifest.is_file():
        return
    if any(environment.get(name) for name in DRIVER_SEARCH_REPLACING_ENVIRONMENT_VARIABLES):
        return

    manifests_already_added = [
        manifest
        for manifest in environment.get(DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE, "").split(os.pathsep)
        if manifest
    ]
    if str(bundled_icd_manifest) in manifests_already_added:
        return
    environment[DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE] = os.pathsep.join(
        [*manifests_already_added, str(bundled_icd_manifest)]
    )

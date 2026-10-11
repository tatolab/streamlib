# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The lend still finds its Vulkan driver when the macOS app bundle splits it.

Code signing admits no Mach-O under `Contents/Resources/`, so the app bundle
lays the lend out in `Contents/Resources/lend` with every Mach-O replaced by a
relative symlink into `Contents/Frameworks/tatolab-runtime/`, at the same path
below `tatolab/runtime/`. The engine looks for its loader in `_vulkan_driver/`
beside the canonical path dyld names for `_engine`
(`vulkan_loader_library.rs`), and the ICD manifest stays a real file in the
lend, naming MoltenVK by a path relative to itself. Built here from the runtime
unit's own lend, as the app's build lays it out.
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from test_wheel_portability import _is_mach_o, runtime_unit_lend_directory

pytestmark = pytest.mark.skipif(
    sys.platform != "darwin", reason="the app bundle and its bundled Vulkan driver are macOS-only"
)

LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS = Path("Resources") / "lend"
LENT_RUNTIME_PACKAGE_MACH_O_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS = (
    Path("Frameworks") / "tatolab-runtime"
)
LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND = Path("tatolab") / "runtime"
BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME = "_vulkan_driver"

# Run in a fresh interpreter importing the lent package from the app bundle's
# lend. Names the loader the engine's rule picks — `_vulkan_driver/` beside the
# canonical directory of the image dyld names for `_engine` — opens it by that
# path, and reports which file dyld actually mapped.
OBSERVE_THE_LOADER_THE_ENGINE_RESOLVES = r"""
import ctypes, json, os
import tatolab.runtime
from tatolab.runtime import _engine

class DlInfo(ctypes.Structure):
    _fields_ = [
        ("dli_fname", ctypes.c_char_p),
        ("dli_fbase", ctypes.c_void_p),
        ("dli_sname", ctypes.c_char_p),
        ("dli_saddr", ctypes.c_void_p),
    ]

system_library = ctypes.CDLL(None)
system_library.dladdr.argtypes = [ctypes.c_void_p, ctypes.POINTER(DlInfo)]
system_library._dyld_image_count.restype = ctypes.c_uint32
system_library._dyld_get_image_name.argtypes = [ctypes.c_uint32]
system_library._dyld_get_image_name.restype = ctypes.c_char_p

engine_image = ctypes.CDLL(_engine.__file__)
image_containing_the_engine = DlInfo()
assert system_library.dladdr(
    ctypes.cast(engine_image.PyInit__engine, ctypes.c_void_p),
    ctypes.byref(image_containing_the_engine),
)
engine_image_path_dyld_names = os.fsdecode(image_containing_the_engine.dli_fname)
loader_the_engine_resolves = os.path.join(
    os.path.dirname(os.path.realpath(engine_image_path_dyld_names)),
    "_vulkan_driver",
    "libvulkan.1.dylib",
)
loader = ctypes.CDLL(loader_the_engine_resolves)
assert loader.vkGetInstanceProcAddr
images_dyld_mapped = [
    os.fsdecode(system_library._dyld_get_image_name(index))
    for index in range(system_library._dyld_image_count())
]
print(json.dumps({
    "engine_image_path_dyld_names": engine_image_path_dyld_names,
    "loader_the_engine_resolves": loader_the_engine_resolves,
    "vulkan_loaders_dyld_mapped": [
        image for image in images_dyld_mapped if os.path.basename(image).startswith("libvulkan")
    ],
    "vk_add_driver_files": os.environ.get("VK_ADD_DRIVER_FILES", ""),
}))
"""


def lend_directory_in_the_app_bundle(app_bundle_contents: Path) -> Path:
    return app_bundle_contents / LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS


def mach_o_directory_in_the_app_bundle(app_bundle_contents: Path) -> Path:
    return app_bundle_contents / LENT_RUNTIME_PACKAGE_MACH_O_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS


def lay_out_the_lend_as_the_app_bundle_does(lend_directory: Path, app_bundle_contents: Path) -> None:
    """Copy `lend_directory` into the bundle: Mach-Os under `Frameworks/`, the rest
    under `Resources/lend` beside a relative symlink to each Mach-O."""
    lent_runtime_package_directory = lend_directory / LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND
    for lent_file in sorted(lend_directory.rglob("*")):
        if not lent_file.is_file():
            continue
        bundle_lend_path = lend_directory_in_the_app_bundle(app_bundle_contents) / lent_file.relative_to(
            lend_directory
        )
        bundle_lend_path.parent.mkdir(parents=True, exist_ok=True)
        if not _is_mach_o(lent_file.read_bytes()[:4]):
            shutil.copy2(lent_file, bundle_lend_path)
            continue
        mach_o_path = mach_o_directory_in_the_app_bundle(app_bundle_contents) / lent_file.relative_to(
            lent_runtime_package_directory
        )
        mach_o_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(lent_file, mach_o_path)
        bundle_lend_path.symlink_to(os.path.relpath(mach_o_path, bundle_lend_path.parent))


@pytest.fixture(scope="module")
def app_bundle_contents(tmp_path_factory: pytest.TempPathFactory) -> Path:
    app_bundle_contents = tmp_path_factory.mktemp("app-bundle").resolve() / "Tatolab.app" / "Contents"
    (app_bundle_contents / "MacOS").mkdir(parents=True)
    lay_out_the_lend_as_the_app_bundle_does(runtime_unit_lend_directory(), app_bundle_contents)
    return app_bundle_contents


@pytest.fixture(scope="module")
def bundled_vulkan_driver_directory_among_the_mach_os(app_bundle_contents: Path) -> Path:
    return mach_o_directory_in_the_app_bundle(app_bundle_contents) / BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME


@pytest.fixture(scope="module")
def bundled_vulkan_driver_directory_in_the_lend(app_bundle_contents: Path) -> Path:
    return (
        lend_directory_in_the_app_bundle(app_bundle_contents)
        / LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND
        / BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME
    )


@pytest.fixture(scope="module")
def observed_from_the_app_bundles_lend(app_bundle_contents: Path) -> dict:
    environment = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith("VK_") and name != "VULKAN_SDK"
    }
    environment["PYTHONPATH"] = str(lend_directory_in_the_app_bundle(app_bundle_contents))
    observed = subprocess.run(
        [sys.executable, "-c", OBSERVE_THE_LOADER_THE_ENGINE_RESOLVES],
        env=environment,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert observed.returncode == 0, observed.stderr
    return json.loads(observed.stdout.strip().splitlines()[-1])


def test_the_app_bundle_holds_no_mach_o_under_resources(app_bundle_contents: Path):
    mach_os_under_resources = [
        resource
        for resource in (app_bundle_contents / "Resources").rglob("*")
        if resource.is_file() and not resource.is_symlink() and _is_mach_o(resource.read_bytes()[:4])
    ]

    assert mach_os_under_resources == []


def test_the_engine_is_imported_from_the_app_bundles_lend(
    app_bundle_contents: Path, observed_from_the_app_bundles_lend: dict
):
    engine_image_path = Path(observed_from_the_app_bundles_lend["engine_image_path_dyld_names"])

    assert engine_image_path.resolve().is_relative_to(
        mach_o_directory_in_the_app_bundle(app_bundle_contents)
    ), engine_image_path


def test_the_loader_the_engine_resolves_is_the_app_bundles_own(
    bundled_vulkan_driver_directory_among_the_mach_os: Path, observed_from_the_app_bundles_lend: dict
):
    app_bundle_loader = bundled_vulkan_driver_directory_among_the_mach_os / "libvulkan.1.dylib"

    assert Path(observed_from_the_app_bundles_lend["loader_the_engine_resolves"]) == app_bundle_loader
    vulkan_loaders_dyld_mapped = observed_from_the_app_bundles_lend["vulkan_loaders_dyld_mapped"]
    assert [Path(loader).resolve() for loader in vulkan_loaders_dyld_mapped] == [app_bundle_loader]


def test_the_loader_beside_the_engines_symlink_is_the_same_file(
    bundled_vulkan_driver_directory_in_the_lend: Path,
    bundled_vulkan_driver_directory_among_the_mach_os: Path,
):
    assert (bundled_vulkan_driver_directory_in_the_lend / "libvulkan.1.dylib").resolve() == (
        bundled_vulkan_driver_directory_among_the_mach_os / "libvulkan.1.dylib"
    )


def test_the_icd_manifest_in_the_lend_names_the_app_bundles_moltenvk(
    bundled_vulkan_driver_directory_in_the_lend: Path,
    bundled_vulkan_driver_directory_among_the_mach_os: Path,
    observed_from_the_app_bundles_lend: dict,
):
    bundled_icd_manifest = bundled_vulkan_driver_directory_in_the_lend / "MoltenVK_icd.json"
    moltenvk_library_path = json.loads(bundled_icd_manifest.read_text())["ICD"]["library_path"]

    assert observed_from_the_app_bundles_lend["vk_add_driver_files"] == str(bundled_icd_manifest)
    assert (bundled_icd_manifest.parent / moltenvk_library_path).resolve() == (
        bundled_vulkan_driver_directory_among_the_mach_os / "libMoltenVK.dylib"
    )

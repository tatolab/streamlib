# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Stage the browser host, `tatolab-browser-subprocess` and the CEF runtime into one directory.

Run after `cargo build --release --bin tatolab-browser-host --bin tatolab-browser-subprocess`,
with `CEF_PATH` naming the CEF distribution the build linked (the directory
`cef-dll-sys` downloads into), and `--into` a runtime unit's
`lib/tatolab/browser-host/`, which `cargo xtask build-runtime` passes.

On Linux it copies both executables, the runtime libraries, resources, locales
and CEF's licence into that directory, and strips the debug sections CEF's
distribution ships in its shared libraries; both executables find `libcef.so`
beside themselves. On macOS it builds `tatolab-browser-host.app` there: the
host as the bundle's executable, CEF's framework under `Frameworks/`, and the
subprocess as each helper app CEF launches its child processes from, named
after the host, every bundle signed ad hoc once its contents are in place.
"""

from __future__ import annotations

import argparse
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

PACKAGE_DIRECTORY = Path(__file__).resolve().parent
DEFAULT_STAGED_DIRECTORY = PACKAGE_DIRECTORY / "target" / "browser-host"
HOST_EXECUTABLE_NAME = "tatolab-browser-host"
SUBPROCESS_EXECUTABLE_NAME = "tatolab-browser-subprocess"
RUNTIME_FILE_SUFFIXES = (".so", ".pak", ".dat", ".bin", ".json")
RUNTIME_FILE_NAMES = ("LICENSE.txt", "CREDITS.html")
REQUIRED_LOCALE_PACK_NAME = "en-US.pak"
CEF_FRAMEWORK_DIRECTORY_NAME = "Chromium Embedded Framework.framework"
HOST_APP_BUNDLE_DIRECTORY_NAME = f"{HOST_EXECUTABLE_NAME}.app"
HOST_BUNDLE_IDENTIFIER = "com.tatolab.browser-host"
MACOS_MINIMUM_SYSTEM_VERSION = "26.0"
# The helper apps CEF looks for beside its framework, each named
# "<host executable> <suffix>", with the bundle identifier suffix each takes.
CEF_HELPER_APP_NAME_SUFFIXES = (
    ("Helper", "helper"),
    ("Helper (Alerts)", "helper.alerts"),
    ("Helper (GPU)", "helper.gpu"),
    ("Helper (Plugin)", "helper.plugin"),
    ("Helper (Renderer)", "helper.renderer"),
)


def cef_distribution_directory(cef_path: Path) -> Path:
    """The directory holding CEF's library: `cef_path` itself or the one download-cef made under it."""
    library_name = CEF_FRAMEWORK_DIRECTORY_NAME if sys.platform == "darwin" else "libcef.so"
    if (cef_path / library_name).exists():
        return cef_path
    found = sorted(path.parent for path in cef_path.glob(f"**/{library_name}"))
    if not found:
        raise SystemExit(f"no {library_name} under {cef_path}; build the crate with CEF_PATH set first")
    if len(found) > 1:
        raise SystemExit(f"more than one CEF distribution under {cef_path}: {found}")
    return found[0]


def strip_executable_on_path() -> str:
    """The `strip` that removes CEF's debug sections, refused by name when absent."""
    strip_executable = shutil.which("strip")
    if strip_executable is None:
        raise SystemExit("no `strip` on PATH; CEF's libraries ship stripped of debug sections")
    return strip_executable


def strip_debug_sections(strip_executable: str, shared_library: Path) -> None:
    """Remove `shared_library`'s debug sections in place; its dynamic symbols stay."""
    subprocess.run([strip_executable, "--strip-debug", str(shared_library)], check=True)


def is_a_shared_library(path: Path) -> bool:
    return path.name.endswith(".so") or ".so." in path.name


def stage(cef_directory: Path, executables: list[Path], staged_directory: Path) -> None:
    """Build the staged directory beside `staged_directory`, then put it in its
    place, so a failed copy or strip never leaves a partial browser host."""
    strip_executable = strip_executable_on_path()
    for required_name in RUNTIME_FILE_NAMES:
        if not (cef_directory / required_name).is_file():
            raise SystemExit(
                f"no {required_name} in {cef_directory}; the runtime unit ships CEF only with "
                "it (export-cef-dir leaves LICENSE.txt out: copy it from the official archive)"
            )
    locale_packs = sorted((cef_directory / "locales").glob("*.pak"))
    if not any(locale.name == REQUIRED_LOCALE_PACK_NAME for locale in locale_packs):
        raise SystemExit(
            f"no locales/{REQUIRED_LOCALE_PACK_NAME} in {cef_directory}; CEF falls back to it "
            "for every locale"
        )
    staged_directory.parent.mkdir(parents=True, exist_ok=True)
    staging_directory = Path(
        tempfile.mkdtemp(dir=staged_directory.parent, prefix=staged_directory.name + ".staging-")
    )
    staging_directory.chmod(0o755)
    try:
        (staging_directory / "locales").mkdir()
        for entry in sorted(cef_directory.iterdir()):
            if entry.is_file() and (
                entry.name.endswith(RUNTIME_FILE_SUFFIXES)
                or ".so." in entry.name
                or entry.name in RUNTIME_FILE_NAMES
            ):
                staged_entry = staging_directory / entry.name
                shutil.copy2(entry, staged_entry)
                if is_a_shared_library(staged_entry):
                    strip_debug_sections(strip_executable, staged_entry)
        for locale in locale_packs:
            shutil.copy2(locale, staging_directory / "locales" / locale.name)
        for executable in executables:
            staged_executable = staging_directory / executable.name
            shutil.copy2(executable, staged_executable)
            staged_executable.chmod(0o755)
    except BaseException:
        shutil.rmtree(staging_directory, ignore_errors=True)
        raise
    put_in_place(staging_directory, staged_directory)


def info_plist_of(executable_name: str, bundle_identifier: str) -> bytes:
    """An `Info.plist` for a bundle running `executable_name` with no Dock icon or menu bar."""
    return plistlib.dumps(
        {
            "CFBundleExecutable": executable_name,
            "CFBundleIdentifier": bundle_identifier,
            "CFBundleName": executable_name,
            "CFBundlePackageType": "APPL",
            "CFBundleInfoDictionaryVersion": "6.0",
            "LSMinimumSystemVersion": MACOS_MINIMUM_SYSTEM_VERSION,
            "LSUIElement": True,
            "NSSupportsAutomaticGraphicsSwitching": True,
        }
    )


def lay_out_app_bundle(
    bundle_directory: Path, executable: Path, executable_name: str, bundle_identifier: str
) -> None:
    """`bundle_directory/Contents/` with its `Info.plist` and `executable` as `MacOS/<executable_name>`."""
    macos_directory = bundle_directory / "Contents" / "MacOS"
    macos_directory.mkdir(parents=True)
    (bundle_directory / "Contents" / "Info.plist").write_bytes(
        info_plist_of(executable_name, bundle_identifier)
    )
    staged_executable = macos_directory / executable_name
    shutil.copy2(executable, staged_executable)
    staged_executable.chmod(0o755)


def sign_ad_hoc(bundle: Path) -> None:
    """Sign `bundle` ad hoc, which Apple silicon requires of every Mach-O it runs."""
    subprocess.run(["codesign", "--force", "--sign", "-", str(bundle)], check=True)


def stage_macos_app_bundle(cef_directory: Path, host_executable: Path, subprocess_executable: Path, staging_directory: Path) -> None:
    """Lay `tatolab-browser-host.app` out in `staging_directory`, signed inside out."""
    app_bundle = staging_directory / HOST_APP_BUNDLE_DIRECTORY_NAME
    lay_out_app_bundle(app_bundle, host_executable, HOST_EXECUTABLE_NAME, HOST_BUNDLE_IDENTIFIER)
    frameworks_directory = app_bundle / "Contents" / "Frameworks"
    frameworks_directory.mkdir()
    staged_framework = frameworks_directory / CEF_FRAMEWORK_DIRECTORY_NAME
    shutil.copytree(cef_directory / CEF_FRAMEWORK_DIRECTORY_NAME, staged_framework, symlinks=True)
    resources_directory = app_bundle / "Contents" / "Resources"
    resources_directory.mkdir()
    for licence_name in RUNTIME_FILE_NAMES:
        licence = cef_directory / licence_name
        if licence.is_file():
            shutil.copy2(licence, resources_directory / licence_name)
    sign_ad_hoc(staged_framework)
    for helper_name_suffix, helper_identifier_suffix in CEF_HELPER_APP_NAME_SUFFIXES:
        helper_executable_name = f"{HOST_EXECUTABLE_NAME} {helper_name_suffix}"
        helper_bundle = frameworks_directory / f"{helper_executable_name}.app"
        lay_out_app_bundle(
            helper_bundle,
            subprocess_executable,
            helper_executable_name,
            f"{HOST_BUNDLE_IDENTIFIER}.{helper_identifier_suffix}",
        )
        sign_ad_hoc(helper_bundle)
    sign_ad_hoc(app_bundle)


def stage_macos(cef_directory: Path, host_executable: Path, subprocess_executable: Path, staged_directory: Path) -> None:
    """Build the staged directory holding the app bundle beside `staged_directory`, then put it in its place."""
    if not (cef_directory / "LICENSE.txt").is_file():
        raise SystemExit(
            f"no LICENSE.txt in {cef_directory}; the runtime unit ships CEF only with it "
            "(export-cef-dir leaves LICENSE.txt out: copy it from the official archive)"
        )
    staged_directory.parent.mkdir(parents=True, exist_ok=True)
    staging_directory = Path(
        tempfile.mkdtemp(dir=staged_directory.parent, prefix=staged_directory.name + ".staging-")
    )
    staging_directory.chmod(0o755)
    try:
        stage_macos_app_bundle(cef_directory, host_executable, subprocess_executable, staging_directory)
    except BaseException:
        shutil.rmtree(staging_directory, ignore_errors=True)
        raise
    put_in_place(staging_directory, staged_directory)


def put_in_place(staging_directory: Path, staged_directory: Path) -> None:
    """Replace `staged_directory` with `staging_directory`, keeping the old one until the swap holds."""
    replaced_directory = staged_directory.with_name(staged_directory.name + ".replaced")
    shutil.rmtree(replaced_directory, ignore_errors=True)
    if staged_directory.exists():
        staged_directory.rename(replaced_directory)
    try:
        staging_directory.rename(staged_directory)
    except BaseException:
        if replaced_directory.exists():
            replaced_directory.rename(staged_directory)
        shutil.rmtree(staging_directory, ignore_errors=True)
        raise
    shutil.rmtree(replaced_directory, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--executable-directory",
        type=Path,
        default=PACKAGE_DIRECTORY / "target" / "release",
        help="where cargo built the host and the subprocess",
    )
    parser.add_argument("--into", type=Path, default=DEFAULT_STAGED_DIRECTORY)
    arguments = parser.parse_args()
    cef_path = os.environ.get("CEF_PATH")
    if not cef_path:
        raise SystemExit("CEF_PATH names the CEF distribution the build linked")
    executables = [
        arguments.executable_directory / name
        for name in (HOST_EXECUTABLE_NAME, SUBPROCESS_EXECUTABLE_NAME)
    ]
    for executable in executables:
        if not executable.is_file():
            raise SystemExit(f"no {executable}; build it first")
    cef_directory = cef_distribution_directory(Path(cef_path))
    if sys.platform == "darwin":
        stage_macos(cef_directory, executables[0], executables[1], arguments.into)
    else:
        stage(cef_directory, executables, arguments.into)
    staged = sorted(path.name for path in arguments.into.iterdir())
    print(f"staged {cef_directory} into {arguments.into}: {staged}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

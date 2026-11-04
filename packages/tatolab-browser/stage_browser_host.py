# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Stage the browser host, `tatolab-browser-subprocess` and the CEF runtime into one directory.

Run after `cargo build --release --bin tatolab-browser-host --bin tatolab-browser-subprocess`,
with `CEF_PATH` naming the CEF distribution the build linked (the directory
`cef-dll-sys` downloads into). Copies both executables, the runtime libraries,
resources, locales and CEF's licence into `--into` — a runtime unit's
`lib/tatolab/browser-host/`, which `cargo xtask build-runtime` passes — and
strips the debug sections CEF's distribution ships in its shared libraries.
Both executables find `libcef.so` beside themselves.
"""

from __future__ import annotations

import argparse
import os
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


def cef_distribution_directory(cef_path: Path) -> Path:
    """The directory holding `libcef.so`: `cef_path` itself or the one download-cef made under it."""
    if (cef_path / "libcef.so").is_file():
        return cef_path
    found = sorted(path.parent for path in cef_path.glob("**/libcef.so"))
    if not found:
        raise SystemExit(f"no libcef.so under {cef_path}; build the crate with CEF_PATH set first")
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
    stage(cef_directory, executables, arguments.into)
    staged = sorted(path.name for path in arguments.into.iterdir())
    print(f"staged {cef_directory} into {arguments.into}: {staged}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

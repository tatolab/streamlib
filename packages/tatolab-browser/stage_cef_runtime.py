# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Stage the CEF runtime and `tatolab-browser-subprocess` into the wheel's package.

Run after `cargo build --release --bin tatolab-browser-subprocess` and before
`maturin build`, with `CEF_PATH` naming the CEF distribution the build linked
(the directory `cef-dll-sys` downloads into). Copies the runtime libraries,
resources, locales and CEF's licence into `python/tatolab/browser/cef/`.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

PACKAGE_DIRECTORY = Path(__file__).resolve().parent
STAGED_RUNTIME_DIRECTORY = PACKAGE_DIRECTORY / "python" / "tatolab" / "browser" / "cef"
SUBPROCESS_EXECUTABLE_NAME = "tatolab-browser-subprocess"
RUNTIME_FILE_SUFFIXES = (".so", ".pak", ".dat", ".bin", ".json")
RUNTIME_FILE_NAMES = ("LICENSE.txt", "CREDITS.html")


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


def stage(cef_directory: Path, subprocess_executable: Path) -> None:
    for required_name in RUNTIME_FILE_NAMES:
        if not (cef_directory / required_name).is_file():
            raise SystemExit(
                f"no {required_name} in {cef_directory}; the wheel ships CEF only with it "
                "(export-cef-dir leaves LICENSE.txt out: copy it from the official archive)"
            )
    if STAGED_RUNTIME_DIRECTORY.exists():
        shutil.rmtree(STAGED_RUNTIME_DIRECTORY)
    (STAGED_RUNTIME_DIRECTORY / "locales").mkdir(parents=True)
    for entry in sorted(cef_directory.iterdir()):
        if entry.is_file() and (
            entry.name.endswith(RUNTIME_FILE_SUFFIXES)
            or ".so." in entry.name
            or entry.name in RUNTIME_FILE_NAMES
        ):
            shutil.copy2(entry, STAGED_RUNTIME_DIRECTORY / entry.name)
    for locale in sorted((cef_directory / "locales").glob("*.pak")):
        shutil.copy2(locale, STAGED_RUNTIME_DIRECTORY / "locales" / locale.name)
    staged_subprocess = STAGED_RUNTIME_DIRECTORY / SUBPROCESS_EXECUTABLE_NAME
    shutil.copy2(subprocess_executable, staged_subprocess)
    staged_subprocess.chmod(0o755)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--subprocess-executable",
        type=Path,
        default=PACKAGE_DIRECTORY / "target" / "release" / SUBPROCESS_EXECUTABLE_NAME,
    )
    arguments = parser.parse_args()
    cef_path = os.environ.get("CEF_PATH")
    if not cef_path:
        raise SystemExit("CEF_PATH names the CEF distribution the build linked")
    if not arguments.subprocess_executable.is_file():
        raise SystemExit(f"no {arguments.subprocess_executable}; build it first")
    cef_directory = cef_distribution_directory(Path(cef_path))
    stage(cef_directory, arguments.subprocess_executable)
    staged = sorted(path.name for path in STAGED_RUNTIME_DIRECTORY.iterdir())
    print(f"staged {cef_directory} into {STAGED_RUNTIME_DIRECTORY}: {staged}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

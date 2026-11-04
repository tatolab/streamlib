# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Fetch the CEF binary distribution the browser host links, for `CEF_PATH`.

    python3 -I scripts/fetch_cef_binary_distribution.py <directory>

Reads the CEF version `cef-dll-sys` pins in `packages/tatolab-browser/Cargo.lock`,
downloads that version's minimal Linux x86_64 archive from CEF's build CDN,
checks it against the SHA-1 the CDN's index gives (the check `cef-dll-sys`
makes itself), and lays it out in `<directory>` the way `cef-dll-sys` lays out
a download, with `archive.json` naming it, plus the distribution's
`LICENSE.txt`, which the runtime unit ships CEF only with.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent
BROWSER_PACKAGE_LOCKFILE = WORKSPACE_ROOT / "packages" / "tatolab-browser" / "Cargo.lock"
CEF_BINDINGS_CRATE = "cef-dll-sys"
CEF_BUILDS_CDN_URL = "https://cef-builds.spotifycdn.com"
CEF_PLATFORM = "linux64"
CEF_ARCHIVE_TYPE = "minimal"
DOWNLOAD_TIMEOUT_SECONDS = 600
ARCHIVE_ENTRIES_LAID_OUT_BESIDE_THE_LIBRARIES = (
    "CMakeLists.txt",
    "cmake",
    "include",
    "libcef_dll",
    "CREDITS.html",
    "LICENSE.txt",
)


def cef_version_the_bindings_pin() -> str:
    """The CEF version in `cef-dll-sys`'s build metadata, `156.0.15` of `156.3.0+156.0.15`."""
    lockfile = tomllib.loads(BROWSER_PACKAGE_LOCKFILE.read_text())
    versions = [
        package["version"]
        for package in lockfile["package"]
        if package["name"] == CEF_BINDINGS_CRATE
    ]
    if len(versions) != 1:
        raise SystemExit(f"{BROWSER_PACKAGE_LOCKFILE} locks {CEF_BINDINGS_CRATE} {len(versions)} times")
    _, separator, cef_version = versions[0].partition("+")
    if not separator:
        raise SystemExit(f"{CEF_BINDINGS_CRATE} {versions[0]} names no CEF version in its build metadata")
    return cef_version


def minimal_archive_of(cef_version: str) -> dict[str, str]:
    """The index's entry for `cef_version`'s minimal archive: its file name and SHA-1."""
    with urllib.request.urlopen(f"{CEF_BUILDS_CDN_URL}/index.json", timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:
        index = json.load(response)
    for version in index[CEF_PLATFORM]["versions"]:
        if version["cef_version"].split("+", 1)[0] != cef_version:
            continue
        for archive in version["files"]:
            if archive["type"] == CEF_ARCHIVE_TYPE:
                return {"type": archive["type"], "name": archive["name"], "sha1": archive["sha1"]}
    raise SystemExit(f"CEF's index has no {CEF_ARCHIVE_TYPE} {CEF_PLATFORM} archive of {cef_version}")


def download_checked(archive: dict[str, str], into: Path) -> Path:
    """Download `archive` into `into`, refusing it when its SHA-1 is not the index's."""
    archive_path = into / archive["name"]
    digest = hashlib.sha1()
    with (
        urllib.request.urlopen(f"{CEF_BUILDS_CDN_URL}/{archive['name']}", timeout=DOWNLOAD_TIMEOUT_SECONDS) as response,
        archive_path.open("wb") as archive_file,
    ):
        while chunk := response.read(1 << 20):
            digest.update(chunk)
            archive_file.write(chunk)
    if digest.hexdigest() != archive["sha1"]:
        raise SystemExit(f"{archive['name']} has SHA-1 {digest.hexdigest()}, not the index's {archive['sha1']}")
    return archive_path


def lay_out(archive_path: Path, archive: dict[str, str], cef_directory: Path) -> None:
    """Unpack the archive's `Release/` and `Resources/` into `cef_directory`, with the build inputs and licence."""
    with tempfile.TemporaryDirectory(dir=archive_path.parent) as extraction_directory:
        with tarfile.open(archive_path, "r:bz2") as archive_tar:
            archive_tar.extractall(extraction_directory, filter="data")
        extracted = Path(extraction_directory) / archive["name"].removesuffix(".tar.bz2")
        shutil.move(extracted / "Release", cef_directory)
        for resource in (extracted / "Resources").iterdir():
            shutil.move(resource, cef_directory / resource.name)
        for entry_name in ARCHIVE_ENTRIES_LAID_OUT_BESIDE_THE_LIBRARIES:
            shutil.move(extracted / entry_name, cef_directory / entry_name)
    (cef_directory / "archive.json").write_text(json.dumps(archive, indent=2))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="the CEF_PATH to lay the distribution out in")
    arguments = parser.parse_args()
    cef_directory: Path = arguments.directory.resolve()
    if cef_directory.exists():
        raise SystemExit(f"{cef_directory} already exists")
    cef_version = cef_version_the_bindings_pin()
    archive = minimal_archive_of(cef_version)
    cef_directory.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=cef_directory.parent) as download_directory:
        archive_path = download_checked(archive, Path(download_directory))
        lay_out(archive_path, archive, cef_directory)
    print(f"CEF {cef_version} ({archive['name']}) is in {cef_directory}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Tatolab's static PEP 503 package index, laid out as get.tatolab.com serves it.

An index tree holds `packages/<normalized name>/<wheel>` with two sidecars beside
each wheel, `<wheel>.sha256` and `<wheel>.metadata` (the wheel's own METADATA,
which PEP 658 / PEP 714 let a resolver read without downloading the wheel), and
the pages `simple/index.html` and `simple/<normalized name>/index.html`. The
pages are written from the sidecars alone, so the release regenerates them from a
copy of the bucket that leaves the wheels behind.

Stdlib only: the release runs this on a runner with nothing else installed.
Commands: `add-wheels <index root> <wheel>...` and `write-pages <index root>`.
"""

import hashlib
import html
import re
import sys
import zipfile
from email.parser import HeaderParser
from pathlib import Path

PACKAGES_DIRECTORY_NAME = "packages"
SIMPLE_DIRECTORY_NAME = "simple"
SHA256_SIDECAR_SUFFIX = ".sha256"
CORE_METADATA_SIDECAR_SUFFIX = ".metadata"


class WheelAlreadyHeldWithOtherBytes(Exception):
    """A wheel file name the index already holds, with a different sha256."""


def normalized_project_name(distribution_name):
    """PEP 503's normalized form of a project name."""
    return re.sub(r"[-_.]+", "-", distribution_name).lower()


def project_name_of_wheel_file_name(wheel_file_name):
    """The normalized project name a wheel's file name carries first."""
    return normalized_project_name(wheel_file_name.split("-", 1)[0])


def sha256_of_bytes(content):
    return hashlib.sha256(content).hexdigest()


def core_metadata_of_wheel(wheel_path):
    """The bytes of the wheel's `*.dist-info/METADATA`."""
    with zipfile.ZipFile(wheel_path) as wheel_archive:
        metadata_member_names = [
            member_name
            for member_name in wheel_archive.namelist()
            if re.fullmatch(r"[^/]+\.dist-info/METADATA", member_name)
        ]
        if len(metadata_member_names) != 1:
            raise ValueError(f"{wheel_path} holds {len(metadata_member_names)} dist-info METADATA files, not one")
        return wheel_archive.read(metadata_member_names[0])


def add_wheel_to_index_tree(index_root, wheel_path):
    """Copy one wheel and its sidecars into the tree; True when it was not held yet.

    A wheel the tree already holds byte for byte is left as it is. One it holds
    under the same file name with other bytes is refused: an index file, once
    served, is never replaced.
    """
    wheel_path = Path(wheel_path)
    wheel_bytes = wheel_path.read_bytes()
    wheel_sha256 = sha256_of_bytes(wheel_bytes)
    project_directory = Path(index_root) / PACKAGES_DIRECTORY_NAME / project_name_of_wheel_file_name(wheel_path.name)
    held_sha256_sidecar = project_directory / (wheel_path.name + SHA256_SIDECAR_SUFFIX)
    if held_sha256_sidecar.is_file():
        held_sha256 = held_sha256_sidecar.read_text(encoding="utf-8").split()[0]
        if held_sha256 != wheel_sha256:
            raise WheelAlreadyHeldWithOtherBytes(
                f"{wheel_path.name} is already served with sha256 {held_sha256}, not this build's {wheel_sha256}"
            )
        return False
    project_directory.mkdir(parents=True, exist_ok=True)
    (project_directory / wheel_path.name).write_bytes(wheel_bytes)
    held_sha256_sidecar.write_text(f"{wheel_sha256}  {wheel_path.name}\n", encoding="utf-8")
    (project_directory / (wheel_path.name + CORE_METADATA_SIDECAR_SUFFIX)).write_bytes(core_metadata_of_wheel(wheel_path))
    return True


def project_page_html(project_name, wheel_links):
    anchors = "".join(
        f'    <a href="{html.escape(link["href"])}"'
        + (f' data-requires-python="{html.escape(link["requires_python"])}"' if link["requires_python"] else "")
        + f' data-core-metadata="sha256={link["core_metadata_sha256"]}"'
        + f' data-dist-info-metadata="sha256={link["core_metadata_sha256"]}"'
        + f'>{html.escape(link["file_name"])}</a><br>\n'
        for link in wheel_links
    )
    return (
        "<!DOCTYPE html>\n<html>\n  <head>\n"
        '    <meta name="pypi:repository-version" content="1.1">\n'
        f"    <title>Links for {html.escape(project_name)}</title>\n  </head>\n  <body>\n"
        f"    <h1>Links for {html.escape(project_name)}</h1>\n{anchors}  </body>\n</html>\n"
    )


def root_page_html(project_names):
    anchors = "".join(
        f'    <a href="{html.escape(project_name)}/">{html.escape(project_name)}</a><br>\n'
        for project_name in project_names
    )
    return (
        "<!DOCTYPE html>\n<html>\n  <head>\n"
        '    <meta name="pypi:repository-version" content="1.1">\n'
        "    <title>Tatolab package index</title>\n  </head>\n  <body>\n"
        f"{anchors}  </body>\n</html>\n"
    )


def write_index_pages(index_root):
    """Write every page from the tree's sidecars; returns the project names listed."""
    index_root = Path(index_root)
    packages_directory = index_root / PACKAGES_DIRECTORY_NAME
    project_names = sorted(path.name for path in packages_directory.iterdir() if path.is_dir()) if packages_directory.is_dir() else []
    for project_name in project_names:
        wheel_links = []
        for sha256_sidecar in sorted((packages_directory / project_name).glob("*.whl" + SHA256_SIDECAR_SUFFIX)):
            wheel_file_name = sha256_sidecar.name[: -len(SHA256_SIDECAR_SUFFIX)]
            core_metadata = (packages_directory / project_name / (wheel_file_name + CORE_METADATA_SIDECAR_SUFFIX)).read_bytes()
            requires_python = HeaderParser().parsestr(core_metadata.decode("utf-8")).get("Requires-Python", "")
            wheel_links.append(
                {
                    "file_name": wheel_file_name,
                    "href": f"../../{PACKAGES_DIRECTORY_NAME}/{project_name}/{wheel_file_name}"
                    + f"#sha256={sha256_sidecar.read_text(encoding='utf-8').split()[0]}",
                    "requires_python": requires_python.strip(),
                    "core_metadata_sha256": sha256_of_bytes(core_metadata),
                }
            )
        project_page_directory = index_root / SIMPLE_DIRECTORY_NAME / project_name
        project_page_directory.mkdir(parents=True, exist_ok=True)
        (project_page_directory / "index.html").write_text(project_page_html(project_name, wheel_links), encoding="utf-8")
    (index_root / SIMPLE_DIRECTORY_NAME).mkdir(parents=True, exist_ok=True)
    (index_root / SIMPLE_DIRECTORY_NAME / "index.html").write_text(root_page_html(project_names), encoding="utf-8")
    return project_names


def main(command_line_arguments):
    if len(command_line_arguments) >= 3 and command_line_arguments[0] == "add-wheels":
        index_root = command_line_arguments[1]
        try:
            for wheel_path in command_line_arguments[2:]:
                added = add_wheel_to_index_tree(index_root, wheel_path)
                print(f"{'added' if added else 'already held'}: {Path(wheel_path).name}")
        except WheelAlreadyHeldWithOtherBytes as refusal:
            print(f"::error::{refusal}", file=sys.stderr)
            return 1
        return 0
    if len(command_line_arguments) == 2 and command_line_arguments[0] == "write-pages":
        for project_name in write_index_pages(command_line_arguments[1]):
            print(f"page: {SIMPLE_DIRECTORY_NAME}/{project_name}/")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

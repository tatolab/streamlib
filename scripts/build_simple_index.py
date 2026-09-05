#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Generate the PEP 503 simple index that serves this repo's wheels from its releases.

`pip install streamlib --index-url <pages-url>` is the one stable incantation
until the project rename makes PyPI publication possible; the artifact behind it
is identical either way. The extension wheels published beside the engine wheel
resolve from the same index, which is what makes one `--index-url` enough for an
app that installs both. The index is rebuilt from the full release history on
every run rather than appended to, so a re-run repairs it and a deleted release
drops out on its own.
"""

from __future__ import annotations

import argparse
import html
import json
import re
import sys
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

# PEP 503: the projects this index serves, normalized, in the order the root
# page lists them. Each is released on its own tag and lands as an asset on its
# own release, so the generator's only way to tell them apart is the wheel's own
# name — and a wheel naming a project that is not here is a packaging mistake,
# not a fourth project to publish.
PUBLISHED_PROJECT_NAMES = ("streamlib", "streamlib-moq", "streamlib-webrtc")

WHEEL_FILE_SUFFIX = ".whl"


class WheelAsset:
    """One published wheel: the file name pip matches, and where to fetch it."""

    def __init__(self, file_name: str, download_url: str) -> None:
        self.file_name = file_name
        self.download_url = download_url

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, WheelAsset):
            return NotImplemented
        return (self.file_name, self.download_url) == (
            other.file_name,
            other.download_url,
        )

    def __repr__(self) -> str:
        return f"WheelAsset({self.file_name!r}, {self.download_url!r})"


def normalize_project_name(project_name: str) -> str:
    """The PEP 503 normalized form pip resolves a directory name by."""
    return re.sub(r"[-_.]+", "-", project_name).lower()


def project_name_of_wheel(wheel_file_name: str) -> str:
    """The distribution a wheel file belongs to, per the wheel file-name spec."""
    return normalize_project_name(wheel_file_name.split("-", 1)[0])


def collect_wheel_assets(
    releases: "Iterable[dict[str, Any]]",
) -> "dict[str, list[WheelAsset]]":
    """Every published wheel across `releases`, by project, newest release first.

    Every published project gets an entry whether or not it has been released
    yet: a project page that exists and resolves nothing is one pip can install
    from the moment its first wheel lands, and a missing directory is a 404.

    Drafts are skipped — their assets are not publicly fetchable, and an index
    entry pip cannot download is worse than one that is missing.
    """
    collected: "dict[str, list[WheelAsset]]" = {
        project_name: [] for project_name in PUBLISHED_PROJECT_NAMES
    }
    for release in releases:
        if release.get("draft"):
            continue
        for asset in release.get("assets") or []:
            file_name = asset.get("name", "")
            if not file_name.endswith(WHEEL_FILE_SUFFIX):
                continue
            project_name = project_name_of_wheel(file_name)
            if project_name not in collected:
                continue
            download_url = asset.get("browser_download_url")
            if not download_url:
                continue
            collected[project_name].append(WheelAsset(file_name, download_url))
    return collected


def render_project_page(project_name: str, wheel_assets: "Sequence[WheelAsset]") -> str:
    """The per-project page: one anchor per file, which is all pip reads."""
    anchors = "\n".join(
        f'    <a href="{html.escape(asset.download_url, quote=True)}">'
        f"{html.escape(asset.file_name)}</a><br />"
        for asset in wheel_assets
    )
    return (
        "<!DOCTYPE html>\n"
        '<html><head><meta name="pypi:repository-version" content="1.0">'
        f"<title>Links for {project_name}</title></head>\n"
        f"  <body>\n    <h1>Links for {project_name}</h1>\n"
        f"{anchors}\n  </body>\n</html>\n"
    )


def render_root_page(project_names: "Sequence[str]") -> str:
    """The index root: every project this index serves, one anchor each."""
    anchors = "\n".join(
        f'    <a href="{project_name}/">{project_name}</a><br />'
        for project_name in project_names
    )
    return (
        "<!DOCTYPE html>\n"
        '<html><head><meta name="pypi:repository-version" content="1.0">'
        "<title>Simple index</title></head>\n"
        f"  <body>\n{anchors}\n  </body>\n</html>\n"
    )


def write_simple_index(
    output_directory: Path,
    wheel_assets_by_project: "Mapping[str, Sequence[WheelAsset]]",
) -> Path:
    """Write the `simple/` tree pip resolves against, and return its root."""
    simple_root = output_directory / "simple"
    project_names = sorted(wheel_assets_by_project)

    simple_root.mkdir(parents=True, exist_ok=True)
    (simple_root / "index.html").write_text(
        render_root_page(project_names), encoding="utf-8"
    )
    for project_name in project_names:
        project_directory = simple_root / project_name
        project_directory.mkdir(parents=True, exist_ok=True)
        (project_directory / "index.html").write_text(
            render_project_page(project_name, wheel_assets_by_project[project_name]),
            encoding="utf-8",
        )
    return simple_root


def main(argv: "Sequence[str] | None" = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Build the PEP 503 simple index for this repo's wheels from a "
            "GitHub releases listing (`gh api repos/OWNER/REPO/releases "
            "--paginate --slurp`)."
        )
    )
    parser.add_argument(
        "--releases-json",
        type=Path,
        required=True,
        help="File holding the GitHub releases listing as a JSON array.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="Directory to write `simple/` into.",
    )
    arguments = parser.parse_args(argv)

    releases = json.loads(arguments.releases_json.read_text(encoding="utf-8"))
    # `gh api --paginate --slurp` yields a list of pages; a single call yields a
    # flat list. Accept both rather than making the caller flatten.
    if releases and isinstance(releases[0], list):
        releases = [release for page in releases for release in page]

    wheel_assets_by_project = collect_wheel_assets(releases)
    simple_root = write_simple_index(arguments.output_dir, wheel_assets_by_project)

    for project_name, wheel_assets in sorted(wheel_assets_by_project.items()):
        print(f"{project_name}: {len(wheel_assets)} wheel link(s)")
    print(f"wrote {len(wheel_assets_by_project)} project page(s) to {simple_root}")

    projects_resolving_nothing = [
        project_name
        for project_name, wheel_assets in sorted(wheel_assets_by_project.items())
        if not wheel_assets
    ]
    if projects_resolving_nothing:
        print(
            "warning: no published wheels found for "
            f"{', '.join(projects_resolving_nothing)} — the index will resolve "
            "nothing for them",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())

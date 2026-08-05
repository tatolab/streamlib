#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Generate the PEP 503 simple index that serves the wheel from repo releases.

`pip install streamlib --index-url <pages-url>` is the one stable incantation
until the project rename makes PyPI publication possible; the artifact behind it
is identical either way. The index is rebuilt from the full release history on
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
from typing import Any, Iterable, Sequence

# PEP 503: the project a file belongs to, normalized. This index serves exactly
# one project, so a wheel naming anything else is a packaging mistake, not a
# second project to publish.
PUBLISHED_PROJECT_NAME = "streamlib"

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


def collect_wheel_assets(releases: "Iterable[dict[str, Any]]") -> "list[WheelAsset]":
    """Every published wheel across `releases`, newest release first.

    Drafts are skipped — their assets are not publicly fetchable, and an index
    entry pip cannot download is worse than one that is missing.
    """
    collected: "list[WheelAsset]" = []
    for release in releases:
        if release.get("draft"):
            continue
        for asset in release.get("assets") or []:
            file_name = asset.get("name", "")
            if not file_name.endswith(WHEEL_FILE_SUFFIX):
                continue
            if project_name_of_wheel(file_name) != PUBLISHED_PROJECT_NAME:
                continue
            download_url = asset.get("browser_download_url")
            if not download_url:
                continue
            collected.append(WheelAsset(file_name, download_url))
    return collected


def render_project_page(wheel_assets: "Sequence[WheelAsset]") -> str:
    """The per-project page: one anchor per file, which is all pip reads."""
    anchors = "\n".join(
        f'    <a href="{html.escape(asset.download_url, quote=True)}">'
        f"{html.escape(asset.file_name)}</a><br />"
        for asset in wheel_assets
    )
    return (
        "<!DOCTYPE html>\n"
        '<html><head><meta name="pypi:repository-version" content="1.0">'
        f"<title>Links for {PUBLISHED_PROJECT_NAME}</title></head>\n"
        f"  <body>\n    <h1>Links for {PUBLISHED_PROJECT_NAME}</h1>\n"
        f"{anchors}\n  </body>\n</html>\n"
    )


def render_root_page() -> str:
    """The index root: the one project this index serves."""
    return (
        "<!DOCTYPE html>\n"
        '<html><head><meta name="pypi:repository-version" content="1.0">'
        "<title>Simple index</title></head>\n"
        "  <body>\n"
        f'    <a href="{PUBLISHED_PROJECT_NAME}/">{PUBLISHED_PROJECT_NAME}</a><br />\n'
        "  </body>\n</html>\n"
    )


def write_simple_index(
    output_directory: Path, wheel_assets: "Sequence[WheelAsset]"
) -> Path:
    """Write the `simple/` tree pip resolves against, and return its root."""
    simple_root = output_directory / "simple"
    project_directory = simple_root / PUBLISHED_PROJECT_NAME
    project_directory.mkdir(parents=True, exist_ok=True)

    (simple_root / "index.html").write_text(render_root_page(), encoding="utf-8")
    (project_directory / "index.html").write_text(
        render_project_page(wheel_assets), encoding="utf-8"
    )
    return simple_root


def main(argv: "Sequence[str] | None" = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Build the PEP 503 simple index for the streamlib wheel from a "
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

    wheel_assets = collect_wheel_assets(releases)
    simple_root = write_simple_index(arguments.output_dir, wheel_assets)

    print(f"wrote {len(wheel_assets)} wheel link(s) to {simple_root}")
    if not wheel_assets:
        print(
            "warning: no published wheels found — the index will resolve nothing",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())

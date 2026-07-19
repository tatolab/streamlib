# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for the hand-rolled streamlib.yaml package-block reader.

The reader is package-identity-only: the `processors:` set is derived from
`@processor` decorators (see `test_processor_extraction.py`), never read from
the manifest.
"""

from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from streamlib._manifest import ManifestParseError, read_package_block


def _write(tmp_path: Path, body: str) -> Path:
    p = tmp_path / "streamlib.yaml"
    p.write_text(textwrap.dedent(body).lstrip("\n"))
    return p


class TestPackageBlockReader:
    def test_parses_package_block(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              org: tatolab
              name: cyberpunk-processor
              version: 0.1.0
              description: "Some description"

            processors:
              - name: AvatarCharacter
                runtime: python
                execution: reactive
            """,
        )
        package = read_package_block(path)
        assert package.org == "tatolab"
        assert package.name == "cyberpunk-processor"
        assert package.version == "0.1.0"

    def test_strips_double_quotes_around_version(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              org: tatolab
              name: example
              version: "1.2.3"
            """,
        )
        package = read_package_block(path)
        assert package.version == "1.2.3"

    def test_strips_single_quotes(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              org: 'tatolab'
              name: 'example'
              version: '1.2.3'
            """,
        )
        package = read_package_block(path)
        assert package.org == "tatolab"
        assert package.name == "example"

    def test_ignores_trailing_comments(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              org: tatolab  # inline comment
              name: example
              version: 0.1.0

            # standalone comment
            processors:
              - name: Foo
            """,
        )
        package = read_package_block(path)
        assert package.org == "tatolab"
        assert package.name == "example"

    def test_ignores_processors_section(self, tmp_path: Path) -> None:
        # The reader must not choke on a `processors:` block; it is simply not
        # read (the decorator is the truth-source).
        path = _write(
            tmp_path,
            """
            package:
              org: tatolab
              name: example
              version: 0.1.0

            processors:
              - name: Foo
                inputs:
                  - name: video_in
                    schema: any
              - name: Bar
            """,
        )
        package = read_package_block(path)
        assert package.name == "example"

    def test_missing_package_block_errors(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            processors:
              - name: Foo
            """,
        )
        with pytest.raises(ManifestParseError, match="missing required"):
            read_package_block(path)

    def test_missing_org_field_errors(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              name: example
              version: 0.1.0
            """,
        )
        with pytest.raises(ManifestParseError, match=r"org"):
            read_package_block(path)

    def test_no_file_raises_filenotfound(self, tmp_path: Path) -> None:
        with pytest.raises(FileNotFoundError):
            read_package_block(tmp_path / "does-not-exist.yaml")

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for the hand-rolled streamlib.yaml minimal manifest reader."""

from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from streamlib._manifest import ManifestParseError, read_manifest_summary


def _write(tmp_path: Path, body: str) -> Path:
    p = tmp_path / "streamlib.yaml"
    p.write_text(textwrap.dedent(body).lstrip("\n"))
    return p


class TestManifestReader:
    def test_parses_package_block_and_processors_list(self, tmp_path: Path) -> None:
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
              - name: CyberpunkLowerThird
                runtime: python
            """,
        )
        summary = read_manifest_summary(path)
        assert summary.package.org == "tatolab"
        assert summary.package.name == "cyberpunk-processor"
        assert summary.package.version == "0.1.0"
        assert summary.processor_names == ["AvatarCharacter", "CyberpunkLowerThird"]

    def test_strips_double_quotes_around_version(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              org: tatolab
              name: example
              version: "1.2.3"

            processors:
              - name: Foo
            """,
        )
        summary = read_manifest_summary(path)
        assert summary.package.version == "1.2.3"

    def test_strips_single_quotes(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              org: 'tatolab'
              name: 'example'
              version: '1.2.3'

            processors: []
            """,
        )
        summary = read_manifest_summary(path)
        assert summary.package.org == "tatolab"
        assert summary.processor_names == []

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
        summary = read_manifest_summary(path)
        assert summary.package.org == "tatolab"
        assert summary.processor_names == ["Foo"]

    def test_skips_nested_processor_body(self, tmp_path: Path) -> None:
        # Inner ports/inputs/outputs must NOT show up as processor entries.
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
                outputs:
                  - name: video_out
                    schema: any
              - name: Bar
            """,
        )
        summary = read_manifest_summary(path)
        assert summary.processor_names == ["Foo", "Bar"]

    def test_missing_package_block_errors(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            processors:
              - name: Foo
            """,
        )
        with pytest.raises(ManifestParseError, match="missing required"):
            read_manifest_summary(path)

    def test_missing_org_field_errors(self, tmp_path: Path) -> None:
        path = _write(
            tmp_path,
            """
            package:
              name: example
              version: 0.1.0

            processors:
              - name: Foo
            """,
        )
        with pytest.raises(ManifestParseError, match=r"org"):
            read_manifest_summary(path)

    def test_no_file_raises_filenotfound(self, tmp_path: Path) -> None:
        with pytest.raises(FileNotFoundError):
            read_manifest_summary(tmp_path / "does-not-exist.yaml")

    def test_processor_entry_with_non_name_first_key_errors(self, tmp_path: Path) -> None:
        # Defends the "name-first ordering" assumption with a clear failure.
        path = _write(
            tmp_path,
            """
            package:
              org: tatolab
              name: example
              version: 0.1.0

            processors:
              - runtime: python
                name: Foo
            """,
        )
        with pytest.raises(ManifestParseError, match="name`-first"):
            read_manifest_summary(path)

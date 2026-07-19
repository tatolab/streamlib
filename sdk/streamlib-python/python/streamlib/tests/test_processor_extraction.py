# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Golden-extraction tests for the import-and-enumerate processor extractor.

Mirrors the Rust `golden_extraction_over_a_fixture_crate` shape in
`sdk/streamlib-processor-extract/src/lib.rs`: a fixture package with several
processors across several modules (plus a non-processor module that must be
ignored), extracted by importing and enumerating the registry rather than
reading the manifest's `processors:` list.
"""

from __future__ import annotations

import json
import subprocess
import sys
import textwrap
from pathlib import Path

from streamlib.extract_processors import extract_processors_from_dir


def _write(dir_path: Path, rel: str, body: str) -> None:
    (dir_path / rel).write_text(textwrap.dedent(body).lstrip("\n"))


def _fixture_package(root: Path) -> None:
    _write(
        root,
        "streamlib.yaml",
        """
        package:
          org: tatolab
          name: demo-pack
          version: 0.2.0
        """,
    )
    # Two processors in two modules; a nested port declaration on one; and a
    # module that declares no processor (must be ignored).
    _write(
        root,
        "blur.py",
        """
        from streamlib import processor, input, output, SchemaIdent

        VIDEO = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")

        @processor("Blur")
        class Blur:
            @input(name="frames_in", schema=VIDEO)
            def handle_in(self): ...
            @output(name="frames_out", schema=VIDEO)
            def handle_out(self): ...
        """,
    )
    _write(
        root,
        "camera.py",
        """
        from streamlib import processor

        @processor("Camera")
        class Camera:
            pass
        """,
    )
    _write(
        root,
        "not_a_processor.py",
        """
        class JustAHelper:
            pass
        """,
    )


class TestProcessorExtraction:
    def test_golden_extraction_over_a_fixture_package(self, tmp_path: Path) -> None:
        _fixture_package(tmp_path)

        procs = extract_processors_from_dir(tmp_path)
        names = [p.short_name for p in procs]
        # Deterministic: sorted by joined schema-ident string.
        assert names == ["Blur", "Camera"]

        blur = next(p for p in procs if p.short_name == "Blur")
        assert str(blur.schema_ident) == "@tatolab/demo-pack/Blur@0.2.0"
        assert [port["name"] for port in blur.inputs] == ["frames_in"]
        assert [port["name"] for port in blur.outputs] == ["frames_out"]
        assert blur.inputs[0]["schema"].type_ == "VideoFrame"

        camera = next(p for p in procs if p.short_name == "Camera")
        assert str(camera.schema_ident) == "@tatolab/demo-pack/Camera@0.2.0"
        assert camera.inputs == ()

    def test_repeated_calls_are_isolated(self, tmp_path: Path) -> None:
        # The registry is cleared per call — extracting twice must not
        # accumulate duplicates.
        _fixture_package(tmp_path)
        first = extract_processors_from_dir(tmp_path)
        second = extract_processors_from_dir(tmp_path)
        assert [p.short_name for p in first] == [p.short_name for p in second]

    def test_schema_only_package_yields_no_processors(self, tmp_path: Path) -> None:
        _write(
            tmp_path,
            "streamlib.yaml",
            """
            package:
              org: tatolab
              name: schema-only
              version: 1.0.0
            """,
        )
        _write(tmp_path, "types.py", "class JustAType:\n    pass\n")
        assert extract_processors_from_dir(tmp_path) == []

    def test_cli_emits_manifest_json(self, tmp_path: Path) -> None:
        # The path `pkg build` drives: a fresh subprocess printing JSON.
        _fixture_package(tmp_path)
        result = subprocess.run(
            [sys.executable, "-m", "streamlib.extract_processors", str(tmp_path)],
            capture_output=True,
            text=True,
            check=True,
        )
        payload = json.loads(result.stdout)
        names = [entry["name"] for entry in payload]
        assert names == ["Blur", "Camera"]
        blur = next(e for e in payload if e["name"] == "Blur")
        assert blur["schema_ident"] == {
            "org": "tatolab",
            "package": "demo-pack",
            "type": "Blur",
            "version": "0.2.0",
        }
        assert blur["inputs"][0]["name"] == "frames_in"
        assert blur["inputs"][0]["schema"]["type"] == "VideoFrame"

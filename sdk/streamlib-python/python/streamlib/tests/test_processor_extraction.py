# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Golden-extraction tests for the import-and-enumerate processor extractor.

Mirrors the Rust `golden_extraction_over_a_fixture_crate` shape in
`sdk/streamlib-processor-extract/src/lib.rs`: a fixture package with several
processors across several modules under `processors/` (plus a non-processor
module, a test module, and a top-level module beside the `streamlib.yaml` —
all of which must be ignored), extracted by importing and enumerating the
registry rather than reading the manifest's `processors:` list. Identity,
execution mode, and ports are declared in code — the decorator reads no
`streamlib.yaml`.
"""

from __future__ import annotations

import json
import subprocess
import sys
import textwrap
from pathlib import Path

from streamlib.extract_processors import extract_processors_from_dir


def _write(dir_path: Path, rel: str, body: str) -> None:
    target = dir_path / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(textwrap.dedent(body).lstrip("\n"))


def _fixture_package(root: Path) -> None:
    # Three processors across three modules under `processors/` (one nested, to
    # pin the recursive walk); a nested port declaration on one; a module that
    # declares no processor; and four modules declaring a processor that must
    # NOT be discovered — a `test_`-prefixed module, a `_test`-suffixed module,
    # a module under `processors/tests/`, and a module beside the manifest. No
    # streamlib.yaml is needed — identity is declared in code, version-free.
    _write(
        root,
        "processors/blur.py",
        """
        from streamlib import processor, input, output, SchemaIdent

        VIDEO = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")

        @processor("@tatolab/demo-pack/Blur", execution="reactive")
        class Blur:
            @input(name="frames_in", schema=VIDEO)
            def handle_in(self): ...
            @output(name="frames_out", schema=VIDEO)
            def handle_out(self): ...
        """,
    )
    _write(
        root,
        "processors/camera.py",
        """
        from streamlib import processor

        @processor("@tatolab/demo-pack/Camera", execution="manual")
        class Camera:
            pass
        """,
    )
    _write(
        root,
        "processors/nested/deep.py",
        """
        from streamlib import processor

        @processor("@tatolab/demo-pack/Deep", execution="manual")
        class Deep:
            pass
        """,
    )
    _write(
        root,
        "processors/not_a_processor.py",
        """
        class JustAHelper:
            pass
        """,
    )
    _write(
        root,
        "processors/test_helper.py",
        """
        from streamlib import processor

        @processor("@tatolab/demo-pack/TestOnly", execution="manual")
        class TestOnly:
            pass
        """,
    )
    _write(
        root,
        "processors/helper_test.py",
        """
        from streamlib import processor

        @processor("@tatolab/demo-pack/SuffixTestOnly", execution="manual")
        class SuffixTestOnly:
            pass
        """,
    )
    _write(
        root,
        "processors/tests/fixtures.py",
        """
        from streamlib import processor

        @processor("@tatolab/demo-pack/InTestsDir", execution="manual")
        class InTestsDir:
            pass
        """,
    )
    _write(
        root,
        "top_level.py",
        """
        from streamlib import processor

        @processor("@tatolab/demo-pack/TopLevel", execution="manual")
        class TopLevel:
            pass
        """,
    )


class TestProcessorExtraction:
    def test_golden_extraction_over_a_fixture_package(self, tmp_path: Path) -> None:
        _fixture_package(tmp_path)

        procs = extract_processors_from_dir(tmp_path)
        names = [p.short_name for p in procs]
        # Deterministic: sorted by joined schema-ident string. Test scaffolding
        # (`test_helper.py`, `helper_test.py`, `tests/fixtures.py`) and a module
        # beside the manifest are not processor modules, so none is discovered.
        assert names == ["Blur", "Camera", "Deep"]

        blur = next(p for p in procs if p.short_name == "Blur")
        # Version-free identity: the extracted ident carries the 0.0.0 sentinel;
        # the concrete version is derived at package-build time (#1409).
        assert str(blur.schema_ident) == "@tatolab/demo-pack/Blur@0.0.0"
        assert blur.execution == "reactive"
        assert [port["name"] for port in blur.inputs] == ["frames_in"]
        assert [port["name"] for port in blur.outputs] == ["frames_out"]
        assert blur.inputs[0]["schema"].type_ == "VideoFrame"

        camera = next(p for p in procs if p.short_name == "Camera")
        assert str(camera.schema_ident) == "@tatolab/demo-pack/Camera@0.0.0"
        assert camera.execution == "manual"
        assert camera.inputs == ()

        deep = next(p for p in procs if p.short_name == "Deep")
        assert str(deep.schema_ident) == "@tatolab/demo-pack/Deep@0.0.0"

    def test_a_module_beside_the_manifest_is_not_discovered(
        self, tmp_path: Path
    ) -> None:
        # `processors/` is the one discovery root: a processor authored beside
        # the `streamlib.yaml` is invisible, with no fallback to the old
        # top-level glob.
        _write(
            tmp_path,
            "top_level.py",
            """
            from streamlib import processor

            @processor("@tatolab/demo-pack/TopLevel", execution="manual")
            class TopLevel:
                pass
            """,
        )
        _write(tmp_path, "processors/keep.py", "class JustAHelper:\n    pass\n")
        assert extract_processors_from_dir(tmp_path) == []

    def test_repeated_calls_are_isolated(self, tmp_path: Path) -> None:
        # The registry is cleared per call — extracting twice must not
        # accumulate duplicates.
        _fixture_package(tmp_path)
        first = extract_processors_from_dir(tmp_path)
        second = extract_processors_from_dir(tmp_path)
        assert [p.short_name for p in first] == [p.short_name for p in second]

    def test_repeated_calls_over_distinct_packages_are_isolated(
        self, tmp_path: Path
    ) -> None:
        # The `processors` package materialised by the first extraction must
        # not shadow the second package's modules — the stash/restore covers
        # ancestor package names, not just leaf modules. Both fixtures ship a
        # `processors/__init__.py` on purpose: that makes `processors` a
        # *regular* package with a static `__path__` pinned at the first root.
        # A PEP 420 namespace package recovers on its own (its `__path__`
        # recomputes when `sys.path` changes), so without the `__init__.py`
        # this test passes even with the ancestor stash reverted.
        first_root = tmp_path / "first"
        second_root = tmp_path / "second"
        _fixture_package(first_root)
        _write(first_root, "processors/__init__.py", "")
        _write(second_root, "processors/__init__.py", "")
        _write(
            second_root,
            "processors/other.py",
            """
            from streamlib import processor

            @processor("@tatolab/other-pack/Other", execution="manual")
            class Other:
                pass
            """,
        )
        assert [p.short_name for p in extract_processors_from_dir(first_root)] == [
            "Blur",
            "Camera",
            "Deep",
        ]
        assert [p.short_name for p in extract_processors_from_dir(second_root)] == [
            "Other"
        ]

    def test_emitted_order_is_codepoint(self, tmp_path: Path) -> None:
        # Codepoint order, the ordering the Deno and Rust extractors also emit:
        # `L` (0x4C) sorts before `l` (0x6C), so `BLUR2` precedes `Blur`. A
        # locale-collated sort would swap them.
        _write(
            tmp_path,
            "processors/blur.py",
            """
            from streamlib import processor

            @processor("@tatolab/demo-pack/Blur", execution="manual")
            class Blur:
                pass
            """,
        )
        _write(
            tmp_path,
            "processors/blur2.py",
            """
            from streamlib import processor

            @processor("@tatolab/demo-pack/BLUR2", execution="manual")
            class BLUR2:
                pass
            """,
        )
        procs = extract_processors_from_dir(tmp_path)
        assert [p.short_name for p in procs] == ["BLUR2", "Blur"]

    def test_package_without_a_processors_dir_yields_no_processors(
        self, tmp_path: Path
    ) -> None:
        # A schema-only package declares no processors and has no
        # `processors/` — that is not an error.
        _write(tmp_path, "types.py", "class JustAType:\n    pass\n")
        assert extract_processors_from_dir(tmp_path) == []

    def test_cli_emits_manifest_json(self, tmp_path: Path) -> None:
        # The path `pkg publish` drives: a fresh subprocess printing JSON.
        _fixture_package(tmp_path)
        result = subprocess.run(
            [sys.executable, "-m", "streamlib.extract_processors", str(tmp_path)],
            capture_output=True,
            text=True,
            check=True,
        )
        payload = json.loads(result.stdout)
        names = [entry["name"] for entry in payload]
        assert names == ["Blur", "Camera", "Deep"]
        blur = next(e for e in payload if e["name"] == "Blur")
        assert blur["schema_ident"] == {
            "org": "tatolab",
            "package": "demo-pack",
            "type": "Blur",
            "version": "0.0.0",
        }
        assert blur["execution"] == "reactive"
        assert blur["scheduling"] is None
        assert blur["inputs"][0]["name"] == "frames_in"
        assert blur["inputs"][0]["schema"]["type"] == "VideoFrame"
        camera = next(e for e in payload if e["name"] == "Camera")
        assert camera["execution"] == "manual"

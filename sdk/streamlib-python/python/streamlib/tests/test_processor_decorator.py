# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for `@processor("PascalCase")` and structured `SchemaIdent` parity."""

from __future__ import annotations

import importlib
import sys
import textwrap
from pathlib import Path

import pytest

from streamlib import SchemaIdent, processor, input, output


# =============================================================================
# SchemaIdent dataclass
# =============================================================================


class TestSchemaIdent:
    def test_constructs_with_valid_segments(self) -> None:
        ident = SchemaIdent(org="tatolab", package="core", type_="VideoFrame", version="1.0.0")
        assert ident.org == "tatolab"
        assert ident.package == "core"
        assert ident.type_ == "VideoFrame"
        assert ident.version == "1.0.0"

    def test_str_renders_joined_form(self) -> None:
        ident = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")
        assert str(ident) == "@tatolab/core/VideoFrame@1.0.0"

    def test_to_wire_dict_uses_type_key(self) -> None:
        ident = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")
        assert ident.to_wire_dict() == {
            "org": "tatolab",
            "package": "core",
            "type": "VideoFrame",
            "version": "1.0.0",
        }

    def test_frozen(self) -> None:
        ident = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")
        with pytest.raises(Exception):
            ident.org = "other"  # type: ignore[misc]

    def test_rejects_uppercase_org(self) -> None:
        with pytest.raises(ValueError, match="invalid org"):
            SchemaIdent("Tatolab", "core", "VideoFrame", "1.0.0")

    def test_rejects_uppercase_package(self) -> None:
        with pytest.raises(ValueError, match="invalid package"):
            SchemaIdent("tatolab", "Core", "VideoFrame", "1.0.0")

    def test_rejects_lowercase_type(self) -> None:
        with pytest.raises(ValueError, match="invalid type"):
            SchemaIdent("tatolab", "core", "videoFrame", "1.0.0")

    def test_rejects_underscore_in_org(self) -> None:
        with pytest.raises(ValueError, match="invalid org"):
            SchemaIdent("tato_lab", "core", "VideoFrame", "1.0.0")

    def test_rejects_malformed_version(self) -> None:
        with pytest.raises(ValueError, match="invalid version"):
            SchemaIdent("tatolab", "core", "VideoFrame", "1.0")

    def test_accepts_hyphen_in_package(self) -> None:
        ident = SchemaIdent("tatolab", "camera-python-display", "Foo", "0.1.0")
        assert ident.package == "camera-python-display"


# =============================================================================
# @processor decorator — manifest-driven structured-ident emission
# =============================================================================


def _write_manifest(dir_path: Path, body: str) -> None:
    (dir_path / "streamlib.yaml").write_text(textwrap.dedent(body).lstrip("\n"))


def _import_class_from_dir(dir_path: Path, module_name: str, body: str):
    """Write `<module_name>.py` containing `body` and import it fresh."""
    (dir_path / f"{module_name}.py").write_text(textwrap.dedent(body).lstrip("\n"))
    sys.path.insert(0, str(dir_path))
    try:
        if module_name in sys.modules:
            del sys.modules[module_name]
        return importlib.import_module(module_name)
    finally:
        sys.path.remove(str(dir_path))


class TestProcessorDecorator:
    def test_attaches_structured_schema_ident_from_manifest(self, tmp_path: Path) -> None:
        _write_manifest(
            tmp_path,
            """
            package:
              org: tatolab
              name: cyberpunk-processor
              version: 0.1.0

            processors:
              - name: CyberpunkProcessor
                runtime: python
                execution: reactive
            """,
        )
        module = _import_class_from_dir(
            tmp_path,
            "decorator_pkg_module",
            """
            from streamlib import processor

            @processor("CyberpunkProcessor")
            class CyberpunkProcessor:
                pass
            """,
        )
        ident = module.CyberpunkProcessor.__streamlib_schema_ident__
        assert ident.org == "tatolab"
        assert ident.package == "cyberpunk-processor"
        assert ident.type_ == "CyberpunkProcessor"
        assert ident.version == "0.1.0"
        assert str(ident) == "@tatolab/cyberpunk-processor/CyberpunkProcessor@0.1.0"

    def test_prerelease_package_version_projects_to_release_core(
        self, tmp_path: Path
    ) -> None:
        # A `-dev.N` / `-rc.N` package version is legal; the schema ident it
        # mints must project onto the release core (the 3-part SchemaIdent
        # validator would otherwise reject the dev-versioned package).
        _write_manifest(
            tmp_path,
            """
            package:
              org: tatolab
              name: camera
              version: 0.4.33-dev.2

            processors:
              - name: Camera
                runtime: python
                execution: reactive
            """,
        )
        module = _import_class_from_dir(
            tmp_path,
            "decorator_prerelease_module",
            """
            from streamlib import processor

            @processor("Camera")
            class Camera:
                pass
            """,
        )
        ident = module.Camera.__streamlib_schema_ident__
        assert ident.version == "0.4.33"
        assert str(ident) == "@tatolab/camera/Camera@0.4.33"

    def test_unknown_prerelease_channel_rejected_not_projected(
        self, tmp_path: Path
    ) -> None:
        # Only `-dev.N` / `-rc.N` project; an alpha (or any foreign channel)
        # must raise — the same manifest is rejected by Rust's parser, and
        # silently projecting here would let the runtimes disagree.
        _write_manifest(
            tmp_path,
            """
            package:
              org: tatolab
              name: camera
              version: 0.4.33-alpha.1

            processors:
              - name: Camera
                runtime: python
                execution: reactive
            """,
        )
        with pytest.raises(ValueError, match="invalid package version"):
            _import_class_from_dir(
                tmp_path,
                "decorator_alpha_module",
                """
                from streamlib import processor

                @processor("Camera")
                class Camera:
                    pass
                """,
            )

    def test_missing_manifest_errors_with_expected_path(self, tmp_path: Path) -> None:
        # No streamlib.yaml in tmp_path
        with pytest.raises(FileNotFoundError, match="streamlib.yaml"):
            _import_class_from_dir(
                tmp_path,
                "no_manifest_module",
                """
                from streamlib import processor

                @processor("Anything")
                class Anything:
                    pass
                """,
            )

    def test_short_name_not_in_manifest_lists_available(self, tmp_path: Path) -> None:
        _write_manifest(
            tmp_path,
            """
            package:
              org: tatolab
              name: example
              version: 0.1.0

            processors:
              - name: Camera
              - name: Display
            """,
        )
        with pytest.raises(ValueError, match=r"Available processors"):
            _import_class_from_dir(
                tmp_path,
                "missing_short_name_module",
                """
                from streamlib import processor

                @processor("MissingProcessor")
                class MissingProcessor:
                    pass
                """,
            )

    def test_manifest_missing_org_errors(self, tmp_path: Path) -> None:
        _write_manifest(
            tmp_path,
            """
            package:
              name: example
              version: 0.1.0

            processors:
              - name: Foo
            """,
        )
        with pytest.raises(ValueError, match="missing required `package:` field"):
            _import_class_from_dir(
                tmp_path,
                "missing_org_module",
                """
                from streamlib import processor

                @processor("Foo")
                class Foo:
                    pass
                """,
            )

    def test_legacy_kwarg_form_rejected(self) -> None:
        # The legacy `name=`/`description=`/`execution=` kwarg form is gone.
        # `@processor(name="...")` now raises immediately because the first
        # positional must be a str.
        with pytest.raises(TypeError):
            processor(name="Camera")  # type: ignore[arg-type]


# =============================================================================
# @input / @output schema validation
# =============================================================================


class TestPortSchemaResolution:
    def test_accepts_schema_ident_instance(self) -> None:
        ident = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")

        @input(schema=ident)
        def video_in(self):
            pass

        assert video_in._streamlib_input_port["schema"] is ident

    def test_rejects_bare_string_schema(self) -> None:
        with pytest.raises(TypeError, match="string schema references are no longer accepted"):
            @input(schema="VideoFrame")
            def video_in(self):
                pass

    def test_rejects_joined_string_schema(self) -> None:
        with pytest.raises(TypeError, match="string schema references"):
            @output(schema="@tatolab/core/VideoFrame@1.0.0")
            def video_out(self):
                pass

    def test_accepts_class_carrying_schema_ident(self) -> None:
        class MySchema:
            __streamlib_schema_ident__ = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")

        @input(schema=MySchema)
        def video_in(self):
            pass

        assert isinstance(video_in._streamlib_input_port["schema"], SchemaIdent)
        assert video_in._streamlib_input_port["schema"].type_ == "VideoFrame"

    def test_class_without_schema_metadata_rejected(self) -> None:
        class Plain:
            pass

        with pytest.raises(TypeError, match="does not carry a structured SchemaIdent"):
            @output(schema=Plain)
            def video_out(self):
                pass

    def test_none_schema_ok(self) -> None:
        @input(schema=None)
        def control(self):
            pass

        assert control._streamlib_input_port["schema"] is None


# =============================================================================
# Codegen-emitted classes carry __streamlib_schema_ident__
# =============================================================================


class TestGeneratedSchemaIdents:
    """Lock the codegen → `@input(schema=GeneratedClass)` path end-to-end.

    Imports a real codegen-emitted class from the in-tree `_generated_/`
    tree and asserts the structured `SchemaIdent` attribute it carries
    matches the manifest-declared identity. If `streamlib generate` is
    rerun, this test catches a regression in the post-processor's
    SchemaIdent injection.
    """

    def test_video_frame_carries_structured_schema_ident(self) -> None:
        from streamlib._generated_.tatolab__core import VideoFrame

        ident = getattr(VideoFrame, "__streamlib_schema_ident__", None)
        assert isinstance(ident, SchemaIdent), (
            "VideoFrame must carry __streamlib_schema_ident__: SchemaIdent. "
            "If this fails, rerun `cargo xtask generate-schemas --runtime python "
            "--project-dir libs/streamlib --output sdk/streamlib-python/python/streamlib/_generated_`."
        )
        assert ident.org == "tatolab"
        assert ident.package == "core"
        assert ident.type_ == "VideoFrame"
        assert ident.version == "1.0.0"

    def test_input_port_resolves_codegen_emitted_class(self) -> None:
        from streamlib._generated_.tatolab__core import AudioFrame

        @input(schema=AudioFrame)
        def audio_in(self):
            pass

        resolved = audio_in._streamlib_input_port["schema"]
        assert isinstance(resolved, SchemaIdent)
        assert resolved == SchemaIdent("tatolab", "core", "AudioFrame", "1.0.0")

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for `@processor("@org/package/Type", execution=...)` — identity, mode,
and ports declared in code (no decoration-time `streamlib.yaml` read).
"""

from __future__ import annotations

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
# @processor decorator — in-code identity, version-free sentinel
# =============================================================================


class TestProcessorIdentity:
    def test_attaches_structured_schema_ident_from_code(self) -> None:
        @processor("@tatolab/camera/Camera", execution="manual")
        class Camera:
            pass

        ident = Camera.__streamlib_schema_ident__
        assert ident.org == "tatolab"
        assert ident.package == "camera"
        assert ident.type_ == "Camera"
        # Version-free identity synthesizes the 0.0.0 sentinel; the concrete
        # version is derived at package-build time (#1409).
        assert ident.version == "0.0.0"
        assert str(ident) == "@tatolab/camera/Camera@0.0.0"

    def test_hyphenated_org_and_package_accepted(self) -> None:
        @processor(
            "@tatolab/camera-python-display/CyberpunkProcessor", execution="reactive"
        )
        class CyberpunkProcessor:
            pass

        ident = CyberpunkProcessor.__streamlib_schema_ident__
        assert ident.package == "camera-python-display"
        assert ident.type_ == "CyberpunkProcessor"

    def test_omitted_identity_synthesizes_app_local(self) -> None:
        # A bare module with no streamlib.yaml defines a working local
        # processor: identity synthesizes @app/local/<ClassName>.
        @processor(execution="reactive")
        class LocalFilter:
            pass

        ident = LocalFilter.__streamlib_schema_ident__
        assert ident.org == "app"
        assert ident.package == "local"
        assert ident.type_ == "LocalFilter"
        assert ident.version == "0.0.0"

    def test_app_local_synth_rejects_non_pascalcase_class(self) -> None:
        with pytest.raises(ValueError, match="cannot synthesize an `@app/local`"):
            @processor(execution="reactive")
            class lowercaseName:  # noqa: N801 — intentionally invalid
                pass

    def test_versioned_identity_is_rejected(self) -> None:
        # The grammar is version-free (#1409): a hand-authored `@<version>` is
        # rejected. Mentally revert the version-free `_parse_identity_str` and
        # this passes when it must fail.
        with pytest.raises(ValueError, match="must be version-free"):
            @processor("@tatolab/camera/Camera@1.0.0", execution="manual")
            class Camera:
                pass

    def test_identity_without_at_prefix_is_rejected(self) -> None:
        with pytest.raises(ValueError, match="must start with `@`"):
            @processor("tatolab/camera/Camera", execution="manual")
            class Camera:
                pass

    def test_identity_wrong_segment_count_is_rejected(self) -> None:
        with pytest.raises(ValueError, match="three `/`-separated segments"):
            @processor("@tatolab/Camera", execution="manual")
            class Camera:
                pass

    def test_non_string_identity_is_rejected(self) -> None:
        with pytest.raises(TypeError, match="identity must be a version-free"):
            processor(123, execution="manual")  # type: ignore[arg-type]


# =============================================================================
# @processor decorator — execution + scheduling declared in code
# =============================================================================


class TestProcessorExecution:
    def test_reactive_execution_is_a_bare_string(self) -> None:
        @processor("@tatolab/demo/Reactive", execution="reactive")
        class Reactive:
            pass

        assert Reactive.__streamlib_execution__ == "reactive"

    def test_manual_execution_is_a_bare_string(self) -> None:
        @processor("@tatolab/demo/Manual", execution="manual")
        class Manual:
            pass

        assert Manual.__streamlib_execution__ == "manual"

    def test_continuous_execution_carries_interval(self) -> None:
        @processor("@tatolab/demo/Loop", execution="continuous", interval_ms=16)
        class Loop:
            pass

        assert Loop.__streamlib_execution__ == {
            "type": "continuous",
            "interval_ms": 16,
        }

    def test_continuous_defaults_interval_to_zero(self) -> None:
        @processor("@tatolab/demo/Loop", execution="continuous")
        class Loop:
            pass

        assert Loop.__streamlib_execution__ == {
            "type": "continuous",
            "interval_ms": 0,
        }

    def test_execution_is_required(self) -> None:
        with pytest.raises(TypeError):
            @processor("@tatolab/demo/NoMode")  # type: ignore[call-arg]
            class NoMode:
                pass

    def test_unknown_execution_mode_is_rejected(self) -> None:
        with pytest.raises(ValueError, match="invalid execution"):
            @processor("@tatolab/demo/Bad", execution="sideways")
            class Bad:
                pass

    def test_scheduling_projects_to_priority_mapping(self) -> None:
        from streamlib._processor_registry import registered_processors

        @processor("@tatolab/demo/Scheduled", execution="manual", scheduling="high")
        class Scheduled:
            pass

        entry = next(
            e for e in registered_processors() if e.short_name == "Scheduled"
        )
        assert entry.scheduling == {"priority": "high"}

    def test_unknown_scheduling_priority_is_rejected(self) -> None:
        with pytest.raises(ValueError, match="invalid scheduling"):
            @processor("@tatolab/demo/Bad", execution="manual", scheduling="turbo")
            class Bad:
                pass


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

    def test_processor_collects_ports_declared_in_code(self) -> None:
        VIDEO = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")

        @processor("@tatolab/demo/Ports", execution="reactive")
        class Ports:
            @input(name="video_in", schema=VIDEO, description="frames")
            def handle_in(self):
                pass

            @output(name="video_out", schema=VIDEO)
            def handle_out(self):
                pass

        ports = Ports.__streamlib_ports__
        assert [p["name"] for p in ports["inputs"]] == ["video_in"]
        assert [p["name"] for p in ports["outputs"]] == ["video_out"]
        assert ports["inputs"][0]["description"] == "frames"
        assert ports["inputs"][0]["schema"].type_ == "VideoFrame"


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

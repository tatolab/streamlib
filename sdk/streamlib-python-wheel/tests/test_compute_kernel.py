# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Named N-binding compute dispatch, from Python.

The v1 compute wire carried a single `surface_uuid`, bound as a storage image
at slot 0, output-only — which is why no Python compute filter had ever
existed: the wire could not express read-one-write-another. These tests are
that pass, written the way a user writes it.

A kernel is an object: built in `setup()` from GLSL text the engine compiles,
dispatched per frame in `process()`, with bindings passed at dispatch by the
shader's own names and never persisting on the kernel. Nothing here shells out
to a shader compiler, and that absence is load-bearing: authoring a kernel
requires no toolchain beyond the installed wheel. What is worth breaking a build over is that
two distinct surfaces really are bound by name, and that every way of getting
the bindings wrong is refused before any GPU work is submitted, with a message
naming what the shader actually declares.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line; the tests drive the app out of process and
assert on that line.
"""

import json
import os
import re
import shutil
from pathlib import Path

import pytest

from compute_kernel_probes import (
    FILLED_SOURCE_RGBA,
    OUTPUT_BINDING,
    SOURCE_BINDING,
)

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "compute_kernel_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_probe(start_app_under_test, probe_class_name: str) -> dict:
    """One probe, one observation dict — or a failure carrying the probe's own
    traceback, which names the cause better than a missing marker."""
    app = start_app_under_test(APP, probe_class_name)
    app.await_output_containing(
        "MARKER:PROBE_RESULT", f"the {probe_class_name} result"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


def test_a_python_processor_reads_one_surface_and_writes_another(start_app_under_test):
    """The whole point: one dispatch, two distinct surfaces, bound by name."""
    observed = run_probe(start_app_under_test, "ReadOneWriteAnotherProbe")

    assert observed["dispatched"] is True
    assert observed["surfaces_are_distinct"], (
        "the source and the output must be different surfaces — a pass that "
        "wrote back into its own input is the v1 shape, not this one"
    )
    assert observed["binding_names"] == [SOURCE_BINDING, OUTPUT_BINDING], (
        "the kernel reports the shader's own binding names, which is what a "
        "dispatch resolves against"
    )


def test_the_kernel_takes_its_binding_names_from_the_shader(start_app_under_test):
    """Nothing declares these names but the shader itself."""
    observed = run_probe(start_app_under_test, "ReadOneWriteAnotherProbe")
    assert observed["binding_names"] == [SOURCE_BINDING, OUTPUT_BINDING]


def test_an_unsupplied_binding_is_refused_naming_the_shaders_bindings(
    start_app_under_test,
):
    """No implicit default and no carried-over value: the kernel holds no
    binding state between dispatches to fall back on."""
    observed = run_probe(start_app_under_test, "BindingRefusalProbe")

    missing = observed["missing"]
    assert OUTPUT_BINDING in missing, f"must name the missing binding: {missing}"
    assert "not supplied" in missing, missing
    assert "do not persist between dispatches" in missing, (
        f"must say why there is no fallback: {missing}"
    )


def test_a_binding_the_shader_does_not_declare_is_refused(start_app_under_test):
    observed = run_probe(start_app_under_test, "BindingRefusalProbe")

    unknown = observed["unknown"]
    assert "sharpen_amount" in unknown, f"must name the unknown binding: {unknown}"
    assert SOURCE_BINDING in unknown and OUTPUT_BINDING in unknown, (
        f"must name what the shader does declare: {unknown}"
    )


def test_a_declaration_disagreeing_with_reflection_is_refused_at_construction(
    start_app_under_test,
):
    """`bindings={name: kind}` at create asserts against reflection — a name
    the shader lacks refuses before a kernel exists, naming what it has."""
    observed = run_probe(start_app_under_test, "BindingRefusalProbe")

    wrong_declaration = observed["wrong_declaration"]
    assert "sharpen_amount" in wrong_declaration, wrong_declaration
    assert SOURCE_BINDING in wrong_declaration and OUTPUT_BINDING in wrong_declaration, (
        f"must name the shader's own bindings: {wrong_declaration}"
    )


def test_a_push_constant_payload_of_the_wrong_size_is_refused(start_app_under_test):
    observed = run_probe(start_app_under_test, "BindingRefusalProbe")

    wrong_size = observed["wrong_push_constant_size"]
    assert "push-constant" in wrong_size, wrong_size
    assert "4" in wrong_size and "1" in wrong_size, (
        f"must name both the declared size and the size supplied: {wrong_size}"
    )


def test_a_binding_naming_an_unknown_surface_is_refused(start_app_under_test):
    observed = run_probe(start_app_under_test, "BindingRefusalProbe")

    unresolvable = observed["unregistered_surface"]
    assert "no-such-surface" in unresolvable, (
        f"must name the surface it could not resolve: {unresolvable}"
    )
    assert OUTPUT_BINDING in unresolvable, (
        f"must name the binding that named it: {unresolvable}"
    )


def test_a_texture_backed_surfaces_pixels_reach_the_cpu_with_numpy_alone(
    start_app_under_test,
):
    """The staged CPU door, both ways, with no GPU package in the process:
    a write door fills an acquired texture, and a read door answers with a
    kernel's own output pixels."""
    observed = run_probe(start_app_under_test, "TextureBackedPixelsReachTheCpuProbe")

    assert observed["surface_id"], "an acquired texture carries the id it travels under"
    assert observed["width"] == 64 and observed["height"] == 64
    assert observed["surface_id"] != observed["source_surface_id"], (
        "two acquires are two surfaces"
    )

    assert observed["published_pixel"] == list(FILLED_SOURCE_RGBA), (
        "the staged edit must publish into the surface's own allocation at the "
        f"block edge: {observed['published_pixel']!r}"
    )

    # The shader writes `1.0 - source.rgb` with a zero bias and passes alpha
    # through; unorm round-trip is worth one code point of slack, no more.
    inverted = [255 - channel for channel in FILLED_SOURCE_RGBA[:3]]
    read_back = observed["kernel_output_pixel"]
    assert all(abs(read - want) <= 1 for read, want in zip(read_back[:3], inverted)), (
        f"the CPU read of the kernel output must show the kernel's own pixels: "
        f"{read_back!r} against {inverted!r}"
    )
    assert read_back[3] == FILLED_SOURCE_RGBA[3], (
        f"the shader passes alpha through unchanged: {read_back!r}"
    )


def test_a_raise_inside_the_staged_cpu_door_discards_the_edit(start_app_under_test):
    """Over a texture backing the door publishes at the block edge, so a
    propagating raise leaves the frame the engine already held."""
    observed = run_probe(start_app_under_test, "StagedCpuDoorDiscardsOnRaiseProbe")

    assert observed["raised"] == "the edit does not finish", (
        f"discarding must never suppress the exception: {observed['raised']!r}"
    )
    assert observed["pixel_after_the_raise"] == list(FILLED_SOURCE_RGBA), (
        "the discarded edit must not reach the surface; the frame keeps the "
        f"pixels it already held: {observed['pixel_after_the_raise']!r}"
    )


def test_an_acquired_texture_takes_a_write_back_with_no_copy_usage_spelled(
    start_app_under_test,
):
    """The zero-ceremony bar for the LUT flow: one usage token is enough,
    because the engine implies both copy bits rather than refusing about a
    flag the author had no reason to name."""
    observed = run_probe(start_app_under_test, "AcquiredTextureImpliesCopyUsageProbe")

    assert observed["surface_id"], "the acquire answers with a surface id"
    assert observed["takes_a_write_back"] is True, (
        "a texture this processor acquired can take a recorded copy in, so the "
        "engine's write-back answer must be yes"
    )


def test_a_kernel_is_built_with_no_shader_toolchain_on_path(
    start_app_under_test, tmp_path, monkeypatch
):
    """The claim the whole change rests on, made falsifiable.

    Asserting "the probe does not shell out" by *reading* the probe proves
    nothing durable — someone adds a `subprocess.run(["glslc", ...])` later and
    every test still passes. So `glslc` and `glslangValidator` are shadowed
    with stubs that exit 127 and say so, and the app plus the helper child it
    spawns inherit that PATH. A kernel still builds, so the compiler that built
    it is the one linked into the wheel.
    """
    sabotaged = tmp_path / "no-shader-toolchain"
    sabotaged.mkdir()
    for tool in ("glslc", "glslangValidator"):
        stub = sabotaged / tool
        stub.write_text(
            f"#!/bin/sh\necho '{tool} was invoked; the engine must not shell out' >&2\nexit 127\n"
        )
        stub.chmod(0o755)
    monkeypatch.setenv("PATH", f"{sabotaged}{os.pathsep}{os.environ['PATH']}")
    assert shutil.which("glslc") == str(sabotaged / "glslc"), (
        "the stub must be what a shell-out would find, or this test proves nothing"
    )

    observed = run_probe(start_app_under_test, "ReadOneWriteAnotherProbe")
    assert observed["dispatched"] is True
    assert sorted(observed["binding_names"]) == sorted([SOURCE_BINDING, OUTPUT_BINDING])


def test_a_kernel_built_from_neither_source_nor_spirv_is_refused_naming_both(
    start_app_under_test,
):
    observed = run_probe(start_app_under_test, "ShaderSourceRefusalProbe")
    assert "source" in observed["neither"]
    assert "spv_hex" in observed["neither"]


def test_a_kernel_built_from_both_source_and_spirv_is_refused_naming_both(
    start_app_under_test,
):
    """They are alternatives; which one to run is not something to guess at."""
    observed = run_probe(start_app_under_test, "ShaderSourceRefusalProbe")
    assert "source" in observed["both"]
    assert "spv_hex" in observed["both"]


def test_a_shader_that_does_not_compile_reports_the_compilers_own_diagnostic(
    start_app_under_test,
):
    """The author reads this message, so it carries the offending line and the
    name of what the shader got wrong — not just "compilation failed"."""
    observed = run_probe(start_app_under_test, "ShaderSourceRefusalProbe")
    assert "no_such_function" in observed["does_not_compile"]
    assert ":2" in observed["does_not_compile"]


def test_a_glsl_entry_point_other_than_main_is_refused(start_app_under_test):
    """glslang will not rename a GLSL entry point, so accepting one would build
    a pipeline against a function the module does not contain."""
    observed = run_probe(start_app_under_test, "ShaderSourceRefusalProbe")
    assert "sharpen" in observed["non_main_entry_point"]
    assert "main" in observed["non_main_entry_point"]

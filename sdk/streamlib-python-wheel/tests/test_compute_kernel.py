# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Named N-binding compute dispatch, from Python.

The v1 compute wire carried a single `surface_uuid`, bound as a storage image
at slot 0, output-only — which is why no Python compute filter had ever
existed: the wire could not express read-one-write-another. These tests are
that pass, written the way a user writes it.

A kernel is an object: built in `setup()`, dispatched per frame in
`process()`, with bindings passed at dispatch by the shader's own names and
never persisting on the kernel. What is worth breaking a build over is that
two distinct surfaces really are bound by name, and that every way of getting
the bindings wrong is refused before any GPU work is submitted, with a message
naming what the shader actually declares.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line; the tests drive the app out of process and
assert on that line.
"""

import json
import re
import shutil
from pathlib import Path

import pytest

from compute_kernel_probes import OUTPUT_BINDING, SOURCE_BINDING

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "compute_kernel_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


@pytest.fixture(scope="module", autouse=True)
def _needs_a_shader_compiler() -> None:
    """Until GLSL is the source contract the engine compiles, a caller hands
    over SPIR-V — and these tests produce it with `glslc`."""
    if shutil.which("glslc") is None:
        pytest.skip("glslc is not on PATH; the probes compile their own SPIR-V")


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


def test_an_acquired_texture_is_a_name_not_a_local_mapping(start_app_under_test):
    """`acquire_texture` stops raising, but what it returns is the id a
    dispatch binds — not addressable pixels. Asking for them says so."""
    observed = run_probe(start_app_under_test, "TextureIsNotLocallyMappedProbe")

    assert observed["surface_id"], "an acquired texture carries the id it travels under"
    assert observed["width"] == 64 and observed["height"] == 64
    assert observed["surface_id"] != observed["source_surface_id"], (
        "two acquires are two surfaces"
    )

    refusal = observed["pixels_refusal"]
    assert refusal is not None, (
        "a device texture has no mapping in this process, so reading its "
        "pixels here must refuse rather than answer"
    )
    assert "not mapped into this process" in refusal, refusal

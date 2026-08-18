# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Named-binding graphics draws, from Python.

Graphics dispatch is an always-present capability of `GpuContext`: there is no
installation step and no runtime-absent case, so the pass here — a fullscreen
triangle sampling an acquired texture into an acquired colour target — needs
nothing from the application but the processor itself.

A kernel is an object: built in `setup()` from GLSL text the engine compiles,
drawn per frame in `process()`, with bindings passed at draw by the shaders'
own names and never persisting on the kernel. What is worth breaking a build
over is that the draw runs, and that every way of getting the bindings wrong is
refused with a message naming what the shaders actually declare — the stage
claim before a kernel exists, the rest before any GPU work is submitted.

The device-dependent tests are `requires_gpu` and execute on the rig only; CI
green is not proof for them. The two that read the surface itself are not, and
they are what keeps "no vertex buffer, no index buffer, no depth target" from
being a claim only prose makes.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line; the tests drive the app out of process and
assert on that line.
"""

import inspect
import json
import re
from pathlib import Path

import pytest

from graphics_kernel_probes import SOURCE_BINDING
from streamlib._engine import GpuContextFullAccess, GraphicsKernel

APP = Path(__file__).parent / "graphics_kernel_app.py"

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


def spelled_the_same_way(message: str) -> str:
    """A message with the two spellings of a binding kind — the wire's
    `storage_image` and the engine type's `StorageImage` — made comparable."""
    return message.lower().replace("_", "")


def test_a_draw_takes_no_vertex_buffer_no_index_buffer_and_no_depth_target():
    """The recon constraints, stated where a caller meets them.

    No escalate op mints a `VertexBuffer` or an `IndexBuffer`, and the
    offscreen pass a draw runs attaches colour targets only — so the honest
    surface is one that cannot ask for them at all. Asserted against the
    signature rather than a refusal message, because a parameter that quietly
    reappears is exactly what this forbids.
    """
    parameters = inspect.signature(GraphicsKernel.draw).parameters
    unsupported = [
        name
        for name in parameters
        if "vertex_buffer" in name or "index_buffer" in name or "depth" in name
    ]
    assert unsupported == [], (
        f"a draw cannot honour {unsupported}: no escalate op mints a vertex or "
        "index buffer, and the pass attaches colour targets only"
    )
    assert "vertex_count" in parameters, (
        "the vertices are the shaders' own — a draw still says how many of them"
    )


def test_a_graphics_kernel_carries_no_depth_or_vertex_input_state():
    """The pipeline the wire builds has no depth attachment and no vertex
    input, so neither is a knob `create_graphics_kernel` offers."""
    parameters = inspect.signature(GpuContextFullAccess.create_graphics_kernel).parameters
    unsupported = [
        name
        for name in parameters
        if "depth" in name or "vertex_input" in name or "multisample" in name
    ]
    assert unsupported == [], (
        f"the graphics kernel builds single-sampled colour-only pipelines: {unsupported}"
    )


@pytest.mark.requires_gpu
def test_a_python_processor_draws_through_a_graphics_kernel(start_app_under_test):
    """The demo: a pass rendered from a helper process, with named bindings and
    no application-supplied bridge."""
    observed = run_probe(start_app_under_test, "FullscreenTriangleDrawProbe")

    assert observed["drew"] is True
    assert observed["surfaces_are_distinct"], (
        "the sampled input and the colour target must be different surfaces — "
        "the pass discards its target's contents on entry"
    )
    assert observed["binding_names"] == [SOURCE_BINDING], (
        "the kernel reports the shaders' own binding names, which is what a "
        "draw resolves against"
    )


@pytest.mark.requires_gpu
def test_a_binding_declared_for_a_stage_that_does_not_read_it_is_refused_at_construction(
    start_app_under_test,
):
    """The ticket's named validation case.

    A draw never revisits which stage reads what — the descriptor set layout is
    built once — so a wrong stage claim has to refuse where the multi-stage
    declaration is built. The traceback is asserted on, not just the message:
    "at construction" means the `create_graphics_kernel` line raised and no
    kernel object was ever handed back to draw with.
    """
    observed = run_probe(start_app_under_test, "GraphicsStageMismatchProbe")

    stage_mismatch = observed["stage_mismatch"]
    assert SOURCE_BINDING in stage_mismatch, stage_mismatch
    assert "vertex" in stage_mismatch and "fragment" in stage_mismatch, (
        f"must name both the stage claimed and the stage the shaders read it in: "
        f"{stage_mismatch}"
    )

    raised_at = observed["stage_mismatch_traceback"]
    assert "create_graphics_kernel" in raised_at, (
        f"the refusal must come from the construction line: {raised_at}"
    )
    assert "kernel.draw(" not in raised_at, (
        f"a stage mismatch caught at the draw is caught too late: {raised_at}"
    )
    assert observed["the_corrected_declaration_drew"] is True, (
        "the same shaders with the stage claim corrected must still draw, or "
        "the refusal rejected the pass rather than the declaration"
    )


@pytest.mark.requires_gpu
def test_a_binding_the_shaders_do_not_declare_is_refused_at_construction(
    start_app_under_test,
):
    """`bindings={name: (kind, stages)}` asserts against reflection: a name the
    shaders lack refuses before a kernel exists, naming what they have."""
    observed = run_probe(start_app_under_test, "GraphicsBindingRefusalProbe")

    unknown = observed["unknown_at_construction"]
    assert "tint_amount" in unknown, f"must name the unknown binding: {unknown}"
    assert SOURCE_BINDING in unknown, (
        f"must name what the shaders do declare: {unknown}"
    )


@pytest.mark.requires_gpu
def test_a_binding_declared_as_the_wrong_kind_is_refused_at_construction(
    start_app_under_test,
):
    observed = run_probe(start_app_under_test, "GraphicsBindingRefusalProbe")

    mismatch = observed["kind_mismatch_at_construction"]
    assert SOURCE_BINDING in mismatch, mismatch
    assert "storageimage" in spelled_the_same_way(mismatch), (
        f"must name the kind claimed: {mismatch}"
    )
    assert "sampledtexture" in spelled_the_same_way(mismatch), (
        f"must name the kind the shaders declare: {mismatch}"
    )


@pytest.mark.requires_gpu
def test_leaving_one_of_the_shaders_bindings_undeclared_is_refused(
    start_app_under_test,
):
    """A declaration is total: an unmentioned binding is how a draw silently
    binds nothing."""
    observed = run_probe(start_app_under_test, "GraphicsBindingRefusalProbe")

    undeclared = observed["undeclared_at_construction"]
    assert "undeclared" in undeclared, undeclared
    assert "accounted for" in undeclared, undeclared


@pytest.mark.requires_gpu
def test_a_binding_the_shaders_do_not_declare_is_refused_at_the_draw(
    start_app_under_test,
):
    observed = run_probe(start_app_under_test, "GraphicsBindingRefusalProbe")

    unknown = observed["unknown_at_draw"]
    assert "tint_amount" in unknown, f"must name the unknown binding: {unknown}"
    assert SOURCE_BINDING in unknown, (
        f"must name what the shaders do declare: {unknown}"
    )


@pytest.mark.requires_gpu
def test_an_unsupplied_binding_is_refused_naming_the_shaders_bindings(
    start_app_under_test,
):
    """No implicit default and no carried-over value: the kernel holds no
    binding state between draws to fall back on."""
    observed = run_probe(start_app_under_test, "GraphicsBindingRefusalProbe")

    missing = observed["missing_at_draw"]
    assert SOURCE_BINDING in missing, f"must name the missing binding: {missing}"
    assert "not supplied" in missing, missing
    assert "do not persist between draws" in missing, (
        f"must say why there is no fallback: {missing}"
    )


@pytest.mark.requires_gpu
def test_a_binding_naming_an_unknown_surface_is_refused(start_app_under_test):
    observed = run_probe(start_app_under_test, "GraphicsBindingRefusalProbe")

    unresolvable = observed["unregistered_surface_at_draw"]
    assert "no-such-surface" in unresolvable, (
        f"must name the surface it could not resolve: {unresolvable}"
    )
    assert SOURCE_BINDING in unresolvable, (
        f"must name the binding that named it: {unresolvable}"
    )


@pytest.mark.requires_gpu
def test_a_refused_draw_leaves_the_kernel_drawable(start_app_under_test):
    """Every refusal above raises before anything is submitted, so none of them
    strands the kernel holding half a draw's bindings."""
    observed = run_probe(start_app_under_test, "GraphicsBindingRefusalProbe")
    assert observed["drew_after_the_refusals"] is True


@pytest.mark.requires_gpu
def test_a_buffer_kind_binding_is_refused_naming_the_kinds_a_draw_can_bind(
    start_app_under_test,
):
    """The only by-surface-id resolution the engine has is texture-shaped, so a
    uniform-buffer binding is refused rather than pointed at a texture."""
    observed = run_probe(start_app_under_test, "GraphicsBufferBindingRefusalProbe")

    refusal = observed["buffer_kind_binding"]
    assert observed["buffer_binding"] in refusal, (
        f"must name the binding it cannot resolve: {refusal}"
    )
    assert "uniform_buffer" in refusal, refusal
    assert "storage_image" in refusal and "sampled_texture" in refusal, (
        f"must name the kinds a draw can bind a surface for: {refusal}"
    )


@pytest.mark.requires_gpu
def test_a_draw_naming_anything_but_one_colour_target_is_refused(
    start_app_under_test,
):
    observed = run_probe(start_app_under_test, "GraphicsPassShapeRefusalProbe")

    for observation_key in ("two_color_targets", "no_color_target"):
        refusal = observed[observation_key]
        assert "exactly one" in refusal, f"{observation_key}: {refusal}"

    two_formats = observed["two_attachment_formats"]
    assert "attachment_color_formats" in two_formats, two_formats
    assert "exactly one" in two_formats, two_formats


@pytest.mark.requires_gpu
def test_a_draw_offers_no_argument_for_the_shapes_the_host_cannot_honour(
    start_app_under_test,
):
    """The signature test's runtime twin: passing one anyway is a `TypeError`
    naming the keyword, not a silently dropped argument."""
    observed = run_probe(start_app_under_test, "GraphicsPassShapeRefusalProbe")

    for keyword in ("vertex_buffers", "index_buffer", "depth_target"):
        refusal = observed[keyword]
        assert keyword in refusal, f"must name the keyword it refuses: {refusal}"
        assert "unexpected keyword argument" in refusal, refusal

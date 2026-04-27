# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Smoke test for the Python OpenGL adapter wrapper module.

Confirms the module imports, the GL_TEXTURE_2D constant matches the
Rust side and the GL spec, the Protocol shapes match what adapter
authors implement, and the PyOpenGL configuration helper is
idempotent.

A real subprocess Python smoke test (PyOpenGL fragment-shader
colorize via the adapter, PNG sample assertion) lives in the
end-to-end fixture under ``examples/`` once #515 (the rewritten
Glitch port) lands; this file exercises the Python module's
contract in isolation.
"""

from __future__ import annotations

import os

from streamlib.adapters import opengl as gl_adapter
from streamlib.surface_adapter import STREAMLIB_ADAPTER_ABI_VERSION


def test_module_re_exports_abi_version_constant():
    assert gl_adapter.STREAMLIB_ADAPTER_ABI_VERSION == STREAMLIB_ADAPTER_ABI_VERSION


def test_gl_texture_2d_matches_spec_value():
    # 0x0DE1 is the canonical GL spec value — same constant the
    # Rust crate exposes.
    assert gl_adapter.GL_TEXTURE_2D == 0x0DE1


def test_views_carry_texture_id_and_default_target():
    rv = gl_adapter.OpenGLReadView(gl_texture_id=42)
    wv = gl_adapter.OpenGLWriteView(gl_texture_id=77)
    assert rv.gl_texture_id == 42
    assert wv.gl_texture_id == 77
    assert rv.target == gl_adapter.GL_TEXTURE_2D
    assert wv.target == gl_adapter.GL_TEXTURE_2D


def test_protocols_describe_expected_method_set():
    # `runtime_checkable` Protocols only check method NAMES, not
    # signatures — that's exactly the structural fit we want for
    # subprocess-side adapter implementations.
    assert hasattr(gl_adapter.OpenGLSurfaceAdapter, "acquire_read")
    assert hasattr(gl_adapter.OpenGLSurfaceAdapter, "acquire_write")
    assert hasattr(gl_adapter.OpenGLSurfaceAdapter, "try_acquire_read")
    assert hasattr(gl_adapter.OpenGLSurfaceAdapter, "try_acquire_write")
    assert hasattr(gl_adapter.OpenGLContext, "acquire_write")


def test_pyopengl_config_helper_is_idempotent_and_sets_expected_vars():
    # Save + restore — the test process inherits the parent env,
    # which may already have these set.
    saved = {
        k: os.environ.get(k)
        for k in (
            "PYOPENGL_PLATFORM",
            "PYOPENGL_CONTEXT_CHECKING",
            "PYOPENGL_ERROR_CHECKING",
        )
    }
    try:
        for k in saved:
            os.environ.pop(k, None)
        gl_adapter.configure_pyopengl_for_streamlib_subprocess()
        assert os.environ["PYOPENGL_PLATFORM"] == "egl"
        assert os.environ["PYOPENGL_CONTEXT_CHECKING"] == "False"
        assert os.environ["PYOPENGL_ERROR_CHECKING"] == "False"
        # Idempotent — second call preserves prior values.
        os.environ["PYOPENGL_PLATFORM"] = "user_override"
        gl_adapter.configure_pyopengl_for_streamlib_subprocess()
        assert os.environ["PYOPENGL_PLATFORM"] == "user_override"
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

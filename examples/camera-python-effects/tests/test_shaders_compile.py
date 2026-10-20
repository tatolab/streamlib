# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Every shader in this stream compiles to SPIR-V.

Authoring a kernel needs no shader toolchain — the runtime compiles GLSL —
so this is not how the stream builds its kernels. It is how a typo in a shader
fails here, in a second, instead of at `setup()` on a rig with a camera plugged
into it. Skipped where no compiler is installed, which is most machines.
"""

from __future__ import annotations

import shutil
import subprocess

import pytest

from camera_python_effects.gpu_surface_conventions import read_shader_source

GLSL_COMPILER = shutil.which("glslangValidator")


def glsl_compiler_on_path() -> str:
    """The compiler a test the `skipif` let through runs."""
    assert GLSL_COMPILER is not None
    return GLSL_COMPILER


EVERY_SHADER = [
    pytest.param("fullscreen_triangle.vert", "vert", id="fullscreen_triangle"),
    pytest.param("cyberpunk_glitch.frag", "frag", id="cyberpunk_glitch"),
    pytest.param("crt_film_grain.frag", "frag", id="crt_film_grain"),
    pytest.param("breaking_news_composite.frag", "frag", id="breaking_news_composite"),
]


@pytest.mark.skipif(GLSL_COMPILER is None, reason="no glslangValidator on PATH")
@pytest.mark.parametrize(("shader_file_name", "stage"), EVERY_SHADER)
def test_the_shader_compiles_for_vulkan(shader_file_name: str, stage: str) -> None:
    compilation = subprocess.run(
        [glsl_compiler_on_path(), "-V", "--stdin", "-S", stage, "-o", "/dev/null"],
        input=read_shader_source(shader_file_name),
        capture_output=True,
        text=True,
        check=False,
    )
    assert compilation.returncode == 0, (
        f"{shader_file_name} does not compile:\n{compilation.stdout}{compilation.stderr}"
    )


@pytest.mark.skipif(GLSL_COMPILER is None, reason="no glslangValidator on PATH")
def test_the_compiler_check_is_not_vacuous() -> None:
    """A compiler that accepted anything would pass every test above."""
    compilation = subprocess.run(
        [glsl_compiler_on_path(), "-V", "--stdin", "-S", "frag", "-o", "/dev/null"],
        input="#version 450\nvoid main() { this is not glsl }\n",
        capture_output=True,
        text=True,
        check=False,
    )
    assert compilation.returncode != 0

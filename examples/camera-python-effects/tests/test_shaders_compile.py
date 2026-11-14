# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Every shader in this stream compiles to SPIR-V.

Authoring a kernel needs no shader toolchain — the runtime compiles Slang —
so this is not how the stream builds its kernels. It is how a typo in a shader
fails here, in a second, instead of at `setup()` on a rig with a camera plugged
into it. Skipped where no compiler is installed, which is most machines.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from camera_python_effects.gpu_surface_conventions import read_shader_source

SLANG_COMPILER = shutil.which("slangc")


def slang_compiler_on_path() -> str:
    """The compiler a test the `skipif` let through runs."""
    assert SLANG_COMPILER is not None
    return SLANG_COMPILER


def compile_slang_to_spirv(source: str, stage: str, scratch_directory: Path) -> subprocess.CompletedProcess[str]:
    """Run slangc over `source` for `stage`, its entry point `main`."""
    source_path = scratch_directory / "shader.slang"
    source_path.write_text(source, encoding="utf-8")
    return subprocess.run(
        [
            slang_compiler_on_path(),
            str(source_path),
            "-target",
            "spirv",
            "-stage",
            stage,
            "-entry",
            "main",
            "-o",
            str(scratch_directory / "shader.spv"),
        ],
        capture_output=True,
        text=True,
        check=False,
    )


EVERY_SHADER = [
    pytest.param("fullscreen_triangle_vertex.slang", "vertex", id="fullscreen_triangle"),
    pytest.param("cyberpunk_glitch_fragment.slang", "fragment", id="cyberpunk_glitch"),
    pytest.param("crt_film_grain_fragment.slang", "fragment", id="crt_film_grain"),
    pytest.param(
        "breaking_news_composite_fragment.slang", "fragment", id="breaking_news_composite"
    ),
]


@pytest.mark.skipif(SLANG_COMPILER is None, reason="no slangc on PATH")
@pytest.mark.parametrize(("shader_file_name", "stage"), EVERY_SHADER)
def test_the_shader_compiles_for_vulkan(
    shader_file_name: str, stage: str, tmp_path: Path
) -> None:
    compilation = compile_slang_to_spirv(read_shader_source(shader_file_name), stage, tmp_path)
    assert compilation.returncode == 0, (
        f"{shader_file_name} does not compile:\n{compilation.stdout}{compilation.stderr}"
    )


@pytest.mark.skipif(SLANG_COMPILER is None, reason="no slangc on PATH")
def test_the_compiler_check_is_not_vacuous(tmp_path: Path) -> None:
    """A compiler that accepted anything would pass every test above."""
    compilation = compile_slang_to_spirv(
        "float4 main() : SV_Target0 { this is not slang }\n", "fragment", tmp_path
    )
    assert compilation.returncode != 0

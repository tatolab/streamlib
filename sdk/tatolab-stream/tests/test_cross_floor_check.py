# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cross-floor check: each finding kind over a fixture source, the warning
block it renders, and the gate over `tatolab.stream`'s own Python.

The launch that prints the block, and the gate over the runtime's Python and
the scaffold, are tested beside the runtime.
"""

import os
import sys
import textwrap
import warnings
from pathlib import Path

import pytest

from tatolab.stream import _cross_floor_check
from tatolab.stream._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    find_floor_bindings_in_project_manifest,
    find_floor_bindings_in_python_source,
    render_cross_floor_warning_block,
)

STREAM_PACKAGE_DIRECTORY = Path(_cross_floor_check.__file__).resolve().parent

FIXTURE_FILE = Path("nodes/effect.py")
FIXTURE_MANIFEST = Path("pyproject.toml")
FLOOR_CLEAN_STREAM_SOURCE = (
    "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def main(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
)

requires_tomllib = pytest.mark.skipif(
    sys.version_info < (3, 11), reason="the dependency rule reads pyproject.toml with tomllib"
)


def findings_in(source: str) -> "list[tuple[int, str, str]]":
    return [
        (finding.line, finding.what_binds_it, finding.portable_spelling)
        for finding in find_floor_bindings_in_python_source(textwrap.dedent(source), FIXTURE_FILE)
    ]


def manifest_findings_in(manifest: str) -> "list[tuple[int, str, str]]":
    return [
        (finding.line, finding.what_binds_it, finding.portable_spelling)
        for finding in find_floor_bindings_in_project_manifest(
            textwrap.dedent(manifest), FIXTURE_MANIFEST
        )
    ]


# ---------------------------------------------------------------------------
# Floor-bound imports
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "import_statement, named_module",
    [
        ("import cupy", "cupy"),
        ("import cupy.cuda.runtime", "cupy.cuda.runtime"),
        ("from cupy import asnumpy", "cupy"),
        ("import pycuda.driver as cuda_driver", "pycuda.driver"),
        ("import numba.cuda", "numba.cuda"),
        ("from numba import cuda", "numba.cuda"),
        ("import torch.cuda", "torch.cuda"),
        ("from torch.cuda import synchronize", "torch.cuda"),
        ("import mlx.core as mx", "mlx.core"),
        ("from mlx import core", "mlx"),
    ],
)
def test_a_floor_bound_import_is_named_with_its_line(import_statement: str, named_module: str):
    findings = findings_in(f"import numpy\n{import_statement}\n")

    assert len(findings) == 1, findings
    line, what_binds_it, portable_spelling = findings[0]
    assert line == 2
    assert f"`{named_module}`" in what_binds_it
    assert portable_spelling


def test_a_cuda_import_names_the_engine_copy_and_torch_as_the_portable_spelling():
    [(_, what_binds_it, portable_spelling)] = findings_in("import cupy\n")

    assert "CUDA" in what_binds_it
    assert "copy_surface_to_surface" in portable_spelling
    assert "torch.from_dlpack(frame)" in portable_spelling


def test_torch_cuda_reached_as_an_attribute_names_torch_accelerator():
    [(line, what_binds_it, portable_spelling)] = findings_in(
        """\
        import torch
        torch.cuda.synchronize()
        """
    )

    assert line == 2
    assert "`torch.cuda`" in what_binds_it
    assert "torch.accelerator" in portable_spelling


def test_the_portable_torch_path_is_not_flagged():
    assert findings_in(
        """\
        import numba
        import torch

        tensor = torch.from_dlpack(frame)
        result = torch.zeros(4, device=tensor.device)
        accelerator = torch.accelerator.current_accelerator()
        moved = result.to(accelerator)
        """
    ) == []


def test_an_import_under_a_sys_platform_guard_is_not_flagged():
    assert findings_in(
        """\
        import sys

        if sys.platform == "darwin":
            import mlx.core as mx
        else:
            import cupy
        """
    ) == []


def test_the_same_import_outside_the_guard_is_flagged():
    findings = findings_in(
        """\
        import sys

        if sys.platform == "darwin":
            pass
        import mlx.core as mx
        """
    )

    assert [line for line, _, _ in findings] == [5]


@pytest.mark.parametrize(
    "guard",
    [
        'sys.platform == "darwin" and enabled',
        'sys.platform.startswith("darwin")',
        'not sys.platform == "linux"',
        'sys.platform == "darwin" or sys.platform == "ios"',
    ],
)
def test_a_condition_gated_on_the_platform_on_every_path_is_a_guard(guard: str):
    assert findings_in(f"import sys\nif {guard}:\n    import mlx\n") == []


@pytest.mark.parametrize(
    "not_a_guard",
    [
        'sys.platform == "linux" or enabled',
        'enabled or sys.platform == "linux"',
        'print(sys.platform)',
    ],
)
def test_a_condition_that_can_be_true_off_the_platform_is_no_guard(not_a_guard: str):
    assert [line for line, _, _ in findings_in(f"import sys\nif {not_a_guard}:\n    import cupy\n")] == [3]


# ---------------------------------------------------------------------------
# Device literals
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "device_spelling, named_device",
    [
        ('torch.device("cuda")', "cuda"),
        ('torch.device("cuda:1")', "cuda:1"),
        ('torch.device("mps")', "mps"),
        ('torch.zeros(4, device="cuda")', "cuda"),
        ('model(frame, device="mps")', "mps"),
        ('tensor.to("cuda")', "cuda"),
        ('tensor.to("mps", non_blocking=True)', "mps"),
        ('torch.device("cuda" if use_gpu else "cpu")', "cuda"),
    ],
)
def test_a_device_passed_as_a_literal_is_named(device_spelling: str, named_device: str):
    [(line, what_binds_it, portable_spelling)] = findings_in(f"import torch\nx = {device_spelling}\n")

    assert line == 2
    assert repr(named_device) in what_binds_it
    assert "tensor.device" in portable_spelling
    assert "torch.accelerator" in portable_spelling


def test_the_same_word_that_is_not_a_device_is_not_flagged():
    assert findings_in(
        """\
        label = "cuda"
        print("mps")
        backend = {"device": "cuda"}
        path.to_bytes("cuda")
        """
    ) == []


def test_a_cuda_method_call_is_named():
    [(line, what_binds_it, portable_spelling)] = findings_in("tensor = tensor.cuda()\n")

    assert line == 1
    assert "`.cuda()`" in what_binds_it
    assert ".to(" in portable_spelling


# ---------------------------------------------------------------------------
# The stub's single-floor names
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "use, name, peer",
    [
        ("stream_builder.add(VirtualCameraSink)", "VirtualCameraSink", None),
        ("stream_builder.add(tatolab.stream.VirtualCameraSink)", "VirtualCameraSink", None),
        ("ctx.gpu_full_access.create_ray_tracing_kernel(stages, groups)", "create_ray_tracing_kernel", None),
        ("ctx.gpu_full_access.build_triangles_blas(vertices, indices)", "build_triangles_blas", None),
        ("ctx.gpu_full_access.build_tlas(instances)", "build_tlas", None),
        ("ctx.gpu_full_access.export_dma_buf(surface)", "export_dma_buf", "export_iosurface"),
        ("ctx.gpu_full_access.export_opaque_fd(surface)", "export_opaque_fd", "export_iosurface"),
        ("ctx.gpu_full_access.import_dma_buf(fd, 64, 64)", "import_dma_buf", None),
        ("frame.__cuda_array_interface__", "__cuda_array_interface__", "__dlpack__"),
    ],
)
def test_a_single_floor_stub_name_is_named_as_allowed_with_its_peer(
    use: str, name: str, peer: "str | None"
):
    [(line, what_binds_it, portable_spelling)] = findings_in(f"x = 1\n{use}\n")

    assert line == 2
    assert f"`{name}`" in what_binds_it
    assert "single-floor" in what_binds_it and "allowed" in what_binds_it
    if peer is None:
        assert "no peer" in portable_spelling
    else:
        assert f"`{peer}`" in portable_spelling


def test_defining_the_cuda_array_interface_is_named():
    findings = findings_in(
        """\
        class Exporter:
            @property
            def __cuda_array_interface__(self):
                return {}
        """
    )

    assert [line for line, _, _ in findings] == [3]


def test_a_protocol_declaring_a_single_floor_member_is_not_a_use_of_it():
    findings = findings_in(
        """\
        from typing import Protocol

        class GpuContextFullAccess(Protocol):
            def export_dma_buf(self, surface: object) -> tuple[int, int]: ...

        class Exporter:
            def export_dma_buf(self, surface: object) -> tuple[int, int]:
                return (0, 0)
        """
    )

    assert [line for line, _, _ in findings] == [7]


def test_importing_a_single_floor_name_is_not_a_use_of_it():
    assert findings_in(
        """\
        from tatolab.stream import VirtualCameraSink
        __all__ = ["VirtualCameraSink"]
        """
    ) == []


# ---------------------------------------------------------------------------
# Dependencies
# ---------------------------------------------------------------------------


@requires_tomllib
def test_a_floor_bound_dependency_without_a_marker_is_named_with_its_line():
    findings = manifest_findings_in(
        """\
        [project]
        name = "demo"
        dependencies = [
            "streamlib",
            "cupy-cuda13x>=14.2",
            "numpy>=2.1",
        ]

        [project.optional-dependencies]
        apple = ["mlx>=0.32"]

        [dependency-groups]
        dev = ["mlx-lm"]
        """
    )

    assert [(line, what_binds_it.split("`")[1]) for line, what_binds_it, _ in findings] == [
        (5, "cupy-cuda13x"),
        (10, "mlx"),
        (13, "mlx-lm"),
    ]
    assert 'cupy-cuda13x>=14.2; sys_platform == "linux"' in findings[0][2]
    assert 'sys_platform == "darwin"' in findings[1][2]


@requires_tomllib
def test_a_dependency_is_placed_on_its_own_line_in_its_own_array():
    findings = manifest_findings_in(
        """\
        # mlx is the macOS array library
        [project]
        name = "mlx"
        dependencies = [
            "mlx-lm",
            "mlx",
        ]

        [project.optional-dependencies]
        extra = ["numpy"]
        apple = [
            "mlx",  # "mlx"
        ]
        """
    )

    assert [line for line, _, _ in findings] == [5, 6, 12]


@requires_tomllib
def test_a_marked_floor_bound_dependency_is_not_flagged():
    assert manifest_findings_in(
        """\
        [project]
        name = "demo"
        dependencies = [
            'cupy-cuda13x; sys_platform == "linux"',
            "mlx>=0.32; platform_system == 'Darwin'",
            "numpy>=2.1",
        ]
        """
    ) == []


def test_on_a_python_without_tomllib_the_dependency_rule_is_skipped_by_name(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(_cross_floor_check, "tomllib", None)
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\ndependencies = ["cupy-cuda13x"]\n'
    )
    (tmp_path / "effect.py").write_text("import cupy\n")

    report = check_app_directory_for_floor_bindings(tmp_path)
    block = render_cross_floor_warning_block(report, tmp_path)

    assert [finding.file.name for finding in report.findings] == ["effect.py"]
    assert report.skipped_rule_reason is not None
    assert "dependency rule was skipped" in block
    assert "tomllib" in block


# ---------------------------------------------------------------------------
# What the check reads
# ---------------------------------------------------------------------------


def test_the_check_reads_every_layout_and_skips_virtual_environments(tmp_path: Path):
    (tmp_path / "stream.py").write_text(FLOOR_CLEAN_STREAM_SOURCE)
    for node_directory in (
        tmp_path / "nodes",
        tmp_path / "src" / "demo" / "nodes",
    ):
        node_directory.mkdir(parents=True)
        (node_directory / "effect.py").write_text("import cupy\n")
    (tmp_path / ".cache").mkdir()
    (tmp_path / ".cache" / "effect.py").write_text("import cupy\n")
    for virtual_environment in (tmp_path / ".venv", tmp_path / "venv", tmp_path / "env"):
        site_packages = virtual_environment / "lib" / "site-packages" / "cupy"
        site_packages.mkdir(parents=True)
        (site_packages / "__init__.py").write_text("import cupy\n")
    (tmp_path / "env" / "pyvenv.cfg").write_text("home = /usr/bin\n")

    report = check_app_directory_for_floor_bindings(tmp_path)

    assert sorted(
        finding.file.relative_to(tmp_path).as_posix() for finding in report.findings
    ) == ["nodes/effect.py", "src/demo/nodes/effect.py"]


@pytest.mark.skipif(os.geteuid() == 0, reason="root reads through any permission")
def test_an_unreadable_directory_is_passed_over(tmp_path: Path):
    unreadable = tmp_path / "unreadable"
    unreadable.mkdir()
    (unreadable / "effect.py").write_text("import cupy\n")
    (tmp_path / "effect.py").write_text("import cupy\n")
    unreadable.chmod(0)
    try:
        report = check_app_directory_for_floor_bindings(tmp_path)
    finally:
        unreadable.chmod(0o755)

    assert [finding.file for finding in report.findings] == [tmp_path / "effect.py"]


def test_a_file_that_does_not_parse_is_passed_over(tmp_path: Path):
    (tmp_path / "broken.py").write_text("def process(self ctx:\n")
    (tmp_path / "effect.py").write_text("import cupy\n")

    report = check_app_directory_for_floor_bindings(tmp_path)

    assert [finding.file.name for finding in report.findings] == ["effect.py"]


def test_the_block_names_file_line_and_portable_spelling(tmp_path: Path):
    (tmp_path / "nodes").mkdir()
    (tmp_path / "nodes" / "effect.py").write_text(
        'import cupy\nimport torch\ndevice = torch.device("cuda")\n'
    )

    block = render_cross_floor_warning_block(
        check_app_directory_for_floor_bindings(tmp_path), tmp_path
    )

    assert "cross-floor check found 2 things" in block
    assert "starts anyway" in block
    assert "nodes/effect.py:1: imports `cupy`" in block
    assert "nodes/effect.py:3: names the device 'cuda'" in block
    assert block.count("portable: ") == 2


def test_a_parse_adds_no_warning_of_its_own(tmp_path: Path):
    (tmp_path / "helper.py").write_text('pattern = "\\d"\n')

    with warnings.catch_warnings():
        warnings.simplefilter("error")
        report = check_app_directory_for_floor_bindings(tmp_path)

    assert report.findings == []


def test_a_clean_app_renders_nothing(tmp_path: Path):
    (tmp_path / "stream.py").write_text(FLOOR_CLEAN_STREAM_SOURCE)

    assert render_cross_floor_warning_block(
        check_app_directory_for_floor_bindings(tmp_path), tmp_path
    ) == ""


# ---------------------------------------------------------------------------
# The gate over what the package ships
# ---------------------------------------------------------------------------


def test_the_stream_packages_own_python_binds_to_no_floor():
    assert (STREAM_PACKAGE_DIRECTORY / "__init__.py").is_file(), (
        f"the gate must read the package's own Python, not {STREAM_PACKAGE_DIRECTORY}"
    )
    report = check_app_directory_for_floor_bindings(STREAM_PACKAGE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, STREAM_PACKAGE_DIRECTORY
    )

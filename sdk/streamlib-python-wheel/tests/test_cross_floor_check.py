# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cross-floor check: each finding kind over a fixture source, the gate over
what the project ships, and the warning block `run` and `dev` print.

Nothing here boots an engine; the launch that goes on to a live node is in
`test_cli_launch.py`.
"""

import sys
import textwrap
from pathlib import Path

import pytest

from streamlib import _cross_floor_check, cli
from streamlib._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    find_floor_bindings_in_project_manifest,
    find_floor_bindings_in_python_source,
    render_cross_floor_warning_block,
)

WHEEL_PYTHON_PACKAGE_DIRECTORY = Path(__file__).resolve().parents[1] / "python" / "streamlib"
FIXTURE_FILE = Path("processors/effect.py")
FIXTURE_MANIFEST = Path("pyproject.toml")

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
        ("rt.add(VirtualCameraSink)", "VirtualCameraSink", None),
        ("rt.add(streamlib.VirtualCameraSink)", "VirtualCameraSink", None),
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


def test_importing_a_single_floor_name_is_not_a_use_of_it():
    assert findings_in(
        """\
        from streamlib import VirtualCameraSink
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
    (tmp_path / "app.py").write_text("import cupy\n")

    report = check_app_directory_for_floor_bindings(tmp_path)
    block = render_cross_floor_warning_block(report, tmp_path)

    assert [finding.file.name for finding in report.findings] == ["app.py"]
    assert report.skipped_rule_reason is not None
    assert "dependency rule was skipped" in block
    assert "tomllib" in block


# ---------------------------------------------------------------------------
# What the check reads
# ---------------------------------------------------------------------------


def test_the_check_reads_every_layout_and_skips_virtual_environments(tmp_path: Path):
    (tmp_path / "app.py").write_text("def setup(rt):\n    pass\n")
    for processor_directory in (
        tmp_path / "processors",
        tmp_path / "src" / "demo" / "processors",
    ):
        processor_directory.mkdir(parents=True)
        (processor_directory / "effect.py").write_text("import cupy\n")
    for virtual_environment in (tmp_path / ".venv", tmp_path / "venv", tmp_path / "env"):
        site_packages = virtual_environment / "lib" / "site-packages" / "cupy"
        site_packages.mkdir(parents=True)
        (site_packages / "__init__.py").write_text("import cupy\n")
    (tmp_path / "env" / "pyvenv.cfg").write_text("home = /usr/bin\n")

    report = check_app_directory_for_floor_bindings(tmp_path)

    assert sorted(
        finding.file.relative_to(tmp_path).as_posix() for finding in report.findings
    ) == ["processors/effect.py", "src/demo/processors/effect.py"]


def test_a_file_that_does_not_parse_is_passed_over(tmp_path: Path):
    (tmp_path / "broken.py").write_text("def process(self ctx:\n")
    (tmp_path / "effect.py").write_text("import cupy\n")

    report = check_app_directory_for_floor_bindings(tmp_path)

    assert [finding.file.name for finding in report.findings] == ["effect.py"]


def test_the_block_names_file_line_and_portable_spelling(tmp_path: Path):
    (tmp_path / "processors").mkdir()
    (tmp_path / "processors" / "effect.py").write_text(
        'import cupy\nimport torch\ndevice = torch.device("cuda")\n'
    )

    block = render_cross_floor_warning_block(
        check_app_directory_for_floor_bindings(tmp_path), tmp_path
    )

    assert "cross-floor check found 2 things" in block
    assert "starts anyway" in block
    assert "processors/effect.py:1: imports `cupy`" in block
    assert "processors/effect.py:3: names the device 'cuda'" in block
    assert block.count("portable: ") == 2


def test_a_clean_app_renders_nothing(tmp_path: Path):
    (tmp_path / "app.py").write_text("def setup(rt):\n    pass\n")

    assert render_cross_floor_warning_block(
        check_app_directory_for_floor_bindings(tmp_path), tmp_path
    ) == ""


# ---------------------------------------------------------------------------
# The gate over what the project ships
# ---------------------------------------------------------------------------


def test_the_wheels_own_python_binds_to_no_floor():
    report = check_app_directory_for_floor_bindings(WHEEL_PYTHON_PACKAGE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, WHEEL_PYTHON_PACKAGE_DIRECTORY
    )


@requires_tomllib
@pytest.mark.parametrize("use_test_pattern_source", [False, True])
def test_the_scaffold_binds_to_no_floor(tmp_path: Path, use_test_pattern_source: bool):
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=use_test_pattern_source)

    report = check_app_directory_for_floor_bindings(app_directory)

    assert (report.findings, report.skipped_rule_reason) == ([], None), (
        render_cross_floor_warning_block(report, app_directory)
    )


# ---------------------------------------------------------------------------
# At launch
# ---------------------------------------------------------------------------


def launch_until_the_entry_stops_it(app_directory: Path) -> int:
    return cli.launch_app_node(
        "dev",
        requested_anchor_directory=app_directory,
        requested_entry_file=None,
        bind_host=cli.DEFAULT_CONTROL_PLANE_BIND_HOST,
        bind_port=cli.DEFAULT_CONTROL_PLANE_BIND_PORT,
        runtime_name=None,
        mesh_name=None,
        mesh_peer_endpoints=None,
        mesh_listen_endpoints=None,
        mesh_multicast_discovery=None,
    )


@pytest.fixture
def restore_the_launchers_import_path():
    launcher_import_path = list(sys.path)
    try:
        yield
    finally:
        sys.path[:] = launcher_import_path


@pytest.mark.usefixtures("restore_the_launchers_import_path")
def test_the_launch_prints_the_block_before_the_app_runs_and_still_runs_it(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    ran = tmp_path / "entry-ran.txt"
    (tmp_path / "processors").mkdir()
    (tmp_path / "processors" / "effect.py").write_text(
        'import cupy\nimport torch\ndevice = torch.device("cuda")\n'
    )
    (tmp_path / "app.py").write_text(
        f"open({str(ran)!r}, 'w').write('ran')\n"
        "print('the entry file ran')\n"
        "raise RuntimeError('stop before the engine')\n"
    )

    exit_code = launch_until_the_entry_stops_it(tmp_path)

    stdout = capsys.readouterr().out
    assert exit_code == 1, "the entry file stopped the launch on purpose"
    assert ran.read_text() == "ran", "a finding must never keep the app from running"
    assert "processors/effect.py:1: imports `cupy`" in stdout
    assert "processors/effect.py:3: names the device 'cuda'" in stdout
    assert stdout.index("cross-floor check") < stdout.index("the entry file ran"), (
        "the block is printed between resolving the entry file and executing it"
    )


@pytest.mark.usefixtures("restore_the_launchers_import_path")
def test_a_clean_launch_prints_nothing_extra(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    (tmp_path / "app.py").write_text("raise RuntimeError('stop before the engine')\n")

    launch_until_the_entry_stops_it(tmp_path)

    assert capsys.readouterr().out == ""

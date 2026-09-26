# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cross-floor check: what binds an app's Python to one floor.

Reads source, never behaviour. It names a floor-bound import, a device spelled
as a literal, a single-floor name from the stub, and a floor-bound dependency
with no platform marker — each with the file, the line, and the portable
spelling. A dynamic import, or a dependency's own device choice, is out of its
reach by design. It is not the portability gate, which is about native linkage.
"""

from __future__ import annotations

import ast
import os
import re
import sys
import warnings
from pathlib import Path
from typing import NamedTuple, Optional

if sys.version_info >= (3, 11):
    import tomllib
else:
    tomllib = None

__all__ = [
    "CrossFloorFinding",
    "CrossFloorCheckReport",
    "find_floor_bindings_in_python_source",
    "find_floor_bindings_in_project_manifest",
    "check_app_directory_for_floor_bindings",
    "render_cross_floor_warning_block",
]

PROJECT_MANIFEST_FILE_NAME = "pyproject.toml"
VIRTUAL_ENVIRONMENT_DIRECTORY_NAMES = frozenset({".venv", "venv"})
VIRTUAL_ENVIRONMENT_MARKER_FILE_NAME = "pyvenv.cfg"

DEPENDENCY_RULE_SKIPPED_WITHOUT_TOMLLIB = (
    "the dependency rule was skipped: this Python has no `tomllib` (it arrived in 3.11), "
    "so `pyproject.toml` was not read"
)

PORTABLE_GPU_COPY_OR_TORCH = (
    "land a frame in a texture with `ctx.gpu_limited_access.copy_surface_to_surface(...)`, "
    "or do GPU math with `torch.from_dlpack(frame)`"
)
PORTABLE_DEVICE_FROM_TENSOR_OR_ACCELERATOR = (
    "take the device from the tensor (`tensor.device`) or from "
    "`torch.accelerator.current_accelerator()`"
)

# Module name -> (the floor it binds to, its portable spelling). A dotted
# entry also matches its submodules and `from <parent> import <leaf>`.
FLOOR_BOUND_MODULES: "dict[str, tuple[str, str]]" = {
    "cupy": ("CUDA, so Linux only", PORTABLE_GPU_COPY_OR_TORCH),
    "pycuda": ("CUDA, so Linux only", PORTABLE_GPU_COPY_OR_TORCH),
    "numba.cuda": ("CUDA, so Linux only", PORTABLE_GPU_COPY_OR_TORCH),
    "torch.cuda": ("CUDA, so Linux only", PORTABLE_DEVICE_FROM_TENSOR_OR_ACCELERATOR),
    "mlx": (
        "Metal, so macOS only",
        "do GPU math with `torch.from_dlpack(frame)`, or guard the import with "
        '`if sys.platform == "darwin":`',
    ),
}

# The stub's single-floor names -> (why, the other floor's peer if one exists).
# Allowed on their floor; the finding exists so the choice is a known one.
SINGLE_FLOOR_STUB_NAMES: "dict[str, tuple[str, Optional[str]]]" = {
    "VirtualCameraSink": ("Linux only: v4l2loopback and PipeWire", None),
    "create_ray_tracing_kernel": ("Linux only: MoltenVK has no ray tracing", None),
    "build_triangles_blas": ("Linux only: MoltenVK has no ray tracing", None),
    "build_tlas": ("Linux only: MoltenVK has no ray tracing", None),
    "export_dma_buf": ("Linux only: a DMA-BUF is a Linux fd", "`export_iosurface`"),
    "export_opaque_fd": ("Linux only: an OPAQUE_FD is a Linux fd", "`export_iosurface`"),
    "import_dma_buf": ("Linux only: a DMA-BUF is a Linux fd", None),
    "__cuda_array_interface__": (
        "CUDA only",
        "`__dlpack__`, read with `torch.from_dlpack(frame)`",
    ),
}

# The dotted entries above, which code reaches as `torch.cuda.…` without importing them.
FLOOR_BOUND_SUBMODULES = frozenset(name for name in FLOOR_BOUND_MODULES if "." in name)

DEVICE_LITERAL_NAMES = ("cuda", "mps")

# Distribution-name prefixes (PEP 503 normalized) whose wheels exist on one floor.
FLOOR_BOUND_DISTRIBUTIONS: "dict[str, tuple[str, str]]" = {
    "cupy": ("CUDA, so Linux only", "linux"),
    "mlx": ("Metal, so macOS only", "darwin"),
}
PLATFORM_MARKER_VARIABLES = ("sys_platform", "platform_system")
REQUIREMENT_DISTRIBUTION_NAME = re.compile(r"^\s*([A-Za-z0-9][A-Za-z0-9._-]*)")


class CrossFloorFinding(NamedTuple):
    """One thing that binds an app to one floor, where it is, and the portable spelling."""

    file: Path
    line: int
    what_binds_it: str
    portable_spelling: str


class CrossFloorCheckReport(NamedTuple):
    """Every finding over an app directory, and the reason a rule was skipped, if one was."""

    findings: "list[CrossFloorFinding]"
    skipped_rule_reason: Optional[str]


def _is_sys_platform(node: ast.expr) -> bool:
    return (
        isinstance(node, ast.Attribute)
        and node.attr == "platform"
        and isinstance(node.value, ast.Name)
        and node.value.id == "sys"
    )


def _is_platform_guard(condition: ast.expr) -> bool:
    """True when every path through `condition` that can be true depends on `sys.platform`."""
    if isinstance(condition, ast.Compare):
        return any(_is_sys_platform(side) for side in (condition.left, *condition.comparators))
    if isinstance(condition, ast.Call):
        return isinstance(condition.func, ast.Attribute) and _is_sys_platform(
            condition.func.value
        )
    if isinstance(condition, ast.UnaryOp) and isinstance(condition.op, ast.Not):
        return _is_platform_guard(condition.operand)
    if isinstance(condition, ast.BoolOp):
        operand_guards = [_is_platform_guard(operand) for operand in condition.values]
        return any(operand_guards) if isinstance(condition.op, ast.And) else all(operand_guards)
    return False


def _device_named_by_literal(node: ast.expr) -> Optional[str]:
    if isinstance(node, ast.IfExp) and not _is_platform_guard(node.test):
        return _device_named_by_literal(node.body) or _device_named_by_literal(node.orelse)
    if not isinstance(node, ast.Constant) or not isinstance(node.value, str):
        return None
    for device_name in DEVICE_LITERAL_NAMES:
        if node.value == device_name or node.value.startswith(f"{device_name}:"):
            return node.value
    return None


def _floor_bound_module_matching(module_name: str) -> Optional[str]:
    for floor_bound_module in FLOOR_BOUND_MODULES:
        if module_name == floor_bound_module or module_name.startswith(f"{floor_bound_module}."):
            return floor_bound_module
    return None


class _FloorBindingSourceVisitor(ast.NodeVisitor):
    def __init__(self, file: Path) -> None:
        self.file = file
        self.findings: "list[CrossFloorFinding]" = []
        self.platform_guard_depth = 0

    def _record(self, node: ast.AST, what_binds_it: str, portable_spelling: str) -> None:
        if self.platform_guard_depth:
            return
        self.findings.append(
            CrossFloorFinding(self.file, getattr(node, "lineno", 1), what_binds_it, portable_spelling)
        )

    def _record_floor_bound_module(
        self, node: ast.AST, module_name: str, how_it_is_reached: str = "imports"
    ) -> None:
        floor_bound_module = _floor_bound_module_matching(module_name)
        if floor_bound_module is None:
            return
        floor, portable_spelling = FLOOR_BOUND_MODULES[floor_bound_module]
        self._record(
            node, f"{how_it_is_reached} `{module_name}`, which is {floor}", portable_spelling
        )

    def _record_single_floor_name(self, node: ast.AST, name: str) -> None:
        why, peer = SINGLE_FLOOR_STUB_NAMES[name]
        self._record(
            node,
            f"uses `{name}`, which is single-floor ({why}); allowed on that floor",
            f"the other floor's peer is {peer}" if peer else "the other floor has no peer",
        )

    def visit_If(self, node: ast.If) -> None:
        self.visit(node.test)
        guarded = _is_platform_guard(node.test)
        self.platform_guard_depth += guarded
        for statement in (*node.body, *node.orelse):
            self.visit(statement)
        self.platform_guard_depth -= guarded

    def visit_Import(self, node: ast.Import) -> None:
        for alias in node.names:
            self._record_floor_bound_module(node, alias.name)

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        if node.level or node.module is None:
            return
        if _floor_bound_module_matching(node.module):
            self._record_floor_bound_module(node, node.module)
            return
        for alias in node.names:
            self._record_floor_bound_module(node, f"{node.module}.{alias.name}")

    def visit_Attribute(self, node: ast.Attribute) -> None:
        if isinstance(node.value, ast.Name):
            dotted_name = f"{node.value.id}.{node.attr}"
            if dotted_name in FLOOR_BOUND_SUBMODULES:
                self._record_floor_bound_module(node, dotted_name, how_it_is_reached="uses")
        if node.attr in SINGLE_FLOOR_STUB_NAMES:
            self._record_single_floor_name(node, node.attr)
        self.generic_visit(node)

    def visit_Name(self, node: ast.Name) -> None:
        if isinstance(node.ctx, ast.Load) and node.id in SINGLE_FLOOR_STUB_NAMES:
            self._record_single_floor_name(node, node.id)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        if node.name in SINGLE_FLOOR_STUB_NAMES:
            self._record_single_floor_name(node, node.name)
        self.generic_visit(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        if node.name in SINGLE_FLOOR_STUB_NAMES:
            self._record_single_floor_name(node, node.name)
        self.generic_visit(node)

    def visit_Call(self, node: ast.Call) -> None:
        function = node.func
        function_name = (
            function.attr
            if isinstance(function, ast.Attribute)
            else function.id
            if isinstance(function, ast.Name)
            else None
        )
        device_arguments: "list[ast.expr]" = [
            keyword.value for keyword in node.keywords if keyword.arg == "device"
        ]
        if function_name == "device" and node.args:
            device_arguments.append(node.args[0])
        elif function_name == "to" and isinstance(function, ast.Attribute):
            device_arguments.extend(node.args)
        for argument in device_arguments:
            device_name = _device_named_by_literal(argument)
            if device_name is not None:
                self._record(
                    argument,
                    f"names the device {device_name!r}, which exists on one floor only",
                    PORTABLE_DEVICE_FROM_TENSOR_OR_ACCELERATOR,
                )
        if function_name == "cuda" and isinstance(function, ast.Attribute):
            self._record(
                node,
                "calls `.cuda()`, which moves to a CUDA device, so Linux only",
                f"`.to(device)`, where you {PORTABLE_DEVICE_FROM_TENSOR_OR_ACCELERATOR}",
            )
        self.generic_visit(node)


def find_floor_bindings_in_python_source(source: str, file: Path) -> "list[CrossFloorFinding]":
    """Every floor binding in one Python source; raises `SyntaxError` on source that does not parse."""
    visitor = _FloorBindingSourceVisitor(file)
    # A parse emits the file's own SyntaxWarnings; running the app reports those.
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        parsed_module = ast.parse(source, filename=str(file))
    visitor.visit(parsed_module)
    return visitor.findings


class _DeclaredRequirement(NamedTuple):
    table_name: str
    array_key: str
    requirement: str


def _declared_requirements(project_manifest: "dict[str, object]") -> "list[_DeclaredRequirement]":
    requirement_arrays: "list[tuple[str, str, object]]" = []
    project_table = project_manifest.get("project")
    if isinstance(project_table, dict):
        requirement_arrays.append(("project", "dependencies", project_table.get("dependencies")))
        optional_dependencies = project_table.get("optional-dependencies")
        if isinstance(optional_dependencies, dict):
            requirement_arrays.extend(
                ("project.optional-dependencies", key, array)
                for key, array in optional_dependencies.items()
            )
    dependency_groups = project_manifest.get("dependency-groups")
    if isinstance(dependency_groups, dict):
        requirement_arrays.extend(
            ("dependency-groups", key, array) for key, array in dependency_groups.items()
        )
    return [
        _DeclaredRequirement(table_name, array_key, requirement)
        for table_name, array_key, requirement_array in requirement_arrays
        if isinstance(requirement_array, list)
        for requirement in requirement_array
        if isinstance(requirement, str)
    ]


def _line_declaring(manifest_text: str, declared: _DeclaredRequirement) -> int:
    """The line of `declared`'s quoted value inside its own array, or of the array's key."""
    table_header = re.compile(rf"^\s*\[\s*{re.escape(declared.table_name)}\s*\]")
    array_key = re.compile(rf"""^\s*["']?{re.escape(declared.array_key)}["']?\s*=""")
    quoted_requirement = re.compile(
        rf"""(["']){re.escape(declared.requirement)}\1"""
    )
    lines = manifest_text.splitlines()
    table_start = next(
        (index for index, line in enumerate(lines) if table_header.match(line)), None
    )
    if table_start is None:
        return 1
    key_line = next(
        (
            index
            for index in range(table_start + 1, len(lines))
            if array_key.match(lines[index])
        ),
        None,
    )
    if key_line is None:
        return table_start + 1
    for index in range(key_line, len(lines)):
        if index > key_line and lines[index].lstrip().startswith("["):
            break
        if quoted_requirement.search(lines[index].split("#", 1)[0]):
            return index + 1
    return key_line + 1


def find_floor_bindings_in_project_manifest(
    manifest_text: str, file: Path
) -> "list[CrossFloorFinding]":
    """Every floor-bound dependency with no platform marker; needs `tomllib` (Python 3.11+)."""
    if tomllib is None:
        raise RuntimeError(DEPENDENCY_RULE_SKIPPED_WITHOUT_TOMLLIB)
    findings = []
    for declared in _declared_requirements(tomllib.loads(manifest_text)):
        requirement = declared.requirement
        distribution_match = REQUIREMENT_DISTRIBUTION_NAME.match(requirement)
        if distribution_match is None:
            continue
        distribution_name = re.sub(r"[-_.]+", "-", distribution_match.group(1)).lower()
        _, _, marker = requirement.partition(";")
        if any(variable in marker for variable in PLATFORM_MARKER_VARIABLES):
            continue
        for floor_bound_distribution, (floor, platform) in FLOOR_BOUND_DISTRIBUTIONS.items():
            if distribution_name == floor_bound_distribution or distribution_name.startswith(
                f"{floor_bound_distribution}-"
            ):
                findings.append(
                    CrossFloorFinding(
                        file,
                        _line_declaring(manifest_text, declared),
                        f"depends on `{distribution_name}` with no platform marker, "
                        f"and it is {floor}",
                        f'mark it: `{requirement.strip()}; sys_platform == "{platform}"`',
                    )
                )
    return findings


def _python_files_under(app_directory: Path) -> "list[Path]":
    python_files: "list[Path]" = []
    for directory, subdirectory_names, file_names in os.walk(app_directory):
        subdirectory_names[:] = sorted(
            name
            for name in subdirectory_names
            if not name.startswith(".")
            and name not in VIRTUAL_ENVIRONMENT_DIRECTORY_NAMES
            # `os.path.isfile`, not `Path.is_file`: an unreadable directory is
            # False here rather than a PermissionError that stops the launch.
            and not os.path.isfile(
                os.path.join(directory, name, VIRTUAL_ENVIRONMENT_MARKER_FILE_NAME)
            )
        )
        python_files.extend(
            Path(directory) / name for name in sorted(file_names) if name.endswith(".py")
        )
    return python_files


def check_app_directory_for_floor_bindings(app_directory: Path) -> CrossFloorCheckReport:
    """Run the check over every `.py` under `app_directory`, outside virtual environments, and its `pyproject.toml`.

    A file that cannot be read or parsed is passed over: running it reports the
    error far better than this check could.
    """
    findings: "list[CrossFloorFinding]" = []
    for python_file in _python_files_under(app_directory):
        try:
            source = python_file.read_text(encoding="utf-8")
            findings.extend(find_floor_bindings_in_python_source(source, python_file))
        except (OSError, SyntaxError, ValueError, RecursionError):
            continue

    skipped_rule_reason = None
    project_manifest = app_directory / PROJECT_MANIFEST_FILE_NAME
    if project_manifest.is_file():
        if tomllib is None:
            skipped_rule_reason = DEPENDENCY_RULE_SKIPPED_WITHOUT_TOMLLIB
        else:
            try:
                findings.extend(
                    find_floor_bindings_in_project_manifest(
                        project_manifest.read_text(encoding="utf-8"), project_manifest
                    )
                )
            except (OSError, ValueError):
                # An unreadable or malformed manifest is the installer's to report.
                pass
    return CrossFloorCheckReport(findings, skipped_rule_reason)


def render_cross_floor_warning_block(report: CrossFloorCheckReport, app_directory: Path) -> str:
    """The warning block `run` and `dev` print; empty when there is nothing to say."""
    if not report.findings and report.skipped_rule_reason is None:
        return ""
    lines = []
    if report.findings:
        count = len(report.findings)
        lines.append(
            f"streamlib: the cross-floor check found {count} "
            f"{'thing' if count == 1 else 'things'} binding this app to one floor "
            f"(Linux or macOS). The app starts anyway."
        )
    else:
        lines.append("streamlib: the cross-floor check found nothing binding this app to one floor.")
    for finding in report.findings:
        try:
            shown_path = finding.file.relative_to(app_directory)
        except ValueError:
            shown_path = finding.file
        lines.append(f"  {shown_path}:{finding.line}: {finding.what_binds_it}")
        lines.append(f"      portable: {finding.portable_spelling}")
    if report.skipped_rule_reason is not None:
        lines.append(f"  note: {report.skipped_rule_reason}")
    return "\n".join(lines) + "\n"

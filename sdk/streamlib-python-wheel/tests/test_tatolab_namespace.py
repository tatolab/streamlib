# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab` is a namespace every Tatolab distribution shares, and its public names say "node".

`tatolab` is PEP 420: no distribution ships `tatolab/__init__.py`, so a second
distribution's `tatolab.<name>` lands beside these two rather than shadowing
them. `tatolab.stream` and `tatolab.runtime` are regular packages. No public
Python name `tatolab.stream` publishes says "processor".
"""

import importlib.util
import inspect
import pkgutil
import subprocess
import sys
from pathlib import Path
from typing import Callable

import tatolab
import tatolab.runtime
import tatolab.runtime._engine as engine
import tatolab.stream

WHEEL_PYTHON_SOURCE_DIRECTORY = Path(__file__).resolve().parents[1] / "python"

ENGINE_CLASS_NAMES_THE_RENAME_RETIRED = (
    "ProcessorOwnedWindow",
    "ProcessorOwnedWindowEvents",
    "ProcessorLinkDataAccess",
)
CLASS_NAMES_THE_RENAME_RETIRED = (
    *ENGINE_CLASS_NAMES_THE_RENAME_RETIRED,
    "ProcessorOutputTextureRing",
)
ENGINE_CLASS_NAMES_IN_THEIR_PLACE = (
    "NodeOwnedWindow",
    "NodeOwnedWindowEvents",
    "NodeLinkDataAccess",
)
GPU_CONTEXT_METHOD_NAMES_THE_RENAME_RETIRED = (
    "acquire_texture_from_processor_output_pool",
    "acquire_storage_buffer_from_processor_output_pool",
)
GPU_CONTEXT_METHOD_NAMES_IN_THEIR_PLACE = (
    "acquire_texture_from_node_output_pool",
    "acquire_storage_buffer_from_node_output_pool",
)


def test_tatolab_is_a_namespace_package_with_no_file_of_its_own():
    assert getattr(tatolab, "__file__", None) is None
    assert tatolab.__spec__ is not None
    assert tatolab.__spec__.origin is None
    assert tatolab.__spec__.submodule_search_locations is not None


def test_tatolab_stream_and_tatolab_runtime_are_regular_packages():
    for package in (tatolab.stream, tatolab.runtime):
        assert package.__file__ is not None, package.__name__
        assert Path(package.__file__).name == "__init__.py", package.__name__


def test_no_tatolab_init_exists_in_the_source_tree_or_on_the_namespace_path():
    assert (WHEEL_PYTHON_SOURCE_DIRECTORY / "tatolab").is_dir()
    assert not (WHEEL_PYTHON_SOURCE_DIRECTORY / "tatolab" / "__init__.py").exists()
    for namespace_directory in tatolab.__path__:
        assert not (Path(namespace_directory) / "__init__.py").exists(), namespace_directory


def test_no_streamlib_module_is_importable():
    assert importlib.util.find_spec("streamlib") is None
    assert "streamlib" not in {
        top_level_module.name for top_level_module in pkgutil.iter_modules()
    }


def test_the_retired_texture_ring_module_is_not_importable():
    assert importlib.util.find_spec("tatolab.stream.processor_output_texture_ring") is None
    assert importlib.util.find_spec("tatolab.runtime.processor_output_texture_ring") is None


def test_no_package_nor_the_engine_holds_a_class_name_the_rename_retired():
    for module_holding_public_names in (tatolab.stream, tatolab.runtime, engine):
        for class_name in CLASS_NAMES_THE_RENAME_RETIRED:
            assert not hasattr(module_holding_public_names, class_name), (
                module_holding_public_names.__name__,
                class_name,
            )


def test_importing_tatolab_runtime_alone_does_not_import_tatolab_stream():
    """`tatolab.stream` imports `tatolab.runtime._engine`, which runs
    `tatolab.runtime`'s `__init__` first; an import back from there would be
    circular."""
    imported = subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys, tatolab.runtime; print('tatolab.stream' in sys.modules)",
        ],
        capture_output=True,
        text=True,
        timeout=120,
        check=True,
    )
    assert imported.stdout.strip() == "False", imported.stderr


def test_the_engine_publishes_none_of_the_processor_names_the_rename_retired():
    for class_name in ENGINE_CLASS_NAMES_THE_RENAME_RETIRED:
        assert not hasattr(engine, class_name), class_name
    for gpu_context_class in (engine.GpuContextLimitedAccess, engine.GpuContextFullAccess):
        for method_name in GPU_CONTEXT_METHOD_NAMES_THE_RENAME_RETIRED:
            assert not hasattr(gpu_context_class, method_name), (gpu_context_class, method_name)
    for runtime_context_class in (
        engine.RuntimeContextFullAccess,
        engine.RuntimeContextLimitedAccess,
    ):
        assert not hasattr(runtime_context_class, "processor_id"), runtime_context_class
    assert "processor_id" not in inspect.signature(
        engine.RuntimeContextFullAccess.open_for_helper_process
    ).parameters


def test_the_engine_publishes_the_node_names_in_their_place():
    for class_name in ENGINE_CLASS_NAMES_IN_THEIR_PLACE:
        assert inspect.isclass(getattr(engine, class_name)), class_name
        assert getattr(tatolab.stream, class_name) is getattr(engine, class_name), class_name
    for gpu_context_class in (engine.GpuContextLimitedAccess, engine.GpuContextFullAccess):
        for method_name in GPU_CONTEXT_METHOD_NAMES_IN_THEIR_PLACE:
            assert callable(getattr(gpu_context_class, method_name)), (
                gpu_context_class,
                method_name,
            )
    for runtime_context_class in (
        engine.RuntimeContextFullAccess,
        engine.RuntimeContextLimitedAccess,
    ):
        assert hasattr(runtime_context_class, "node_id"), runtime_context_class
    assert "node_id" in inspect.signature(
        engine.RuntimeContextFullAccess.open_for_helper_process
    ).parameters
    assert tatolab.stream.NodeOutputTextureRing.__module__ == (
        "tatolab.stream.node_output_texture_ring"
    )


def test_no_name_tatolab_stream_publishes_nor_any_public_member_of_one_says_processor():
    names_saying_processor: "list[str]" = []
    for exported_name in tatolab.stream.__all__:
        if "processor" in exported_name.lower():
            names_saying_processor.append(exported_name)
        exported = getattr(tatolab.stream, exported_name)
        if inspect.isclass(exported):
            member_names = [name for name in dir(exported) if not name.startswith("_")]
        elif inspect.ismodule(exported):
            member_names = list(getattr(exported, "__all__", ()))
        else:
            continue
        names_saying_processor.extend(
            f"{exported_name}.{member_name}"
            for member_name in member_names
            if "processor" in member_name.lower()
        )

    assert names_saying_processor == []


# Public parameters that say "processor" and are not this rename's to change.
# Each is named here so the sweep below stays exhaustive for everything else.
PUBLIC_PARAMETERS_SAYING_PROCESSOR_OUTSIDE_THIS_RENAME: "tuple[str, ...]" = ()


def _public_callables_tatolab_stream_publishes() -> "list[tuple[str, Callable[..., object]]]":
    public_callables: "list[tuple[str, Callable[..., object]]]" = []
    for exported_name in tatolab.stream.__all__:
        exported = getattr(tatolab.stream, exported_name)
        if inspect.isclass(exported):
            public_callables.append((exported_name, exported))
            for member_name in dir(exported):
                if member_name.startswith("_"):
                    continue
                member = getattr(exported, member_name)
                if callable(member) and not inspect.isclass(member):
                    public_callables.append((f"{exported_name}.{member_name}", member))
        elif inspect.ismodule(exported):
            for member_name in getattr(exported, "__all__", ()):
                member = getattr(exported, member_name)
                if callable(member):
                    public_callables.append((f"{exported_name}.{member_name}", member))
        elif callable(exported):
            public_callables.append((exported_name, exported))
    return public_callables


def test_the_node_decorator_names_its_class_parameter_node_class():
    node_parameter_names = list(inspect.signature(tatolab.stream.node).parameters)
    assert node_parameter_names[0] == "node_class"
    assert not [name for name in node_parameter_names if "processor" in name.lower()]


def test_no_parameter_of_a_public_callable_tatolab_stream_publishes_says_processor():
    parameters_saying_processor = [
        f"{qualified_name}({parameter_name})"
        for qualified_name, public_callable in _public_callables_tatolab_stream_publishes()
        for parameter_name in inspect.signature(public_callable).parameters
        if "processor" in parameter_name.lower()
    ]

    assert [
        parameter
        for parameter in parameters_saying_processor
        if parameter not in PUBLIC_PARAMETERS_SAYING_PROCESSOR_OUTSIDE_THIS_RENAME
    ] == []


def test_every_engine_class_tatolab_stream_publishes_names_tatolab_stream_as_its_module():
    for exported_name in tatolab.stream.__all__:
        exported = getattr(tatolab.stream, exported_name)
        if inspect.isclass(exported) and getattr(engine, exported_name, None) is exported:
            assert exported.__module__ == "tatolab.stream", exported_name

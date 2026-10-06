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
import subprocess
import sys
from pathlib import Path

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


def test_every_engine_class_tatolab_stream_publishes_names_tatolab_stream_as_its_module():
    for exported_name in tatolab.stream.__all__:
        exported = getattr(tatolab.stream, exported_name)
        if inspect.isclass(exported) and getattr(engine, exported_name, None) is exported:
            assert exported.__module__ == "tatolab.stream", exported_name

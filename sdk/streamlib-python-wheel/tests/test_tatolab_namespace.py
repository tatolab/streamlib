# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab` is a namespace every Tatolab distribution shares, and its public names say "node".

`tatolab` is PEP 420: no distribution ships `tatolab/__init__.py`, so a second
distribution's `tatolab.<name>` lands beside these two rather than shadowing
them. `tatolab.stream` and `tatolab.runtime` are regular packages. No public
Python name either publishes, nor any public module beneath them, says "processor".
"""

import importlib.util
import inspect
import pkgutil
import subprocess
import sys
from pathlib import Path
from types import ModuleType
from typing import Callable, is_typeddict

import tatolab
import tatolab.runtime
import tatolab.runtime._engine as engine
import tatolab.runtime.testing
import tatolab.stream
from tatolab.stream._runtime_lend import runtime_backed_protocol_registry

WHEEL_PYTHON_SOURCE_DIRECTORY = Path(__file__).resolve().parents[1] / "python"
STREAM_DISTRIBUTION_SOURCE_DIRECTORY = Path(__file__).resolve().parents[2] / "tatolab-stream"

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
    for source_directory in (WHEEL_PYTHON_SOURCE_DIRECTORY, STREAM_DISTRIBUTION_SOURCE_DIRECTORY):
        assert (source_directory / "tatolab").is_dir(), source_directory
        assert not (source_directory / "tatolab" / "__init__.py").exists(), source_directory
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
    """`tatolab.stream` reaches `tatolab.runtime._engine` when a runtime-backed
    function is first called, which runs `tatolab.runtime`'s `__init__`; an
    import back from there would be circular."""
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


def _native_engine_class(class_name: str) -> type:
    """A native class `tatolab.stream` declares as a Protocol, which the stub does not."""
    native_class = getattr(engine, class_name)
    assert inspect.isclass(native_class), class_name
    return native_class


def test_the_engine_publishes_none_of_the_processor_names_the_rename_retired():
    for class_name in ENGINE_CLASS_NAMES_THE_RENAME_RETIRED:
        assert not hasattr(engine, class_name), class_name
    for gpu_context_class in (
        _native_engine_class("GpuContextLimitedAccess"),
        _native_engine_class("GpuContextFullAccess"),
    ):
        for method_name in GPU_CONTEXT_METHOD_NAMES_THE_RENAME_RETIRED:
            assert not hasattr(gpu_context_class, method_name), (gpu_context_class, method_name)
    for runtime_context_class in (
        _native_engine_class("RuntimeContextFullAccess"),
        _native_engine_class("RuntimeContextLimitedAccess"),
    ):
        assert not hasattr(runtime_context_class, "processor_id"), runtime_context_class
    assert "processor_id" not in inspect.signature(
        engine.open_runtime_context_full_access_for_helper_process
    ).parameters


def test_the_engine_publishes_the_node_names_in_their_place():
    for class_name in ENGINE_CLASS_NAMES_IN_THEIR_PLACE:
        assert _native_engine_class(class_name).__name__ == class_name
        assert getattr(tatolab.stream, class_name).__name__ == class_name
    for gpu_context_class in (
        _native_engine_class("GpuContextLimitedAccess"),
        _native_engine_class("GpuContextFullAccess"),
    ):
        for method_name in GPU_CONTEXT_METHOD_NAMES_IN_THEIR_PLACE:
            assert callable(getattr(gpu_context_class, method_name)), (
                gpu_context_class,
                method_name,
            )
    for runtime_context_class in (
        _native_engine_class("RuntimeContextFullAccess"),
        _native_engine_class("RuntimeContextLimitedAccess"),
    ):
        assert hasattr(runtime_context_class, "node_id"), runtime_context_class
    assert "node_id" in inspect.signature(
        engine.open_runtime_context_full_access_for_helper_process
    ).parameters
    assert tatolab.stream.NodeOutputTextureRing.__module__ == (
        "tatolab.stream.node_output_texture_ring"
    )


def _public_tatolab_modules() -> "list[ModuleType]":
    public_modules: "list[ModuleType]" = []
    for package in (tatolab.stream, tatolab.runtime):
        public_modules.append(package)
        for submodule in pkgutil.iter_modules(package.__path__, f"{package.__name__}."):
            if not submodule.name.rsplit(".", 1)[1].startswith("_"):
                public_modules.append(importlib.import_module(submodule.name))
    return public_modules


def _public_names_tatolab_publishes() -> "list[tuple[str, object]]":
    public_names: "list[tuple[str, object]]" = []
    for public_module in _public_tatolab_modules():
        assert hasattr(public_module, "__all__"), public_module.__name__
        for exported_name in public_module.__all__:
            public_names.append(
                (f"{public_module.__name__}.{exported_name}", getattr(public_module, exported_name))
            )
    return public_names


def test_every_public_tatolab_module_is_swept_for_processor():
    swept_module_names = {public_module.__name__ for public_module in _public_tatolab_modules()}
    assert {
        "tatolab.stream",
        "tatolab.runtime",
        "tatolab.runtime.testing",
        "tatolab.runtime.cli",
        "tatolab.stream.node_output_texture_ring",
    } <= swept_module_names


def test_no_name_tatolab_publishes_nor_any_public_member_of_one_says_processor():
    names_saying_processor: "list[str]" = [
        public_module.__name__
        for public_module in _public_tatolab_modules()
        if "processor" in public_module.__name__.rsplit(".", 1)[-1].lower()
    ]
    for qualified_name, exported in _public_names_tatolab_publishes():
        if "processor" in qualified_name.rsplit(".", 1)[1].lower():
            names_saying_processor.append(qualified_name)
        if inspect.isclass(exported):
            member_names = [name for name in dir(exported) if not name.startswith("_")]
        elif inspect.ismodule(exported):
            member_names = list(getattr(exported, "__all__", ()))
        else:
            continue
        names_saying_processor.extend(
            f"{qualified_name}.{member_name}"
            for member_name in member_names
            if "processor" in member_name.lower()
        )

    assert names_saying_processor == []


def _public_callables_tatolab_publishes() -> "list[tuple[str, Callable[..., object]]]":
    public_callables: "list[tuple[str, Callable[..., object]]]" = []
    for qualified_name, exported in _public_names_tatolab_publishes():
        if is_typeddict(exported):
            continue
        if inspect.isclass(exported):
            public_callables.append((qualified_name, exported))
            for member_name in dir(exported):
                if member_name.startswith("_"):
                    continue
                member = getattr(exported, member_name)
                if callable(member) and not inspect.isclass(member):
                    public_callables.append((f"{qualified_name}.{member_name}", member))
        elif inspect.ismodule(exported):
            for member_name in getattr(exported, "__all__", ()):
                member = getattr(exported, member_name)
                if callable(member):
                    public_callables.append((f"{qualified_name}.{member_name}", member))
        elif callable(exported):
            public_callables.append((qualified_name, exported))
    return public_callables


def test_no_parameter_of_the_node_decorator_says_processor():
    node_parameter_names = list(inspect.signature(tatolab.stream.node).parameters)
    assert not [name for name in node_parameter_names if "processor" in name.lower()]


def test_the_single_node_test_pipeline_names_its_class_parameter_node_class():
    pipeline_parameter_names = list(
        inspect.signature(tatolab.runtime.testing.SingleNodeTestPipeline).parameters
    )
    assert pipeline_parameter_names[0] == "node_class"


def test_no_parameter_of_a_public_callable_tatolab_publishes_says_processor():
    parameters_saying_processor = [
        f"{qualified_name}({parameter_name})"
        for qualified_name, public_callable in _public_callables_tatolab_publishes()
        for parameter_name in inspect.signature(public_callable).parameters
        if "processor" in parameter_name.lower()
    ]
    parameters_saying_processor.extend(
        f"{qualified_name}[{key}]"
        for qualified_name, exported in _public_names_tatolab_publishes()
        if is_typeddict(exported)
        for key in getattr(exported, "__required_keys__")
        | getattr(exported, "__optional_keys__")
        if "processor" in key.lower()
    )

    assert parameters_saying_processor == []


def test_every_native_class_a_tatolab_stream_protocol_declares_names_tatolab_stream_as_its_module():
    protocol_names = sorted(runtime_backed_protocol_registry)
    assert "GpuSurfaceHandle" in protocol_names
    for protocol_name in protocol_names:
        assert _native_engine_class(protocol_name).__module__ == "tatolab.stream", protocol_name

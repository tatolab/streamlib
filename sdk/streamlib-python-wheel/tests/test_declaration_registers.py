# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What `@processor` puts in the processor catalog the moment it runs.

A class is discoverable before anything adds it, which is what lets an agent
read an app's effects off a node that has only imported them. No runtime boots
here: registration is a declaration-time fact, and the catalog is per process.
"""

import json
import os
import subprocess
import sys
import types
from pathlib import Path

import pytest

from streamlib import processor
from streamlib._engine import (
    processor_class_import_paths_in_this_processes_catalog,
)
from streamlib._helper import ENTRYPOINT_ENV

# What a real helper imports and hosts — its own module, as every processor
# class must be.
PROCESSOR_MODULE_A_HELPER_HOSTS = "zero_argument_process_processor"
PROCESSOR_A_HELPER_HOSTS = f"{PROCESSOR_MODULE_A_HELPER_HOSTS}:ZeroArgumentProcess"


@processor(execution="manual", description="Declared here and added nowhere")
class DeclaredAndNeverAdded:
    """A processor this suite imports and never puts in a graph."""


@processor(execution="manual")
class DescribedByItsDocstringAlone:
    """What a processor with no description= keyword falls back to."""


@processor(execution="manual", description="The keyword wins")
class DescribedByBothKeywordAndDocstring:
    """The docstring the keyword outranks."""


@processor(execution="manual")
class DescribedByNothingAtAll:
    pass


def _declare_in_a_module_named(module_name: str, class_name: str) -> type:
    """Run a decoration inside a module of the caller's naming.

    A test function's own classes carry `<locals>` in `__qualname__` and are
    unimportable by design, so a module is the only place a decoration gets a
    real import path — and a name per test is what keeps the process-wide
    catalog from carrying one test's registration into another's.
    """
    module = types.ModuleType(module_name)
    sys.modules[module_name] = module
    source = (
        "from streamlib import processor\n"
        "@processor(execution='manual')\n"
        f"class {class_name}:\n"
        "    pass\n"
    )
    exec(compile(source, f"<{module_name}>", "exec"), module.__dict__)  # noqa: S102
    return getattr(module, class_name)


def test_a_decorated_class_is_in_the_catalog_before_anything_adds_it():
    """The whole point: importing the module is the registration."""
    assert (
        "test_declaration_registers:DeclaredAndNeverAdded"
        in processor_class_import_paths_in_this_processes_catalog()
    )


def test_the_catalog_names_a_class_by_its_import_path():
    """The same string a helper process imports the class back by."""
    declared = _declare_in_a_module_named(
        "a_module_declaring_one_processor", "RegisteredAtDecoration"
    )

    assert declared.__module__ == "a_module_declaring_one_processor"
    assert (
        "a_module_declaring_one_processor:RegisteredAtDecoration"
        in processor_class_import_paths_in_this_processes_catalog()
    )


def test_a_class_declared_inside_a_function_registers_nothing():
    """It has no import path to be registered under.

    `rt.add` is where a class no interpreter can import is refused, with the
    fix named — moving that refusal to decoration would refuse at import what
    the plan refuses at add.
    """
    catalog_before = set(processor_class_import_paths_in_this_processes_catalog())

    @processor(execution="manual")
    class DeclaredInsideThisTest:
        pass

    assert "<locals>" in DeclaredInsideThisTest.__qualname__
    assert (
        set(processor_class_import_paths_in_this_processes_catalog())
        == catalog_before
    )


def test_one_import_path_decorated_twice_is_refused_naming_the_reload():
    """A module loaded twice rebuilds its classes, and both claim one path.

    The registry's duplicate refusal, now met at import where `importlib.reload`
    is the cause a reader can act on.
    """
    module = types.ModuleType("a_module_loaded_twice")
    sys.modules["a_module_loaded_twice"] = module
    source = compile(
        "from streamlib import processor\n"
        "@processor(execution='manual')\n"
        "class DecoratedTwice:\n"
        "    pass\n",
        "<a_module_loaded_twice>",
        "exec",
    )

    exec(source, module.__dict__)  # noqa: S102

    with pytest.raises(ValueError) as refusal:
        exec(source, module.__dict__)  # noqa: S102

    assert "a_module_loaded_twice:DecoratedTwice" in str(refusal.value)
    assert "importlib.reload" in str(refusal.value)


def test_a_refused_second_decoration_leaves_the_first_registration_standing():
    """The registration that arrived first stays; nothing is overwritten."""
    _declare_in_a_module_named("a_module_reloaded_once", "SurvivesTheReload")

    module = sys.modules["a_module_reloaded_once"]
    source = compile(
        "from streamlib import processor\n"
        "@processor(execution='manual')\n"
        "class SurvivesTheReload:\n"
        "    pass\n",
        "<a_module_reloaded_once>",
        "exec",
    )
    with pytest.raises(ValueError):
        exec(source, module.__dict__)  # noqa: S102

    registered = processor_class_import_paths_in_this_processes_catalog()
    assert (
        registered.count("a_module_reloaded_once:SurvivesTheReload") == 1
    ), "a refused duplicate must neither displace the first nor register beside it"


def test_a_processor_with_no_description_is_described_by_its_docstring():
    """The text an author already wrote, rather than a second place to write it."""
    assert (
        DescribedByItsDocstringAlone.__streamlib_processor_description__
        == "What a processor with no description= keyword falls back to."
    )


def test_an_explicit_description_outranks_the_docstring():
    """The keyword is the deliberate one; the docstring is the fallback."""
    assert (
        DescribedByBothKeywordAndDocstring.__streamlib_processor_description__
        == "The keyword wins"
    )


def test_a_processor_with_neither_is_described_by_the_empty_string():
    """Never `None`: the descriptor's description is a string."""
    assert DescribedByNothingAtAll.__streamlib_processor_description__ == ""


def _catalog_of_an_interpreter_carrying(environment: "dict[str, str]") -> "list[str]":
    """The registry of a fresh interpreter that imported one processor module.

    Out of process because the variable under test is read once per import and
    the catalog is per process: neither can be faked by patching inside this one.
    """
    reporter = (
        f"import {PROCESSOR_MODULE_A_HELPER_HOSTS}\n"
        "import json\n"
        "from streamlib._engine import "
        "processor_class_import_paths_in_this_processes_catalog as registered\n"
        "print(json.dumps(registered()))\n"
    )
    reported = subprocess.run(
        [sys.executable, "-c", reporter],
        check=True,
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "PYTHONPATH": str(Path(__file__).parent),
            **environment,
        },
    )
    return json.loads(reported.stdout)


def test_an_interpreter_that_is_not_a_helper_registers_what_it_imports():
    """The control arm: the same import, without the helper's variable set."""
    assert PROCESSOR_A_HELPER_HOSTS in _catalog_of_an_interpreter_carrying({})


def test_an_interpreter_carrying_the_helper_entrypoint_registers_nothing():
    """A helper hosts no graph, so it needs no catalog and builds none.

    The variable is the spawn host's, set on every child it starts and nowhere
    else — which is what makes its presence a reliable "I am a helper".
    """
    catalog = _catalog_of_an_interpreter_carrying(
        {ENTRYPOINT_ENV: PROCESSOR_A_HELPER_HOSTS}
    )

    assert PROCESSOR_A_HELPER_HOSTS not in catalog, (
        f"a helper registered the class it hosts: {catalog}"
    )

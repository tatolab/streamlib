# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor interpreter bootstrap, run by path the way the engine runs it.

Every test starts `tatolab/runtime/_processor_interpreter_bootstrap.py` in a
child with the lend directory and then a project directory of the test's own on
`PYTHONPATH`, the project as its working directory, and the engine build id the
parent would hand it. Nothing here needs a GPU: describing imports a node
module and reads its stamps, and a refusal happens before anything opens.
"""

from __future__ import annotations

import ast
import json
import os
import platform
import shutil
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Any

import pytest

import tatolab.runtime
from tatolab.runtime import _processor_interpreter_bootstrap as processor_interpreter_bootstrap
from tatolab.runtime._engine import engine_build_id_compiled_into_this_extension
from tatolab.stream._node_config_schema import json_schema_for_a_node_declaring_no_config

#: The directory holding the `tatolab/runtime/` this process imported.
LEND_DIRECTORY = Path(tatolab.runtime.__file__).resolve().parent.parent.parent

BOOTSTRAP_PATH = LEND_DIRECTORY / "tatolab" / "runtime" / "_processor_interpreter_bootstrap.py"

SECONDS_A_BOOTSTRAP_HAS_TO_ANSWER = 60.0

DESCRIBED_NODES_SOURCE = '''
from tatolab.stream import AudioWindowContract, node


@node(scheduling="high")
class WindowedRelay:
    """Relays each audio window as a frame."""

    @node.input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=16_000, channels=1, dtype="f32", window_size=512, hop=160
        ),
        description="windows from upstream",
    )
    def audio_from_upstream(self) -> None: ...

    @node.output(description="frames to downstream")
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None: ...


@node(execution="continuous", interval_ms=20, description="ticks on its own")
class Ticker:
    @node.output()
    def ticks_to_downstream(self) -> None: ...

    def process(self, ctx) -> None: ...


class Unstamped:
    def process(self, ctx) -> None: ...


def not_a_class() -> None: ...
'''


def write_project_module(project_directory: Path, module_name: str, source: str) -> None:
    (project_directory / f"{module_name}.py").write_text(textwrap.dedent(source))


@pytest.fixture
def project_directory(tmp_path: Path) -> Path:
    """A project holding `described_nodes`, the module most tests describe from."""
    write_project_module(tmp_path, "described_nodes", DESCRIBED_NODES_SOURCE)
    return tmp_path


def processor_interpreter_environment(
    lend_directory: Path, project_directory: Path
) -> dict[str, str]:
    """This process's environment as the engine hands it to a processor interpreter."""
    environment = {
        name: value
        for name, value in os.environ.items()
        if name not in ("PYTHONHOME", "PYTHONPATH") and not name.startswith("STREAMLIB_")
    }
    environment["PYTHONPATH"] = f"{lend_directory}{os.pathsep}{project_directory}"
    environment[processor_interpreter_bootstrap.ENGINE_BUILD_ID_ENV] = (
        engine_build_id_compiled_into_this_extension()
    )
    return environment


def run_the_bootstrap(
    project_directory: Path,
    *arguments: str,
    lend_directory: Path = LEND_DIRECTORY,
    environment_overrides: "dict[str, str] | None" = None,
) -> subprocess.CompletedProcess[str]:
    environment = processor_interpreter_environment(lend_directory, project_directory)
    environment.update(environment_overrides or {})
    return subprocess.run(
        [
            sys.executable,
            str(lend_directory / "tatolab" / "runtime" / BOOTSTRAP_PATH.name),
            *arguments,
        ],
        cwd=project_directory,
        env=environment,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=SECONDS_A_BOOTSTRAP_HAS_TO_ANSWER,
        check=False,
    )


def describe(project_directory: Path, *import_paths: str) -> subprocess.CompletedProcess[str]:
    return run_the_bootstrap(project_directory, "--describe", *import_paths)


def described_document(described: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    """The one JSON document on stdout, failing with the child's stderr if there is not one."""
    try:
        document = json.loads(described.stdout)
    except json.JSONDecodeError as not_one_document:
        pytest.fail(
            f"stdout is not one JSON document ({not_one_document}):\n{described.stdout}\n"
            f"stderr:\n{described.stderr}"
        )
    assert set(document) == {"described_node_types", "refused_import_paths"}, document
    return document


# ---- describe ---------------------------------------------------------------


def test_a_stamped_node_type_is_described_as_its_stamps(project_directory: Path):
    described = describe(project_directory, "described_nodes:WindowedRelay")

    assert described.returncode == 0, described.stderr
    assert described_document(described) == {
        "described_node_types": [
            {
                "import_path": "described_nodes:WindowedRelay",
                "short_name": "WindowedRelay",
                "description": "Relays each audio window as a frame.",
                "execution": {"mode": "reactive", "interval_ms": 0},
                "scheduling_priority": "high",
                "config_schema": json_schema_for_a_node_declaring_no_config(),
                "input_ports": [
                    {
                        "name": "audio_from_upstream",
                        "description": "windows from upstream",
                        "delivery_profile": "ordered",
                        "audio_window": {
                            "resolved_from": "declaration",
                            "sample_rate": 16_000,
                            "channels": 1,
                            "dtype": "f32",
                            "window_size": 512,
                            "hop": 160,
                        },
                    }
                ],
                "output_ports": [
                    {"name": "frames_to_downstream", "description": "frames to downstream"}
                ],
            }
        ],
        "refused_import_paths": [],
    }


def test_a_continuous_node_carries_its_interval_and_no_priority(project_directory: Path):
    described = describe(project_directory, "described_nodes:Ticker")

    assert described.returncode == 0, described.stderr
    (ticker,) = described_document(described)["described_node_types"]
    assert ticker["execution"] == {"mode": "continuous", "interval_ms": 20}
    assert ticker["scheduling_priority"] is None
    assert ticker["description"] == "ticks on its own"
    assert ticker["input_ports"] == []


def test_a_module_that_will_not_import_is_refused_carrying_the_import_error(
    project_directory: Path,
):
    described = describe(project_directory, "no_such_node_module:Anything")

    assert described.returncode == 1
    assert described_document(described) == {
        "described_node_types": [],
        "refused_import_paths": ["no_such_node_module:Anything"],
    }
    assert "cannot describe no_such_node_module:Anything: " in described.stderr
    assert "ModuleNotFoundError: No module named 'no_such_node_module'" in described.stderr


def test_a_module_that_raises_at_import_is_refused_with_its_traceback(project_directory: Path):
    write_project_module(
        project_directory,
        "raising_nodes",
        """
        def the_module_body():
            raise RuntimeError("the node module's own failure")

        the_module_body()
        """,
    )

    described = describe(project_directory, "raising_nodes:Anything")

    assert described.returncode == 1
    assert described_document(described)["refused_import_paths"] == ["raising_nodes:Anything"]
    assert "cannot describe raising_nodes:Anything: " in described.stderr
    assert "Traceback (most recent call last):" in described.stderr
    assert "in the_module_body" in described.stderr
    assert "RuntimeError: the node module's own failure" in described.stderr


def test_an_unstamped_class_is_refused_naming_it(project_directory: Path):
    described = describe(project_directory, "described_nodes:Unstamped")

    assert described.returncode == 1
    assert described_document(described)["refused_import_paths"] == ["described_nodes:Unstamped"]
    assert (
        "cannot describe described_nodes:Unstamped: the class `Unstamped` carries no "
        "`@node` declaration"
    ) in described.stderr


def test_a_missing_attribute_and_a_function_are_refused(project_directory: Path):
    described = describe(
        project_directory, "described_nodes:NoSuchNode", "described_nodes:not_a_class"
    )

    assert described.returncode == 1
    assert described_document(described)["refused_import_paths"] == [
        "described_nodes:NoSuchNode",
        "described_nodes:not_a_class",
    ]
    assert "cannot describe described_nodes:NoSuchNode: " in described.stderr
    assert "has no attribute 'NoSuchNode'" in described.stderr
    assert (
        "cannot describe described_nodes:not_a_class: it names a function, not a class"
        in described.stderr
    )


def test_a_class_named_by_a_path_other_than_its_own_is_refused(project_directory: Path):
    write_project_module(
        project_directory,
        "reexporting_nodes",
        "from described_nodes import WindowedRelay\n",
    )

    described = describe(project_directory, "reexporting_nodes:WindowedRelay")

    assert described.returncode == 1
    assert described_document(described)["refused_import_paths"] == [
        "reexporting_nodes:WindowedRelay"
    ]
    assert "identifies as `described_nodes:WindowedRelay`" in described.stderr


def test_a_path_that_is_not_module_colon_qualname_is_refused(project_directory: Path):
    described = describe(project_directory, "described_nodes.WindowedRelay")

    assert described.returncode == 1
    assert described_document(described)["refused_import_paths"] == [
        "described_nodes.WindowedRelay"
    ]
    assert "cannot describe described_nodes.WindowedRelay: " in described.stderr


def test_what_a_module_prints_at_import_reaches_stderr_and_never_the_document(
    project_directory: Path,
):
    write_project_module(
        project_directory,
        "noisy_nodes",
        """
        import os
        import subprocess
        import sys

        from tatolab.stream import node

        print("printed at import")
        sys.stdout.write("written to sys.stdout at import\\n")
        sys.stdout.flush()
        os.write(1, b"written to fd 1 at import\\n")
        subprocess.run([sys.executable, "-c", "print('printed by a child at import')"], check=True)


        @node(execution="manual")
        class Noisy:
            @node.output()
            def noise_to_downstream(self) -> None: ...

            def process(self, ctx) -> None:
                print("never called")
        """,
    )

    described = describe(project_directory, "noisy_nodes:Noisy")

    assert described.returncode == 0, described.stderr
    assert [
        described_node_type["import_path"]
        for described_node_type in described_document(described)["described_node_types"]
    ] == ["noisy_nodes:Noisy"]
    for noise in (
        "printed at import",
        "written to sys.stdout at import",
        "written to fd 1 at import",
        "printed by a child at import",
    ):
        assert noise in described.stderr
        assert noise not in described.stdout


def test_several_paths_in_one_call_are_described_and_refused_in_order(project_directory: Path):
    described = describe(
        project_directory,
        "described_nodes:Ticker",
        "no_such_node_module:Anything",
        "described_nodes:WindowedRelay",
        "described_nodes:Unstamped",
    )

    assert described.returncode == 1
    document = described_document(described)
    assert [
        described_node_type["import_path"]
        for described_node_type in document["described_node_types"]
    ] == ["described_nodes:Ticker", "described_nodes:WindowedRelay"]
    assert document["refused_import_paths"] == [
        "no_such_node_module:Anything",
        "described_nodes:Unstamped",
    ]


def test_a_class_stamped_by_hand_without_every_stamp_is_refused_and_the_rest_described(
    project_directory: Path,
):
    write_project_module(
        project_directory,
        "hand_stamped_nodes",
        """
        class HandStamped:
            __tatolab_node_declared__ = True
            __tatolab_node_description__ = "declared by hand, missing every other stamp"
        """,
    )

    described = describe(
        project_directory, "hand_stamped_nodes:HandStamped", "described_nodes:Ticker"
    )

    assert described.returncode == 1
    document = described_document(described)
    assert document["refused_import_paths"] == ["hand_stamped_nodes:HandStamped"]
    assert [
        described_node_type["import_path"]
        for described_node_type in document["described_node_types"]
    ] == ["described_nodes:Ticker"]
    assert (
        "cannot describe hand_stamped_nodes:HandStamped: the class `HandStamped` carries no "
        "`__tatolab_node_execution__`"
    ) in described.stderr


def test_describe_with_no_import_path_is_refused(project_directory: Path):
    described = run_the_bootstrap(project_directory, "--describe")

    assert described.returncode == 1
    assert described.stdout == ""
    assert "`--describe` takes one or more import paths" in described.stderr


def test_an_argument_the_bootstrap_does_not_take_is_refused(project_directory: Path):
    refused = run_the_bootstrap(project_directory, "--no-such-flag")

    assert refused.returncode == 1
    assert "a processor interpreter takes no arguments" in refused.stderr
    assert "--no-such-flag" in refused.stderr


def test_describe_refuses_an_engine_built_other_than_the_parents(project_directory: Path):
    """The build-id check runs before describe, as it does before a processor starts."""
    another_build = (
        "0.0.1+0123456789abcdef0123456789abcdef01234567.00000000000000000000000000000000"
    )

    described = run_the_bootstrap(
        project_directory,
        "--describe",
        "described_nodes:WindowedRelay",
        environment_overrides={processor_interpreter_bootstrap.ENGINE_BUILD_ID_ENV: another_build},
    )

    assert described.returncode == 1
    assert described.stdout == ""
    assert f"its parent is engine build {another_build}" in described.stderr


# ---- the interpreter, and the path the bootstrap runs from ------------------


def test_an_interpreter_that_cannot_load_the_lent_runtime_is_refused_naming_what_it_is(
    tmp_path: Path,
):
    """A lend whose `tatolab/runtime/` has every Python module but no `_engine`
    stands in for an interpreter the extension will not load in — another
    version, another architecture, a build without it."""
    lend_without_an_engine = tmp_path / "lend"
    runtime_package_without_an_engine = lend_without_an_engine / "tatolab" / "runtime"
    runtime_package_without_an_engine.mkdir(parents=True)
    for module_file in BOOTSTRAP_PATH.parent.glob("*.py"):
        shutil.copy2(module_file, runtime_package_without_an_engine)
    project = tmp_path / "project"
    project.mkdir()

    refused = run_the_bootstrap(project, lend_directory=lend_without_an_engine)

    assert refused.returncode != 0
    assert refused.stdout == ""
    assert "cannot load the lent runtime" in refused.stderr
    assert f"interpreter: {sys.executable}" in refused.stderr
    assert f"implementation: {platform.python_implementation()}" in refused.stderr
    assert f"version: {platform.python_version()}" in refused.stderr
    assert "free-threaded: no" in refused.stderr
    assert f"architecture: {platform.machine()}" in refused.stderr
    assert "No module named 'tatolab.runtime._engine'" in refused.stderr


@pytest.mark.parametrize(
    ("make_this_interpreter_unfit", "reported_fact", "reason_the_import_was_not_attempted"),
    [
        (
            "platform.python_implementation = lambda: 'PyPy'",
            "implementation: PyPy",
            "not attempted: the runtime is a CPython extension module",
        ),
        (
            "sys.version_info = (3, 9, 18, 'final', 0)",
            f"version: {platform.python_version()}",
            "not attempted: the runtime needs CPython 3.10 or newer",
        ),
        (
            "_config_variable = sysconfig.get_config_var\n"
            "sysconfig.get_config_var = lambda name: (\n"
            "    1 if name == 'Py_GIL_DISABLED' else _config_variable(name)\n"
            ")",
            "free-threaded: yes",
            "not attempted: the runtime is built for the GIL-enabled CPython",
        ),
    ],
    ids=["not-cpython", "below-3.10", "free-threaded"],
)
def test_an_interpreter_the_runtime_cannot_load_in_is_refused_before_importing_it(
    project_directory: Path,
    make_this_interpreter_unfit: str,
    reported_fact: str,
    reason_the_import_was_not_attempted: str,
):
    """The interpreter's own facts stand in for another one's: the bootstrap runs
    as `__main__` after they are rewritten, exactly as when run by path."""
    run_the_bootstrap_in_an_unfit_interpreter = (
        "import atexit, os, platform, runpy, sys, sysconfig\n"
        "atexit.register(lambda: os.write(\n"
        "    2, f\"RUNTIME_IMPORTED={'tatolab.runtime' in sys.modules}\\n\".encode()\n"
        "))\n"
        f"{make_this_interpreter_unfit}\n"
        f"runpy.run_path({str(BOOTSTRAP_PATH)!r}, run_name='__main__')\n"
    )

    refused = subprocess.run(
        [sys.executable, "-c", run_the_bootstrap_in_an_unfit_interpreter],
        cwd=project_directory,
        env=processor_interpreter_environment(LEND_DIRECTORY, project_directory),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=SECONDS_A_BOOTSTRAP_HAS_TO_ANSWER,
        check=False,
    )

    assert refused.returncode == 1, refused.stderr
    assert refused.stdout == ""
    assert "cannot load the lent runtime" in refused.stderr
    assert f"interpreter: {sys.executable}" in refused.stderr
    assert reported_fact in refused.stderr
    assert f"architecture: {platform.machine()}" in refused.stderr
    assert f"import error: {reason_the_import_was_not_attempted}" in refused.stderr
    assert "RUNTIME_IMPORTED=False" in refused.stderr


def test_the_bootstrap_parses_as_python_3_7():
    """An older interpreter must reach the refusal rather than a SyntaxError."""
    ast.parse(BOOTSTRAP_PATH.read_text(encoding="utf-8"), feature_version=(3, 7))


def test_the_bootstrap_drops_its_own_directory_from_the_import_path(project_directory: Path):
    """Run by path, CPython puts `tatolab/runtime/` first on `sys.path`, where its
    modules would shadow the project's own top-level names."""
    write_project_module(
        project_directory,
        "shadow_probe_nodes",
        """
        import importlib.util

        from tatolab.stream import node

        BESIDE_THE_BOOTSTRAP = ("_processor_hosting", "_node_registry", "testing")


        @node(
            execution="manual",
            description=",".join(
                name for name in BESIDE_THE_BOOTSTRAP if importlib.util.find_spec(name)
            )
        )
        class ShadowProbe:
            @node.output()
            def nothing_to_downstream(self) -> None: ...

            def process(self, ctx) -> None: ...
        """,
    )

    described = describe(project_directory, "shadow_probe_nodes:ShadowProbe")

    assert described.returncode == 0, described.stderr
    (shadow_probe,) = described_document(described)["described_node_types"]
    assert shadow_probe["description"] == "", (
        f"importable from beside the bootstrap: {shadow_probe['description']}"
    )


def test_importing_the_bootstrap_leaves_the_import_path_alone():
    importing = subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys\n"
            "before = list(sys.path)\n"
            "import tatolab.runtime._processor_interpreter_bootstrap\n"
            "assert sys.path == before, (before, sys.path)\n",
        ],
        capture_output=True,
        text=True,
        timeout=SECONDS_A_BOOTSTRAP_HAS_TO_ANSWER,
        check=False,
    )

    assert importing.returncode == 0, importing.stderr

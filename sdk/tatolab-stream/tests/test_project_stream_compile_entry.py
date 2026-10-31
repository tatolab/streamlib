# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The compile entry the runtime runs in the project's interpreter for `tatolab run` and
`tatolab dev`.

Each test runs it as the runtime does: `python -I -m` in a child, with the anchor as
the working directory, reading the one JSON document on stdout — a compiled stream, or
the traceback of the code that gave none — the refusals on stderr, and the exit code.
Nothing here needs a runtime.
"""

import json
import os
import shlex
import shutil
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any, Optional

import pytest

COMPILE_ENTRY_MODULE = "tatolab.stream._project_stream_compile_entry"
SCAFFOLD_TEMPLATE_DIRECTORY = Path(__file__).resolve().parents[1] / "scaffold_template"
# Bounded: a compile imports a few modules and runs one function.
COMPILE_ENTRY_TIMEOUT_SECONDS = 60.0

MINIMAL_STREAM_SOURCE = (
    "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def main(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
)
APP_PY_SOURCE_DEFINING_ONLY_A_SETUP_FUNCTION = "def setup(runtime):\n    pass\n"

FRONT_STREAM_SOURCE = (
    "from tatolab.stream import H264Encoder, StreamBuilder, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def front(stream_builder: StreamBuilder) -> None:\n"
    '    """Front pattern, encoded to H.264.\n'
    "\n"
    '    The second paragraph is not listed."""\n'
    "    source = stream_builder.add(TestPatternSource)\n"
    "    encoder = stream_builder.add(H264Encoder)\n"
    '    stream_builder.connect(source.output("video"), encoder.input("video"))\n'
)
TWO_STREAM_SOURCE = FRONT_STREAM_SOURCE + (
    "\n"
    "\n"
    "@stream\n"
    "def back(stream_builder: StreamBuilder) -> None:\n"
    '    stream_builder.add(TestPatternSource, name="Back Pattern")\n'
)

FRONT_STREAM_GRAPH = {
    "stream": "front",
    "nodes": [
        {"name": "testpatternsource", "type": "tatolab.stream:TestPatternSource", "config": {}},
        {"name": "h264encoder", "type": "tatolab.stream:H264Encoder", "config": {}},
    ],
    "links": [
        {
            "source": {"node": "testpatternsource", "port": "video"},
            "target": {"node": "h264encoder", "port": "video"},
        }
    ],
    "exposed": [],
}
BACK_STREAM_GRAPH = {
    "stream": "back",
    "nodes": [
        {"name": "back-pattern", "type": "tatolab.stream:TestPatternSource", "config": {}}
    ],
    "links": [],
    "exposed": [],
}


@pytest.fixture
def anchor_directory(tmp_path: Path) -> Path:
    """The project directory, as its own working directory reports it — resolved."""
    return tmp_path.resolve()


def write_app(directory: Path, file_name: str, source: str) -> Path:
    entry_file = directory / file_name
    entry_file.parent.mkdir(parents=True, exist_ok=True)
    entry_file.write_text(source, encoding="utf-8")
    return entry_file


def run_compile_entry(
    anchor_directory: Path,
    *arguments: str,
    verb: Optional[str] = "run",
) -> "subprocess.CompletedProcess[str]":
    """The compile entry as `tatolab <verb>` starts it, in `anchor_directory`."""
    verb_arguments = ["--verb", verb] if verb is not None else []
    return subprocess.run(
        [sys.executable, "-I", "-m", COMPILE_ENTRY_MODULE, *verb_arguments, *arguments],
        cwd=anchor_directory,
        capture_output=True,
        text=True,
        timeout=COMPILE_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )


def the_one_document_of(
    finished: "subprocess.CompletedProcess[str]", document_keys: "set[str]"
) -> "dict[str, Any]":
    """The one JSON object the compile wrote to stdout, holding exactly `document_keys`."""
    assert finished.returncode == 0, finished.stderr
    assert finished.stdout.count("\n") == 1 and finished.stdout.endswith("\n"), finished.stdout
    document = json.loads(finished.stdout)
    assert set(document) == document_keys, document
    return document


def every_stream_document(finished: "subprocess.CompletedProcess[str]") -> "dict[str, Any]":
    """The document of a compile of every stream the project's `stream.py` defines."""
    return the_one_document_of(finished, {"project_directory", "streams"})


def _the_one_stream_holding(
    finished: "subprocess.CompletedProcess[str]", stream_keys: "set[str]"
) -> "dict[str, Any]":
    """The one stream a compile wrote, holding exactly `stream_keys` beside the project.

    A compile naming no target writes every stream `stream.py` defines; one naming a target
    writes that stream alone. Either reads here as the stream's keys and the project's.
    """
    document = json.loads(finished.stdout) if finished.returncode == 0 else {}
    if "streams" not in document:
        return the_one_document_of(finished, stream_keys | {"project_directory"})
    streams = every_stream_document(finished)["streams"]
    assert len(streams) == 1, streams
    (stream,) = streams
    assert set(stream) == stream_keys | {"description"}, stream
    return {
        "project_directory": document["project_directory"],
        **{key: stream[key] for key in stream_keys},
    }


def compiled_document(finished: "subprocess.CompletedProcess[str]") -> "dict[str, Any]":
    """The one stream a compile that succeeded writes to stdout."""
    return _the_one_stream_holding(finished, {"stream_graph", "stream_function"})


def stream_function_failure_document(
    finished: "subprocess.CompletedProcess[str]",
) -> "dict[str, Any]":
    """The one stream function that raised while it compiled."""
    return _the_one_stream_holding(finished, {"failed_because", "stream_function"})


def no_stream_defined_because_of(finished: "subprocess.CompletedProcess[str]") -> str:
    """Why an entry that imported gives no stream."""
    return the_one_document_of(finished, {"no_stream_defined_because", "project_directory"})[
        "no_stream_defined_because"
    ]


def stream_function_not_defined_document(
    finished: "subprocess.CompletedProcess[str]",
) -> "dict[str, Any]":
    """The document of a target naming a function the entry does not define as a stream."""
    return the_one_document_of(
        finished, {"not_defined_because", "project_directory", "stream_function"}
    )


def import_failure_of(finished: "subprocess.CompletedProcess[str]") -> str:
    """The traceback of an entry that raised before any stream function was chosen."""
    return the_one_document_of(finished, {"import_failed_because", "project_directory"})[
        "import_failed_because"
    ]


def refusal_of(finished: "subprocess.CompletedProcess[str]") -> str:
    """A refusal's stderr, once it is known to have been one: exit 1, nothing on stdout."""
    assert finished.returncode == 1, finished.stdout + finished.stderr
    assert finished.stdout == "", "a refused compile writes no document"
    assert "Traceback (most recent call last)" not in finished.stderr, finished.stderr
    return finished.stderr


# ---------------------------------------------------------------------------
# Entry resolution — the `stream.py` convention
# ---------------------------------------------------------------------------


def test_no_args_resolves_the_conventional_stream_entry_at_the_anchor(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory))

    assert document["stream_graph"]["stream"] == "main"
    assert document["project_directory"] == str(anchor_directory)


def test_stream_py_resolves_beside_an_app_py(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(anchor_directory, "app.py", APP_PY_SOURCE_DEFINING_ONLY_A_SETUP_FUNCTION)

    document = compiled_document(run_compile_entry(anchor_directory, verb="dev"))

    assert document["stream_graph"]["stream"] == "main"


def test_a_directory_holding_only_an_app_py_is_refused_naming_stream_py(
    anchor_directory: Path,
):
    write_app(anchor_directory, "app.py", APP_PY_SOURCE_DEFINING_ONLY_A_SETUP_FUNCTION)

    message = refusal_of(run_compile_entry(anchor_directory, verb="dev"))

    assert f"error: no `stream.py` in `{anchor_directory}`, only an `app.py`" in message
    assert (
        "`tatolab dev` loads the @stream functions of `stream.py`, and never reads "
        "`app.py`." in message
    )
    assert "    @stream\n    def main(stream_builder: StreamBuilder) -> None:\n" in message, (
        "the refusal must show the shape `stream.py` holds"
    )
    assert "-f <file>" not in message, "a project's entry is always `stream.py`"
    assert "--dir <project-root>" not in message, (
        "the directory is the project root; only its entry file is wrong"
    )


def test_a_missing_conventional_entry_names_the_convention_and_the_anchor(anchor_directory: Path):
    message = refusal_of(run_compile_entry(anchor_directory, verb="dev"))

    assert f"no `stream.py` in `{anchor_directory}`\n" in message, (
        "the error must name the convention and the anchor it searched"
    )
    assert "app.py" not in message, "a directory with no `app.py` hears nothing of one"
    assert "tatolab dev" in message, "the error must name the verb the user typed"
    assert "-f <file>" not in message, "a project's entry is always `stream.py`"
    assert "--dir <project-root>" in message
    assert "never searches parent directories" in message


def test_resolution_never_walks_up_to_a_parent(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(anchor_directory, "app.py", APP_PY_SOURCE_DEFINING_ONLY_A_SETUP_FUNCTION)
    nested = anchor_directory / "nested"
    nested.mkdir()

    message = refusal_of(run_compile_entry(nested))

    assert "never searches parent" in message
    assert "only an `app.py`" not in message, (
        "a parent's `app.py` must not draw the app.py-only refusal in a nested directory"
    )


def test_a_directory_named_like_the_entry_is_not_an_entry(anchor_directory: Path):
    (anchor_directory / "stream.py").mkdir()
    (anchor_directory / "app.py").mkdir()

    message = refusal_of(run_compile_entry(anchor_directory))

    assert "only an `app.py`" not in message


def test_the_anchor_is_the_working_directory_and_dir_only_spells_it(
    anchor_directory: Path,
):
    """`tatolab` starts the entry in the anchor and passes `--dir` as the user typed
    it, relative to a shell this process never saw."""
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)

    document = every_stream_document(run_compile_entry(anchor_directory, "--dir", "../front rig"))

    assert document["project_directory"] == str(anchor_directory)


# ---------------------------------------------------------------------------
# The positional target — `<file>.py[:<function>]` or `<module>:<function>`
# ---------------------------------------------------------------------------


def test_a_file_target_resolves_against_the_anchor(anchor_directory: Path):
    write_app(anchor_directory, "rigs/front.py", MINIMAL_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "rigs/front.py"))

    assert document["stream_graph"]["stream"] == "main"


def test_a_file_target_may_name_its_function(anchor_directory: Path):
    absolute_entry = write_app(anchor_directory, "front.py", TWO_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, f"{absolute_entry}:back"))

    assert document["stream_graph"] == BACK_STREAM_GRAPH


def test_a_module_target_names_a_module_and_its_function(anchor_directory: Path):
    write_app(anchor_directory, "rigs/__init__.py", "")
    write_app(anchor_directory, "rigs/front.py", TWO_STREAM_SOURCE)

    document = compiled_document(
        run_compile_entry(anchor_directory, "rigs.front:front", verb="dev")
    )

    assert document["stream_graph"] == FRONT_STREAM_GRAPH


def test_a_missing_file_target_names_the_path_it_tried(anchor_directory: Path):
    message = refusal_of(run_compile_entry(anchor_directory, "gone.py:main"))

    assert str(anchor_directory / "gone.py") in message
    assert "tatolab run gone.py:main" in message, "the error names what the user typed"


@pytest.mark.parametrize(
    "malformed_target", ["main", "front.py:", ":main", "rigs/front:main", "rigs.front:"]
)
def test_a_target_in_no_known_form_is_refused_naming_the_forms(
    anchor_directory: Path, malformed_target: str
):
    message = refusal_of(run_compile_entry(anchor_directory, malformed_target))

    assert f"`{malformed_target}` names no stream" in message
    assert "tatolab run <file>.py:<function>" in message
    assert "tatolab run <module>:<function>" in message


def test_the_entry_runs_as_main_with_its_own_directory_importable(anchor_directory: Path):
    """`tatolab dev` and `python stream.py` must be the same arrangement.

    An app importing its own `nodes/` package is the case that breaks if the
    entry's directory is not what leads `sys.path`.
    """
    write_app(anchor_directory, "nodes/__init__.py", "")
    write_app(anchor_directory, "nodes/effect.py", "EFFECT_NAME = 'blur'\n")
    write_app(
        anchor_directory,
        "stream.py",
        "import sys\n"
        "from nodes.effect import EFFECT_NAME\n"
        "assert EFFECT_NAME == 'blur'\n"
        "assert __name__ == '__main__', __name__\n"
        f"assert sys.path[0] == {str(anchor_directory)!r}, sys.path\n" + MINIMAL_STREAM_SOURCE,
    )

    compiled_document(run_compile_entry(anchor_directory, verb="dev"))


def test_a_file_entry_in_a_subdirectory_cannot_import_from_the_anchor(anchor_directory: Path):
    """Its processor interpreters start in its own directory, so compiling must too —
    the working directory is never on the path."""
    write_app(anchor_directory, "helpers.py", "HELPER = 1\n")
    write_app(
        anchor_directory,
        "rigs/desk.py",
        "import helpers\n" + MINIMAL_STREAM_SOURCE,
    )

    import_failure = import_failure_of(run_compile_entry(anchor_directory, "rigs/desk.py"))

    assert "ModuleNotFoundError: No module named 'helpers'" in import_failure


def test_the_app_does_not_see_the_launchers_arguments(anchor_directory: Path):
    """`sys.argv` belongs to the app, not to the compile entry `tatolab` started."""
    argv_probe = anchor_directory / "argv.json"
    entry_file = write_app(
        anchor_directory,
        "stream.py",
        "import json, sys\n"
        f"open({str(argv_probe)!r}, 'w').write(json.dumps(sys.argv))\n" + MINIMAL_STREAM_SOURCE,
    )

    compiled_document(run_compile_entry(anchor_directory, "--dir", "desk"))

    assert json.loads(argv_probe.read_text()) == [str(entry_file)], (
        "the app must see only its own path, as `python stream.py` gives it"
    )


def test_a_module_target_sees_its_own_file_as_argv(anchor_directory: Path):
    """`sys.argv` is the module's file while it imports, as `python -m` sets it."""
    argv_probe = anchor_directory / "argv.json"
    write_app(anchor_directory, "argv_probe_rigs/__init__.py", "")
    write_app(
        anchor_directory,
        "argv_probe_rigs/desk.py",
        "import json, sys\n"
        f"open({str(argv_probe)!r}, 'w').write(json.dumps(sys.argv))\n" + MINIMAL_STREAM_SOURCE,
    )

    compiled_document(run_compile_entry(anchor_directory, "argv_probe_rigs.desk:main"))

    assert json.loads(argv_probe.read_text()) == [
        str(anchor_directory / "argv_probe_rigs" / "desk.py")
    ]


def test_what_the_app_prints_goes_to_stderr_and_stdout_carries_only_the_document(
    anchor_directory: Path,
):
    write_app(
        anchor_directory,
        "stream.py",
        "import os, subprocess, sys\n"
        "print('printed at import')\n"
        "os.write(1, b'written to descriptor one\\n')\n"
        "subprocess.run([sys.executable, '-c', 'print(\"a child printed\")'], check=True)\n"
        "\n" + MINIMAL_STREAM_SOURCE + "    print('printed by the stream function')\n",
    )

    finished = run_compile_entry(anchor_directory)

    assert compiled_document(finished)["stream_graph"]["stream"] == "main"
    for printed in (
        "printed at import",
        "written to descriptor one",
        "a child printed",
        "printed by the stream function",
    ):
        assert printed in finished.stderr


# ---------------------------------------------------------------------------
# Selecting and compiling a stream
# ---------------------------------------------------------------------------


def test_the_compile_entry_prints_the_stream_graph_and_its_project_directory(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", FRONT_STREAM_SOURCE)

    finished = run_compile_entry(anchor_directory, verb="dev")

    assert compiled_document(finished) == {
        "stream_graph": FRONT_STREAM_GRAPH,
        "project_directory": str(anchor_directory),
        "stream_function": "stream:front",
    }
    assert finished.stderr == "", "a clean compile says nothing on stderr"


def test_a_file_target_with_a_function_compiles_that_stream(anchor_directory: Path):
    write_app(anchor_directory, "rig.py", TWO_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "rig.py:back"))

    assert document["stream_graph"] == BACK_STREAM_GRAPH


def test_a_file_target_without_a_function_takes_its_sole_stream(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)
    write_app(anchor_directory, "solo.py", MINIMAL_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "solo.py"))

    assert document["stream_graph"]["stream"] == "main"


def test_a_module_target_compiles_with_the_anchor_as_the_project(anchor_directory: Path):
    """The module imports with the anchor leading `sys.path`, and the anchor is the
    project directory its processor interpreters start in."""
    write_app(anchor_directory, "module_target_rigs/__init__.py", "")
    write_app(anchor_directory, "module_target_rigs/desk.py", TWO_STREAM_SOURCE)

    document = compiled_document(
        run_compile_entry(anchor_directory, "module_target_rigs.desk:front", verb="dev")
    )

    assert document == {
        "stream_graph": FRONT_STREAM_GRAPH,
        "project_directory": str(anchor_directory),
        "stream_function": "module_target_rigs.desk:front",
    }


def test_a_file_target_in_a_subdirectory_compiles_with_that_subdirectory_as_the_project(
    anchor_directory: Path,
):
    write_app(anchor_directory, "rigs/desk.py", TWO_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "rigs/desk.py:back"))

    assert document == {
        "stream_graph": BACK_STREAM_GRAPH,
        "project_directory": str(anchor_directory / "rigs"),
        "stream_function": "desk:back",
    }


# ---------------------------------------------------------------------------
# The stream function the document reports, half of the stream's location
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("arguments", "stream_function"),
    [
        ((), "stream:main"),
        (("stream.py",), "stream:main"),
        (("stream.py:main",), "stream:main"),
        (("stream:main",), "stream:main"),
        (("rig.py:back",), "rig:back"),
        (("solo.py",), "solo:main"),
        (("located_rigs.desk:front",), "located_rigs.desk:front"),
    ],
)
def test_the_document_reports_the_stream_function_as_module_and_qualname_never_main(
    anchor_directory: Path, arguments: "tuple[str, ...]", stream_function: str
):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(anchor_directory, "rig.py", TWO_STREAM_SOURCE)
    write_app(anchor_directory, "solo.py", MINIMAL_STREAM_SOURCE)
    write_app(anchor_directory, "located_rigs/__init__.py", "")
    write_app(anchor_directory, "located_rigs/desk.py", TWO_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, *arguments))

    assert document["stream_function"] == stream_function


def test_an_entry_file_in_a_subdirectory_reports_its_module_relative_to_its_own_directory(
    anchor_directory: Path,
):
    write_app(anchor_directory, "rigs/desk.py", MINIMAL_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "rigs/desk.py"))

    assert document["project_directory"] == str(anchor_directory / "rigs")
    assert document["stream_function"] == "desk:main"


def test_a_stream_imported_into_the_entry_file_reports_the_module_that_defines_it(
    anchor_directory: Path,
):
    write_app(anchor_directory, "imported_location_rigs.py", TWO_STREAM_SOURCE)
    write_app(
        anchor_directory,
        "stream.py",
        "from imported_location_rigs import front\n" + MINIMAL_STREAM_SOURCE,
    )

    document = compiled_document(run_compile_entry(anchor_directory, "stream.py:front"))

    assert document["stream_function"] == "imported_location_rigs:front"


@pytest.mark.parametrize("missing_module_name", ["no_such_rig_module", "no_such_rig_package.desk"])
def test_a_module_target_that_does_not_import_is_refused_naming_the_anchor(
    anchor_directory: Path, missing_module_name: str
):
    message = refusal_of(run_compile_entry(anchor_directory, f"{missing_module_name}:main"))

    assert f"no module `{missing_module_name}` is importable" in message
    assert str(anchor_directory) in message
    assert "`tatolab run <file>.py:<function>`" in message


def test_a_module_target_another_module_shadows_is_refused_naming_both_files(
    anchor_directory: Path,
):
    """`os` is imported before any project code can be, so `import os` never reaches
    the project's."""
    write_app(anchor_directory, "os.py", MINIMAL_STREAM_SOURCE)

    message = refusal_of(run_compile_entry(anchor_directory, "os:main"))

    assert (
        f"`os` (from `tatolab run os:main`) resolves to "
        f"`{os.__spec__.origin}`, not to `os.py` in `{anchor_directory}`"
    ) in message
    assert "Rename the project's module, or launch its file instead: " in message
    assert "`tatolab run os.py:main`" in message

    document = compiled_document(run_compile_entry(anchor_directory, "os.py:main"))
    assert document["stream_graph"]["stream"] == "main"


def test_a_module_target_whose_parent_package_is_held_elsewhere_names_that_package(
    anchor_directory: Path,
):
    """The compile entry imports `json`, so `json.desk` is searched inside the
    stdlib's `json`, never the project's — though `json/desk.py` is there, as a
    namespace portion the regular package wins over."""
    write_app(anchor_directory, "json/desk.py", MINIMAL_STREAM_SOURCE)

    message = refusal_of(
        run_compile_entry(anchor_directory, "--dir", str(anchor_directory), "json.desk:main")
    )

    assert (
        f"`json.desk` (from `tatolab run json.desk:main`) does not resolve to "
        f"`json/desk.py` in `{anchor_directory}`: its parent package `json` resolves to "
        f"`{json.__file__}`, outside the project"
    ) in message
    assert "no module `json.desk` is importable" not in message
    assert (
        f"Rename the project's package, or launch its file instead: "
        f"`tatolab run --dir {anchor_directory} json/desk.py:main`."
    ) in message

    document = compiled_document(run_compile_entry(anchor_directory, "json/desk.py:main"))
    assert document["stream_graph"]["stream"] == "main"


def test_a_module_target_naming_the_running_main_module_is_refused_naming_the_file_form(
    anchor_directory: Path,
):
    """The compile entry itself is `__main__`, so that name holds no module of the project's."""
    write_app(anchor_directory, "__main__.py", MINIMAL_STREAM_SOURCE)

    message = refusal_of(
        run_compile_entry(anchor_directory, "--dir", str(anchor_directory), "__main__:main")
    )

    assert (
        "error: `__main__` (from `tatolab run __main__:main`) is a module already running"
    ) in message
    assert (
        f"Name the file that defines the stream instead: "
        f"`tatolab run --dir {anchor_directory} <file>.py:main`."
    ) in message


def test_a_module_targets_raising_parent_package_is_the_first_frame_of_its_traceback(
    anchor_directory: Path,
):
    """Locating a dotted target runs its parent packages through `importlib.util`,
    frozen since CPython 3.11 — none of those frames may sit above the package's."""
    write_app(
        anchor_directory, "raising_parent_rigs/__init__.py", "raise ValueError('bad package')\n"
    )
    write_app(anchor_directory, "raising_parent_rigs/desk.py", MINIMAL_STREAM_SOURCE)

    import_failure = import_failure_of(
        run_compile_entry(anchor_directory, "raising_parent_rigs.desk:main")
    )

    assert "ValueError: bad package" in import_failure
    first_frame = import_failure.index("File ")
    assert import_failure.startswith(
        f'File "{anchor_directory / "raising_parent_rigs" / "__init__.py"}", line 1, in <module>',
        first_frame,
    ), import_failure
    assert "importlib" not in import_failure, import_failure


def test_a_module_targets_function_it_lacks_names_the_file_its_module_resolved_to(
    anchor_directory: Path,
):
    write_app(anchor_directory, "refusal_naming_rigs/__init__.py", "")
    write_app(anchor_directory, "refusal_naming_rigs/desk.py", TWO_STREAM_SOURCE)

    document = stream_function_not_defined_document(
        run_compile_entry(
            anchor_directory, "--dir", str(anchor_directory), "refusal_naming_rigs.desk:side"
        )
    )
    message = document["not_defined_because"]

    assert document["stream_function"] == "refusal_naming_rigs.desk:side"

    assert (
        f"`refusal_naming_rigs.desk` (`{anchor_directory / 'refusal_naming_rigs' / 'desk.py'}`) "
        f"defines no @stream function named `side`"
    ) in message
    assert (
        f"Name one of them: `tatolab run --dir {anchor_directory} "
        f"refusal_naming_rigs.desk:<function>`."
    ) in message


def test_the_compile_takes_no_name_and_names_the_graph_by_its_function(anchor_directory: Path):
    ran = anchor_directory / "entry-ran.txt"
    write_app(
        anchor_directory,
        "stream.py",
        f"open({str(ran)!r}, 'w').write('ran')\n" + MINIMAL_STREAM_SOURCE,
    )

    refused = run_compile_entry(anchor_directory, "--name", "Front Rig")

    assert refused.returncode == 2, refused.stdout + refused.stderr
    assert "unrecognized arguments: --name" in refused.stderr
    assert not ran.exists(), "a refused flag must not cost the entry file a run"
    document = compiled_document(run_compile_entry(anchor_directory))
    assert document["stream_graph"]["stream"] == "main"


def test_every_stream_is_compiled_in_definition_order_with_its_description(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    document = every_stream_document(
        run_compile_entry(anchor_directory, "--every-stream", "--dir", str(anchor_directory))
    )

    assert document == {
        "project_directory": str(anchor_directory),
        "streams": [
            {
                "stream_function": "stream:front",
                "description": "Front pattern, encoded to H.264.\n\nThe second paragraph is not listed.",
                "stream_graph": FRONT_STREAM_GRAPH,
            },
            {"stream_function": "stream:back", "description": "", "stream_graph": BACK_STREAM_GRAPH},
        ],
    }


def test_no_target_compiles_every_stream_as_every_stream_does(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    assert every_stream_document(run_compile_entry(anchor_directory)) == every_stream_document(
        run_compile_entry(anchor_directory, "--every-stream")
    )


def test_every_stream_and_a_target_together_are_a_usage_error(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    refused = run_compile_entry(anchor_directory, "--every-stream", "stream.py:front")

    assert refused.returncode == 2, refused.stdout + refused.stderr
    assert "not allowed with argument" in refused.stderr


def test_a_raising_stream_is_its_own_failed_entry_beside_the_streams_that_compile(
    anchor_directory: Path,
):
    write_app(
        anchor_directory,
        "stream.py",
        TWO_STREAM_SOURCE
        + "\n\n@stream\ndef broken(stream_builder: StreamBuilder) -> None:\n"
        + '    """Raises."""\n    raise ValueError("bad wiring")\n',
    )

    streams = every_stream_document(run_compile_entry(anchor_directory, "--every-stream"))[
        "streams"
    ]

    assert [stream["stream_function"] for stream in streams] == [
        "stream:front",
        "stream:back",
        "stream:broken",
    ]
    assert streams[0]["stream_graph"] == FRONT_STREAM_GRAPH
    assert streams[1]["stream_graph"] == BACK_STREAM_GRAPH
    broken = streams[2]
    assert set(broken) == {"stream_function", "description", "failed_because"}, broken
    assert broken["description"] == "Raises."
    assert broken["failed_because"].startswith("Traceback (most recent call last):\n")
    assert broken["failed_because"].endswith("ValueError: bad wiring")
    assert "_project_stream_compile_entry.py" not in broken["failed_because"]


def test_an_entry_that_does_not_import_exits_with_its_traceback_under_every_stream(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", "raise RuntimeError('no camera')\n")

    import_failure = import_failure_of(run_compile_entry(anchor_directory, "--every-stream"))

    assert import_failure.endswith("RuntimeError: no camera")


def not_defined_because_of(finished: "subprocess.CompletedProcess[str]") -> str:
    """What a target naming a function the entry does not define as a stream was told."""
    return stream_function_not_defined_document(finished)["not_defined_because"]


@pytest.mark.parametrize(
    ("target_arguments", "message_of"),
    [(["my rig.py"], refusal_of), (["my rig.py:side"], not_defined_because_of)],
    ids=["several-streams", "a-function-the-file-lacks"],
)
def test_a_suggestion_quotes_an_entry_file_whose_name_holds_a_space(
    anchor_directory: Path,
    target_arguments: "list[str]",
    message_of: "Callable[[subprocess.CompletedProcess[str]], str]",
):
    write_app(anchor_directory, "my rig.py", TWO_STREAM_SOURCE)

    message = message_of(
        run_compile_entry(anchor_directory, "--dir", str(anchor_directory), *target_arguments)
    )

    suggestion = f"tatolab run --dir {anchor_directory} 'my rig.py':<function>"
    assert f"`{suggestion}`" in message
    _, verb, *suggested_arguments = shlex.split(suggestion.replace("<function>", "back"))
    document = compiled_document(run_compile_entry(anchor_directory, *suggested_arguments, verb=verb))
    assert document["stream_graph"] == BACK_STREAM_GRAPH


def test_a_second_name_bound_to_a_stream_is_still_one_stream(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE + "\n\ndefault = main\n")

    document = compiled_document(run_compile_entry(anchor_directory))

    assert document["stream_graph"]["stream"] == "main"


def test_a_target_naming_a_second_name_of_a_stream_defined_in_the_file_selects_it(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE + "\n\ndefault = back\n")

    document = compiled_document(run_compile_entry(anchor_directory, "stream.py:default"))

    assert document["stream_graph"]["stream"] == "back"


def test_an_entry_that_defines_no_stream_is_a_project_with_no_stream_and_a_sample(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", "PIPELINE = 1\n")

    message = no_stream_defined_because_of(run_compile_entry(anchor_directory, verb="dev"))

    assert "stream.py` defines no @stream function" in message
    assert "    @stream\n    def main(stream_builder: StreamBuilder) -> None:\n" in message
    assert "stream_builder.add(CameraSource)" in message
    assert "-f <file>" not in message


def test_a_file_target_that_defines_no_stream_answers_the_same_document(anchor_directory: Path):
    write_app(anchor_directory, "app.py", APP_PY_SOURCE_DEFINING_ONLY_A_SETUP_FUNCTION)

    message = no_stream_defined_because_of(run_compile_entry(anchor_directory, "app.py"))

    assert f"`{anchor_directory / 'app.py'}` defines no @stream function\n" in message
    assert "    @stream\n    def main(stream_builder: StreamBuilder) -> None:\n" in message


def test_a_named_function_the_file_lacks_is_reported_under_its_location_listing_its_streams(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    document = stream_function_not_defined_document(
        run_compile_entry(anchor_directory, "stream.py:side")
    )
    message = document["not_defined_because"]

    assert document["stream_function"] == "stream:side"
    assert document["project_directory"] == str(anchor_directory)

    assert "defines no @stream function named `side`" in message
    assert "    front — Front pattern, encoded to H.264.\n" in message
    assert "    back" in message


def test_a_named_function_in_a_file_with_no_stream_is_reported_naming_the_decorator(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", "PIPELINE = 1\n")

    document = stream_function_not_defined_document(
        run_compile_entry(anchor_directory, "stream.py:main")
    )
    message = document["not_defined_because"]

    assert document["stream_function"] == "stream:main"

    assert "defines no @stream function named `main`, nor any other" in message
    assert (
        "`@stream` above a module-level `def main(stream_builder: StreamBuilder) -> None:`"
        in message
    )


def test_a_named_function_that_is_not_a_stream_is_reported_naming_the_fix(anchor_directory: Path):
    write_app(
        anchor_directory,
        "stream.py",
        MINIMAL_STREAM_SOURCE
        + "\n\ndef helper(stream_builder: StreamBuilder) -> None:\n    pass\n",
    )

    message = stream_function_not_defined_document(
        run_compile_entry(anchor_directory, "stream.py:helper")
    )["not_defined_because"]

    assert "`helper` in" in message and "is not a @stream function" in message
    assert "decorate it with `@stream`" in message
    assert "(stream_builder: StreamBuilder) -> None:" in message


def test_a_stream_imported_into_the_entry_compiles_when_a_target_names_it(
    anchor_directory: Path,
):
    """An imported stream is never the entry's sole stream, yet a target may name it."""
    write_app(anchor_directory, "imported_rigs.py", TWO_STREAM_SOURCE)
    write_app(
        anchor_directory, "stream.py", "from imported_rigs import front\n" + MINIMAL_STREAM_SOURCE
    )

    assert compiled_document(run_compile_entry(anchor_directory))["stream_graph"]["stream"] == (
        "main"
    )
    assert compiled_document(run_compile_entry(anchor_directory, "stream.py:front"))[
        "stream_graph"
    ] == FRONT_STREAM_GRAPH


def test_a_stream_imported_under_another_name_compiles_by_that_name(anchor_directory: Path):
    write_app(anchor_directory, "renamed_import_rigs.py", TWO_STREAM_SOURCE)
    write_app(
        anchor_directory,
        "stream.py",
        "from renamed_import_rigs import front as side\n" + MINIMAL_STREAM_SOURCE,
    )

    document = compiled_document(run_compile_entry(anchor_directory, "stream.py:side"))

    assert document["stream_graph"] == FRONT_STREAM_GRAPH


def test_a_package_target_compiles_a_stream_its_init_re_exports(anchor_directory: Path):
    """`acme_rover:camera_rig`, where `acme_rover/__init__.py` re-exports it."""
    write_app(
        anchor_directory,
        "re_exporting_rover/__init__.py",
        "from re_exporting_rover.streams import front\n",
    )
    write_app(anchor_directory, "re_exporting_rover/streams.py", TWO_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "re_exporting_rover:front"))

    assert document["stream_graph"] == FRONT_STREAM_GRAPH


def test_a_package_target_naming_a_value_that_is_not_a_stream_is_reported_naming_the_fix(
    anchor_directory: Path,
):
    write_app(
        anchor_directory,
        "re_exporting_helper_rover/__init__.py",
        "from re_exporting_helper_rover.helpers import wire_cameras\n",
    )
    write_app(
        anchor_directory,
        "re_exporting_helper_rover/helpers.py",
        "def wire_cameras(stream_builder):\n    pass\n",
    )

    document = stream_function_not_defined_document(
        run_compile_entry(anchor_directory, "re_exporting_helper_rover:wire_cameras")
    )
    message = document["not_defined_because"]

    assert document["stream_function"] == "re_exporting_helper_rover:wire_cameras"

    assert "`wire_cameras` in" in message and "is not a @stream function" in message
    assert "decorate it with `@stream`" in message
    assert "(stream_builder: StreamBuilder) -> None:" in message


def test_a_directory_holding_only_an_app_py_compiles_nothing(anchor_directory: Path):
    write_app(anchor_directory, "app.py", MINIMAL_STREAM_SOURCE)

    message = refusal_of(run_compile_entry(anchor_directory))

    assert "only an `app.py`" in message, (
        "the convention never compiles `app.py`, even one that defines a stream"
    )


def test_a_stream_that_adds_nothing_compiles_and_is_the_runtimes_to_refuse(
    anchor_directory: Path,
):
    write_app(
        anchor_directory,
        "stream.py",
        "from tatolab.stream import StreamBuilder, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None:\n"
        "    pass\n",
    )

    document = compiled_document(run_compile_entry(anchor_directory))

    assert document["stream_graph"] == {"stream": "main", "nodes": [], "links": [], "exposed": []}


def test_a_raising_stream_function_is_reported_under_its_location_with_its_traceback(
    anchor_directory: Path,
):
    write_app(
        anchor_directory,
        "stream.py",
        "from tatolab.stream import StreamBuilder, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None:\n"
        "    raise ValueError('bad wiring')\n",
    )

    document = stream_function_failure_document(run_compile_entry(anchor_directory, verb="dev"))
    failed_because = document["failed_because"]

    assert document["stream_function"] == "stream:main"
    assert document["project_directory"] == str(anchor_directory)
    assert failed_because.startswith("Traceback (most recent call last)"), failed_because
    assert failed_because.endswith("ValueError: bad wiring"), failed_because
    assert "_project_stream" not in failed_because, "the compile entry's frames are stripped"
    assert "_stream_graph_builder.py" not in failed_because, (
        "the compile call into the stream function is the launcher's, not the author's"
    )
    assert f'File "{anchor_directory / "stream.py"}", line 6, in main' in failed_because


def test_a_typed_duplicate_is_refused_on_the_authors_own_line(anchor_directory: Path):
    write_app(
        anchor_directory,
        "stream.py",
        "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None:\n"
        '    stream_builder.add(TestPatternSource, name="Pattern")\n'
        '    stream_builder.add(TestPatternSource, name="pattern")\n',
    )

    failed_because = stream_function_failure_document(run_compile_entry(anchor_directory))[
        "failed_because"
    ]

    first_frame = failed_because.index("File ")
    assert failed_because.startswith(
        f'File "{anchor_directory / "stream.py"}", line 7, in main', first_frame
    ), failed_because
    assert "casts to `pattern`" in failed_because


# ---------------------------------------------------------------------------
# The failure surfaces — a bad save must cost nothing
# ---------------------------------------------------------------------------


def test_a_syntax_error_is_reported_as_the_apps_traceback(anchor_directory: Path):
    write_app(
        anchor_directory,
        "stream.py",
        "from tatolab.stream import StreamBuilder, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None\n"
        "    pass\n",
    )

    import_failure = import_failure_of(run_compile_entry(anchor_directory, verb="dev"))

    assert "SyntaxError" in import_failure, import_failure
    assert "stream.py" in import_failure, "the traceback must name the file"


def test_a_bad_save_in_the_effect_module_names_that_module_not_the_entry_file(tmp_path: Path):
    """The bad save the scaffold invites.

    `stream.py` holds wiring the user rarely touches; the file they edit is the
    node module, which reaches the compile only as an import from the entry
    file. So the traceback has to walk through `stream.py` and land in the
    module — naming only the entry file would point at the wrong file.
    """
    app_directory = tmp_path.resolve() / "demo"
    shutil.copytree(SCAFFOLD_TEMPLATE_DIRECTORY, app_directory)
    (app_directory / "nodes" / "halftone_lens_effect.py").write_text(
        "def process(self ctx:\n    this does not parse\n"
    )

    import_failure = import_failure_of(run_compile_entry(app_directory, verb="dev"))

    assert "SyntaxError" in import_failure, import_failure
    assert "halftone_lens_effect.py" in import_failure, (
        f"the traceback must name the module the user edited:\n{import_failure}"
    )


def test_a_raise_at_import_time_surfaces_as_the_apps_traceback(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", "raise ValueError('bad wiring')\n")

    finished = run_compile_entry(anchor_directory)

    assert "ValueError: bad wiring" in import_failure_of(finished)
    assert json.loads(finished.stdout)["project_directory"] == str(anchor_directory)
    assert "bad wiring" not in finished.stderr, "the traceback is the document's alone"


def test_a_missing_entry_exits_without_a_python_traceback(anchor_directory: Path):
    """A missing `stream.py` is a usage error, not an internal failure."""
    message = refusal_of(run_compile_entry(anchor_directory, verb="dev"))

    assert "no `stream.py`" in message
    assert "never searches parent directories" in message


def test_the_apps_traceback_carries_none_of_the_launchers_frames(anchor_directory: Path):
    """The user's own line must be the first frame they read.

    `runpy` is frozen since CPython 3.11 and its frames report `<frozen runpy>`,
    so matching only `runpy.__file__` leaves its frames sitting on top of the
    app's.
    """
    write_app(anchor_directory, "stream.py", "raise ValueError('bad wiring')\n")

    import_failure = import_failure_of(run_compile_entry(anchor_directory))

    assert "runpy" not in import_failure, (
        f"no launcher frame may appear in the app's traceback:\n{import_failure}"
    )
    assert "_project_stream" not in import_failure, (
        f"the compile entry's own frames must be stripped:\n{import_failure}"
    )
    first_frame = import_failure.index("File ")
    assert import_failure.startswith(
        f'File "{anchor_directory / "stream.py"}", line 1, in <module>', first_frame
    ), import_failure


def test_an_app_that_exits_on_purpose_keeps_its_own_exit_code(anchor_directory: Path):
    """`sys.exit()` at module scope is a choice, not a failure to report."""
    write_app(anchor_directory, "stream.py", "import sys\nsys.exit(3)\n")

    finished = run_compile_entry(anchor_directory)

    assert finished.returncode == 3, f"stderr was:\n{finished.stderr}"
    assert finished.stdout == ""
    assert "error:" not in finished.stderr, (
        f"a deliberate exit must not be reported as a failure; stderr was:\n{finished.stderr}"
    )


def test_a_stream_function_that_exits_on_purpose_keeps_its_own_exit_code(
    anchor_directory: Path,
):
    """The same from inside the `@stream` function, which runs at compile time."""
    write_app(
        anchor_directory,
        "stream.py",
        "import sys\n"
        "\n"
        "from tatolab.stream import StreamBuilder, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None:\n"
        "    sys.exit(4)\n",
    )

    finished = run_compile_entry(anchor_directory)

    assert finished.returncode == 4, f"stderr was:\n{finished.stderr}"
    assert finished.stdout == ""
    assert "error:" not in finished.stderr, (
        f"a deliberate exit must not be reported as a failure; stderr was:\n{finished.stderr}"
    )


@pytest.mark.parametrize(
    "exit_call", ["sys.exit(0)", "sys.exit()"], ids=["exit-zero", "exit-no-code"]
)
@pytest.mark.parametrize("exits_from", ["module-scope", "stream-function"])
def test_an_app_that_exits_zero_on_purpose_exits_zero_with_no_document(
    anchor_directory: Path, exit_call: str, exits_from: str
):
    """Exit 0 with an empty stdout is the app's choice to exit: there is no stream to start."""
    if exits_from == "module-scope":
        source = f"import sys\n{exit_call}\n"
    else:
        source = (
            "import sys\n"
            "\n"
            "from tatolab.stream import StreamBuilder, stream\n"
            "\n"
            "\n"
            "@stream\n"
            "def main(stream_builder: StreamBuilder) -> None:\n"
            f"    {exit_call}\n"
        )
    write_app(anchor_directory, "stream.py", source)

    finished = run_compile_entry(anchor_directory)

    assert finished.returncode == 0, f"stderr was:\n{finished.stderr}"
    assert finished.stdout == ""
    assert "error:" not in finished.stderr, finished.stderr


def test_what_the_app_arranges_to_print_after_the_compile_never_follows_the_document(
    anchor_directory: Path,
):
    """An `atexit` handler and a non-daemon thread write after the document; both reach stderr."""
    write_app(
        anchor_directory,
        "stream.py",
        "import atexit\n"
        "import os\n"
        "import threading\n"
        "import time\n"
        "\n"
        "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
        "\n"
        "atexit.register(lambda: print('printed at exit'))\n"
        "\n"
        "\n"
        "def print_late() -> None:\n"
        "    time.sleep(0.5)\n"
        "    print('printed by a late thread', flush=True)\n"
        "    os.write(1, b'written to fd 1 by a late thread\\n')\n"
        "\n"
        "\n"
        "threading.Thread(target=print_late).start()\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None:\n"
        "    stream_builder.add(TestPatternSource)\n",
    )

    finished = run_compile_entry(anchor_directory)

    assert compiled_document(finished)["stream_graph"]["stream"] == "main"
    for late_output in (
        "printed at exit",
        "printed by a late thread",
        "written to fd 1 by a late thread",
    ):
        assert late_output in finished.stderr, finished.stderr


def test_a_project_module_named_like_one_the_entry_imports_cannot_replace_it(
    anchor_directory: Path,
):
    """The anchor's `json.py` never answers `json`: `-I` keeps the anchor off the path."""
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(
        anchor_directory,
        "json.py",
        "print('the project json module was imported')\n"
        "\n"
        "\n"
        "def dumps(*arguments, **keyword_arguments):\n"
        "    return 'HIJACKED'\n",
    )

    finished = run_compile_entry(anchor_directory)

    assert compiled_document(finished)["stream_graph"]["stream"] == "main"
    assert "the project json module was imported" not in finished.stdout + finished.stderr


def containers_nested_in(json_value: Any) -> int:
    """How many containers deep `json_value` nests, counting itself."""
    if isinstance(json_value, dict):
        return 1 + max((containers_nested_in(value) for value in json_value.values()), default=0)
    if isinstance(json_value, list):
        return 1 + max((containers_nested_in(value) for value in json_value), default=0)
    return 0


def test_a_config_at_the_deepest_a_graph_carries_compiles_to_a_document_serde_json_parses(
    anchor_directory: Path,
):
    """serde_json's default recursion limit parses at most 127 nested containers."""
    write_app(
        anchor_directory,
        "stream.py",
        "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
        "\n"
        "nested = []\n"
        "for _ in range(121):\n"
        "    nested = [nested]\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None:\n"
        "    stream_builder.add(TestPatternSource, config={'nested': nested})\n",
    )

    document = compiled_document(run_compile_entry(anchor_directory))

    assert containers_nested_in(document["stream_graph"]) == 126
    assert containers_nested_in(document) == 127


@pytest.mark.parametrize(
    "arguments",
    [[], ["--verb", "new"], ["--verb", "run", "--no-such-flag", "desk"]],
    ids=["no-verb", "a-verb-it-does-not-serve", "an-unknown-flag"],
)
def test_a_usage_error_exits_two(anchor_directory: Path, arguments: "list[str]"):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)

    finished = run_compile_entry(anchor_directory, *arguments, verb=None)

    assert finished.returncode == 2, finished.stdout + finished.stderr
    assert finished.stdout == ""
    assert "usage:" in finished.stderr


# ---------------------------------------------------------------------------
# The cross-floor check, before the entry runs
# ---------------------------------------------------------------------------


def test_the_block_is_printed_on_stderr_before_the_app_runs_and_the_stream_still_compiles(
    anchor_directory: Path,
):
    write_app(
        anchor_directory,
        "processors/effect.py",
        'import cupy\nimport torch\ndevice = torch.device("cuda")\n',
    )
    write_app(anchor_directory, "stream.py", "print('the entry file ran')\n" + MINIMAL_STREAM_SOURCE)

    finished = run_compile_entry(anchor_directory, verb="dev")

    assert compiled_document(finished)["stream_graph"]["stream"] == "main", (
        "a finding must never keep the stream from compiling"
    )
    assert "processors/effect.py:1: imports `cupy`" in finished.stderr
    assert "processors/effect.py:3: names the device 'cuda'" in finished.stderr
    assert finished.stderr.index("cross-floor check") < finished.stderr.index(
        "the entry file ran"
    ), "the block is printed between resolving the entry file and executing it"


def test_a_check_that_fails_is_reported_and_the_stream_still_compiles(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)
    compile_entry_with_a_failing_check = (
        "import runpy\n"
        "from tatolab.stream import _cross_floor_check\n"
        "def failing_check(app_directory):\n"
        "    raise RuntimeError('a defect in the check')\n"
        "_cross_floor_check.check_app_directory_for_floor_bindings = failing_check\n"
        f"runpy.run_module({COMPILE_ENTRY_MODULE!r}, run_name='__main__', alter_sys=True)\n"
    )

    finished = subprocess.run(
        [sys.executable, "-I", "-c", compile_entry_with_a_failing_check, "--verb", "run"],
        cwd=anchor_directory,
        capture_output=True,
        text=True,
        timeout=COMPILE_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )

    assert compiled_document(finished)["stream_graph"]["stream"] == "main", (
        "a failing check must never keep the stream from compiling"
    )
    assert (
        "tatolab: the cross-floor check could not run: RuntimeError('a defect in the check')"
        in finished.stderr
    )


def test_a_clean_compile_prints_nothing_extra(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)

    finished = run_compile_entry(anchor_directory)

    assert compiled_document(finished)["stream_graph"]["stream"] == "main"
    assert finished.stderr == ""

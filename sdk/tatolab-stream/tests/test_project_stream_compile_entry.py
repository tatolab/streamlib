# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The compile entry `tatolab run` and `tatolab dev` run in the project's interpreter.

Each test runs it as `tatolab` does: `python -m` in a child, with the anchor as the
working directory, reading the one JSON document on stdout, the refusals and
tracebacks on stderr, and the exit code. Nothing here needs a runtime.
"""

import json
import os
import shlex
import shutil
import subprocess
import sys
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
    "from tatolab.stream import DisplayWindow, StreamBuilder, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def front(stream_builder: StreamBuilder) -> None:\n"
    '    """Front camera, in a window.\n'
    "\n"
    '    The second paragraph is not listed."""\n'
    "    source = stream_builder.add(TestPatternSource)\n"
    "    window = stream_builder.add(DisplayWindow)\n"
    '    stream_builder.connect(source.output("video"), window.input("video"))\n'
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
        {"name": "displaywindow", "type": "tatolab.stream:DisplayWindow", "config": {}},
    ],
    "links": [
        {
            "source": {"node": "testpatternsource", "port": "video"},
            "target": {"node": "displaywindow", "port": "video"},
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
    anchor_directory: Path, *arguments: str, verb: Optional[str] = "run"
) -> "subprocess.CompletedProcess[str]":
    """The compile entry as `tatolab <verb>` starts it, in `anchor_directory`."""
    verb_arguments = ["--verb", verb] if verb is not None else []
    return subprocess.run(
        [sys.executable, "-m", COMPILE_ENTRY_MODULE, *verb_arguments, *arguments],
        cwd=anchor_directory,
        capture_output=True,
        text=True,
        timeout=COMPILE_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )


def compiled_document(finished: "subprocess.CompletedProcess[str]") -> "dict[str, Any]":
    """The one JSON object a compile that succeeded writes to stdout."""
    assert finished.returncode == 0, finished.stderr
    assert finished.stdout.count("\n") == 1 and finished.stdout.endswith("\n"), finished.stdout
    document = json.loads(finished.stdout)
    assert set(document) == {"stream_graph", "project_directory"}, document
    return document


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


def test_a_directory_holding_only_an_app_py_is_refused_naming_stream_py_and_the_file_flag(
    anchor_directory: Path,
):
    write_app(anchor_directory, "app.py", APP_PY_SOURCE_DEFINING_ONLY_A_SETUP_FUNCTION)

    message = refusal_of(run_compile_entry(anchor_directory, verb="dev"))

    assert f"error: no `stream.py` in `{anchor_directory}`, only an `app.py`" in message
    assert (
        "`tatolab dev` launches a @stream function from `stream.py`, and reads "
        "`app.py` only when `-f` or a target names it." in message
    )
    assert "    @stream\n    def main(stream_builder: StreamBuilder) -> None:\n" in message, (
        "the refusal must show the shape `stream.py` holds"
    )
    assert "-f <file>" in message, "the refusal must offer the `-f` override"
    assert "tatolab dev <file>.py[:<function>]" in message
    assert "--dir <project-root>" not in message, (
        "the directory is the project root; only its entry file is wrong"
    )


def test_explicit_entry_file_overrides_the_convention(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", FRONT_STREAM_SOURCE)
    write_app(anchor_directory, "other.py", MINIMAL_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "-f", "other.py"))

    assert document["stream_graph"]["stream"] == "main"


def test_explicit_entry_file_may_be_absolute(anchor_directory: Path):
    absolute_entry = write_app(anchor_directory, "elsewhere.py", MINIMAL_STREAM_SOURCE)

    document = compiled_document(
        run_compile_entry(anchor_directory, "-f", str(absolute_entry), verb="dev")
    )

    assert document["stream_graph"]["stream"] == "main"


def test_a_missing_conventional_entry_names_the_convention_and_the_anchor(anchor_directory: Path):
    message = refusal_of(run_compile_entry(anchor_directory, verb="dev"))

    assert f"no `stream.py` in `{anchor_directory}`\n" in message, (
        "the error must name the convention and the anchor it searched"
    )
    assert "app.py" not in message, "a directory with no `app.py` hears nothing of one"
    assert "tatolab dev" in message, "the error must name the verb the user typed"
    assert "-f <file>" in message, "the error must offer the `-f` escape hatch"
    assert "tatolab dev <file>.py[:<function>]" in message
    assert "--dir <project-root>" in message
    assert "never searches parent directories" in message


def test_a_missing_explicit_entry_names_the_path_it_tried(anchor_directory: Path):
    message = refusal_of(run_compile_entry(anchor_directory, "-f", "gone.py"))

    assert f"no entry file at `{anchor_directory / 'gone.py'}` (from `-f gone.py`)" in message


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
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    message = refusal_of(run_compile_entry(anchor_directory, "--dir", "../front rig"))

    assert f"`{anchor_directory / 'stream.py'}` defines 2 @stream functions" in message
    assert "`tatolab run --dir '../front rig' stream.py:<function>`" in message


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


def test_a_target_and_an_entry_file_together_are_refused_naming_both(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(anchor_directory, "other.py", MINIMAL_STREAM_SOURCE)

    message = refusal_of(
        run_compile_entry(anchor_directory, "-f", "other.py", "stream.py:main", verb="dev")
    )

    assert "`-f other.py`" in message
    assert "`stream.py:main`" in message
    assert "`tatolab dev -f <file>`" in message


# ---------------------------------------------------------------------------
# Executing the entry
# ---------------------------------------------------------------------------


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
    `-m`'s working directory never stays on the path."""
    write_app(anchor_directory, "helpers.py", "HELPER = 1\n")
    write_app(
        anchor_directory,
        "rigs/desk.py",
        "import helpers\n" + MINIMAL_STREAM_SOURCE,
    )

    finished = run_compile_entry(anchor_directory, "rigs/desk.py")

    assert finished.returncode == 1, finished.stdout + finished.stderr
    assert "ModuleNotFoundError: No module named 'helpers'" in finished.stderr


def test_the_app_does_not_see_the_launchers_arguments(anchor_directory: Path):
    """`sys.argv` belongs to the app, not to the compile entry `tatolab` started."""
    argv_probe = anchor_directory / "argv.json"
    entry_file = write_app(
        anchor_directory,
        "stream.py",
        "import json, sys\n"
        f"open({str(argv_probe)!r}, 'w').write(json.dumps(sys.argv))\n" + MINIMAL_STREAM_SOURCE,
    )

    compiled_document(run_compile_entry(anchor_directory, "--name", "desk"))

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
    }


def test_an_entry_file_outside_the_anchor_compiles_with_its_own_directory_as_the_project(
    tmp_path: Path,
):
    """The nodes an entry file imports sit beside it, so its processor interpreters
    start there rather than in the anchor."""
    anchor_directory = tmp_path.resolve() / "anchor"
    anchor_directory.mkdir()
    entry_file = write_app(tmp_path.resolve() / "elsewhere" / "rigs", "stream.py", FRONT_STREAM_SOURCE)

    document = compiled_document(
        run_compile_entry(anchor_directory, "-f", str(entry_file), verb="dev")
    )

    assert document["project_directory"] == str(entry_file.parent)


def test_a_file_target_in_a_subdirectory_compiles_with_that_subdirectory_as_the_project(
    anchor_directory: Path,
):
    write_app(anchor_directory, "rigs/desk.py", TWO_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "rigs/desk.py:back"))

    assert document == {
        "stream_graph": BACK_STREAM_GRAPH,
        "project_directory": str(anchor_directory / "rigs"),
    }


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

    finished = run_compile_entry(anchor_directory, "raising_parent_rigs.desk:main")

    assert finished.returncode == 1, finished.stderr
    assert finished.stdout == ""
    assert "ValueError: bad package" in finished.stderr
    first_frame = finished.stderr.index("File ")
    assert finished.stderr.startswith(
        f'File "{anchor_directory / "raising_parent_rigs" / "__init__.py"}", line 1, in <module>',
        first_frame,
    ), finished.stderr
    assert "importlib" not in finished.stderr, finished.stderr


def test_a_module_targets_refusal_names_the_file_its_module_resolved_to(anchor_directory: Path):
    write_app(anchor_directory, "refusal_naming_rigs/__init__.py", "")
    write_app(anchor_directory, "refusal_naming_rigs/desk.py", TWO_STREAM_SOURCE)

    message = refusal_of(
        run_compile_entry(
            anchor_directory, "--dir", str(anchor_directory), "refusal_naming_rigs.desk:side"
        )
    )

    assert (
        f"`refusal_naming_rigs.desk` (`{anchor_directory / 'refusal_naming_rigs' / 'desk.py'}`) "
        f"defines no @stream function named `side`"
    ) in message
    assert (
        f"Name one of them: `tatolab run --dir {anchor_directory} "
        f"refusal_naming_rigs.desk:<function>`."
    ) in message


def test_name_overrides_the_streams_own_name_and_is_cast(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", MINIMAL_STREAM_SOURCE)

    document = compiled_document(run_compile_entry(anchor_directory, "--name", "Front Rig"))

    assert document["stream_graph"]["stream"] == "front-rig"


def test_a_name_that_casts_to_nothing_is_refused_before_the_entry_runs(anchor_directory: Path):
    ran = anchor_directory / "entry-ran.txt"
    write_app(
        anchor_directory,
        "stream.py",
        f"open({str(ran)!r}, 'w').write('ran')\n" + MINIMAL_STREAM_SOURCE,
    )

    message = refusal_of(run_compile_entry(anchor_directory, "--name", "!!!"))

    assert "--name '!!!' cannot name a stream" in message
    assert not ran.exists(), "a refused flag must not cost the entry file a run"


def test_several_streams_are_refused_listing_each_with_its_description(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    message = refusal_of(run_compile_entry(anchor_directory, "--dir", str(anchor_directory)))

    assert "defines 2 @stream functions" in message
    assert "    front — Front camera, in a window.\n" in message
    assert "The second paragraph" not in message, "only the description's first line is listed"
    assert "    back\n" in message
    assert f"`tatolab run --dir {anchor_directory} stream.py:<function>`" in message


def test_the_several_streams_suggestion_quotes_the_dir_it_was_given(tmp_path: Path):
    project_directory = tmp_path.resolve() / "front rig"
    write_app(project_directory, "stream.py", TWO_STREAM_SOURCE)

    message = refusal_of(
        run_compile_entry(project_directory, "--dir", str(project_directory), verb="dev")
    )

    assert f"`tatolab dev --dir '{project_directory}' stream.py:<function>`" in message


@pytest.mark.parametrize(
    "target_arguments",
    [["my rig.py"], ["my rig.py:side"]],
    ids=["several-streams", "a-function-the-file-lacks"],
)
def test_a_suggestion_quotes_an_entry_file_whose_name_holds_a_space(
    anchor_directory: Path, target_arguments: "list[str]"
):
    write_app(anchor_directory, "my rig.py", TWO_STREAM_SOURCE)

    message = refusal_of(
        run_compile_entry(anchor_directory, "--dir", str(anchor_directory), *target_arguments)
    )

    suggestion = f"tatolab run --dir {anchor_directory} 'my rig.py':<function>"
    assert f"`{suggestion}`" in message
    _, verb, *suggested_arguments = shlex.split(suggestion.replace("<function>", "back"))
    document = compiled_document(run_compile_entry(anchor_directory, *suggested_arguments, verb=verb))
    assert document["stream_graph"] == BACK_STREAM_GRAPH


def test_the_several_streams_suggestion_names_no_dir_when_none_was_given(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    message = refusal_of(run_compile_entry(anchor_directory))

    assert "`tatolab run stream.py:<function>`" in message


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


def test_an_entry_that_defines_no_stream_is_refused_with_a_sample(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", "PIPELINE = 1\n")

    message = refusal_of(run_compile_entry(anchor_directory, verb="dev"))

    assert "stream.py` defines no @stream function" in message
    assert "    @stream\n    def main(stream_builder: StreamBuilder) -> None:\n" in message
    assert "stream_builder.add(CameraSource)" in message
    assert "-f <file>" in message
    assert "tatolab dev <file>.py:<function>" in message


def test_a_named_function_the_file_lacks_is_refused_listing_its_streams(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", TWO_STREAM_SOURCE)

    message = refusal_of(run_compile_entry(anchor_directory, "stream.py:side"))

    assert "defines no @stream function named `side`" in message
    assert "    front — Front camera, in a window.\n" in message
    assert "    back" in message


def test_an_app_py_named_by_the_file_flag_that_defines_only_setup_defines_no_stream(
    anchor_directory: Path,
):
    write_app(anchor_directory, "app.py", APP_PY_SOURCE_DEFINING_ONLY_A_SETUP_FUNCTION)

    message = refusal_of(run_compile_entry(anchor_directory, "-f", "app.py"))

    assert f"`{anchor_directory / 'app.py'}` defines no @stream function\n" in message
    assert "    @stream\n    def main(stream_builder: StreamBuilder) -> None:\n" in message


def test_a_named_function_in_a_file_with_no_stream_is_refused_naming_the_decorator(
    anchor_directory: Path,
):
    write_app(anchor_directory, "stream.py", "PIPELINE = 1\n")

    message = refusal_of(run_compile_entry(anchor_directory, "stream.py:main"))

    assert "defines no @stream function named `main`, nor any other" in message
    assert (
        "`@stream` above a module-level `def main(stream_builder: StreamBuilder) -> None:`"
        in message
    )


def test_a_named_function_that_is_not_a_stream_is_refused_naming_the_fix(anchor_directory: Path):
    write_app(
        anchor_directory,
        "stream.py",
        MINIMAL_STREAM_SOURCE
        + "\n\ndef helper(stream_builder: StreamBuilder) -> None:\n    pass\n",
    )

    message = refusal_of(run_compile_entry(anchor_directory, "stream.py:helper"))

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


def test_a_package_target_naming_a_value_that_is_not_a_stream_is_refused_naming_the_fix(
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

    message = refusal_of(
        run_compile_entry(anchor_directory, "re_exporting_helper_rover:wire_cameras")
    )

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


def test_a_raising_stream_function_prints_its_traceback(anchor_directory: Path):
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

    finished = run_compile_entry(anchor_directory, verb="dev")

    assert finished.returncode == 1, finished.stdout + finished.stderr
    assert finished.stdout == ""
    assert f"error: `{anchor_directory / 'stream.py'}` failed" in finished.stderr
    assert "ValueError: bad wiring" in finished.stderr
    assert "Traceback (most recent call last)" in finished.stderr
    assert "_project_stream" not in finished.stderr, "the compile entry's frames are stripped"
    assert "_stream_graph_builder.py" not in finished.stderr, (
        "the compile call into the stream function is the launcher's, not the author's"
    )
    assert f'File "{anchor_directory / "stream.py"}", line 6, in main' in finished.stderr


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

    finished = run_compile_entry(anchor_directory)

    assert finished.returncode == 1, finished.stderr
    first_frame = finished.stderr.index("File ")
    assert finished.stderr.startswith(
        f'File "{anchor_directory / "stream.py"}", line 7, in main', first_frame
    ), finished.stderr
    assert "casts to `pattern`" in finished.stderr


# ---------------------------------------------------------------------------
# The failure surfaces — a bad save must cost nothing
# ---------------------------------------------------------------------------


def test_a_syntax_error_prints_the_apps_traceback(anchor_directory: Path):
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

    finished = run_compile_entry(anchor_directory, verb="dev")

    assert finished.returncode == 1, f"stderr was:\n{finished.stderr}"
    assert finished.stdout == ""
    assert "SyntaxError" in finished.stderr, (
        f"the user's own error must be the headline; stderr was:\n{finished.stderr}"
    )
    assert "stream.py" in finished.stderr, "the traceback must name the file"


def test_a_bad_save_in_the_effect_module_names_that_module_not_the_entry_file(tmp_path: Path):
    """The bad save the scaffold invites.

    `stream.py` holds wiring the user rarely touches; the file they edit is the
    node module, which reaches the compile only as an import from the entry
    file. So the traceback has to walk through `stream.py` and land in the
    module — naming only the entry file would point at the wrong file.
    """
    app_directory = tmp_path.resolve() / "demo"
    shutil.copytree(SCAFFOLD_TEMPLATE_DIRECTORY, app_directory)
    (app_directory / "nodes" / "inverting_effect.py").write_text(
        "def process(self ctx:\n    this does not parse\n"
    )

    finished = run_compile_entry(app_directory, verb="dev")

    assert finished.returncode == 1, f"stderr was:\n{finished.stderr}"
    assert "SyntaxError" in finished.stderr, (
        f"the user's own error must be the headline; stderr was:\n{finished.stderr}"
    )
    assert "inverting_effect.py" in finished.stderr, (
        f"the traceback must name the module the user edited; stderr was:\n{finished.stderr}"
    )


def test_a_raise_at_import_time_surfaces_as_the_apps_traceback(anchor_directory: Path):
    write_app(anchor_directory, "stream.py", "raise ValueError('bad wiring')\n")

    finished = run_compile_entry(anchor_directory)

    assert finished.returncode == 1
    assert finished.stdout == ""
    assert "ValueError: bad wiring" in finished.stderr


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

    finished = run_compile_entry(anchor_directory)

    assert "runpy" not in finished.stderr, (
        f"no launcher frame may appear in the app's traceback; stderr was:\n{finished.stderr}"
    )
    assert "_project_stream" not in finished.stderr, (
        f"the compile entry's own frames must be stripped; stderr was:\n{finished.stderr}"
    )
    first_frame = finished.stderr.index("File ")
    assert finished.stderr.startswith(
        f'File "{anchor_directory / "stream.py"}", line 1, in <module>', first_frame
    ), finished.stderr


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
    "arguments",
    [[], ["--verb", "new"], ["--verb", "run", "--runtime-name", "desk"]],
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
        [sys.executable, "-c", compile_entry_with_a_failing_check, "--verb", "run"],
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

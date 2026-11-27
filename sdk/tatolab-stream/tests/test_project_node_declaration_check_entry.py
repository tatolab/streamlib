# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The entry the runtime checks a project's node declarations through, in the project's interpreter.

Each test runs it as the runtime does: `python -I -m` in a child, the project directory as the
working directory, the request on stdin, reading one JSON object per line on stdout. Nothing
here needs a runtime.
"""

import hashlib
import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Optional

import pytest

CHECK_ENTRY_MODULE = "tatolab.stream._project_node_declaration_check_entry"
# Bounded: a check imports a few small modules.
CHECK_ENTRY_TIMEOUT_SECONDS = 60.0

TYPED_NODE_SOURCE = '''\
from tatolab.stream import node


@node
def Gain(level: float = 1.0) -> float:
    return level * 2.0
'''

# A typed input of no default, refused at decoration with its code; the decorator is on line 4.
NODE_WITH_A_DECLARATION_ERROR_SOURCE = '''\
from tatolab.stream import node


@node
def Blur(radius: float) -> float:
    return radius
'''

RETIRED_FUNCTION_NODE_SOURCE = '''\
from tatolab.stream import node


@node.function
def Clamp(x: float = 0.0) -> float:
    return x
'''

MODULE_THAT_RAISES_SOURCE = '''\
from tatolab.stream import node

print("the module prints while it imports")
raise RuntimeError("the lens table is missing")


@node
def Lens(focus: float = 1.0) -> float:
    return focus
'''

MODULE_THAT_DOES_NOT_PARSE_SOURCE = '''\
from tatolab.stream import node

def Broken(:
'''

MODULE_WHOSE_IMPORT_NEVER_RETURNS_SOURCE = '''\
import time

from tatolab.stream import node

while True:
    time.sleep(1)


@node
def Stuck(level: float = 1.0) -> float:
    return level
'''


def write_project(project_directory: Path, files: dict[str, str]) -> None:
    for path, source in files.items():
        file_path = project_directory / path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(source, encoding="utf-8")


def run_check(project_directory: Path, paths: Optional[list[str]] = None) -> list[dict[str, Any]]:
    finished = subprocess.run(
        [sys.executable, "-I", "-m", CHECK_ENTRY_MODULE],
        cwd=project_directory,
        input=json.dumps({"paths": paths}),
        capture_output=True,
        text=True,
        timeout=CHECK_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )
    assert finished.returncode == 0, finished.stderr
    return [json.loads(line) for line in finished.stdout.splitlines()]


def the_findings_of(lines: list[dict[str, Any]], path: str) -> list[dict[str, Any]]:
    (module_line,) = [line for line in lines[1:] if line["path"] == path]
    return module_line["findings"]


@pytest.fixture
def project_directory(tmp_path: Path) -> Path:
    return tmp_path


def test_a_declaration_error_is_answered_with_its_path_line_code_and_words(
    project_directory: Path,
) -> None:
    write_project(project_directory, {"nodes/blur.py": NODE_WITH_A_DECLARATION_ERROR_SOURCE})

    lines = run_check(project_directory)

    (finding,) = the_findings_of(lines, "nodes/blur.py")
    assert finding["finding_code"] == "value_input_has_no_default"
    assert finding["severity"] == "error"
    assert finding["path"] == "nodes/blur.py"
    assert finding["line"] == 4
    assert finding["node_type"] == "nodes.blur:Blur"
    assert finding["port"] == "radius"
    assert "To fix:" in finding["words"]


def test_a_retired_declaration_is_an_error_naming_its_replacement(
    project_directory: Path,
) -> None:
    write_project(project_directory, {"nodes/clamp.py": RETIRED_FUNCTION_NODE_SOURCE})

    lines = run_check(project_directory)

    (finding,) = the_findings_of(lines, "nodes/clamp.py")
    assert finding["finding_code"] == "declaration_retired"
    assert finding["severity"] == "error"
    assert (finding["line"], finding["column"]) == (4, 1)
    assert finding["node_type"] == "nodes.clamp:Clamp"
    assert "`@node.function`" in finding["words"] and "bare `@node`" in finding["words"]


RETIRED_IMPORT_SOURCE = '''\
from tatolab.stream import Output, node
'''


def test_a_module_holding_only_a_retired_import_is_checked_and_found_at_it(
    project_directory: Path,
) -> None:
    """A module declaring no node still answers for the retired name it imports.

    Mental-revert: checking only the modules the listing names leaves this one out.
    """
    write_project(project_directory, {"nodes/ports.py": RETIRED_IMPORT_SOURCE})

    (finding,) = the_findings_of(run_check(project_directory), "nodes/ports.py")
    assert (finding["finding_code"], finding["severity"]) == ("declaration_retired", "error")
    assert (finding["line"], finding["node_type"]) == (1, None)
    assert finding["words"].startswith("`tatolab.stream.Output` is retired. To fix:")


def test_a_module_that_does_not_import_is_one_finding_with_its_traceback(
    project_directory: Path,
) -> None:
    write_project(project_directory, {"nodes/lens.py": MODULE_THAT_RAISES_SOURCE})

    lines = run_check(project_directory)

    (finding,) = the_findings_of(lines, "nodes/lens.py")
    assert finding["finding_code"] == "module_does_not_import"
    assert finding["line"] == 4
    assert "RuntimeError: the lens table is missing" in finding["words"]
    assert "importlib" not in finding["words"]


def test_a_module_that_does_not_parse_is_found_at_its_syntax_error(
    project_directory: Path,
) -> None:
    write_project(project_directory, {"nodes/broken.py": MODULE_THAT_DOES_NOT_PARSE_SOURCE})

    lines = run_check(project_directory)

    (finding,) = the_findings_of(lines, "nodes/broken.py")
    assert finding["finding_code"] == "module_does_not_import"
    assert finding["line"] == 3
    assert finding["column"] is not None


def test_a_module_with_no_mistake_has_no_finding_and_its_hash(project_directory: Path) -> None:
    write_project(project_directory, {"nodes/gain.py": TYPED_NODE_SOURCE, "helpers.py": "X = 1\n"})

    lines = run_check(project_directory)

    assert lines[0]["modules"] == [{"module": "nodes.gain", "path": "nodes/gain.py"}]
    assert lines[0]["checked_with"][0].startswith("tatolab-stream ")
    assert lines[1] == {
        "path": "nodes/gain.py",
        "content_sha256": hashlib.sha256(TYPED_NODE_SOURCE.encode("utf-8")).hexdigest(),
        "findings": [],
    }


def test_paths_name_the_modules_checked(project_directory: Path) -> None:
    write_project(
        project_directory,
        {
            "nodes/gain.py": TYPED_NODE_SOURCE,
            "nodes/blur.py": NODE_WITH_A_DECLARATION_ERROR_SOURCE,
            "notes.txt": "",
        },
    )

    lines = run_check(project_directory, ["nodes/gain.py", "notes.txt"])

    assert lines[0]["modules"] == [
        {"module": "nodes.gain", "path": "nodes/gain.py"},
        {"module": None, "path": "notes.txt"},
    ]
    assert the_findings_of(lines, "nodes/gain.py") == []
    (finding,) = the_findings_of(lines, "notes.txt")
    assert finding["finding_code"] == "path_not_a_python_module"


def test_what_a_module_prints_never_reaches_the_lines(project_directory: Path) -> None:
    write_project(project_directory, {"nodes/lens.py": MODULE_THAT_RAISES_SOURCE})

    lines = run_check(project_directory)

    assert [list(line) for line in lines] == [
        ["checked_with", "modules"],
        ["content_sha256", "findings", "path"],
    ]


def test_each_module_is_written_as_it_finishes_so_a_check_killed_at_its_bound_keeps_them(
    project_directory: Path,
) -> None:
    write_project(
        project_directory,
        {
            "nodes/a_gain.py": TYPED_NODE_SOURCE,
            "nodes/b_stuck.py": MODULE_WHOSE_IMPORT_NEVER_RETURNS_SOURCE,
        },
    )
    check = subprocess.Popen(
        [sys.executable, "-I", "-m", CHECK_ENTRY_MODULE],
        cwd=project_directory,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    assert check.stdin is not None and check.stdout is not None
    check.stdin.write(json.dumps({"paths": None}).encode("utf-8"))
    check.stdin.close()
    try:
        lines = [json.loads(check.stdout.readline()) for _ in range(2)]
        time.sleep(0.5)
        assert check.poll() is None
    finally:
        check.kill()
        check.wait(timeout=CHECK_ENTRY_TIMEOUT_SECONDS)

    assert [module["path"] for module in lines[0]["modules"]] == [
        "nodes/a_gain.py",
        "nodes/b_stuck.py",
    ]
    assert lines[1]["path"] == "nodes/a_gain.py"
    assert check.stdout.read() == b""

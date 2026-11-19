# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""check_in_tree_projects_node_code.py reaches every project and first-party module, and judges
`tatolab check --json` by its codes: an error or an unfinished Python node declaration check
fails a project, and each retired declaration is counted by project."""

import json
import tempfile
import unittest
from pathlib import Path

from check_in_tree_projects_node_code import (
    FAILED_EXIT_CODE,
    NOT_FULLY_CHECKED_EXIT_CODE,
    PASSED_EXIT_CODE,
    InTreeProject,
    ProjectCheckOutcome,
    holds_python_of_its_own,
    in_tree_projects,
    judge_a_check_answer,
    the_directory_to_check,
    the_exit_code_of,
    the_report,
)

A_PROJECT = InTreeProject("examples/a", Path("examples/a"), True)


def write(path: Path, text: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def python_node_declarations_run(state: str, **extra: object) -> dict[str, object]:
    return {"check": "python_node_declarations", "state": state, **extra}


def finding(finding_code: str, severity: str, **extra: object) -> dict[str, object]:
    return {
        "check": "python_node_declarations",
        "finding_code": finding_code,
        "severity": severity,
        "words": "what was found",
        "path": "nodes/a.py",
        "line": 4,
        **extra,
    }


class InTreeProjectsTest(unittest.TestCase):
    def test_every_project_file_and_every_first_party_module_without_one_is_reached(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            repository = Path(scratch)
            write(repository / "examples/sample/tatolab-project.json", "{}")
            write(repository / "examples/no-project/pyproject.toml", '[project]\nname = "paint"\n')
            write(repository / "packages/tatolab-camera/tatolab-project.json", "{}")
            write(repository / "packages/tatolab-camera/pyproject.toml", '[project]\nname = "tatolab-camera"\n')
            write(repository / "packages/tatolab-camera/streams/module-camera/tatolab-project.json", "{}")
            write(repository / "packages/tatolab-camera/.venv/lib/x/tatolab-project.json", "{}")
            write(repository / "packages/tatolab-compositor/pyproject.toml", '[project]\nname = "tatolab-compositor"\n')
            write(repository / "packages/test-fixtures/Cargo.toml")
            write(repository / "packages/third-party/pyproject.toml", '[project]\nname = "someone-elses"\n')

            reached = [(project.name, project.has_a_project_file) for project in in_tree_projects(repository)]

        self.assertEqual(
            reached,
            [
                ("examples/sample", True),
                ("packages/tatolab-camera", True),
                ("packages/tatolab-camera/streams/module-camera", True),
                ("packages/tatolab-compositor", False),
            ],
        )

    def test_python_only_in_nested_projects_imports_or_tests_is_not_a_projects_own(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            project = InTreeProject("p", directory, True)
            write(directory / "streams/nested/tatolab-project.json", "{}")
            write(directory / "streams/nested/node.py")
            write(directory / "imports/m/node.py")
            write(directory / "tests/test_node.py")
            write(directory / ".venv/lib/site.py")
            self.assertFalse(holds_python_of_its_own(project))
            write(directory / "python/tatolab/m/node.py")
            self.assertTrue(holds_python_of_its_own(project))

    def test_python_in_a_streams_folder_that_is_no_project_is_a_projects_own(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            write(directory / "streams/s/node.py")
            self.assertTrue(holds_python_of_its_own(InTreeProject("p", directory, True)))

    def test_a_module_without_a_project_file_is_checked_in_a_scratch_project_of_its_package(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            package = Path(scratch) / "packages/tatolab-compositor"
            write(package / "pyproject.toml", '[project]\nname = "tatolab-compositor"\n')
            write(package / "python/tatolab/compositor/node.py")
            write(package / "streams/s/main.graph.json", "{}")
            write(package / "tests/test_node.py")
            scratch_projects = Path(scratch) / "scratch"
            scratch_projects.mkdir()

            checked = the_directory_to_check(
                InTreeProject("packages/tatolab-compositor", package, False), scratch_projects
            )

            self.assertEqual(checked, scratch_projects / "tatolab-compositor")
            self.assertTrue((checked / "python/tatolab/compositor/node.py").is_file())
            self.assertTrue((checked / "streams/s/main.graph.json").is_file())
            self.assertFalse((checked / "tests").exists())
            self.assertEqual(json.loads((checked / "tatolab-project.json").read_text())["name"], "tatolab-compositor")
            self.assertFalse((package / "tatolab-project.json").exists())


class JudgeACheckAnswerTest(unittest.TestCase):
    def test_retired_declarations_alone_pass_and_are_counted(self) -> None:
        outcome = judge_a_check_answer(
            A_PROJECT,
            {
                "verdict": "passed",
                "checks": [python_node_declarations_run("passed")],
                "findings": [finding("declaration_retired", "warning", node_type="Hum")],
            },
        )
        self.assertEqual((outcome.failed_because, outcome.retired_declarations), ([], ["nodes/a.py:4: Hum"]))

    def test_an_error_finding_fails_with_its_code_and_words(self) -> None:
        outcome = judge_a_check_answer(
            A_PROJECT,
            {
                "verdict": "failed",
                "checks": [python_node_declarations_run("failed")],
                "findings": [finding("value_input_has_no_default", "error")],
            },
        )
        self.assertEqual(outcome.failed_because, ["nodes/a.py:4: value_input_has_no_default: what was found"])

    def test_a_finding_in_a_tests_folder_is_left_out(self) -> None:
        outcome = judge_a_check_answer(
            A_PROJECT,
            {
                "verdict": "failed",
                "checks": [python_node_declarations_run("failed")],
                "findings": [
                    finding("module_does_not_import", "error", path="tests/test_nodes.py"),
                    finding("declaration_retired", "warning", path="tests/live/sink.py", node_type="Sink"),
                    finding("declaration_retired", "warning", path="python/tests_of_mine.py", node_type="Kept"),
                ],
            },
        )
        self.assertEqual(
            (outcome.failed_because, outcome.retired_declarations), ([], ["python/tests_of_mine.py:4: Kept"])
        )

    def test_a_python_node_declaration_check_that_did_not_finish_fails(self) -> None:
        for state in ["not_checked", "a_state_added_later"]:
            with self.subTest(state=state):
                outcome = judge_a_check_answer(
                    A_PROJECT,
                    {
                        "verdict": "not_fully_checked",
                        "checks": [
                            python_node_declarations_run(
                                state,
                                not_checked_because={
                                    "refusal_code": "check_ran_out_of_time",
                                    "runtime_words": "ran out of time",
                                },
                            )
                        ],
                        "findings": [],
                    },
                )
                self.assertEqual(
                    outcome.failed_because,
                    ["python_node_declarations: not checked: check_ran_out_of_time: ran out of time"],
                )

    def test_another_check_that_did_not_finish_passes(self) -> None:
        outcome = judge_a_check_answer(
            A_PROJECT,
            {
                "verdict": "not_fully_checked",
                "checks": [python_node_declarations_run("passed"), {"check": "views", "state": "not_checked"}],
                "findings": [],
            },
        )
        self.assertEqual(outcome.failed_because, [])

    def test_an_answer_with_no_python_node_declaration_check_fails(self) -> None:
        outcome = judge_a_check_answer(A_PROJECT, {"verdict": "passed", "checks": [], "findings": []})
        self.assertEqual(outcome.failed_because, ["python_node_declarations: the runtime did not run it"])


class ReportTest(unittest.TestCase):
    def test_the_report_lists_retired_declarations_by_project_with_the_total(self) -> None:
        first = ProjectCheckOutcome(A_PROJECT, retired_declarations=["a.py:1: A", "a.py:9: B"])
        second = ProjectCheckOutcome(InTreeProject("packages/b", Path("packages/b"), True))
        third = ProjectCheckOutcome(
            InTreeProject("packages/c", Path("packages/c"), False), retired_declarations=["c.py:2: C"]
        )
        report = the_report([first, second, third], [])
        self.assertIn("  examples/a: 2\n    a.py:1: A\n    a.py:9: B\n  packages/c: 1\n", report)
        self.assertNotIn("packages/b:", report)
        self.assertIn("Total declarations in a retired form: 3\n", report)

    def test_a_failure_outranks_an_unfinished_run(self) -> None:
        passed = ProjectCheckOutcome(A_PROJECT)
        failed = ProjectCheckOutcome(A_PROJECT, failed_because=["x"])
        unfinished = ProjectCheckOutcome(A_PROJECT, not_fully_checked_because=["y"])
        self.assertEqual(the_exit_code_of([passed]), PASSED_EXIT_CODE)
        self.assertEqual(the_exit_code_of([passed, unfinished]), NOT_FULLY_CHECKED_EXIT_CODE)
        self.assertEqual(the_exit_code_of([unfinished, failed]), FAILED_EXIT_CODE)


if __name__ == "__main__":
    unittest.main()

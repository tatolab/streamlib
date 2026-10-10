# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The harness runs only a runtime unit whose binaries keep the machine's
directories under a test root, so no test can take this machine's real lock."""

from __future__ import annotations

from pathlib import Path

import pytest

from runtime_unit_under_test import (
    LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT,
    MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME,
    PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND,
    RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE,
    RuntimeUnitUnderTest,
    locate_the_runtime_unit,
)


def a_runtime_unit_laid_out_at(runtime_unit_directory: Path) -> None:
    """The two binaries and the lend's bootstrap, as empty files."""
    for relative_path in (
        Path("bin/tatolabd"),
        Path("bin/tatolab"),
        LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT / PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND,
    ):
        (runtime_unit_directory / relative_path).parent.mkdir(parents=True, exist_ok=True)
        (runtime_unit_directory / relative_path).touch()


def test_a_runtime_unit_without_the_test_root_marker_is_refused_by_name(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    a_runtime_unit_laid_out_at(tmp_path)
    monkeypatch.setenv(RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE, str(tmp_path))

    refusal = locate_the_runtime_unit()

    assert isinstance(refusal, str), refusal
    assert f"`{MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME}` marker" in refusal
    assert "real runtime lock" in refusal
    assert "cargo xtask build-runtime --machine-directories-under-a-test-root" in refusal


def test_a_runtime_unit_carrying_the_marker_is_run(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    a_runtime_unit_laid_out_at(tmp_path)
    (tmp_path / MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME).touch()
    monkeypatch.setenv(RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE, str(tmp_path))

    located = locate_the_runtime_unit()

    assert isinstance(located, RuntimeUnitUnderTest), located
    assert located.tatolabd_executable == tmp_path / "bin" / "tatolabd"

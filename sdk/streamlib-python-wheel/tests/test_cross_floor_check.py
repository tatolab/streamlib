# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cross-floor check where the runtime runs it: the gate over the runtime's
own Python and the scaffold it writes, and the warning block `run` and `dev`
print.

Each finding kind and the block's rendering are the stream suite's
(`sdk/tatolab-stream/tests/test_cross_floor_check.py`). Nothing here boots an
engine; the launch that goes on to a live node is in `test_cli_launch.py`.
"""

import sys
from pathlib import Path

import pytest

from tatolab.runtime import cli
from tatolab.stream._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    render_cross_floor_warning_block,
)

RUNTIME_PACKAGE_DIRECTORY = Path(cli.__file__).resolve().parent


# ---------------------------------------------------------------------------
# The gate over what the project ships
# ---------------------------------------------------------------------------


def test_the_runtimes_own_python_binds_to_no_floor():
    assert (RUNTIME_PACKAGE_DIRECTORY / "__init__.py").is_file(), (
        f"the gate must read the runtime's own Python, not {RUNTIME_PACKAGE_DIRECTORY}"
    )
    report = check_app_directory_for_floor_bindings(RUNTIME_PACKAGE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, RUNTIME_PACKAGE_DIRECTORY
    )



@pytest.mark.parametrize("use_test_pattern_source", [False, True])
def test_the_scaffold_binds_to_no_floor(tmp_path: Path, use_test_pattern_source: bool):
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=use_test_pattern_source)

    report = check_app_directory_for_floor_bindings(app_directory)

    assert report.findings == [], (
        render_cross_floor_warning_block(report, app_directory)
    )


# ---------------------------------------------------------------------------
# At launch
# ---------------------------------------------------------------------------


def launch_until_the_entry_stops_it(app_directory: Path) -> int:
    return cli.launch_app_node(
        "dev",
        requested_anchor_directory=app_directory,
        requested_entry_file=None,
        requested_stream_target=None,
        requested_stream_name=None,
        runtime_name=None,
    )


@pytest.fixture
def restore_the_launchers_import_path():
    launcher_import_path = list(sys.path)
    try:
        yield
    finally:
        sys.path[:] = launcher_import_path


@pytest.mark.usefixtures("restore_the_launchers_import_path")
def test_the_launch_prints_the_block_before_the_app_runs_and_still_runs_it(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    ran = tmp_path / "entry-ran.txt"
    (tmp_path / "processors").mkdir()
    (tmp_path / "processors" / "effect.py").write_text(
        'import cupy\nimport torch\ndevice = torch.device("cuda")\n'
    )
    (tmp_path / "stream.py").write_text(
        f"open({str(ran)!r}, 'w').write('ran')\n"
        "print('the entry file ran')\n"
        "raise RuntimeError('stop before the engine')\n"
    )

    exit_code = launch_until_the_entry_stops_it(tmp_path)

    stdout = capsys.readouterr().out
    assert exit_code == 1, "the entry file stopped the launch on purpose"
    assert ran.read_text() == "ran", "a finding must never keep the app from running"
    assert "processors/effect.py:1: imports `cupy`" in stdout
    assert "processors/effect.py:3: names the device 'cuda'" in stdout
    assert stdout.index("cross-floor check") < stdout.index("the entry file ran"), (
        "the block is printed between resolving the entry file and executing it"
    )


@pytest.mark.usefixtures("restore_the_launchers_import_path")
def test_a_check_that_fails_is_reported_and_the_app_still_runs(
    tmp_path: Path, capsys: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch
):
    def failing_check(app_directory: Path) -> None:
        raise RuntimeError("a defect in the check")

    monkeypatch.setattr(cli, "check_app_directory_for_floor_bindings", failing_check)
    ran = tmp_path / "entry-ran.txt"
    (tmp_path / "stream.py").write_text(
        f"open({str(ran)!r}, 'w').write('ran')\n"
        "raise RuntimeError('stop before the engine')\n"
    )

    launch_until_the_entry_stops_it(tmp_path)

    assert ran.read_text() == "ran", "a failing check must never keep the app from running"
    assert "cross-floor check could not run" in capsys.readouterr().out


@pytest.mark.usefixtures("restore_the_launchers_import_path")
def test_a_clean_launch_prints_nothing_extra(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
):
    (tmp_path / "stream.py").write_text("raise RuntimeError('stop before the engine')\n")

    launch_until_the_entry_stops_it(tmp_path)

    assert capsys.readouterr().out == ""

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""How the harness reads a node's `MARKER:<NAME>` lines off `tatolabd`'s standard error.

The lines are the pretty log mirror's own: a node's `log.info` reaches it
through the helper log drain and ends with the record's ` processor_id=<id>`.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

from runtime_process_under_test import (
    NOT_A_MARKER_LINE,
    TATOLABD_REFUSAL_LINE_PREFIX,
    MarkerLineParser,
    RuntimeProcessUnderTest,
)

HELPER_LOG_LINE_PREFIX = (
    "04:31:48.995 [ INFO] [Rkw2n8cmioeuanqbpm42ybutl/python] streamlib::polyglot::python — "
)
HELPER_LOG_RECORD_FIELDS = " processor_id=Pxhsdghlr4yohiat6rbo7g5ds\n"


def helper_log_line(message: str) -> str:
    return HELPER_LOG_LINE_PREFIX + message + HELPER_LOG_RECORD_FIELDS


def test_a_markers_json_payload_is_decoded_up_to_where_it_ends():
    line = helper_log_line('MARKER:BAGS_PROCESSED {"bags_processed": 20, "sink": "a b"}')

    assert MarkerLineParser("BAGS_PROCESSED").payload_of(line) == {
        "bags_processed": 20,
        "sink": "a b",
    }


def test_a_marker_followed_only_by_the_records_fields_carries_no_payload():
    assert MarkerLineParser("CLEAN_EXIT").payload_of(helper_log_line("MARKER:CLEAN_EXIT")) is None


def test_a_markers_text_payload_comes_without_the_records_fields():
    line = helper_log_line("MARKER:PROBE_SKIPPED no shader toolchain")

    assert MarkerLineParser("PROBE_SKIPPED").payload_of(line) == "no shader toolchain"


def test_a_marker_whose_name_only_begins_with_the_awaited_one_is_not_it():
    line = helper_log_line("MARKER:BAGS_PROCESSED_TWICE {}")

    assert MarkerLineParser("BAGS_PROCESSED").payload_of(line) is NOT_A_MARKER_LINE


def test_waits_read_every_marker_and_line_from_the_start_of_standard_error(tmp_path: Path):
    written_lines = [
        helper_log_line('MARKER:FIRST {"n": 1}'),
        helper_log_line("MARKER:SECOND"),
        helper_log_line('MARKER:FIRST {"n": 2}'),
        "tatolabd: a closing line\n",
    ]
    writer_script = tmp_path / "write_standard_error.py"
    writer_script.write_text(
        f"import sys\nsys.stderr.write({''.join(written_lines)!r})\nsys.stderr.flush()\n"
    )
    process_under_test = RuntimeProcessUnderTest(
        subprocess.Popen(
            [sys.executable, str(writer_script)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        ),
        command_description="write_standard_error.py",
        refusal_line_prefix=TATOLABD_REFUSAL_LINE_PREFIX,
    )
    try:
        assert process_under_test.await_marker("FIRST", occurrence=2, timeout=10) == {"n": 2}
        assert process_under_test.await_every_marker("SECOND", "FIRST", timeout=10) == {
            "SECOND": None,
            "FIRST": {"n": 1},
        }
        assert "MARKER:SECOND" in process_under_test.await_stderr_containing("MARKER:", occurrence=2)
        assert process_under_test.marker_payloads("FIRST") == [{"n": 1}, {"n": 2}]
        assert process_under_test.await_exit(timeout=10) == 0
        assert process_under_test.refusal() == "a closing line"
    finally:
        process_under_test.kill_every_process_it_started()

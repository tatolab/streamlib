# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Puts the module's Python on the path, as its installed wheel would, with its native half stood
in for so no browser host is needed, and keeps the log records a helper would send its parent."""

import sys
from pathlib import Path
from typing import Iterator

import pytest
from tatolab.stream import log

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import native_page_stand_in  # noqa: E402

sys.modules["tatolab.browser._native"] = native_page_stand_in  # type: ignore[assignment]


@pytest.fixture(autouse=True)
def log_records_kept_as_a_helper_sends_its_parent() -> "Iterator[list[tuple[str, str, object]]]":
    """Stands in for the parent's end of a helper's log route, which refuses a record made
    anywhere else."""
    sent: "list[tuple[str, str, object]]" = []
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setattr(log, "_helper_process_sink", lambda *record: sent.append(record))
        yield sent

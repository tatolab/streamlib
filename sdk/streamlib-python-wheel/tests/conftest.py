# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Fixtures shared by the suites that drive an app out of process."""

from pathlib import Path

import pytest

from typing import Callable

from app_under_test import AppUnderTest, start_app


@pytest.fixture
def start_app_under_test():
    """Hands out apps and kills their process groups no matter how a test ends.

    Without the teardown a failed assertion strands a live engine holding a GPU
    context, an iceoryx2 node and a socket, silently contaminating every later
    run on the same rig.

    `launcher` picks the launch arrangement — `python app.py` unless a suite
    names one of `app_under_test`'s others. The reaping is the same whichever
    it is, which is the point of routing them all through here.
    """
    started: "list[AppUnderTest]" = []

    def start(
        app_path: Path,
        *arguments: str,
        launcher: "Callable[..., AppUnderTest]" = start_app,
    ) -> AppUnderTest:
        app = launcher(app_path, *arguments)
        started.append(app)
        return app

    try:
        yield start
    finally:
        for app in started:
            app.kill_process_group()

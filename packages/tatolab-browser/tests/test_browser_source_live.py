# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `BrowserSource` changes its page or its size while it plays: its body runs once, a new tab
opens while the one before keeps publishing, and the one before is closed once it is replaced; a
page that cannot open leaves the one before publishing.

The node function runs on the wheel's node scope, as its helper runs it, with this wheel's native
page stood in for by one that records each open and close (`native_page_stand_in`).
"""

from __future__ import annotations

import time
from pathlib import Path
from typing import Any, Callable

import pytest

from native_page_stand_in import URL_THE_HOST_REFUSES, WHAT_THE_TABS_DID
from tatolab.browser.browser_source import BrowserSource
from tatolab.runtime._node_function_running import NodeFunctionRunning

PAGE_INPUT_PORTS = (
    "video_in_0", "video_in_1", "video_in_2", "video_in_3",
    "audio_in_0", "audio_in_1", "audio_in_2", "audio_in_3",
)  # fmt: skip


class PinWiredOrNot:
    """Stands in for an input pin, wired while `wired` holds."""

    def __init__(self) -> None:
        self.wired = False

    def inbound_link_names(self) -> "list[str]":
        return ["a-link"] if self.wired else []


class ContextLendingPorts:
    """Stands in for the full access context the body is handed: records each table it opens."""

    def __init__(self) -> None:
        self.tables_opened: "list[tuple[list[str], list[str]]]" = []

    def open_module_host_table(self, *, output_ports: "list[str]", input_ports: "list[str]") -> object:
        self.tables_opened.append((list(output_ports), list(input_ports)))
        return object()


def wait_until(done: Callable[[], bool], timeout_s: float = 5.0) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if done():
            return
        time.sleep(0.01)
    raise AssertionError(f"never done; the tabs did {WHAT_THE_TABS_DID}")


FIRST_PAGE = "https://first.example/"


@pytest.fixture
def playing() -> "Any":
    """The node set up on `FIRST_PAGE` at 640x360, once its tab opened, and its pins."""
    WHAT_THE_TABS_DID.clear()
    ctx = ContextLendingPorts()
    pins: "dict[str, Any]" = {port: PinWiredOrNot() for port in PAGE_INPUT_PORTS}
    pins.update({port: object() for port in ("data", "video", "audio", "events")})
    running = NodeFunctionRunning(
        BrowserSource,
        values_to_build_with={"url": FIRST_PAGE, "width": 640, "height": 360},
        pins=pins,
        bind_pins=lambda members: {},
        slot_counts={},
    )
    running.setup(ctx)
    wait_until(lambda: WHAT_THE_TABS_DID == [("opened", FIRST_PAGE, 640, 360, ())])
    yield running, ctx, pins
    running.teardown(ctx)


def test_a_new_url_opens_a_new_tab_and_closes_the_one_before(playing: "Any") -> None:
    """Mental-revert: read `url` once in the body, and a set opens no new tab; open the table in
    the build, and every tab lends the ports again."""
    running, ctx, _ = playing

    running.write_inputs({"url": "https://second.example/"})
    wait_until(lambda: len(WHAT_THE_TABS_DID) == 3)

    assert WHAT_THE_TABS_DID == [
        ("opened", FIRST_PAGE, 640, 360, ()),
        ("opened", "https://second.example/", 640, 360, ()),
        ("closed", FIRST_PAGE, 640, 360, ()),
    ]
    assert ctx.tables_opened == [
        (["video", "events", "audio"], ["data", *PAGE_INPUT_PORTS]),
    ]


def test_a_new_size_opens_a_tab_of_that_size_with_the_inputs_wired_then(playing: "Any") -> None:
    """Mental-revert: read which inputs are wired once in the body, and a tab opened after a
    link was made hands the page no track for it."""
    running, _, pins = playing
    pins["video_in_1"].wired = True
    pins["audio_in_0"].wired = True

    running.write_inputs({"width": 1280, "height": 720})
    wait_until(lambda: len(WHAT_THE_TABS_DID) == 3)

    assert WHAT_THE_TABS_DID[1:] == [
        ("opened", FIRST_PAGE, 1280, 720, ("video_in_1", "audio_in_0")),
        ("closed", FIRST_PAGE, 640, 360, ()),
    ]


def test_a_page_that_cannot_open_leaves_the_one_before_publishing(
    playing: "Any", tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Mental-revert: close the tab before the new one opens, and a page the host refuses leaves
    the node publishing nothing."""
    running, _, _ = playing
    monkeypatch.chdir(tmp_path)

    for url, why in (
        (URL_THE_HOST_REFUSES, f"RuntimeError: the browser host did not open {URL_THE_HOST_REFUSES}"),
        ("templates/missing_page.html", "BrowserPageFileMissing: "),
    ):
        running.write_inputs({"url": url})
        assert running._running is not None
        assert running._running.wait_until_settled(5.0)
        (page,) = running.builds_of_each_computed_value()
        assert (page.has_a_value, page.last_build.failure) == (True, "raised")
        assert page.last_build.why is not None and page.last_build.why.startswith(why)

    assert WHAT_THE_TABS_DID == [("opened", FIRST_PAGE, 640, 360, ())]


def test_stopping_the_node_closes_its_tab(playing: "Any") -> None:
    running, ctx, _ = playing

    running.stop(ctx)

    assert WHAT_THE_TABS_DID[-1] == ("closed", FIRST_PAGE, 640, 360, ())

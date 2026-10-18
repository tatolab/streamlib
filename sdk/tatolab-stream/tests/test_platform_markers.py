# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The platform markers are held to what the plan allows them to say.

`linux_only_capability` names only the closed list in `docs/plan/ARCHITECTURE.md`
§Product, so a test red on macOS cannot
be quietly skipped. `rig_only_capability` names only what a CI runner cannot
give, so a test red on the bundled software device cannot be quietly moved to
the rig. `awaiting_macos_parity` names only a ticket an active
change still lists under `## Tickets`, so the mark cannot outlive the work it
waits on.
"""

import re
from pathlib import Path

import pytest

REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
ACTIVE_CHANGES_DIRECTORY = REPOSITORY_ROOT / "docs" / "plan" / "changes"
# The suites that run on both floors and so may carry a platform marker. Read
# as text, so this suite holds the runtime's tests without importing them.
MARKED_TEST_DIRECTORIES = (
    REPOSITORY_ROOT / "sdk" / "tatolab-stream" / "tests",
    REPOSITORY_ROOT / "sdk" / "streamlib-python-wheel" / "tests",
    REPOSITORY_ROOT / "sdk" / "streamlib-python-wheel" / "tests_against_the_runtime_unit_lend",
    REPOSITORY_ROOT / "tests" / "stream-on-runtime",
)

LINUX_ONLY_CAPABILITY_CLOSED_LIST_REASONS = {
    "MoltenVK has no VK_KHR_ray_tracing_pipeline",
    "VirtualCameraSink is v4l2loopback and PipeWire",
    "DMA-BUF and OPAQUE_FD are Linux file-descriptor handles",
    "the CUDA Array Interface is CUDA",
    # A test whose body is a Linux mechanism.
    "v4l2loopback and udev are Linux",
    "the boot session is a Linux kernel file",
    "only Linux resolves the runtime directory from XDG_RUNTIME_DIR",
    "only Linux owns SIGHUP",
    "only Linux hands SIGINT back when a run ends",
}

RIG_ONLY_CAPABILITY_CLOSED_LIST_REASONS = {
    "Vulkan Video",
    "DMA-BUF modifiers",
    "CUDA",
    "a camera",
    "an audio device",
}

CLOSED_LIST_REASONS_BY_REASONED_MARKER_NAME = {
    "linux_only_capability": LINUX_ONLY_CAPABILITY_CLOSED_LIST_REASONS,
    "rig_only_capability": RIG_ONLY_CAPABILITY_CLOSED_LIST_REASONS,
}
WHAT_AN_UNLISTED_REASON_HIDES_BY_REASONED_MARKER_NAME = {
    "linux_only_capability": "a test red on macOS for any other reason is a parity bug",
    "rig_only_capability": (
        "a test red on the bundled software device for any other reason is an engine or "
        "wheel bug"
    ),
}

REASONED_MARKER_NAME_ALTERNATION = "|".join(
    map(re.escape, CLOSED_LIST_REASONS_BY_REASONED_MARKER_NAME)
)
# Any `.<marker>` attribute, so `from pytest import mark` and any alias of it are held too.
MARKER_USE = re.compile(
    rf"\.({REASONED_MARKER_NAME_ALTERNATION}|awaiting_macos_parity)\s*\(([^)]*)\)"
)
BARE_REASONED_MARKER_USE = re.compile(
    rf"\.({REASONED_MARKER_NAME_ALTERNATION})\b(?!\s*\()"
)
REASONED_MARKER_ARGUMENTS = re.compile(r'^reason="([^"]+)"$')
AWAITING_MACOS_PARITY_ARGUMENTS = re.compile(r"^issue=(\d+)$")
TICKET_LIST_ENTRY = re.compile(r"^\d+\. #(\d+)\b", re.M)


def _marked_test_files() -> "list[Path]":
    this_file = Path(__file__).resolve()
    return [
        test_file
        for marked_test_directory in MARKED_TEST_DIRECTORIES
        for test_file in sorted(marked_test_directory.glob("test_*.py"))
        if test_file.resolve() != this_file
    ]


def _marker_uses() -> "list[tuple[str, str, str]]":
    uses = []
    for test_file in _marked_test_files():
        test_file_name = str(test_file.relative_to(REPOSITORY_ROOT))
        for marker_name, arguments in MARKER_USE.findall(test_file.read_text(encoding="utf-8")):
            uses.append((test_file_name, marker_name, arguments.strip()))
    return uses


def test_the_markers_are_in_use():
    marker_names = {marker_name for _, marker_name, _ in _marker_uses()}
    assert CLOSED_LIST_REASONS_BY_REASONED_MARKER_NAME.keys() <= marker_names


def test_a_reasoned_marker_is_never_bare():
    for test_file in _marked_test_files():
        bare_marker_names = BARE_REASONED_MARKER_USE.findall(test_file.read_text(encoding="utf-8"))
        assert not bare_marker_names, (
            f"{test_file.relative_to(REPOSITORY_ROOT)}: {bare_marker_names[0]} needs reason= from "
            "its closed list"
        )


@pytest.mark.parametrize("reasoned_marker_name", sorted(CLOSED_LIST_REASONS_BY_REASONED_MARKER_NAME))
def test_a_reasoned_marker_names_only_its_closed_list(reasoned_marker_name: str):
    closed_list_reasons = CLOSED_LIST_REASONS_BY_REASONED_MARKER_NAME[reasoned_marker_name]
    for test_file_name, marker_name, arguments in _marker_uses():
        if marker_name != reasoned_marker_name:
            continue
        reason = REASONED_MARKER_ARGUMENTS.match(arguments)
        assert reason is not None, f"{test_file_name}: spell the reason as one literal: {arguments}"
        assert reason.group(1) in closed_list_reasons, (
            f"{test_file_name}: {reason.group(1)!r} is not on the {reasoned_marker_name} closed "
            f"list — {WHAT_AN_UNLISTED_REASON_HIDES_BY_REASONED_MARKER_NAME[reasoned_marker_name]}"
        )


def parity_tickets_an_active_change_still_lists() -> "set[int]":
    """Every ticket numbered under an active change's `## Tickets`, but #2400,
    which installs the markers rather than awaiting one."""
    listed = set()
    for change in ACTIVE_CHANGES_DIRECTORY.glob("*.md"):
        tickets_section = change.read_text(encoding="utf-8").partition("\n## Tickets\n")[2]
        tickets_section = tickets_section.split("\n## ", 1)[0]
        listed.update(int(number) for number in TICKET_LIST_ENTRY.findall(tickets_section))
    return listed - {2400}


def test_a_ticket_named_only_in_prose_is_not_read_as_listed():
    assert 2357 not in parity_tickets_an_active_change_still_lists(), (
        "#2357 is a shipped floor ticket the changes cite in prose"
    )


def test_awaiting_macos_parity_names_a_ticket_an_active_change_carries():
    parity_tickets = parity_tickets_an_active_change_still_lists()
    for test_file_name, marker_name, arguments in _marker_uses():
        if marker_name != "awaiting_macos_parity":
            continue
        issue = AWAITING_MACOS_PARITY_ARGUMENTS.match(arguments)
        assert issue is not None, f"{test_file_name}: spell the issue as one literal: {arguments}"
        assert int(issue.group(1)) in parity_tickets, (
            f"{test_file_name}: #{issue.group(1)} is listed under `## Tickets` by no active "
            f"change in {ACTIVE_CHANGES_DIRECTORY}"
        )

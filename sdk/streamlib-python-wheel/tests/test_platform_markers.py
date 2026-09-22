# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two platform markers are held to what the plan allows them to say.

`linux_only_capability` names only the closed list in
`docs/plan/changes/macos-capability-parity.md`, so a test red on macOS cannot
be quietly skipped. `awaiting_macos_parity` names only a ticket an active
change still lists under `## Tickets`, so the mark cannot outlive the work it
waits on.
"""

import re
from pathlib import Path

TESTS_DIRECTORY = Path(__file__).resolve().parent
ACTIVE_CHANGES_DIRECTORY = TESTS_DIRECTORY.parents[2] / "docs" / "plan" / "changes"

LINUX_ONLY_CAPABILITY_CLOSED_LIST_REASONS = {
    "MoltenVK has no VK_KHR_ray_tracing_pipeline",
    "VirtualCameraSink is v4l2loopback and PipeWire",
    "DMA-BUF and OPAQUE_FD are Linux file-descriptor handles",
    "the CUDA Array Interface is CUDA",
    # A test whose body is a Linux mechanism.
    "v4l2loopback and udev are Linux",
    "the boot session is a Linux kernel file",
    "only Linux resolves the runtime directory from XDG_RUNTIME_DIR",
}

MARKER_USE = re.compile(r"pytest\.mark\.(linux_only_capability|awaiting_macos_parity)\(([^)]*)\)")
LINUX_ONLY_CAPABILITY_ARGUMENTS = re.compile(r'^reason="([^"]+)"$')
AWAITING_MACOS_PARITY_ARGUMENTS = re.compile(r"^issue=(\d+)$")
TICKET_LIST_ENTRY = re.compile(r"^\d+\. #(\d+)\b", re.M)


def _marker_uses() -> "list[tuple[str, str, str]]":
    uses = []
    for test_file in sorted(TESTS_DIRECTORY.glob("test_*.py")):
        if test_file.name == Path(__file__).name:
            continue
        for marker_name, arguments in MARKER_USE.findall(test_file.read_text(encoding="utf-8")):
            uses.append((test_file.name, marker_name, arguments.strip()))
    return uses


def test_the_markers_are_in_use():
    marker_names = {marker_name for _, marker_name, _ in _marker_uses()}
    assert marker_names == {"linux_only_capability", "awaiting_macos_parity"}


def test_linux_only_capability_names_only_the_closed_list():
    for test_file_name, marker_name, arguments in _marker_uses():
        if marker_name != "linux_only_capability":
            continue
        reason = LINUX_ONLY_CAPABILITY_ARGUMENTS.match(arguments)
        assert reason is not None, f"{test_file_name}: spell the reason as one literal: {arguments}"
        assert reason.group(1) in LINUX_ONLY_CAPABILITY_CLOSED_LIST_REASONS, (
            f"{test_file_name}: {reason.group(1)!r} is not on the closed list — a test red on "
            "macOS for any other reason is a parity bug"
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

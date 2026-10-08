# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `streamlib` console script: the observation verbs and the one
machine-setup verb, `enable-virtual-camera`, until the native `tatolab` CLI
takes them.

`nodes`, `graph`, `tap`, `logs`, `exchange` and `mcp` observe nodes that are
already running — `nodes` off the on-disk registry, the rest as clients of a
node's local API. None of them mutates a graph; the control plane's mutation
tools (`add_node`, `connect`, `disconnect`, `remove_node`) are reached over
MCP. Starting a stream is `tatolab run`'s.

`enable-virtual-camera` touches no node and speaks no control plane: it installs
the standard udev grant the virtual camera's loopback door needs — the module
loaded with no devices and its control node tagged `uaccess` — as one
privileged step behind the desktop's own password prompt. The engine never
runs it; a sink without the permission names it and refuses.

streamlib:lint-logging:allow-file — a console script's user-facing output is
not a log event: every site prints a verb's result or reports a verb that has
already failed, where routing the user's own error into a log pipeline would
bury it.
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, Optional, Sequence

from ._control_plane_client import (
    ControlPlaneError,
    call_tool,
    resolve_local_api_socket_of_requested_node,
)
from ._local_api_mcp_stdio_pipe import pipe_stdio_to_the_runtimes_mcp_server
from ._node_registry import UntrustedRuntimeDirectoryError
from ._surface_image_exchange import (
    DEFAULT_SURFACE_ID_BAG_FIELD_NAME,
    SampledChannelExchangeReport,
    exchange_one_published_surface_id_into_directory,
    sample_channel_into_exchanged_surface_images,
)

if TYPE_CHECKING:
    from ._runtime_log_reader import LogRecordFilters

__all__ = ["main"]


class ObservationVerbUsageError(Exception):
    """An observation verb invoked with flags that contradict each other."""


class RuntimeLogFileNotFoundError(Exception):
    """`logs` named a runtime with no log file in the log directory."""


# ─── Observation verbs ───────────────────────────────────────────────────────


def print_the_node_registry_table() -> int:
    """`streamlib nodes`: this machine's registered control planes, one aligned row each."""
    from ._node_registry import registry_directory, scan_check_and_prune

    nodes = scan_check_and_prune()
    if not nodes:
        print(f"No running nodes found in {registry_directory()}.")
        return 0

    runtime_name_width = max(
        [len(node.entry.runtime_name) for node in nodes] + [len("RUNTIME_NAME")]
    )
    runtime_id_width = max(
        [len(node.entry.runtime_id) for node in nodes] + [len("RUNTIME_ID")]
    )
    local_api_socket_width = max(
        [len(node.entry.local_api_socket_path) for node in nodes]
        + [len("LOCAL_API_SOCKET")]
    )
    print(
        f"{'RUNTIME_NAME':<{runtime_name_width}}  {'RUNTIME_ID':<{runtime_id_width}}  "
        f"{'LOCAL_API_SOCKET':<{local_api_socket_width}}  "
        f"{'PID':>7}  {'ALIVE?':<6}  HINT"
    )
    for node in nodes:
        print(
            f"{node.entry.runtime_name:<{runtime_name_width}}  "
            f"{node.entry.runtime_id:<{runtime_id_width}}  "
            f"{node.entry.local_api_socket_path:<{local_api_socket_width}}  "
            f"{node.entry.pid:>7}  {'yes' if node.reachable else 'no':<6}  "
            f"{node.entry.hint}"
        )
    return 0


def call_observation_tool(
    tool_name: str,
    *,
    requested_node: "Optional[str]",
    arguments: "Optional[dict[str, Any]]" = None,
) -> int:
    """Resolve the target node, drive one tool, print its result."""
    local_api_socket = resolve_local_api_socket_of_requested_node(requested_node)
    print(call_tool(local_api_socket, tool_name, arguments or {}))
    return 0


def render_runtime_logs(
    *,
    runtime_id: "Optional[str]",
    list_runtimes: bool,
    follow: bool,
    filters: "LogRecordFilters",
) -> int:
    """`streamlib logs` in on-disk mode: enumerate runtimes, or render one's file."""
    from ._runtime_log_reader import (
        enumerate_runtime_log_files,
        format_size,
        format_started_at,
        newest_log_file_for_runtime,
        read_log_file,
        runtime_log_directory_path,
        wait_for_runtime_log_file,
    )

    log_directory = runtime_log_directory_path()

    if list_runtimes:
        ignored_alongside_list = [
            name
            for name, value in (
                ("RUNTIME_ID", runtime_id),
                ("--follow", follow),
                ("--processor", filters.processor),
                ("--pipeline", filters.pipeline),
                ("--rhi", filters.rhi_only),
                ("--level", filters.minimum_level),
                ("--source", filters.source),
                ("--intercepted-only", filters.intercepted_only),
            )
            if value
        ]
        if ignored_alongside_list:
            raise ObservationVerbUsageError(
                f"`--list` enumerates the runtimes that have log files and reads "
                f"none of them, so it takes no {', '.join(ignored_alongside_list)}."
            )
        log_files = sorted(
            enumerate_runtime_log_files(log_directory),
            key=lambda log_file: log_file.started_at_millis,
            reverse=True,
        )
        if not log_files:
            print(f"(no runtime log files in {log_directory})")
            return 0
        print(f"{'RUNTIME_ID':<24}  {'STARTED_AT':<24}  SIZE")
        for log_file in log_files:
            print(
                f"{log_file.runtime_id:<24}  "
                f"{format_started_at(log_file.started_at_millis):<24}  "
                f"{format_size(log_file.size_bytes)}"
            )
        return 0

    if runtime_id is None:
        raise ObservationVerbUsageError(
            "missing RUNTIME_ID.\n"
            "`streamlib logs --list` enumerates the runtimes that have log files, "
            "and `--node` reads a running node's live event stream instead."
        )

    log_file = newest_log_file_for_runtime(log_directory, runtime_id)
    if log_file is None:
        if not follow:
            raise RuntimeLogFileNotFoundError(
                f"no log file for runtime `{runtime_id}` in {log_directory}.\n"
                f"Use `streamlib logs --list` to see the runtimes that have one."
            )
        try:
            log_file = wait_for_runtime_log_file(log_directory, runtime_id, sys.stderr)
        except KeyboardInterrupt:
            return 0

    try:
        for rendered in read_log_file(
            log_file,
            filters,
            follow=follow,
            errors=sys.stderr,
            log_directory=log_directory,
        ):
            print(rendered)
    except KeyboardInterrupt:
        # Ctrl-C out of a `--follow` tail is how it ends, not a failure.
        pass
    return 0


# ---------------------------------------------------------------------------
# `enable-virtual-camera` — the one-time grant behind the loopback door
# ---------------------------------------------------------------------------

VIRTUAL_CAMERA_MODULE_NAME = "v4l2loopback"
VIRTUAL_CAMERA_CONTROL_NODE = Path("/dev/v4l2loopback")

# The three files the grant is, keyed by their destination. `modules-load.d`
# loads the module at boot, `modprobe.d` keeps it device-less so each sink
# creates its own, and the udev rule hands the seat's user the control node.
VIRTUAL_CAMERA_GRANT_FILES: "dict[Path, str]" = {
    Path("/etc/modules-load.d/streamlib-virtual-camera.conf"): (
        "# Installed by `streamlib enable-virtual-camera`: load the loopback module at boot.\n"
        f"{VIRTUAL_CAMERA_MODULE_NAME}\n"
    ),
    Path("/etc/modprobe.d/streamlib-virtual-camera.conf"): (
        "# Installed by `streamlib enable-virtual-camera`: no pre-made devices — each\n"
        "# StreamLib VirtualCameraSink creates and removes its own.\n"
        f"options {VIRTUAL_CAMERA_MODULE_NAME} devices=0\n"
    ),
    Path("/etc/udev/rules.d/70-streamlib-virtual-camera.rules"): (
        "# Installed by `streamlib enable-virtual-camera`: the logged-in seat user may\n"
        "# open the loopback control node, so a StreamLib graph can add a camera.\n"
        f'KERNEL=="{VIRTUAL_CAMERA_MODULE_NAME}", SUBSYSTEM=="misc", TAG+="uaccess"\n'
    ),
}


class MachineSetupError(Exception):
    """A setup verb that could not do its one job, with the reason shaped for a terminal."""


def render_virtual_camera_grant() -> str:
    """The three files as one printable block, for a user placing them by hand."""
    blocks = []
    for destination, contents in VIRTUAL_CAMERA_GRANT_FILES.items():
        blocks.append(f"# ---- {destination} ----\n{contents}")
    blocks.append(
        "# ---- then, as root ----\n"
        f"modprobe {VIRTUAL_CAMERA_MODULE_NAME} devices=0\n"
        "udevadm control --reload\n"
        f"udevadm trigger --subsystem-match=misc --sysname-match={VIRTUAL_CAMERA_MODULE_NAME}\n"
    )
    return "\n".join(blocks)


def virtual_camera_privileged_script() -> str:
    """One shell script that writes the files and reloads, run once with privilege."""
    lines = ["set -eu"]
    for destination, contents in VIRTUAL_CAMERA_GRANT_FILES.items():
        lines.append(f"mkdir -p {destination.parent}")
        lines.append(f"cat > {destination} <<'STREAMLIB_EOF'\n{contents}STREAMLIB_EOF")
    lines.append(f"modprobe {VIRTUAL_CAMERA_MODULE_NAME} devices=0")
    lines.append("udevadm control --reload")
    lines.append(f"udevadm trigger --subsystem-match=misc --sysname-match={VIRTUAL_CAMERA_MODULE_NAME}")
    lines.append("udevadm settle || true")
    return "\n".join(lines) + "\n"


def control_node_is_writable_by_this_user(control_node: Path = VIRTUAL_CAMERA_CONTROL_NODE) -> bool:
    """Whether this user can open the module's control node read-write — the
    same probe the sink makes at `setup()`. A raw descriptor, because the node
    is a character device and Python's buffered `open` would try to seek it."""
    try:
        descriptor = os.open(control_node, os.O_RDWR)
    except OSError:
        return False
    os.close(descriptor)
    return True


def virtual_camera_module_is_installed(kernel_release: str) -> bool:
    modules_root = Path("/lib/modules") / kernel_release
    return any(modules_root.rglob(f"{VIRTUAL_CAMERA_MODULE_NAME}.ko*"))


def choose_privilege_helper(
    which: "Optional[Callable[[str], Optional[str]]]" = None,
    environ: "Optional[dict[str, str]]" = None,
) -> "Optional[list[str]]":
    """`pkexec` under a desktop session, `sudo` in a headless shell, else `None`.

    `pkexec` needs a polkit agent to put a password dialog on screen, which a
    session has and an SSH shell does not; `sudo` prompts wherever there is a
    terminal.
    """
    if which is None:
        which = shutil.which
    environment = environ if environ is not None else dict(os.environ)
    has_session = bool(environment.get("DISPLAY") or environment.get("WAYLAND_DISPLAY"))
    if has_session and which("pkexec"):
        return ["pkexec"]
    if which("sudo"):
        return ["sudo"]
    if which("pkexec"):
        return ["pkexec"]
    return None


def enable_virtual_camera(*, print_only: bool) -> int:
    """Install the loopback grant once, or print it for a hand install."""
    if print_only:
        print(render_virtual_camera_grant(), end="")
        return 0
    if platform.system() != "Linux":
        raise MachineSetupError(
            f"`streamlib enable-virtual-camera` is Linux-only: the virtual camera is a "
            f"v4l2loopback device, and this is {platform.system()}."
        )
    kernel_release = platform.release()
    if not virtual_camera_module_is_installed(kernel_release):
        raise MachineSetupError(
            f"the {VIRTUAL_CAMERA_MODULE_NAME} module is not installed for kernel "
            f"{kernel_release}. Install `v4l2loopback-dkms` (Debian/Ubuntu; it builds "
            f"against the running kernel), or on a kernel that ships the module, "
            f"`linux-modules-{kernel_release}` — then re-run."
        )
    helper = choose_privilege_helper()
    if helper is None:
        raise MachineSetupError(
            "neither `pkexec` nor `sudo` is available to run the one privileged step. "
            "Place the files by hand instead: `streamlib enable-virtual-camera --print` "
            "writes them and the commands to run as root."
        )
    print(
        f"Installing the virtual camera permission via {helper[0]} — this is the one "
        "privileged step, and it asks for your password.",
        flush=True,
    )
    completed = subprocess.run(
        [*helper, "sh", "-c", virtual_camera_privileged_script()],
        text=True,
    )
    if completed.returncode != 0:
        raise MachineSetupError(
            f"{helper[0]} did not complete the privileged step (exit {completed.returncode}). "
            "Nothing else was changed; `--print` shows what it would have written."
        )
    if not VIRTUAL_CAMERA_CONTROL_NODE.exists():
        raise MachineSetupError(
            f"{VIRTUAL_CAMERA_CONTROL_NODE} did not appear after loading the module — "
            f"`modinfo {VIRTUAL_CAMERA_MODULE_NAME}` and `dmesg` say why."
        )
    if not control_node_is_writable_by_this_user():
        raise MachineSetupError(
            f"{VIRTUAL_CAMERA_CONTROL_NODE} exists but this user still cannot open it "
            "read-write. The udev rule tags it `uaccess`, which logind applies to the "
            "active seat: log out and back in, or if this is an SSH session, run a graph "
            "from the desktop."
        )
    print(
        f"Done: {VIRTUAL_CAMERA_CONTROL_NODE} is writable by this user. A VirtualCameraSink "
        "now creates its own camera; re-running this command is harmless.",
        flush=True,
    )
    return 0


def build_argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="streamlib",
        description="StreamLib — a realtime streaming engine with Python authoring.",
    )
    subcommands = parser.add_subparsers(dest="verb", required=True)

    enable_virtual_camera_command = subcommands.add_parser(
        "enable-virtual-camera",
        help="Grant this machine's users the permission a VirtualCameraSink needs, once.",
        description=(
            "Install the standard grant behind the virtual camera's loopback door: load "
            "v4l2loopback with no devices (persisted in modules-load.d and modprobe.d) "
            "and tag its control node `uaccess` for the logged-in user. One privileged "
            "step through pkexec (sudo in a headless shell); the engine never runs it."
        ),
    )
    enable_virtual_camera_command.add_argument(
        "--print",
        dest="print_only",
        action="store_true",
        help="Write the three files' contents and the commands to stdout and change nothing.",
    )

    def add_control_target_flag(command: argparse.ArgumentParser) -> None:
        """`--node`, which pins the node a verb drives.

        Without it, the verb resolves the sole live node, which is the whole
        ceremony for the common case of one node on the machine.
        """
        command.add_argument(
            "--node",
            dest="requested_node",
            metavar="RUNTIME_NAME_OR_ID",
            help=(
                "Registered runtime name or runtime_id to target, reached through its "
                "local API socket (resolved via the node registry)."
            ),
        )

    subcommands.add_parser(
        "nodes",
        help="List the running StreamLib nodes on this machine.",
        description=(
            "Scans the node registry, liveness-checks every entry, prunes the "
            "ones that are gone, and prints runtime_name, runtime_id, "
            "local_api_socket, pid, alive? and hint. Only runtimes hosting a "
            "control plane register."
        ),
    )

    graph_command = subcommands.add_parser(
        "graph",
        help="Export a running node's live graph as JSON.",
        description=(
            "Processors, ports, links, channel names, states and metrics, as the "
            "node reports them right now."
        ),
    )
    add_control_target_flag(graph_command)

    tap_command = subcommands.add_parser(
        "tap",
        help="Collect a bounded sample of raw bags from one channel.",
        description=(
            "Attaches a read-only tap to CHANNEL and collects a bounded sample. "
            "The tap forwards bags verbatim and never blocks the producer, so a "
            "quiet channel returns a partial sample rather than hanging."
        ),
    )
    tap_command.add_argument(
        "channel",
        help=(
            "The output port's address, <runtime_name>/<node>/<port>, as `graph` "
            "names them: its top-level runtime_name and a node's name."
        ),
    )
    tap_command.add_argument(
        "--count",
        type=int,
        metavar="N",
        help="Bags to collect before returning (default: a small sample).",
    )
    tap_command.add_argument(
        "--max-bag-bytes",
        type=int,
        metavar="BYTES",
        help=(
            "Per-bag ceiling on the bytes returned. A bag over the cap comes "
            "back flagged and cannot be decoded, so raise this rather than "
            "accept one (default: high enough to carry any audio block whole)."
        ),
    )
    add_control_target_flag(tap_command)

    exchange_command = subcommands.add_parser(
        "exchange",
        help="Exchange published surface ids for PNG files on disk.",
        description=(
            "With SURFACE_ID, exchanges that one id. With --channel, taps the "
            "channel, reads a surface id out of each sampled bag, and exchanges "
            "it — one warm process, no window in the graph and no display server "
            "in the path. Writes exact full-resolution PNGs into --out and prints "
            "their paths on stdout, one per line — those paths are this run's "
            "frames, and --out is not cleared, so read them rather than listing "
            "the directory."
        ),
    )
    exchange_command.add_argument(
        "surface_id",
        nargs="?",
        metavar="SURFACE_ID",
        help="A surface id a bag published, e.g. `{slot}#{generation}`.",
    )
    exchange_command.add_argument(
        "--out",
        dest="output_directory",
        required=True,
        type=Path,
        metavar="DIR",
        help="Directory the PNGs are written into (created when absent).",
    )
    exchange_command.add_argument(
        "--channel",
        metavar="CHANNEL",
        help="Sample this channel instead of naming one id, e.g. {proc}/{port}.",
    )
    exchange_command.add_argument(
        "--count",
        type=int,
        metavar="N",
        help="(--channel only) Frames to exchange before returning. Default 1.",
    )
    exchange_command.add_argument(
        "--every",
        dest="every_nth_bag",
        type=int,
        metavar="N",
        help="(--channel only) Exchange every Nth sampled bag. Default 1.",
    )
    exchange_command.add_argument(
        "--field",
        dest="surface_id_bag_field_name",
        metavar="NAME",
        help=(
            "(--channel only) Bag field carrying the surface id "
            f"(default: {DEFAULT_SURFACE_ID_BAG_FIELD_NAME})."
        ),
    )
    add_control_target_flag(exchange_command)

    mcp_command = subcommands.add_parser(
        "mcp",
        help="Connect an MCP host to a running node over this command's stdin and stdout.",
        description=(
            "For an MCP host to launch: `claude mcp add streamlib -- streamlib mcp`, or "
            "`ssh <machine> streamlib mcp` for a node on another machine. Copies bytes "
            "between stdio and the node's MCP server, through its local API socket, "
            "without reading them."
        ),
    )
    add_control_target_flag(mcp_command)

    logs_command = subcommands.add_parser(
        "logs",
        help="Read a runtime's JSONL log file, or a running node's event stream.",
        description=(
            "With RUNTIME_ID, renders that runtime's on-disk JSONL log exactly as "
            "the runtime mirrored it. With --node, collects a bounded "
            "sample of a running node's live event stream instead."
        ),
    )
    logs_command.add_argument(
        "runtime_id",
        nargs="?",
        metavar="RUNTIME_ID",
        help="Runtime to read logs for. Omit with --list or --node.",
    )
    logs_command.add_argument(
        "--list",
        dest="list_runtimes",
        action="store_true",
        help="Enumerate the runtimes that have log files instead of reading one.",
    )
    logs_command.add_argument(
        "-f",
        "--follow",
        action="store_true",
        help="Follow the log file as new records land (like `tail -F`).",
    )
    logs_command.add_argument(
        "--processor", metavar="ID", help="Only records from this processor id."
    )
    logs_command.add_argument(
        "--pipeline", metavar="ID", help="Only records from this pipeline id."
    )
    logs_command.add_argument(
        "--rhi", action="store_true", help="Only RHI operations (records with rhi_op)."
    )
    logs_command.add_argument(
        "--level",
        choices=["trace", "debug", "info", "warn", "error"],
        help="Minimum severity to show.",
    )
    logs_command.add_argument(
        "--source",
        choices=["rust", "python"],
        help="Only records emitted by this runtime language.",
    )
    logs_command.add_argument(
        "--intercepted-only",
        dest="intercepted_only",
        action="store_true",
        help="Only intercepted records (captured stdout/stderr/print).",
    )
    logs_command.add_argument(
        "--count",
        type=int,
        metavar="N",
        help="(--node only) Max events to collect before returning.",
    )
    add_control_target_flag(logs_command)

    return parser


def _print_sampled_channel_exchange_report(
    channel: str, report: "SampledChannelExchangeReport", wanted_image_count: int
) -> None:
    """Say what the run exchanged and what it had to retry, on stderr.

    stdout carries the paths and nothing else, so a harness can consume it
    directly; the accounting a human needs goes beside it rather than into it.
    """
    print(
        f"exchanged {len(report.written_image_paths)} of {wanted_image_count} "
        f"requested frames from `{channel}` "
        f"({report.bags_examined} bags examined over {report.tap_rounds} tap "
        f"{'round' if report.tap_rounds == 1 else 'rounds'})",
        file=sys.stderr,
    )
    if report.retried_recycled_surface_ids:
        print(
            f"retried {len(report.retried_recycled_surface_ids)} recycled "
            f"{'frame' if len(report.retried_recycled_surface_ids) == 1 else 'frames'} "
            f"against newer bags: {', '.join(report.retried_recycled_surface_ids)}",
            file=sys.stderr,
        )
    if report.bags_missing_the_surface_id_field:
        missing = report.bags_missing_the_surface_id_field
        print(
            f"{missing} {'bag' if missing == 1 else 'bags'} carried no surface id in "
            f"the named field — name the right one with `--field`",
            file=sys.stderr,
        )
    if report.stopped_early_because:
        print(f"error: {report.stopped_early_because}", file=sys.stderr)


def _run_exchange_verb(arguments: argparse.Namespace) -> int:
    """`exchange` has two forms; naming an id or a channel picks one.

    The channel-form flags have no meaning against a single id, so passing them
    with one is a wiring error rather than a silently-ignored flag.
    """
    if arguments.surface_id and arguments.channel:
        raise ObservationVerbUsageError(
            "`exchange` takes a surface id or `--channel`, not both. One id is one "
            "exchange; `--channel` samples ids off a channel."
        )
    if not arguments.surface_id and not arguments.channel:
        raise ObservationVerbUsageError(
            "`exchange` needs a surface id or `--channel`. Ids come from bags — "
            "`streamlib tap <channel>` shows what one carries."
        )

    if arguments.surface_id:
        channel_form_flags = [
            name
            for name, given in (
                ("--count", arguments.count is not None),
                ("--every", arguments.every_nth_bag is not None),
                ("--field", arguments.surface_id_bag_field_name is not None),
            )
            if given
        ]
        if channel_form_flags:
            raise ObservationVerbUsageError(
                f"{', '.join(channel_form_flags)} sample a channel, and a surface id "
                f"names one frame already. Use `--channel` instead of SURFACE_ID."
            )
        local_api_socket = resolve_local_api_socket_of_requested_node(arguments.requested_node)
        try:
            written_image_path = exchange_one_published_surface_id_into_directory(
                local_api_socket, arguments.surface_id, arguments.output_directory
            )
        except OSError as write_failure:
            # A `--out` that names an existing file, or a directory this user
            # cannot write: a typo, and typos get a message, not a traceback.
            raise ObservationVerbUsageError(
                f"could not write into `{arguments.output_directory}`: {write_failure}"
            ) from write_failure
        print(written_image_path)
        return 0

    wanted_image_count = 1 if arguments.count is None else arguments.count
    every_nth_bag = 1 if arguments.every_nth_bag is None else arguments.every_nth_bag
    if wanted_image_count < 1:
        raise ObservationVerbUsageError("`--count` must be at least 1.")
    if every_nth_bag < 1:
        raise ObservationVerbUsageError("`--every` must be at least 1.")

    local_api_socket = resolve_local_api_socket_of_requested_node(arguments.requested_node)
    report = sample_channel_into_exchanged_surface_images(
        local_api_socket,
        arguments.channel,
        arguments.output_directory,
        wanted_image_count=wanted_image_count,
        every_nth_bag=every_nth_bag,
        surface_id_bag_field_name=(
            arguments.surface_id_bag_field_name or DEFAULT_SURFACE_ID_BAG_FIELD_NAME
        ),
    )
    for image_path in report.written_image_paths:
        print(image_path)
    _print_sampled_channel_exchange_report(
        arguments.channel, report, wanted_image_count
    )
    # A short sample is a failure, not a partial success: a harness that read the
    # directory and found fewer frames than it asked for would otherwise take
    # exit 0 as "this is all the channel had".
    return 0 if len(report.written_image_paths) == wanted_image_count else 1


def _run_logs_verb(arguments: argparse.Namespace) -> int:
    """`logs` has two modes; `--node` picks the live one.

    The on-disk filters have no meaning against a live event stream (the tool
    takes a count and nothing else), so asking for both is a wiring error rather
    than a silently-ignored flag.
    """
    from ._runtime_log_reader import LogRecordFilters

    targets_a_running_node = bool(arguments.requested_node)
    if targets_a_running_node:
        conflicting = [
            name
            for name, value in (
                ("RUNTIME_ID", arguments.runtime_id),
                ("--list", arguments.list_runtimes),
                ("--follow", arguments.follow),
                ("--processor", arguments.processor),
                ("--pipeline", arguments.pipeline),
                ("--rhi", arguments.rhi),
                ("--level", arguments.level),
                ("--source", arguments.source),
                ("--intercepted-only", arguments.intercepted_only),
            )
            if value
        ]
        if conflicting:
            raise ObservationVerbUsageError(
                f"`--node` reads a running node's live event stream, which "
                f"takes no {', '.join(conflicting)}. Drop `--node` to read "
                f"an on-disk log file instead."
            )
        return call_observation_tool(
            "logs",
            requested_node=arguments.requested_node,
            arguments={"count": arguments.count} if arguments.count else {},
        )

    if arguments.count is not None:
        raise ObservationVerbUsageError(
            "`--count` bounds a live event-stream sample; it has no meaning for an "
            "on-disk log file. Use `--node`, or drop `--count`."
        )
    return render_runtime_logs(
        runtime_id=arguments.runtime_id,
        list_runtimes=arguments.list_runtimes,
        follow=arguments.follow,
        filters=LogRecordFilters(
            processor=arguments.processor,
            pipeline=arguments.pipeline,
            rhi_only=arguments.rhi,
            minimum_level=arguments.level,
            source=arguments.source,
            intercepted_only=arguments.intercepted_only,
        ),
    )


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_argument_parser()
    arguments = parser.parse_args(argv)

    try:
        if arguments.verb == "nodes":
            return print_the_node_registry_table()
        if arguments.verb == "enable-virtual-camera":
            return enable_virtual_camera(print_only=arguments.print_only)
        if arguments.verb == "graph":
            return call_observation_tool(
                "graph",
                requested_node=arguments.requested_node,
            )
        if arguments.verb == "tap":
            tap_arguments: "dict[str, Any]" = {"channel": arguments.channel}
            if arguments.count is not None:
                tap_arguments["count"] = arguments.count
            if arguments.max_bag_bytes is not None:
                tap_arguments["max_bag_bytes"] = arguments.max_bag_bytes
            return call_observation_tool(
                "tap",
                requested_node=arguments.requested_node,
                arguments=tap_arguments,
            )
        if arguments.verb == "exchange":
            return _run_exchange_verb(arguments)
        if arguments.verb == "logs":
            return _run_logs_verb(arguments)
        return pipe_stdio_to_the_runtimes_mcp_server(arguments.requested_node)
    except (
        RuntimeLogFileNotFoundError,
        ObservationVerbUsageError,
        ControlPlaneError,
        MachineSetupError,
        UntrustedRuntimeDirectoryError,
    ) as failure:
        print(f"error: {failure}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


from __future__ import annotations

import select
import sys
import traceback
from typing import Any, Optional

from tatolab.stream import (
    NodeLinkDataAccess,
    log,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
    NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP,
)

from tatolab.runtime._engine import (
    BuildsOfANodeScopeComputedValue,
    not_ready_report_on_runtime_context,
    note_execution_mode_from_parent_on_runtime_context,
    note_pause_state_from_parent_on_runtime_context,
    note_stop_request_from_parent_on_runtime_context,
    queue_a_fire_of_each_trigger_input,
    read_each_unlinked_input_port_as_its_pin_value,
    run_the_scheduling_loop_of_this_helper,
    take_each_function_node_raise_newly_noted,
    why_each_function_node_its_islands_call_is_raising,
)
from tatolab.runtime._function_node_compilation import (
    FunctionNodeIslandsFedToTheirPorts,
    feed_each_input_port_from_its_function_node_island,
)
from tatolab.runtime._function_node_pool import (
    FunctionNodePool,
    take_the_carries_a_running_pool_is_handed,
    the_node_class_the_function_node_pool_runs_as,
)
from tatolab.runtime._function_node_run_as_a_standalone_node import (
    evaluate_a_promoted_function_node_at_its_setup,
    ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs,
)
from tatolab.runtime._not_ready_placeholder_frames import (
    PLACEHOLDER_FRAMES_PER_SECOND_OF_A_MANUAL_NODE,
    TransparentFramesOnTheImageOutputsOfANodeNotReady,
)
from tatolab.runtime._node_function_pin_group_slots import NodeFunctionPinGroupSlotsFollowed
from tatolab.runtime._node_hosting import (
    class_node_pin_groups_of,
    is_a_node_function,
    the_slots_its_pin_groups_are_built_with,
)
from tatolab.runtime._node_the_wheel_hosts import NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG
from tatolab.runtime._parent_process_bridge import EscalateRequestError, ParentProcessBridge
from tatolab.runtime._node_class_description import load_node_class
from tatolab.runtime._hosted_node import (
    HostedNode,
    construct_hosted_node,
    the_words_a_process_raise_is_shown_in,
    unwire_link_data_access,
    wire_link_data_access,
)


if sys.platform == "darwin":
    from tatolab.runtime._engine import (
        note_this_helper_processes_callbacks_returned_after_its_parent_went_away,
    )


# How long `teardown` waits, before answering, for the releases already queued to
# reach the parent. The parent gives a helper up the moment it answers, and a
# release still queued then is never taken; bounded well inside the shutdown
# ladder's five-second teardown budget.
RELEASES_SENT_BEFORE_TEARDOWN_ANSWERS_TIMEOUT_SECONDS = 1.0


def computed_value_builds_on_the_escalate_wire(
    builds: BuildsOfANodeScopeComputedValue,
) -> "dict[str, Any]":
    """How the builds of one computed value stand, as `set_computed_value_builds`
    carries them: its latest build's failure by its own kind, `None` while none
    failed."""
    last_build = builds.last_build
    latest_build_failed: "Optional[dict[str, Any]]" = None
    if last_build.state == "failed":
        latest_build_failed = {"kind": last_build.failure}
        if last_build.failure != "read_a_computed_value_not_yet_built":
            latest_build_failed["why"] = last_build.why
    return {
        "declared": builds.declared,
        "has_a_value": builds.has_a_value,
        "building_again": builds.building_again,
        "latest_build_failed": latest_build_failed,
    }


class HelperProcessLifecycle:

    def __init__(
        self,
        bridge: ParentProcessBridge,
        node_class: type,
        runtime_id: str,
        node_run_id: str,
        link_data_access: NodeLinkDataAccess,
    ) -> None:
        self._bridge = bridge
        self._node_class = node_class
        self._runs_the_function_node_pool = False
        self._runtime_id = runtime_id
        self._node_run_id = node_run_id
        self._link_data_access = link_data_access
        self._hosted: Optional[HostedNode] = None
        self._set_up_succeeded = False
        self._setup_raised_on_its_own = False
        self._running = False
        self._torn_down = False
        self._board_service_name_of_a_stream_clock_that_may_step: "Optional[str]" = None
        self._paused = False
        self._scheduling_loop_runs = False
        self._pause_hook_last_called: "Optional[str]" = None
        self._function_node_islands_it_reads: "Optional[FunctionNodeIslandsFedToTheirPorts]" = None
        self._pin_group_slots_followed: "Optional[NodeFunctionPinGroupSlotsFollowed]" = None
        self._field_wire_values_last_refused: "Optional[str]" = None
        self._whether_the_parent_last_took_process_as_raising: "Optional[bool]" = False
        self._raises_the_parent_holds_by_function_node: "dict[str, str]" = {}
        self._not_ready_reason_the_parent_last_took: "Optional[str]" = None
        self._the_parent_holds_the_last_not_ready_report = True
        self._transparent_frames_while_not_ready: "Optional[TransparentFramesOnTheImageOutputsOfANodeNotReady]" = None
        self._not_ready_warnings_already_logged: "set[str]" = set()
        self._computed_value_builds_the_parent_last_took: "Optional[list[dict[str, Any]]]" = []

    def run_until_the_parent_is_done(self) -> None:
        while not self._torn_down:
            try:
                command = self._next_lifecycle_command_ticking_a_manual_node()
                if command is None:
                    log.info("the parent closed the channel; shutting down")
                    self._tear_down_after_the_parent_went_away()
                    return
                self._dispatch(command)
            except KeyboardInterrupt:
                # The ladder's interrupt can land anywhere the main thread is,
                # not only inside a hook — a `poll`, a queue wait, a native
                # call returning. Wherever it lands it leaves the execution
                # loop and never the process: `stop` and `teardown` are already
                # queued behind it.
                self._running = False
                log.warn(
                    "the engine's shutdown ladder interrupted this node; "
                    "the bag in flight is lost"
                )

    def _next_lifecycle_command_ticking_a_manual_node(
        self,
    ) -> "Optional[dict[str, Any]]":
        """The next lifecycle command. While a manual node runs, the wait ticks
        on a steady beat: each tick tells the parent whether the node is ready,
        since a node's own thread may report either; and while it is not paused
        takes the values a node function's wires gave since the last, as a
        frame of any other node does, telling the parent when a function node
        that read called starts or stops raising, and, while it is not ready, sends a
        transparent frame on each of its Image outputs."""
        tick_seconds = 1.0 / PLACEHOLDER_FRAMES_PER_SECOND_OF_A_MANUAL_NODE
        while self._a_manual_node_runs():
            self._bridge.clear_lifecycle_command_arrivals()
            was_waiting, command = self._bridge.next_lifecycle_command_if_waiting()
            if was_waiting:
                return command
            self._tell_the_parent_whether_the_node_is_ready()
            if not self._paused:
                self._take_the_new_values_at_a_tick()
                self._tell_the_parent_when_a_function_node_starts_or_stops_raising()
                self._send_transparent_frames_while_the_node_is_not_ready()
            select.select([self._bridge.lifecycle_command_arrival_fd()], [], [], tick_seconds)
        return self._bridge.next_lifecycle_command()

    def _take_the_new_values_at_a_tick(self) -> None:
        """Give each value input of a node written as one function that a wire feeds the newest
        value its wire gave, written to its scope as a frame of any other node does; a paused node
        reads no wire, as a paused frame-driven node does. Never waits on a build."""
        assert self._hosted is not None
        if self._hosted.a_node_function:
            self._take_each_new_value_a_wire_gives_a_field()

    def _a_manual_node_runs(self) -> bool:
        return self._running and not self._scheduling_loop_runs and self._hosted is not None

    def _tell_the_parent_whether_the_node_is_ready(self) -> None:
        """Send the parent why the node is not ready when that changes, and
        `None` once it is ready again, then how the builds of a node function's
        computed values stand. After a report the parent refused or did not
        answer, what it holds is unknown, so the next call sends it again."""
        if self._hosted is None:
            return
        self._tell_the_parent_how_the_computed_value_builds_stand()
        not_ready_report = not_ready_report_on_runtime_context(self._hosted.full_access_context)
        not_ready_because = None if not_ready_report is None else not_ready_report[0]
        if (
            self._the_parent_holds_the_last_not_ready_report
            and not_ready_because == self._not_ready_reason_the_parent_last_took
        ):
            return
        try:
            self._bridge.request_from_parent(
                {"op": "set_not_ready_because", "not_ready_because": not_ready_because}
            )
        except EscalateRequestError as refusal:
            log.error(
                "the parent did not take whether this node is ready; the next hook tells it "
                "again",
                error=str(refusal),
            )
            self._the_parent_holds_the_last_not_ready_report = False
            return
        if not_ready_because is not None and not_ready_because != self._not_ready_reason_the_parent_last_took:
            log.warn("this node is not ready", reason=not_ready_because)
        self._not_ready_reason_the_parent_last_took = not_ready_because
        self._the_parent_holds_the_last_not_ready_report = True

    def _tell_the_parent_how_the_computed_value_builds_stand(self) -> None:
        """Send the parent how the builds of each computed value of a node
        function stand when that changes, so the graph shows a build still
        running or failed on the node."""
        assert self._hosted is not None
        if not self._hosted.a_node_function:
            return
        builds = [
            computed_value_builds_on_the_escalate_wire(of_one)
            for of_one in self._hosted.instance.builds_of_each_computed_value()
        ]
        if builds == self._computed_value_builds_the_parent_last_took:
            return
        try:
            self._bridge.request_from_parent({"op": "set_computed_value_builds", "builds": builds})
        except EscalateRequestError as refusal:
            log.error(
                "the parent did not take how this node's computed values build; the next hook "
                "tells it again",
                error=str(refusal),
            )
            self._computed_value_builds_the_parent_last_took = None
            return
        self._computed_value_builds_the_parent_last_took = builds

    def _send_transparent_frames_while_the_node_is_not_ready(self) -> None:
        """Send a transparent frame on each of the node's Image outputs, while
        it reports itself not ready."""
        if self._hosted is None:
            return
        not_ready_report = not_ready_report_on_runtime_context(self._hosted.full_access_context)
        if not_ready_report is None:
            return
        _, image_width, image_height = not_ready_report
        if self._transparent_frames_while_not_ready is None:
            self._transparent_frames_while_not_ready = (
                TransparentFramesOnTheImageOutputsOfANodeNotReady(
                    list(getattr(self._node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP, [])),
                    self._warn_once_while_not_ready,
                )
            )
        self._transparent_frames_while_not_ready.send_one_transparent_frame_on_each_image_output(
            self._hosted.limited_access_context, image_width, image_height
        )

    def _warn_once_while_not_ready(self, message: str, fields: "dict[str, Any]") -> None:
        key = f"{message}:{fields.get('port')}"
        if key in self._not_ready_warnings_already_logged:
            return
        self._not_ready_warnings_already_logged.add(key)
        log.warn(message, **fields)

    def _tear_down_after_the_parent_went_away(self) -> None:
        if self._hosted is None or self._torn_down or self._setup_raised_on_its_own:
            return
        self._stop()
        if sys.platform == "darwin":
            note_this_helper_processes_callbacks_returned_after_its_parent_went_away()
        self._teardown()

    def _dispatch(self, command: "dict[str, Any]") -> None:
        verb = command.get("cmd", "")
        if verb == "setup":
            self._setup(command)
        elif verb == "run":
            # Deliberately unanswered: `run` enters the execution loop, and a
            # reply here would be read as the answer to whatever the parent
            # sends next.
            self._run(command)
        elif verb == "stop":
            self._stop()
        elif verb == "teardown":
            self._teardown()
        elif verb in ("on_pause", "on_resume"):
            self._note_pause(verb)
        elif verb == "carry_links":
            self._carry_links(command)
        elif verb == "unwire_link":
            # Deliberately unanswered, like `run`: the parent sends this from
            # its compiler while it holds the graph write lock, so it cannot
            # wait, and a reply nobody reads becomes the answer to whatever it
            # sends next.
            self._unwire_link(command)
        elif verb == "wire_link":
            # A link the engine handed over after `setup` read the envelope.
            # Answered, unlike `unwire_link`: the parent reports the link wired
            # only once this side says its port is open, and the answer rides
            # its own link-scoped rpc tag so it never becomes the reply to the
            # next lifecycle command.
            self._wire_link(command)
        elif verb == "feed_island":
            # Answered on the island's link, like `wire_link`.
            self._feed_island(command)
        elif verb == "unfeed_island":
            # Unanswered, like `unwire_link`.
            self._unfeed_island(command)
        elif verb == "set_input_values":
            # Unanswered, like `unwire_link`. Taken here on the main thread,
            # between two `process` calls or at a manual node's steady tick.
            self._set_input_values(command)
        else:
            log.warn("the parent sent an unknown lifecycle command", cmd=verb)

    def _setup(self, command: "dict[str, Any]") -> None:
        try:
            input_values = command.get("input_values") or {}
            configuration = _the_settings_a_node_the_wheel_builds_takes(self._node_class, input_values)
            if self._node_class is FunctionNodePool:
                self._runs_the_function_node_pool = True
                self._node_class = the_node_class_the_function_node_pool_runs_as(
                    {"carries": input_values.get("carries") or []}, self._link_data_access
                )
                configuration = None
            if command.get("function_node_evaluated_at_setup"):
                evaluate_a_promoted_function_node_at_its_setup(self._node_class)
            port_wiring = command.get("ports") or {}
            slot_counts, slot_input_ports, slot_output_ports = the_slots_its_pin_groups_are_built_with(
                self._node_class, port_wiring
            )
            self._link_data_access.declare_ports(
                [
                    *(
                        port["name"]
                        for port in getattr(
                            self._node_class, NODE_DECLARATION_INPUT_PORTS_STAMP, []
                        )
                    ),
                    *slot_input_ports,
                ],
                [
                    *(
                        port["name"]
                        for port in getattr(
                            self._node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP, []
                        )
                    ),
                    *slot_output_ports,
                ],
            )
            self._link_data_access.keep_the_last_message_of_output_ports(
                command.get("output_ports_keeping_their_last_message") or []
            )
            # Opened before any link is wired, so every link mirrors its losses
            # from its first. Only a stand-in parent sends no board.
            loss_count_board = command.get("loss_count_board")
            if loss_count_board is not None:
                self._link_data_access.open_loss_count_board(
                    loss_count_board["service_name"],
                    loss_count_board["output_ports"],
                    [
                        (entry["breadcrumb_number"], entry["resumes_from_f64_bits"])
                        for entry in loss_count_board.get("function_node_states", [])
                    ],
                    loss_count_board.get("function_node_frame_interval_ns"),
                )
            stream_clock = command.get("stream_clock") or {}
            stream_clock_board_service_name = stream_clock.get("board_service_name")
            if stream_clock_board_service_name is not None:
                self._link_data_access.adopt_the_stream_clock_board(
                    stream_clock_board_service_name
                )
                if stream_clock.get("mode") in ("stepped", "scaled") and not self._paced_by_real_time():
                    self._board_service_name_of_a_stream_clock_that_may_step = (
                        stream_clock_board_service_name
                    )
            wire_link_data_access(self._link_data_access, port_wiring)
            self._function_node_islands_it_reads = (
                feed_each_input_port_from_its_function_node_island(
                    self._link_data_access,
                    port_wiring.get("islands") or [],
                    load_node_class,
                )
            )
            read_each_unlinked_input_port_as_its_pin_value(
                link_data_access=self._link_data_access,
                pin_values=_the_number_each_input_value_reads(port_wiring.get("input_values")),
            )
            self._hosted = construct_hosted_node(
                self._node_class,
                configuration,
                self._link_data_access,
                self._runtime_id,
                self._node_run_id,
                self._bridge,
                port_wiring.get("input_values"),
                slot_counts,
            )
            self._hosted.call_hook_letting_failure_propagate(
                "setup", self._hosted.full_access_context
            )
            pin_groups = class_node_pin_groups_of(self._node_class)
            if pin_groups and is_a_node_function(self._node_class):
                self._pin_group_slots_followed = NodeFunctionPinGroupSlotsFollowed(
                    pin_groups, port_wiring, link_data_access=self._link_data_access
                )
        except KeyboardInterrupt:
            # The ladder interrupted a setup that outran its budget. The parent
            # hears a refusal rather than nothing, and this helper stays up to
            # take the `teardown` the same ladder sends next.
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": "setup() was interrupted by the engine's shutdown ladder",
                }
            )
            return
        except Exception as setup_failure:
            self._setup_raised_on_its_own = True
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": f"{setup_failure}\n{traceback.format_exc()}",
                }
            )
            return
        self._set_up_succeeded = True
        self._tell_the_parent_whether_the_node_is_ready()
        ready: "dict[str, Any]" = {"rpc": "ready"}
        function_nodes_ran_compiled = (
            ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(
                self._node_class
            )
        )
        if self._function_node_islands_it_reads is not None:
            function_nodes_ran_compiled.update(
                self._function_node_islands_it_reads.ran_compiled_by_node_id
            )
        if function_nodes_ran_compiled:
            ready["function_nodes_ran_compiled"] = function_nodes_ran_compiled
        if self._pin_group_slots_followed is not None:
            # The parent gives this node no rebuild when its slots change: it follows them here.
            ready["takes_its_pin_group_slots_live"] = True
        self._bridge.send(ready)

    def _run(self, command: "dict[str, Any]") -> None:
        if self._hosted is None:
            log.warn("the parent sent `run` before `setup`")
            return
        execution_mode = command.get("execution", "reactive")
        try:
            note_execution_mode_from_parent_on_runtime_context(
                self._hosted.full_access_context, execution_mode
            )
        except ValueError as refusal:
            log.error(f"the node cannot run: {refusal}")
            return
        self._running = True
        note_stop_request_from_parent_on_runtime_context(
            self._hosted.full_access_context, False
        )
        if execution_mode in ("reactive", "continuous"):
            self._run_the_scheduling_loop(execution_mode, int(command.get("interval_ms") or 0))
        elif execution_mode == "manual":
            self._start_failing_the_node_when_it_raises()
        else:
            log.warn("the parent named an unknown execution mode", mode=execution_mode)

    def _start_failing_the_node_when_it_raises(self) -> None:
        assert self._hosted is not None
        try:
            self._hosted.call_hook_letting_failure_propagate(
                "start", self._hosted.full_access_context
            )
            self._tell_the_parent_whether_the_node_is_ready()
        except KeyboardInterrupt:
            log.warn(
                "start() was interrupted by the engine's shutdown ladder",
                hook="start",
            )
        except Exception as start_failure:
            log.error(
                "start() raised",
                error=str(start_failure),
                traceback=traceback.format_exc(),
            )
            self._running = False
            try:
                self._bridge.request_from_parent(
                    {
                        "op": "fail_the_node",
                        "reason": f"start() raised: {start_failure}",
                    }
                )
            except EscalateRequestError as refusal:
                log.error(
                    "the parent did not take this node's failure; the graph may still "
                    "show it running",
                    error=str(refusal),
                )

    def _run_the_scheduling_loop(self, execution_mode: str, interval_ms: int) -> None:
        board_service_name = self._board_service_name_of_a_stream_clock_that_may_step
        if (
            execution_mode == "continuous"
            and board_service_name is not None
            and self._link_data_access.stream_clock_board_publish_listener_fd() is None
        ):
            self._link_data_access.listen_for_each_stream_clock_board_publish(board_service_name)
        self._scheduling_loop_runs = True
        try:
            run_the_scheduling_loop_of_this_helper(
                node_interpreter=self,
                link_data_access=self._link_data_access,
                lifecycle_command_arrival_fd=self._bridge.lifecycle_command_arrival_fd(),
                execution_mode=execution_mode,
                interval_ms=interval_ms,
                paced_by_real_time=self._paced_by_real_time(),
                stream_clock_may_step=board_service_name is not None,
                paused=self._paused,
            )
        finally:
            self._scheduling_loop_runs = False

    def _paced_by_real_time(self) -> bool:
        return getattr(self._node_class, NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP, False) is True

    def call_hook_from_the_scheduling_loop(self, hook_name: str) -> None:
        """Make the callback the engine's scheduling loop asks for."""
        assert self._hosted is not None
        if hook_name in ("on_pause", "on_resume"):
            self._call_the_pause_hook_once_per_transition(hook_name)
            return
        if hook_name == "process" and self._hosted.fields is not None:
            self._take_each_new_value_a_wire_gives_a_field()
        if hook_name == "process":
            self._send_transparent_frames_while_the_node_is_not_ready()
        raised = self._hosted.call_hook(hook_name, self._hosted.limited_access_context)
        if hook_name == "process":
            self._tell_the_parent_when_process_starts_or_stops_raising(raised)
            self._tell_the_parent_when_a_function_node_starts_or_stops_raising()
            self._tell_the_parent_whether_the_node_is_ready()

    def _tell_the_parent_when_process_starts_or_stops_raising(
        self, raised: "Optional[Exception]"
    ) -> None:
        """Send the parent what `process()` raised when a run of raises begins,
        and `None` when a clean call ends one, so the graph shows the node
        raising while it keeps running; a call that ends as the last one did
        sends nothing. After a report the parent refused or did not answer,
        what it holds is unknown, so the next call sends its own outcome."""
        process_is_raising = raised is not None
        if process_is_raising == self._whether_the_parent_last_took_process_as_raising:
            return
        try:
            self._bridge.request_from_parent(
                {
                    "op": "set_process_raising_because",
                    "raising_because": (
                        None if raised is None else the_words_a_process_raise_is_shown_in(raised)
                    ),
                }
            )
        except EscalateRequestError as refusal:
            log.error(
                "the parent did not take whether this node's process() raises; the next "
                "call tells it again",
                error=str(refusal),
            )
            self._whether_the_parent_last_took_process_as_raising = None
            return
        self._whether_the_parent_last_took_process_as_raising = process_is_raising

    def _tell_the_parent_when_a_function_node_starts_or_stops_raising(self) -> None:
        """Send the parent what each function node this helper's islands call
        raised when a run of its raises begins, and `None` when a clean call
        ends one, so the graph shows the function node raising while this node
        keeps running. A report the parent refused or did not answer is sent
        again after the next call. The first raise of each run is logged once,
        with its traceback."""
        for function_node, raising_because, raised_traceback in (
            take_each_function_node_raise_newly_noted(link_data_access=self._link_data_access)
        ):
            log.error(
                "a function node this node's islands call raised; it hands out nothing until "
                "it runs cleanly",
                function_node=function_node,
                error=raising_because,
                traceback=raised_traceback,
            )
        raising_because_by_function_node = why_each_function_node_its_islands_call_is_raising(
            link_data_access=self._link_data_access
        )
        raises_the_parent_holds = self._raises_the_parent_holds_by_function_node
        if raising_because_by_function_node == raises_the_parent_holds:
            return
        reports: "list[tuple[str, Optional[str]]]" = [
            (function_node, raising_because)
            for function_node, raising_because in raising_because_by_function_node.items()
            if function_node not in raises_the_parent_holds
        ]
        reports.extend(
            (function_node, None)
            for function_node in raises_the_parent_holds
            if function_node not in raising_because_by_function_node
        )
        for function_node, raising_because in reports:
            try:
                self._bridge.request_from_parent(
                    {
                        "op": "set_function_node_raising_because",
                        "function_node": function_node,
                        "raising_because": raising_because,
                    }
                )
            except EscalateRequestError as refusal:
                log.error(
                    "the parent did not take whether a function node raises; the next call "
                    "tells it again",
                    function_node=function_node,
                    error=str(refusal),
                )
                continue
            if raising_because is None:
                del raises_the_parent_holds[function_node]
            else:
                raises_the_parent_holds[function_node] = raising_because

    def _take_each_new_value_a_wire_gives_a_field(self) -> None:
        assert self._hosted is not None and self._hosted.fields is not None
        try:
            self._hosted.fields.take_each_new_value_a_wire_gives(
                self._hosted.instance, fed_by_an_island=self._input_ports_an_island_feeds()
            )
        except Exception as wire_value_refused:
            if str(wire_value_refused) != self._field_wire_values_last_refused:
                self._field_wire_values_last_refused = str(wire_value_refused)
                log.error(
                    "this node's fields could not take the values their wires gave",
                    error=str(wire_value_refused),
                )
            return
        self._field_wire_values_last_refused = None

    def _input_ports_an_island_feeds(self) -> "frozenset[str]":
        islands = self._function_node_islands_it_reads
        return frozenset() if islands is None else islands.input_ports_fed

    def _hand_back_what_a_link_stopped_driving(self, input_port: str) -> None:
        """Give the field of `input_port` its own value again, now a link stopped feeding it."""
        if self._hosted is None or self._hosted.fields is None:
            return
        try:
            self._hosted.fields.hand_back_the_set_value_of(
                self._hosted.instance,
                input_port,
                fed_by_an_island=self._input_ports_an_island_feeds(),
            )
        except Exception as set_value_refused:
            log.error(
                "this node's field could not take its set value again once its wire was gone",
                input_port=input_port,
                error=str(set_value_refused),
            )

    def take_the_lifecycle_commands_that_woke_the_scheduling_loop(self) -> "tuple[bool, bool]":
        self._bridge.clear_lifecycle_command_arrivals()
        self._drain_commands_arriving_mid_run()
        return (self._running and not self._torn_down, self._paused)

    def _call_the_pause_hook_once_per_transition(self, hook_name: str) -> None:
        """Call `on_pause` or `on_resume`, never the one called last again: a
        pause noted before the loop started is not announced twice."""
        assert self._hosted is not None
        if hook_name == self._pause_hook_last_called:
            return
        self._pause_hook_last_called = hook_name
        self._hosted.call_hook(hook_name, self._hosted.limited_access_context)

    def _drain_commands_arriving_mid_run(self) -> None:
        while self._running:
            was_waiting, command = self._bridge.next_lifecycle_command_if_waiting()
            if not was_waiting:
                return
            if command is None:
                self._running = False
                return
            self._dispatch_mid_run(command)

    def _dispatch_mid_run(self, command: "dict[str, Any]") -> None:
        if command.get("cmd") == "run":
            log.warn("the parent sent `run` while this node was already running")
            return
        self._dispatch(command)

    def _stop(self) -> None:
        self._running = False
        if self._hosted is not None:
            note_stop_request_from_parent_on_runtime_context(
                self._hosted.full_access_context, True
            )
            self._hosted.call_hook("stop", self._hosted.full_access_context)
        self._bridge.send({"rpc": "stopped"})

    def _teardown(self) -> None:
        self._running = False
        self._torn_down = True
        if self._hosted is not None:
            note_stop_request_from_parent_on_runtime_context(
                self._hosted.full_access_context, True
            )
            self._hosted.call_hook("teardown", self._hosted.full_access_context)
        try:
            every_release_sent = self._bridge.wait_for_every_release_queued_so_far(
                RELEASES_SENT_BEFORE_TEARDOWN_ANSWERS_TIMEOUT_SECONDS
            )
        except KeyboardInterrupt:
            # The ladder's interrupt ends the wait, never the answer: the
            # parent is still owed `done`.
            every_release_sent = False
        if not every_release_sent:
            log.warn(
                "this helper answered teardown with releases still unsent; what they "
                "name may stay allocated until the runtime stops"
            )
        self._bridge.send({"rpc": "done"})

    def _note_pause(self, verb: str) -> None:
        """Record the pause the parent announced; while a scheduling loop runs,
        the loop makes the callback itself."""
        self._paused = verb == "on_pause"
        if self._hosted is not None:
            note_pause_state_from_parent_on_runtime_context(
                self._hosted.full_access_context, verb == "on_pause"
            )
            if not self._scheduling_loop_runs:
                self._call_the_pause_hook_on_the_step_ledger(verb)
        self._bridge.send({"rpc": "ok"})

    def _call_the_pause_hook_on_the_step_ledger(self, hook_name: str) -> None:
        """Call `hook_name` outside a scheduling loop, counted in flight on the
        step ledger while it runs when the stream's clock may step."""
        if self._board_service_name_of_a_stream_clock_that_may_step is None:
            self._call_the_pause_hook_once_per_transition(hook_name)
            return
        self._link_data_access.note_a_step_callback_started()
        try:
            self._call_the_pause_hook_once_per_transition(hook_name)
        finally:
            self._link_data_access.note_a_step_callback_returned(None)

    def _unwire_link(self, command: "dict[str, Any]") -> None:
        try:
            unwire_link_data_access(self._link_data_access, command)
        except Exception as unwire_failure:
            log.error(
                "this node could not release a disconnected link's port",
                link_id=command.get("link_id"),
                error=str(unwire_failure),
            )
        if command.get("direction") == "input" and isinstance(command.get("port"), str):
            self._hand_back_what_a_link_stopped_driving(command["port"])
        self._follow_a_link_unwired_from_a_pin_group_slot(command.get("link_id"))

    def _wire_link(self, command: "dict[str, Any]") -> None:
        direction = command.get("direction")
        link = command.get("link") or {}
        link_id = link.get("link_id")
        if not self._set_up_succeeded:
            self._answer_the_parents_wire_link(
                link_id, "this node's setup did not succeed, so it opens no port"
            )
            return
        if direction == "input":
            port_wiring = {"inputs": [link]}
        elif direction == "output":
            port_wiring = {"outputs": [link]}
        else:
            log.warn(
                "the parent asked to wire a link in an unknown direction",
                direction=direction,
                link_id=link_id,
            )
            self._answer_the_parents_wire_link(link_id, f"unknown link direction {direction!r}")
            return
        slots_refused = self._follow_a_link_wired_into_a_pin_group_slot(link_id, link.get("name"))
        if slots_refused is not None:
            self._follow_a_link_unwired_from_a_pin_group_slot(link_id)
            self._answer_the_parents_wire_link(link_id, slots_refused)
            return
        try:
            wire_link_data_access(self._link_data_access, port_wiring)
        except Exception as wire_failure:
            log.error(
                "this node could not open its port for a link wired after setup",
                link_id=link_id,
                direction=direction,
                error=str(wire_failure),
            )
            self._follow_a_link_unwired_from_a_pin_group_slot(link_id)
            self._answer_the_parents_wire_link(link_id, str(wire_failure))
            return
        self._answer_the_parents_wire_link(link_id, None)

    def _feed_island(self, command: "dict[str, Any]") -> None:
        island = command.get("island") or {}
        link_id = island.get("link_id")
        input_port = island.get("input_port")
        if not self._set_up_succeeded or self._function_node_islands_it_reads is None:
            self._answer_the_parents_wire_link(
                link_id, "this node's setup did not succeed, so no island feeds its inputs"
            )
            return
        slots_refused = self._follow_a_link_wired_into_a_pin_group_slot(link_id, input_port)
        if slots_refused is not None:
            self._follow_a_link_unwired_from_a_pin_group_slot(link_id)
            self._answer_the_parents_wire_link(link_id, slots_refused)
            return
        try:
            self._function_node_islands_it_reads.feed_an_input_port_from_its_island(island)
        except Exception as feed_failure:
            log.error(
                "this node could not evaluate an island the engine handed it after setup",
                link_id=link_id,
                input_port=input_port,
                error=str(feed_failure),
            )
            self._follow_a_link_unwired_from_a_pin_group_slot(link_id)
            self._answer_the_parents_wire_link(link_id, str(feed_failure))
            return
        self._answer_the_parents_wire_link(link_id, None)

    def _unfeed_island(self, command: "dict[str, Any]") -> None:
        input_port = command.get("port")
        if self._function_node_islands_it_reads is None or not isinstance(input_port, str):
            return
        try:
            self._function_node_islands_it_reads.stop_feeding_an_input_port(input_port)
        except Exception as unfeed_failure:
            log.error(
                "this node could not stop evaluating an island unlinked from it",
                link_id=command.get("link_id"),
                input_port=input_port,
                error=str(unfeed_failure),
            )
        self._hand_back_what_a_link_stopped_driving(input_port)
        self._follow_a_link_unwired_from_a_pin_group_slot(command.get("link_id"))

    def _set_input_values(self, command: "dict[str, Any]") -> None:
        try:
            read_each_unlinked_input_port_as_its_pin_value(
                link_data_access=self._link_data_access,
                pin_values=_the_number_each_input_value_reads(command.get("input_values")),
            )
        except Exception as input_values_refused:
            log.error(
                "this node could not read the input values the engine handed it",
                error=str(input_values_refused),
            )
        if self._hosted is not None and self._hosted.fields is not None:
            try:
                self._hosted.fields.take_set_values(
                    self._hosted.instance, command.get("input_values") or {}
                )
            except Exception as field_values_refused:
                log.error(
                    "this node's fields could not take the input values the engine handed it",
                    error=str(field_values_refused),
                )
        if self._pin_group_slots_followed is not None:
            self._pin_group_slots_followed.note_the_input_values(command.get("input_values") or {})
            self._give_the_node_function_its_pin_group_slots()
        fired = command.get("fired") or []
        if not fired:
            return
        try:
            queue_a_fire_of_each_trigger_input(
                link_data_access=self._link_data_access,
                fired=fired,
            )
        except Exception as fire_refused:
            log.error(
                "this node could not queue the fires the engine handed it",
                fired=fired,
                error=str(fire_refused),
            )

    def _follow_a_link_wired_into_a_pin_group_slot(
        self, link_id: "Optional[str]", port: "Optional[str]"
    ) -> "Optional[str]":
        """Give a node function the slot a link or an island wired into its port `port` reaches,
        while it plays; why it could not hold it, or `None`."""
        followed = self._pin_group_slots_followed
        if followed is None or not isinstance(link_id, str) or not isinstance(port, str):
            return None
        followed.note_a_link_wired(link_id, port)
        return self._give_the_node_function_its_pin_group_slots()

    def _follow_a_link_unwired_from_a_pin_group_slot(self, link_id: "Optional[str]") -> None:
        """Drop the slots of a node function no link or island reaches any more, once link or
        island `link_id` is unwired, while it plays."""
        followed = self._pin_group_slots_followed
        if followed is None or not isinstance(link_id, str):
            return
        followed.note_a_link_unwired(link_id)
        self._give_the_node_function_its_pin_group_slots()

    def _give_the_node_function_its_pin_group_slots(self) -> "Optional[str]":
        """Give a node function the slots its pin groups' wiring gives them; why it could not,
        logged, with every group left holding the slots it held, or `None`."""
        followed, hosted = self._pin_group_slots_followed, self._hosted
        if followed is None or hosted is None or hosted.fields is None:
            return None
        try:
            followed.give_the_node_its_slots(hosted.instance, hosted.fields)
        except Exception as slots_refused:
            log.error(
                "this node could not hold the slots its pin groups' wiring gives them",
                slot_counts=followed.slot_counts(),
                error=str(slots_refused),
            )
            return str(slots_refused)
        return None

    def _answer_the_parents_wire_link(self, link_id: "Optional[str]", refusal: "Optional[str]") -> None:
        if link_id is None:
            log.warn("the parent asked to wire a link it did not name")
            return
        if refusal is None:
            self._bridge.send({"rpc": "link_wired", "link_id": link_id})
        else:
            self._bridge.send(
                {"rpc": "link_wire_failed", "link_id": link_id, "reason": refusal}
            )

    def _carry_links(self, command: "dict[str, Any]") -> None:
        # Answered with the cause rather than logged: the parent puts the pool back as it was
        # only when it hears the refusal.
        if self._hosted is None or not self._set_up_succeeded or not self._runs_the_function_node_pool:
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": "this helper runs no function node pool that is set up, so it "
                    "carries no link",
                }
            )
            return
        try:
            take_the_carries_a_running_pool_is_handed(
                type(self._hosted.instance), {"carries": command.get("carries") or []}, self._link_data_access
            )
            self._link_data_access.keep_the_last_message_of_output_ports(
                command.get("output_ports_keeping_their_last_message") or []
            )
        except Exception as carries_refused:
            self._bridge.send({"rpc": "error", "error": str(carries_refused)})
            return
        self._bridge.send({"rpc": "ok"})
        self._tell_the_parent_whether_the_node_is_ready()

def _the_number_each_input_value_reads(input_values: "Optional[dict[str, Any]]") -> "dict[str, float]":
    """The number each input of `input_values` reads while no link feeds it:
    its number, or 1 and 0 for a bool. A value of another kind reaches no
    port, so its input reads only its links."""
    return {
        input_name: float(value)
        for input_name, value in (input_values or {}).items()
        if isinstance(value, (bool, int, float))
    }


def _the_settings_a_node_the_wheel_builds_takes(
    node_class: Any, input_values: "dict[str, Any]"
) -> "Optional[dict[str, Any]]":
    """The settings a node the wheel builds from them, a function node run as a node of its own,
    takes of `input_values`: each naming none of its input ports. `None` for an author's node,
    which takes each value as the input of its name."""
    if not getattr(node_class, NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG, False):
        return None
    input_ports = {port["name"] for port in getattr(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP, [])}
    return {name: value for name, value in input_values.items() if name not in input_ports}

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


from __future__ import annotations

import select
import struct
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
    not_ready_report_on_runtime_context,
    note_execution_mode_from_parent_on_runtime_context,
    note_pause_state_from_parent_on_runtime_context,
    note_stop_request_from_parent_on_runtime_context,
    read_each_unlinked_input_port_as_its_pin_value,
    run_the_scheduling_loop_of_this_helper,
    take_each_function_node_raise_newly_noted,
    why_each_function_node_its_islands_call_is_raising,
)
from tatolab.runtime._function_node_compilation import (
    FunctionNodeIslandsFedToTheirPorts,
    feed_each_input_port_from_its_function_node_island,
    the_value_a_config_field_takes,
)
from tatolab.runtime._function_node_pool import (
    FunctionNodePool,
    the_node_class_the_function_node_pool_runs_as,
)
from tatolab.runtime._function_node_run_as_a_standalone_node import (
    ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs,
)
from tatolab.runtime._not_ready_placeholder_frames import (
    PLACEHOLDER_FRAMES_PER_SECOND_OF_A_MANUAL_NODE,
    TransparentFramesOnTheImageOutputsOfANodeNotReady,
)
from tatolab.runtime._node_hosting import (
    apply_configuration,
)
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

#: Wire contract: a control value is one little-endian f64 sample.
_CONTROL_VALUE_SAMPLE_FORMAT = "<d"

# How long `teardown` waits, before answering, for the releases already queued to
# reach the parent. The parent gives a helper up the moment it answers, and a
# release still queued then is never taken; bounded well inside the shutdown
# ladder's five-second teardown budget.
RELEASES_SENT_BEFORE_TEARDOWN_ANSWERS_TIMEOUT_SECONDS = 1.0


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
        self._config_fields_its_node_exposes_as_inputs: "tuple[str, ...]" = ()
        self._config_as_last_applied: "dict[str, Any]" = {}
        self._config_its_node_was_last_handed: "dict[str, Any]" = {}
        self._exposed_config_values_last_refused: "dict[str, Any]" = {}
        self._whether_the_parent_last_took_process_as_raising: "Optional[bool]" = False
        self._raises_the_parent_holds_by_function_node: "dict[str, str]" = {}
        self._not_ready_reason_the_parent_last_took: "Optional[str]" = None
        self._the_parent_holds_the_last_not_ready_report = True
        self._transparent_frames_while_not_ready: "Optional[TransparentFramesOnTheImageOutputsOfANodeNotReady]" = None
        self._not_ready_warnings_already_logged: "set[str]" = set()

    def run_until_the_parent_is_done(self) -> None:
        while not self._torn_down:
            try:
                command = self._next_lifecycle_command_feeding_a_not_ready_manual_node()
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

    def _next_lifecycle_command_feeding_a_not_ready_manual_node(
        self,
    ) -> "Optional[dict[str, Any]]":
        """The next lifecycle command. While a manual node runs, the wait ticks
        on a steady beat: each tick tells the parent whether the node is ready,
        since a node's own thread may report either, and while it is not ready
        and not paused sends a transparent frame on each of its Image outputs."""
        tick_seconds = 1.0 / PLACEHOLDER_FRAMES_PER_SECOND_OF_A_MANUAL_NODE
        while self._a_manual_node_runs():
            self._bridge.clear_lifecycle_command_arrivals()
            was_waiting, command = self._bridge.next_lifecycle_command_if_waiting()
            if was_waiting:
                return command
            self._tell_the_parent_whether_the_node_is_ready()
            if not self._paused:
                self._send_transparent_frames_while_the_node_is_not_ready()
            select.select([self._bridge.lifecycle_command_arrival_fd()], [], [], tick_seconds)
        return self._bridge.next_lifecycle_command()

    def _a_manual_node_runs(self) -> bool:
        return self._running and not self._scheduling_loop_runs and self._hosted is not None

    def _tell_the_parent_whether_the_node_is_ready(self) -> None:
        """Send the parent why the node is not ready when that changes, and
        `None` once it is ready again. After a report the parent refused or did
        not answer, what it holds is unknown, so the next call sends it again."""
        if self._hosted is None:
            return
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
        elif verb == "update_config":
            self._update_config(command)
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
        elif verb == "set_pin_values":
            # Unanswered, like `unwire_link`.
            self._set_pin_values(command)
        elif verb == "set_exposed_config_fields":
            # Unanswered, like `unwire_link`.
            self._take_the_config_fields_its_node_exposes_as_inputs(
                command.get("exposed_config_fields") or []
            )
        else:
            log.warn("the parent sent an unknown lifecycle command", cmd=verb)

    def _setup(self, command: "dict[str, Any]") -> None:
        try:
            configuration = command.get("config")
            if self._node_class is FunctionNodePool:
                self._node_class = the_node_class_the_function_node_pool_runs_as(
                    configuration or {}, self._link_data_access
                )
                configuration = None
            port_wiring = command.get("ports") or {}
            self._take_the_config_fields_its_node_exposes_as_inputs(
                port_wiring.get("exposed_config_fields") or []
            )
            self._config_as_last_applied = dict(configuration or {})
            self._config_its_node_was_last_handed = dict(configuration or {})
            self._link_data_access.declare_ports(
                [
                    port["name"]
                    for port in getattr(
                        self._node_class, NODE_DECLARATION_INPUT_PORTS_STAMP, []
                    )
                ],
                [
                    port["name"]
                    for port in getattr(
                        self._node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP, []
                    )
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
                pin_values=dict(port_wiring.get("pins") or {}),
            )
            self._hosted = construct_hosted_node(
                self._node_class,
                configuration,
                self._link_data_access,
                self._runtime_id,
                self._node_run_id,
                self._bridge,
            )
            self._hosted.call_hook_letting_failure_propagate(
                "setup", self._hosted.full_access_context
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
        if hook_name == "process" and self._config_fields_its_node_exposes_as_inputs:
            self._take_each_new_value_a_link_gives_an_exposed_config_field()
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

    def _take_the_config_fields_its_node_exposes_as_inputs(self, exposed_config_fields: "list[str]") -> None:
        """Read each of `exposed_config_fields` from the input port of its name, in place of those read before."""
        self._config_fields_its_node_exposes_as_inputs = tuple(exposed_config_fields)
        self._link_data_access.declare_ports(list(exposed_config_fields), [])

    def _take_each_new_value_a_link_gives_an_exposed_config_field(self) -> None:
        assert self._hosted is not None
        changed_values_by_field: "dict[str, Any]" = {}
        for field in self._config_fields_its_node_exposes_as_inputs:
            payload = self._hosted.limited_access_context.inputs.read(field)
            if payload is None:
                continue
            (input_value,) = struct.unpack_from(_CONTROL_VALUE_SAMPLE_FORMAT, bytes(payload))
            value = the_value_a_config_field_takes(self._config_as_last_applied.get(field), input_value)
            if self._config_as_last_applied.get(field) != value:
                changed_values_by_field[field] = value
        if not changed_values_by_field:
            return
        configuration = {**self._config_as_last_applied, **changed_values_by_field}
        try:
            apply_configuration(self._hosted.instance, configuration)
        except Exception as configuration_refused:
            if changed_values_by_field != self._exposed_config_values_last_refused:
                self._exposed_config_values_last_refused = changed_values_by_field
                log.error(
                    "this node refused a value a link gave a config field it exposes as an input",
                    fields=sorted(changed_values_by_field),
                    error=str(configuration_refused),
                )
            return
        self._exposed_config_values_last_refused = {}
        self._config_as_last_applied = configuration

    def _hand_back_the_config_value_of_an_exposed_field_a_link_stopped_driving(self, field: str) -> None:
        if self._hosted is None or field not in self._config_fields_its_node_exposes_as_inputs:
            return
        configuration = {
            config_field: value
            for config_field, value in self._config_as_last_applied.items()
            if config_field != field
        }
        if field in self._config_its_node_was_last_handed:
            configuration[field] = self._config_its_node_was_last_handed[field]
        if configuration == self._config_as_last_applied:
            return
        try:
            apply_configuration(self._hosted.instance, configuration)
        except Exception as configuration_refused:
            log.error(
                "this node refused the config value of a field its link stopped driving",
                field=field,
                error=str(configuration_refused),
            )
            return
        self._config_as_last_applied = configuration

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
            self._hand_back_the_config_value_of_an_exposed_field_a_link_stopped_driving(
                command["port"]
            )

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
        try:
            wire_link_data_access(self._link_data_access, port_wiring)
        except Exception as wire_failure:
            log.error(
                "this node could not open its port for a link wired after setup",
                link_id=link_id,
                direction=direction,
                error=str(wire_failure),
            )
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
        try:
            self._function_node_islands_it_reads.feed_an_input_port_from_its_island(island)
        except Exception as feed_failure:
            log.error(
                "this node could not evaluate an island the engine handed it after setup",
                link_id=link_id,
                input_port=input_port,
                error=str(feed_failure),
            )
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
        self._hand_back_the_config_value_of_an_exposed_field_a_link_stopped_driving(input_port)

    def _set_pin_values(self, command: "dict[str, Any]") -> None:
        try:
            read_each_unlinked_input_port_as_its_pin_value(
                link_data_access=self._link_data_access,
                pin_values=dict(command.get("pins") or {}),
            )
        except Exception as pin_values_refused:
            log.error(
                "this node could not read the pin values the engine handed it",
                error=str(pin_values_refused),
            )

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

    def _update_config(self, command: "dict[str, Any]") -> None:
        # Answered with the cause rather than logged: the parent keeps the
        # graph's previous configuration only when it hears the refusal.
        if self._hosted is None or not self._set_up_succeeded:
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": "this node's setup did not succeed, so it takes no "
                    "configuration",
                }
            )
            return
        try:
            apply_configuration(self._hosted.instance, command.get("config"))
        except Exception as configuration_failure:
            self._bridge.send({"rpc": "error", "error": str(configuration_failure)})
            return
        self._config_as_last_applied = dict(command.get("config") or {})
        self._config_its_node_was_last_handed = dict(command.get("config") or {})
        self._bridge.send({"rpc": "ok"})
        self._tell_the_parent_whether_the_node_is_ready()

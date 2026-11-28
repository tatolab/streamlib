# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node a helper hosts, and the link data access it reads and writes through."""

from __future__ import annotations

import os
import traceback
from typing import TYPE_CHECKING, Any, Optional

from tatolab.stream import (
    NodeLinkDataAccess,
    RuntimeContextFullAccess,
    log,
)

from tatolab.runtime._engine import (
    limited_access_view_of_runtime_context_for_helper_process,
    open_runtime_context_full_access_for_helper_process,
)
from tatolab.runtime._node_hosting import (
    is_a_node_function,
    class_node_pin_groups_of,
    construct_node_instance,
    the_members_of_each_slot_built,
)
from tatolab.runtime._node_the_wheel_hosts import (
    is_a_node_the_wheel_builds_itself,
    link_input_data_reader_of,
    link_output_data_writer_of,
)
from tatolab.runtime._parent_process_bridge import ParentProcessBridge

if TYPE_CHECKING:
    from tatolab.stream._class_node_declaration import ClassNodeMember
    from tatolab.runtime._class_node_fields import ClassNodeFields


def wire_link_data_access(
    link_data_access: NodeLinkDataAccess, port_wiring: "dict[str, Any]"
) -> None:
    """Open this node's publishers and subscribers, one call per link.

    Inputs are wired before outputs so a helper that is both a source and a
    destination is ready to receive before it can publish.
    """
    for input_link in port_wiring.get("inputs", []):
        link_data_access.wire_input_link(
            input_link["name"],
            input_link["channel_service_name"],
            input_link["inbound_link_name"],
            input_link["notify_service_name"],
            input_link["read_mode"],
            input_link["channel_service_creation_depth"],
            input_link["input_port_ring_depth"],
            input_link["max_subscribers"],
            input_link["notify_max_notifiers"],
            input_link["link_id"],
            # Absent on every port that declares no window contract, which is
            # unchanged in every respect. Present, it is the values already
            # resolved: a `match_device` sentinel settles in the parent, which is
            # where the device stream is.
            input_link.get("audio_window"),
            # Read with `get` so a helper with its board open names the key the
            # binding was not given, rather than raising a bare `KeyError`.
            loss_count_slot=input_link.get("loss_count_slot"),
            wiring_generation=input_link.get("wiring_generation"),
            input_port_borrow_quota=input_link.get("input_port_borrow_quota"),
            # Present only on a link that starts at one member of a bundle.
            member_taken_at_the_read=input_link.get("member_taken_at_the_read"),
            # Present only on a link that ends at one member of a bundle.
            member_put_at_the_read=input_link.get("member_put_at_the_read"),
        )
    for output_link in port_wiring.get("outputs", []):
        link_data_access.wire_output_link(
            output_link["name"],
            output_link["channel_service_name"],
            output_link["dest_notify_service_name"],
            output_link["expected_payload_bytes"],
            output_link["max_payload_bytes_per_channel"],
            output_link["channel_service_creation_depth"],
            output_link["max_subscribers"],
            output_link["notify_max_notifiers"],
            output_link["link_id"],
            output_port_wiring_generation=output_link.get("output_port_wiring_generation"),
        )


def unwire_link_data_access(
    link_data_access: NodeLinkDataAccess, command: "dict[str, Any]"
) -> None:
    """Release this node's own port for one link the engine disconnected.

    The engine reclaims what it holds and cannot reach what this process opened
    from the envelope, so it names the local port and direction and this side
    drops it. Left open, the port is still counted against its channel when the
    same link reconnects.
    """
    link_id = command["link_id"]
    direction = command["direction"]
    if direction == "output":
        link_data_access.unwire_output_link(command["port"], link_id)
    elif direction == "input":
        link_data_access.unwire_input_link(link_id)
    else:
        log.warn(
            "the parent asked to unwire a link in an unknown direction",
            direction=direction,
            link_id=link_id,
        )


# =============================================================================
# The lifecycle loop
# =============================================================================


class HostedNode:
    """One node, its contexts, and the hooks it actually defined.

    Which hooks exist is resolved once here rather than rediscovered per tick,
    the same way the parent's host does it.
    """

    LIFECYCLE_HOOKS = (
        "setup",
        "teardown",
        "process",
        "start",
        "stop",
        "on_pause",
        "on_resume",
    )

    def __init__(
        self,
        node_instance: Any,
        full_access_context: RuntimeContextFullAccess,
        limited_access_context: Any,
        fields: "Optional[ClassNodeFields]" = None,
        *,
        a_node_function: bool = False,
    ) -> None:
        self.instance = node_instance
        self.full_access_context = full_access_context
        self.limited_access_context = limited_access_context
        self.fields = fields
        """The value inputs of a node function, which follow their values; `None` for any other
        node."""
        self.a_node_function = a_node_function
        """Whether the node is written as one function, whose value inputs are written to its
        scope."""
        self._declared_hooks = {
            hook for hook in self.LIFECYCLE_HOOKS if hasattr(node_instance, hook)
        }

    def call_hook(self, hook_name: str, context: Any) -> "Optional[Exception]":
        """Call `hook_name` if the node defined one, logging and answering what
        it raised.

        A hook raising here is not fatal to the helper — the node keeps
        being driven, exactly as it would in the parent. Nor is the
        `KeyboardInterrupt` the parent's shutdown ladder delivers to a callback
        that outran its budget: the bag in flight is lost and the rungs behind
        it still run.
        """
        if hook_name not in self._declared_hooks:
            return None
        try:
            getattr(self.instance, hook_name)(context)
        except KeyboardInterrupt:
            log.warn(
                f"{hook_name}() was interrupted by the engine's shutdown ladder",
                hook=hook_name,
            )
        except Exception as hook_failure:
            log.error(
                f"{hook_name}() raised",
                error=str(hook_failure),
                traceback=traceback.format_exc(),
            )
            return hook_failure
        return None

    def call_hook_letting_failure_propagate(self, hook_name: str, context: Any) -> None:
        """Call `hook_name`, leaving what it raised to the caller.

        `setup` is the one hook whose failure the parent must hear about: it
        is the handshake the parent is blocked on, and a node that could
        not set itself up must not report ready.
        """
        if hook_name not in self._declared_hooks:
            return
        getattr(self.instance, hook_name)(context)


def construct_hosted_node(
    node_class: type,
    configuration: "Optional[dict[str, Any]]",
    link_data_access: NodeLinkDataAccess,
    runtime_id: str,
    node_run_id: str,
    bridge: ParentProcessBridge,
    stored_input_values: "Optional[dict[str, Any]]" = None,
    slot_counts: "Optional[dict[str, int]]" = None,
) -> HostedNode:
    """Build the user's object and the two contexts its hooks receive.

    The bridge's escalate round trip is what the GPU surface crosses to the
    parent on — without it, `ctx.gpu_limited_access` refuses by name. A node
    function is built from `stored_input_values`, each value input holding
    its value, each pin bound and each pin group holding the slots
    `slot_counts` says before its body runs.
    """
    full_access_context = open_runtime_context_full_access_for_helper_process(
        link_data_access,
        runtime_id,
        node_run_id,
        bridge.request_from_parent,
        bridge.release_to_parent_without_waiting,
    )
    limited_access_context = limited_access_view_of_runtime_context_for_helper_process(
        full_access_context
    )
    if is_a_node_function(node_class):
        return _host_a_node_function(
            node_class,
            full_access_context,
            limited_access_context,
            link_data_access=link_data_access,
            node_run_id=node_run_id,
            stored_input_values=stored_input_values,
            slot_counts=slot_counts or {},
        )
    if not is_a_node_the_wheel_builds_itself(node_class):
        raise TypeError(
            f"`{node_class.__module__}:{node_class.__qualname__}` is not a node written as one "
            f"function, so nothing runs it. To fix: declare it with `@node(...)` on a `def`, "
            f"through a `tatolab-stream` that declares one."
        )
    return HostedNode(
        construct_node_instance(node_class, configuration),
        full_access_context,
        limited_access_context,
    )


def _host_a_node_function(
    node_function: Any,
    full_access_context: RuntimeContextFullAccess,
    limited_access_context: Any,
    *,
    link_data_access: NodeLinkDataAccess,
    node_run_id: str,
    stored_input_values: "Optional[dict[str, Any]]",
    slot_counts: "dict[str, int]",
) -> HostedNode:
    """A node written as one function, each value input holding its stored value, each pin
    bound and each pin group holding the slots `slot_counts` says, its body left for `setup` to
    run."""
    from tatolab.runtime._class_node_bound_pins import bind_the_pins_of_a_class_node
    from tatolab.runtime._class_node_fields import ClassNodeFields
    from tatolab.runtime._node_function_running import NodeFunctionRunning

    inputs = link_input_data_reader_of(full_access_context)
    outputs = link_output_data_writer_of(full_access_context)

    def bind_pins(members: "tuple[ClassNodeMember, ...]") -> "dict[str, Any]":
        return bind_the_pins_of_a_class_node(
            members,
            link_data_access=link_data_access,
            inputs=inputs,
            outputs=outputs,
            node_id=node_run_id,
        )

    members_of_each_slot = the_members_of_each_slot_built(
        class_node_pin_groups_of(node_function), slot_counts
    )
    fields = ClassNodeFields(
        (
            *node_function.members,
            *(
                member
                for slots in members_of_each_slot.values()
                for slot_members in slots
                for member in slot_members
            ),
        ),
        link_data_access=link_data_access,
        inputs=inputs,
        node_id=node_run_id,
        stored_input_values=stored_input_values,
        assign_field_values=NodeFunctionRunning.write_inputs,
    )
    running = NodeFunctionRunning(
        node_function,
        values_to_build_with=fields.values_to_build_with(),
        pins=bind_pins(node_function.members),
        bind_pins=bind_pins,
        slot_counts=slot_counts,
    )
    return HostedNode(
        running, full_access_context, limited_access_context, fields, a_node_function=True
    )


def the_words_a_process_raise_is_shown_in(raised: BaseException) -> str:
    """`process() raised <type>: <message>`, then where in the node's own code it
    was raised, as the graph shows a node whose `process()` keeps raising."""
    return f"process() {the_words_a_raise_is_shown_in(raised)}"


def the_words_a_raise_is_shown_in(raised: BaseException) -> str:
    """`raised <type>: <message>`, then where in the code it was raised, as the
    graph shows a node whose code keeps raising."""
    words = "".join(traceback.format_exception_only(type(raised), raised)).strip()
    raised_at = traceback.extract_tb(raised.__traceback__)[-1:] if raised.__traceback__ else []
    if not raised_at:
        return f"raised {words}"
    frame = raised_at[0]
    filename = frame.filename
    try:
        relative_filename = os.path.relpath(filename)
        if not relative_filename.startswith(os.pardir):
            filename = relative_filename
    except ValueError:
        pass
    return f"raised {words} ({filename}, line {frame.lineno})"

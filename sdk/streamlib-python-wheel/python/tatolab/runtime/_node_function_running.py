# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node written as one function, as its helper runs it: the body once at setup, the function it
returned each frame.

Each value input is an input of the node's scope, holding its set value or its default, each
pin is bound before the body runs, and each pin group is an input of the scope holding its
slots. The body declares its computed values into the scope, which builds them on its worker
from then on; a value set or wired after is written to the scope, and rebuilds only what read
it. A frame runs once every computed value has its first value; a node that paces itself
returns no per-frame function, and its computed values publish on their own. A group's slots
change while the node plays: a value set on a slot's field, or a slot held or dropped, writes
the group's input with the slots it holds now. Stopping the node closes the scope, and with it
every value a build made.
"""

from __future__ import annotations

import copy
import threading
import time
from collections.abc import Mapping
from typing import TYPE_CHECKING, Any, Callable, Optional

from tatolab.stream._class_node_construction import build_pin_group_slot
from tatolab.stream._class_node_pin_group import PIN_GROUP_SLOT_PORT_NAME_SEPARATOR

from tatolab.stream._node_declaration import NODE_DECLARATION_EXECUTION_STAMP
from tatolab.stream._pin_declaration import NO_DEFAULT

from tatolab.runtime._engine import (
    BuildsOfANodeScopeComputedValue,
    NodeScopeBeingDeclared,
    NodeScopeInput,
    RunningNodeScope,
)

if TYPE_CHECKING:
    from tatolab.stream._class_node_declaration import ClassNodeMember, ClassNodePinGroup
    from tatolab.stream._node_function import NodeFunction

    BindPins = Callable[["tuple[ClassNodeMember, ...]"], "Mapping[str, object]"]
    """The pin object of each pin of the members given, by its port's name."""

__all__ = [
    "NodeFunctionBodyRanAgain",
    "NodeFunctionBodyReturnedNoPerFrameFunction",
    "NodeFunctionInputNotDeclared",
    "NodeFunctionPacingItselfReturnedAValue",
    "NodeFunctionPinGroupNotDeclared",
    "NodeFunctionRunning",
    "NodeFunctionValueInputHasNoValue",
]

#: How long a frame waits when a computed value has no first value yet, so a reactive node with a
#: message waiting is not called again at once for as long as its first build runs.
UNBUILT_FRAME_BACKOFF_SECONDS = 0.001


class NodeFunctionBodyRanAgain(RuntimeError):
    """A node function's setup was called after its body had already run."""


class NodeFunctionInputNotDeclared(ValueError):
    """A value was given, by a port's name, that names no value input of the node function."""


class NodeFunctionBodyReturnedNoPerFrameFunction(TypeError):
    """A node function's body returned something that is neither a function nor `None`."""


class NodeFunctionPacingItselfReturnedAValue(TypeError):
    """The body of a node function that paces itself (`execution="manual"`) returned something
    other than `None`, which nothing would use: such a node runs no frame."""


class NodeFunctionValueInputHasNoValue(ValueError):
    """A value input with no default was set up with no value."""


class NodeFunctionPinGroupNotDeclared(ValueError):
    """Slots were given, by a group's name, for a pin group the node function does not take."""


class NodeFunctionRunning:
    """One node function in its helper: its scope, its bound pins and the function its body
    returned, with the hooks the helper calls."""

    def __init__(
        self,
        node_function: "NodeFunction",
        *,
        values_to_build_with: "Mapping[str, Any]",
        pins: "Mapping[str, object]",
        bind_pins: "BindPins",
        slot_counts: "Mapping[str, int]",
    ) -> None:
        """`bind_pins` binds the pins of each slot a pin group holds, and `slot_counts` says how
        many slots each group holds at setup, by its name."""
        self._node_function = node_function
        self._declaring: Optional[NodeScopeBeingDeclared] = NodeScopeBeingDeclared()
        self._input_of_each_port: "dict[str, NodeScopeInput]" = {}
        self._arguments: "dict[str, object]" = {}
        for member in node_function.members:
            if member.form != "field":
                self._arguments[member.attribute] = pins[member.port]
                continue
            if member.port in values_to_build_with:
                value = values_to_build_with[member.port]
            elif member.default is not NO_DEFAULT:
                value = copy.deepcopy(member.default)
            else:
                raise NodeFunctionValueInputHasNoValue(
                    f"`{node_function.__qualname__}`'s input `{member.attribute}` has no default "
                    f"and was set up with no value"
                )
            scope_input = self._declaring.input(value)
            self._input_of_each_port[member.port] = scope_input
            self._arguments[member.attribute] = scope_input
        self._slots_of_each_group: "dict[str, _SlotsOfAPinGroup]" = {}
        self._input_of_each_group: "dict[str, NodeScopeInput]" = {}
        # A `NodeFunction` of a `tatolab-stream` that predates pin groups holds none.
        for pin_group in getattr(node_function, "pin_groups", ()):
            slots = _SlotsOfAPinGroup(node_function, pin_group, bind_pins)
            slots.take_values(values_to_build_with)
            slots.hold(slot_counts.get(pin_group.name, 0))
            scope_input = self._declaring.input(slots.slots())
            self._slots_of_each_group[pin_group.name] = slots
            self._input_of_each_group[pin_group.name] = scope_input
            self._arguments[pin_group.attribute] = scope_input
        self._running: Optional[RunningNodeScope] = None
        self._written_before_the_scope_started: "dict[str, object]" = {}
        self._groups_reshaped_before_the_scope_started: "set[str]" = set()
        self._per_frame: "Optional[Callable[[Any], object]]" = None
        # Held across each write and each reshape of the groups, so a wire's value written and a
        # slot held or dropped never interleave, whichever thread each comes from.
        self._writes_and_reshapes = threading.RLock()

    def setup(self, ctx: Any) -> None:
        """Run the body once, with every computed value it declares declared into the node's
        scope, then start the scope: its worker builds each computed value from here on."""
        declaring = self._declaring
        if declaring is None:
            raise NodeFunctionBodyRanAgain(
                f"`{self._node_function.__qualname__}`'s body has already run, and a node "
                f"function's body runs once, at its setup"
            )
        arguments = dict(self._arguments)
        context_parameter = self._node_function.full_access_context_parameter
        if context_parameter is not None:
            arguments[context_parameter] = ctx
        body = self._node_function.__wrapped__
        returned = declaring.declare(lambda: body(**arguments))
        if returned is not None and self._paces_itself():
            raise NodeFunctionPacingItselfReturnedAValue(
                f"`{self._node_function.__qualname__}` paces itself (`execution=\"manual\"`) and "
                f"its body returned {returned!r}, which nothing would use: a node that paces "
                f"itself runs no frame, and its computed values publish on their own. To fix: "
                f"return `None`."
            )
        if returned is not None and not callable(returned):
            raise NodeFunctionBodyReturnedNoPerFrameFunction(
                f"`{self._node_function.__qualname__}`'s body returned {returned!r}, and a node "
                f"function's body returns the function the node calls each frame, or `None` "
                f"when a computed value publishes on its own"
            )
        self._per_frame = returned
        with self._writes_and_reshapes:
            self._declaring = None
            self._running = declaring.start()
            written, self._written_before_the_scope_started = (
                self._written_before_the_scope_started,
                {},
            )
            reshaped, self._groups_reshaped_before_the_scope_started = (
                self._groups_reshaped_before_the_scope_started,
                set(),
            )
            self._write(written, groups_changed=reshaped)

    def _paces_itself(self) -> bool:
        execution = getattr(self._node_function, NODE_DECLARATION_EXECUTION_STAMP, None)
        return isinstance(execution, dict) and execution.get("mode") == "manual"

    def process(self, ctx: Any) -> None:
        """Call the per-frame function with `ctx`, once every computed value has its first
        value, reading the values built and written since the last frame."""
        running, per_frame = self._running, self._per_frame
        if running is None or per_frame is None:
            return
        if not running.run_frame(lambda: per_frame(ctx)):
            time.sleep(UNBUILT_FRAME_BACKOFF_SECONDS)

    def stop(self, ctx: Any) -> None:
        """Close the node's scope: its worker stops once a build in flight returns, and every
        value a build made is closed."""
        self._close()

    def teardown(self, ctx: Any) -> None:
        """Close the node's scope, if `stop` did not."""
        self._close()

    def write_inputs(self, values: "Mapping[str, object]") -> None:
        """Write each of `values`, by its port's name, to the input of the node's scope it names,
        all in one write: a computed value reading several of them builds once. A field of a
        slot, `layers.0.opacity`, gives its group that slot again holding the value."""
        unknown = sorted(
            port
            for port in values
            if port not in self._input_of_each_port and self._group_holding(port) is None
        )
        if unknown:
            raise NodeFunctionInputNotDeclared(
                f"`{self._node_function.__qualname__}` declares no value input "
                f"{', '.join(f'`{port}`' for port in unknown)}; its value inputs are "
                f"{', '.join(f'`{port}`' for port in self._input_of_each_port) or 'none'}"
                f"{self._and_its_groups_slot_fields()}"
            )
        with self._writes_and_reshapes:
            if self._running is None:
                if self._declaring is not None:
                    self._written_before_the_scope_started.update(values)
                return
            self._write(values, groups_changed=set())

    def slot_counts(self) -> "dict[str, int]":
        """How many slots each pin group holds now, by its name."""
        with self._writes_and_reshapes:
            return {name: slots.count for name, slots in self._slots_of_each_group.items()}

    def hold_slot_counts(
        self, slot_counts: "Mapping[str, int]", values: "Mapping[str, object]"
    ) -> None:
        """Hold `slot_counts` slots in each pin group they name, by its name, while the node
        plays: each new slot with its pins bound and holding its value of `values`, by its port,
        else its default, and each slot past the count dropped. The input of each group whose
        slots changed is written in one write, rebuilding only what read it."""
        undeclared = sorted(set(slot_counts) - set(self._slots_of_each_group))
        if undeclared:
            raise NodeFunctionPinGroupNotDeclared(
                f"`{self._node_function.__qualname__}` takes no pin group "
                f"{', '.join(f'`{name}`' for name in undeclared)}; its pin groups are "
                f"{', '.join(f'`{name}`' for name in self._slots_of_each_group) or 'none'}"
            )
        with self._writes_and_reshapes:
            count_before_of_each_group = {
                name: slots.count for name, slots in self._slots_of_each_group.items()
            }
            reshaped: "set[str]" = set()
            try:
                for name, count in slot_counts.items():
                    slots = self._slots_of_each_group[name]
                    if count == slots.count:
                        continue
                    slots.take_values(values)
                    slots.hold(count)
                    slots.slots()
                    reshaped.add(name)
            except BaseException:
                # A slot that does not build leaves every group holding the slots it held.
                for name, count in count_before_of_each_group.items():
                    self._slots_of_each_group[name].count = count
                raise
            if self._running is None:
                if self._declaring is not None:
                    self._groups_reshaped_before_the_scope_started.update(reshaped)
                return
            self._write({}, groups_changed=reshaped)

    def _write(self, values: "Mapping[str, object]", *, groups_changed: "set[str]") -> None:
        running = self._running
        assert running is not None
        writes: "list[tuple[NodeScopeInput, object]]" = []
        groups = set(groups_changed)
        for port, value in values.items():
            group = self._group_holding(port)
            if group is None:
                writes.append((self._input_of_each_port[port], value))
                continue
            group.take_values({port: value})
            groups.add(group.name)
        writes.extend(
            (self._input_of_each_group[name], self._slots_of_each_group[name].slots())
            for name in sorted(groups)
        )
        if writes:
            running.write(writes)

    def _group_holding(self, port: str) -> "Optional[_SlotsOfAPinGroup]":
        """The group whose slot's field `port` names, `layers.3.opacity`, of any slot index."""
        group_name = port.split(PIN_GROUP_SLOT_PORT_NAME_SEPARATOR, 1)[0]
        slots = self._slots_of_each_group.get(group_name)
        return slots if slots is not None and slots.names_a_field_of_a_slot(port) else None

    def _and_its_groups_slot_fields(self) -> str:
        if not self._slots_of_each_group:
            return ""
        return ", and the fields of each slot of " + ", ".join(
            f"`{name}`" for name in self._slots_of_each_group
        )

    def builds_of_each_computed_value(self) -> "list[BuildsOfANodeScopeComputedValue]":
        """How the builds of each computed value of the node's scope stand, in the order its body
        declared them; none before the scope starts or once it is closed."""
        running = self._running
        return [] if running is None else running.builds_of_each_computed_value()

    def _close(self) -> None:
        running, self._running, self._per_frame = self._running, None, None
        if running is not None:
            running.close()


class _SlotsOfAPinGroup:
    """The slots one pin group of a node function holds: each slot's pins, bound once for its
    index and kept, and the value of each field of each slot, kept for a slot not held."""

    def __init__(
        self, node_function: "NodeFunction", pin_group: "ClassNodePinGroup", bind_pins: "BindPins"
    ) -> None:
        self._node_function = node_function
        self._pin_group = pin_group
        self._bind_pins = bind_pins
        self._field_of_each_slot_port = {
            member.port: member for member in pin_group.members if member.form == "field"
        }
        self._pins_of_each_slot: "list[dict[str, object]]" = []
        self._value_of_each_slot_field: "dict[str, object]" = {}
        self.count = 0
        """How many slots the group holds."""

    @property
    def name(self) -> str:
        return self._pin_group.name

    def names_a_field_of_a_slot(self, port: str) -> bool:
        """Whether `port` names a field of a slot of the group, of any index, `layers.3.opacity`."""
        parts = port.split(PIN_GROUP_SLOT_PORT_NAME_SEPARATOR, 2)
        return (
            len(parts) == 3
            and parts[0] == self._pin_group.name
            and parts[1].isdigit()
            and parts[2] in self._field_of_each_slot_port
        )

    def take_values(self, values: "Mapping[str, object]") -> None:
        """Hold each of `values` that names a field of a slot of the group, by its port."""
        self._value_of_each_slot_field.update(
            (port, value) for port, value in values.items() if self.names_a_field_of_a_slot(port)
        )

    def hold(self, count: int) -> None:
        """Hold `count` slots: the pins of each slot never held before bound."""
        for slot in range(len(self._pins_of_each_slot), count):
            pin_members = tuple(
                member for member in self._pin_group.members_of_slot(slot) if member.form != "field"
            )
            bound = self._bind_pins(pin_members)
            self._pins_of_each_slot.append(
                {
                    member.port: bound[slot_member.port]
                    for member, slot_member in zip(
                        (member for member in self._pin_group.members if member.form != "field"),
                        pin_members,
                    )
                }
            )
        self.count = count

    def slots(self) -> "tuple[Any, ...]":
        """Each slot the group holds, its dataclass built with its pins and its values: each
        field its value set, else a copy of its default."""
        return tuple(
            build_pin_group_slot(
                self._node_function,
                self._pin_group,
                field_values=self._field_values_of_slot(slot),
                pins=self._pins_of_each_slot[slot],
            )
            for slot in range(self.count)
        )

    def _field_values_of_slot(self, slot: int) -> "dict[str, object]":
        values: "dict[str, object]" = {}
        for port, member in self._field_of_each_slot_port.items():
            slot_port = self._pin_group.slot_port(slot, port)
            if slot_port not in self._value_of_each_slot_field:
                if member.default is NO_DEFAULT:
                    raise NodeFunctionValueInputHasNoValue(
                        f"`{self._node_function.__qualname__}`'s pin group "
                        f"`{self._pin_group.name}` holds `{slot_port}`, which has no default "
                        f"and was given no value"
                    )
                self._value_of_each_slot_field[slot_port] = copy.deepcopy(member.default)
            values[port] = self._value_of_each_slot_field[slot_port]
        return values

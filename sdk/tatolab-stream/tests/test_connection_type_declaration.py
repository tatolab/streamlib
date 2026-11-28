# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@connection_type` declares a module's connection type, named by the class's import path; a class node gives itself out on an `OutputPin` of it and takes another node on a field of it."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest

from tatolab.stream import (
    Connection,
    Input,
    InputPin,
    NodeDeclarationRefusal,
    OutputPin,
    SoleReferrer,
    connection_type,
    node,
)
from tatolab.stream._connection_type_declaration import (
    ConnectionTypeDeclared,
    connection_type_declared_by,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

GATEWAY_CONNECTION = f"{__name__}:GatewayConnection"


@connection_type
class GatewayConnection(Connection):
    pass


@node(execution="manual")
def Gateway(
    *,
    connection: OutputPin[GatewayConnection],
) -> None:
    """Gives itself out on `connection`."""
    return None


@node(execution="manual")
def Send(
    *,
    gateway: Input[GatewayConnection | None] = Input(None),
    track: Input[SoleReferrer[GatewayConnection] | None] = Input(None),
) -> None:
    """Takes a gateway on `gateway`, and on `track` one no other send takes."""
    return None


def the_pin_type_of(ports_stamp: str, node_class: object, port_name: str) -> object:
    (port,) = [port for port in getattr(node_class, ports_stamp) if port["name"] == port_name]
    return port["pin_declaration"]["pin_type"]


def test_the_decorated_class_is_declared_by_its_import_path_and_its_words():
    assert connection_type_declared_by(GatewayConnection) == ConnectionTypeDeclared(
        import_path=GATEWAY_CONNECTION
    )
    assert connection_type_declared_by(SoleReferrer[GatewayConnection]) == ConnectionTypeDeclared(
        import_path=GATEWAY_CONNECTION, one_referrer_per_referenced_node=True
    )

    class NotDeclared(GatewayConnection):
        pass

    assert connection_type_declared_by(NotDeclared) is None, "a subclass names a type of its own"
    assert connection_type_declared_by(SoleReferrer[NotDeclared]) is None


def test_an_output_gives_the_node_out_and_a_field_takes_one_as_a_connection():
    assert the_pin_type_of(NODE_DECLARATION_OUTPUT_PORTS_STAMP, Gateway, "connection") == {
        "kind": "is",
        "pin_type": {"type": "connection", "connection_type": GATEWAY_CONNECTION},
    }
    assert the_pin_type_of(NODE_DECLARATION_INPUT_PORTS_STAMP, Send, "gateway") == {
        "kind": "is",
        "pin_type": {"type": "connection", "connection_type": GATEWAY_CONNECTION},
    }
    assert the_pin_type_of(NODE_DECLARATION_INPUT_PORTS_STAMP, Send, "track") == {
        "kind": "is",
        "pin_type": {
            "type": "connection",
            "connection_type": GATEWAY_CONNECTION,
            "one_referrer_per_referenced_node": True,
        },
    }


def test_a_connection_type_no_import_path_finds_is_refused():
    with pytest.raises(TypeError, match="defined inside a function"):

        @connection_type
        class Local(Connection):
            pass


def test_a_connection_type_on_a_function_is_refused():
    def not_a_class() -> None:
        pass

    with pytest.raises(TypeError, match="@connection_type goes on a class"):
        connection_type(not_a_class)  # pyright: ignore[reportArgumentType]


@dataclass(frozen=True)
class HoldsAGateway:
    gateway: GatewayConnection


def TakesOne(gateway: GatewayConnection) -> float:
    del gateway
    return 0.0


def GivesOneOut(level: float = 0.0) -> GatewayConnection:
    del level
    return GatewayConnection("a gateway")


def _a_function_node_taking_one() -> None:
    node(TakesOne)


def _a_function_node_giving_one_out() -> None:
    node(GivesOneOut)


def _an_input_pin_of_one() -> None:
    @node(execution="manual")
    def ReadsOne(
        *,
        gateway: InputPin[GatewayConnection],
    ) -> None:
        return None


def _a_list_of_them() -> None:
    @node(execution="manual")
    def HoldsMany(
        *,
        gateways: Input[list[GatewayConnection]] = Input([]),
    ) -> None:
        return None


def _a_bundle_member_of_one() -> None:
    @node(execution="manual")
    def HoldsABundle(
        *,
        holder: InputPin[HoldsAGateway],
    ) -> None:
        return None


@pytest.mark.parametrize(
    "declare",
    [
        _a_function_node_taking_one,
        _a_function_node_giving_one_out,
        _an_input_pin_of_one,
        _a_list_of_them,
        _a_bundle_member_of_one,
    ],
)
def test_a_connection_anywhere_but_a_class_nodes_field_or_output_pin_is_refused(declare: Any):
    """A wire of a connection is lowered to the node it brings, and only a class node's field is
    handed one; anywhere else the pin would never reach a node."""
    with pytest.raises(NodeDeclarationRefusal) as refused:
        declare()
    assert refused.value.finding_code in ("pin_type_not_read_yet", "list_element_not_taken"), refused.value


def test_a_connection_type_that_does_not_subclass_connection_is_refused():
    with pytest.raises(TypeError, match=r"does not subclass `Connection`.*class Plain\(Connection\)"):

        @connection_type
        class Plain:
            pass


def test_the_value_a_field_holds_is_the_connection_class_naming_the_node():
    from tatolab.stream._connection_type_declaration import the_connection_to

    assert the_connection_to(GATEWAY_CONNECTION, "relay") == GatewayConnection("relay")
    with pytest.raises(ValueError, match="no `@connection_type` class"):
        the_connection_to(f"{__name__}:Gateway", "relay")

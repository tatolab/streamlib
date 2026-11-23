# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@connection_type` declares a module's connection type, named by the class's import path; a class node gives itself out on an `OutputPin` of it and takes another node on a field of it."""

from __future__ import annotations

from typing import Any

import pytest

from tatolab.stream import OutputPin, SoleReferrer, connection_type, node
from tatolab.stream._connection_type_declaration import (
    ConnectionTypeDeclared,
    connection_type_declared_by,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

GATEWAY_CONNECTION = f"{__name__}:GatewayConnection"


@connection_type(description="A relay session a send writes its track to.")
class GatewayConnection:
    pass


@node(execution="manual")
class Gateway:
    """Gives itself out on `connection`."""

    connection: OutputPin[GatewayConnection]

    def process(self, ctx: Any) -> None:
        del ctx


@node(execution="manual")
class Send:
    """Takes a gateway on `gateway`, and on `track` one no other send takes."""

    gateway: GatewayConnection | None = None
    track: SoleReferrer[GatewayConnection] | None = None

    def process(self, ctx: Any) -> None:
        del ctx


def the_pin_type_of(ports_stamp: str, node_class: type, port_name: str) -> object:
    (port,) = [port for port in getattr(node_class, ports_stamp) if port["name"] == port_name]
    return port["pin_declaration"]["pin_type"]


def test_the_decorated_class_is_declared_by_its_import_path_and_its_words():
    assert connection_type_declared_by(GatewayConnection) == ConnectionTypeDeclared(
        import_path=GATEWAY_CONNECTION,
        description="A relay session a send writes its track to.",
    )
    assert connection_type_declared_by(SoleReferrer[GatewayConnection]) == ConnectionTypeDeclared(
        import_path=GATEWAY_CONNECTION,
        description="A relay session a send writes its track to.",
        one_referrer_per_referenced_node=True,
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


@pytest.mark.parametrize("description", ["", "   ", None])
def test_a_connection_type_with_no_words_is_refused(description: Any):
    with pytest.raises(TypeError, match=r"@connection_type\(description=\.\.\.\) takes"):
        connection_type(description=description)


def test_a_connection_type_no_import_path_finds_is_refused():
    with pytest.raises(TypeError, match="defined inside a function"):

        @connection_type(description="A connection only this function sees.")
        class Local:
            pass


def test_a_connection_type_on_a_function_is_refused():
    def not_a_class() -> None:
        pass

    with pytest.raises(TypeError, match="@connection_type goes on a class"):
        connection_type(description="Not a class.")(not_a_class)  # pyright: ignore[reportArgumentType]

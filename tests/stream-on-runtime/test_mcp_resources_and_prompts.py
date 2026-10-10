# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A scripted client follows a node's own prompt to a spliced live graph.

The client is the official MCP Python SDK's, driven by a script, not a model. What it brings is the names of the two
resources and the one prompt it picks from the node's listings, and the import
path of the effect it wants inserted, which no graph has named yet. Everything
else comes from the server: the resource URIs and the prompt's argument names
from `resources/list` and `prompts/list`, the stream and the link from the
graph resource, and the tool order, names and ports from the recipe text, whose
numbered steps it dispatches as written, each naming the stream the recipe was
rendered against. The graph it
leaves is then checked through `graph`, and the frames through the processor it
inserted.

Starting the engine initializes a GPU context, so the whole module needs a device.
"""

from __future__ import annotations

import asyncio
import json
import re
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any, TypeVar

import pytest
from mcp.client.client import Client
from mcp.types import TextContent, TextResourceContents
from mcp_types import UNSUPPORTED_PROTOCOL_VERSION
from mcp_types.version import LATEST_PROTOCOL_VERSION

from conftest import TatolabdUnderTest
from local_api_client import LocalApiClient, mcp_error_raised_in
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE
from stream_runs_on_tatolabd import TatolabRunOfAProject
from test_cli_launch import NODE_READY_TIMEOUT_SECONDS

pytestmark = pytest.mark.requires_gpu

FIRST_MARKED_BAG_TIMEOUT_SECONDS = 30.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0

Answered = TypeVar("Answered")
# How long a link handed to a running helper has to come back `wired`. The
# helper answers between callbacks, so this is bounded by one frame of the
# processor's own work, not by the wire.
LINK_ANSWER_TIMEOUT_SECONDS = 15.0
# How long a node added to a running graph has to reach `Running`. Its helper
# is spawned, imports the class and starts it, so this is bounded by an
# interpreter launch rather than by the wire.
ADDED_NODE_RUNNING_TIMEOUT_SECONDS = 15.0

STREAM_WITH_A_SOURCE_LINKED_TO_A_SINK = '''\
from tatolab.stream import StreamBuilder, TestPatternSource, stream
from processors.marked_bag_sink import MarkedBagSink


@stream
def main(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(TestPatternSource, name="pattern", config={"width": 320, "height": 180})
    sink = stream_builder.add(MarkedBagSink, name="sink")
    stream_builder.connect(source.output("video"), sink.input("bags_from_upstream"))
'''

# Both `{delivery_profile}` slots are filled per run, so each way the effect's
# input can relate to the sink's — the same profile, a shallower one, a deeper
# one — meets a live node.
BAG_MARKING_EFFECT_SOURCE_TEMPLATE = '''\
from tatolab.stream import RuntimeContextLimitedAccess, node


@node
class BagMarkingEffect:
    """Forwards every bag with one key added, so a consumer can tell it passed through."""

    @node.input(delivery_profile="{delivery_profile}")
    def bags_from_upstream(self) -> None: ...

    @node.output()
    def marked_bags_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("bags_from_upstream")
        if bag is not None:
            ctx.outputs.write("marked_bags_to_downstream", {{**bag, "marked_by_inserted_effect": True}})
'''

MARKED_BAG_SINK_SOURCE_TEMPLATE = '''\
from tatolab.stream import RuntimeContextLimitedAccess, log, node


@node
class MarkedBagSink:
    """Says so once when a bag arrives carrying the inserted effect's mark."""

    def __init__(self) -> None:
        self.announced = False

    @node.input(delivery_profile="{delivery_profile}")
    def bags_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("bags_from_upstream")
        if bag is not None and bag.get("marked_by_inserted_effect") and not self.announced:
            self.announced = True
            log.info("MARKER:SINK_RECEIVED_A_MARKED_BAG")
'''

NUMBERED_STEP = re.compile(r"^\d+\. `([a-z_]+)` — (.*)$")
EXPLICIT_ARGUMENT = re.compile(r"`([a-z_]+)`: `([^`]+)`")


def await_link_state(
    client: "ScriptedMcpClient", stream_name: str, link_id: str, wanted: str
) -> str:
    """Poll the stream's `graph` until one link reaches `wanted`, and report what it reached.

    A `connect` onto a helper-placed processor returns with the link `pending`:
    the helper opens its own port and answers, and only that answer makes the
    link `wired`. A link that reaches `error` is returned as it is, so the
    caller's assertion carries the helper's own reason.
    """
    deadline = time.monotonic() + LINK_ANSWER_TIMEOUT_SECONDS
    link = None
    while time.monotonic() < deadline:
        graph = client.call_tool("graph", {"stream": stream_name})
        link = next((each for each in graph["links"] if each["id"] == link_id), None)
        if link is not None and link["state"] in (wanted, "error"):
            return link["state"] + (
                f" ({link['error_reason']})" if link.get("error_reason") else ""
            )
        time.sleep(0.05)
    return f"still {link['state'] if link else 'absent'} after {LINK_ANSWER_TIMEOUT_SECONDS}s"


def await_added_node_state(
    client: "ScriptedMcpClient", stream_name: str, node_name: str, wanted: str
) -> str:
    """Poll the stream's `graph` until one node reaches `wanted`, and report what it reached.

    A node added to a running graph is placed in a helper process that has to
    be spawned before it can run, so `graph` reports it `Idle` for as long as
    that takes — the same shape as the link that reads `pending` until its
    helper opens its port.
    """
    deadline = time.monotonic() + ADDED_NODE_RUNNING_TIMEOUT_SECONDS
    state = None
    while time.monotonic() < deadline:
        node = next(
            (
                each
                for each in client.call_tool("graph", {"stream": stream_name})["nodes"]
                if each["name"] == node_name
            ),
            None,
        )
        state = node["components"]["state"] if node is not None else None
        if state == wanted:
            return wanted
        time.sleep(0.05)
    return f"still {state or 'absent'} after {ADDED_NODE_RUNNING_TIMEOUT_SECONDS}s"


class ScriptedMcpClient:
    """The official MCP Python SDK's client on the node's local API socket, at
    the revision `server/discover` agrees, and nothing streamlib-specific in
    what it sends."""

    def __init__(self, local_api: LocalApiClient) -> None:
        self.local_api = local_api

    def answer(
        self, operation: "Callable[[Client], Awaitable[Answered]]", *, mode: str = "auto"
    ) -> "Answered":
        """Run one operation on a client connected for it alone; every request
        carries its own revision, so nothing is held between them."""
        return self.local_api.answer_over_mcp(operation, mode=mode)

    def read_json_resource(self, uri: str) -> Any:
        (document,) = self.answer(lambda client: client.read_resource(uri)).contents
        assert isinstance(document, TextResourceContents), document
        return json.loads(document.text)

    def prompt_text(self, prompt_name: str, arguments: "dict[str, str]") -> str:
        (message,) = self.answer(lambda client: client.get_prompt(prompt_name, arguments)).messages
        assert isinstance(message.content, TextContent), message
        return message.content.text

    def call_tool(self, tool_name: str, arguments: "dict[str, Any]") -> Any:
        result = self.answer(lambda client: client.call_tool(tool_name, arguments))
        assert result.is_error is False, f"`{tool_name}` failed: {result.content}"
        stated = result.content[0]
        assert isinstance(stated, TextContent), result.content
        return json.loads(stated.text)


def numbered_steps(prompt_text: str) -> "list[tuple[str, str]]":
    return [
        (match.group(1), match.group(2))
        for match in map(NUMBERED_STEP.match, prompt_text.splitlines())
        if match is not None
    ]


# The pairs earn their keep below the recipe, which reads the same for all
# three: the sink is only the first consumer to open the source's channel —
# which is created deep enough for a consumer of any profile — and the effect's
# input then joins it live, in the last pair reading deeper than the opener.
@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
@pytest.mark.parametrize(
    ("sink_input_delivery_profile", "inserted_input_delivery_profile"),
    [
        ("newest", "newest"),
        ("ordered", "newest"),
        ("newest", "ordered"),
    ],
)
def test_a_client_following_the_insert_prompt_splices_a_processor_into_a_live_link(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    sink_input_delivery_profile: str,
    inserted_input_delivery_profile: str,
):
    app_directory = make_tatolab_project(
        {
            "processors/__init__.py": "",
            "processors/bag_marking_effect.py": BAG_MARKING_EFFECT_SOURCE_TEMPLATE.format(
                delivery_profile=inserted_input_delivery_profile
            ),
            "processors/marked_bag_sink.py": MARKED_BAG_SINK_SOURCE_TEMPLATE.format(
                delivery_profile=sink_input_delivery_profile
            ),
            "stream.py": STREAM_WITH_A_SOURCE_LINKED_TO_A_SINK,
        }
    )

    tatolabd = start_tatolabd()
    tatolab_run = tatolabd.run_stream_attached(TatolabRunOfAProject(working_directory=app_directory))
    tatolab_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    client = ScriptedMcpClient(tatolabd.local_api_client())

    negotiated_protocol_version, capabilities = client.answer(
        lambda connected: asyncio.sleep(0, (connected.protocol_version, connected.server_capabilities))
    )
    assert negotiated_protocol_version == LATEST_PROTOCOL_VERSION
    assert capabilities.tools and capabilities.resources and capabilities.prompts, capabilities
    with pytest.raises(Exception) as raised_by_the_handshake:
        client.answer(lambda connected: connected.list_tools(), mode="legacy")
    handshake_refusal = mcp_error_raised_in(raised_by_the_handshake.value)
    assert handshake_refusal.code == UNSUPPORTED_PROTOCOL_VERSION, handshake_refusal
    served_tool_names = {tool.name for tool in client.answer(lambda connected: connected.list_tools()).tools}

    resource_uris_by_name = {
        resource.name: resource.uri
        for resource in client.answer(lambda connected: connected.list_resources()).resources
    }
    prompts_by_name = {
        prompt.name: prompt for prompt in client.answer(lambda connected: connected.list_prompts()).prompts
    }
    insert_prompt = prompts_by_name["insert_node_between_linked_nodes"]

    machine_wide_graph_before = client.read_json_resource(resource_uris_by_name["graph"])
    (graph_before,) = machine_wide_graph_before["streams"]
    stream_name = graph_before["stream"]
    assert len(graph_before["links"]) == 1, graph_before["links"]
    replaced_link = graph_before["links"][0]

    catalog = client.read_json_resource(resource_uris_by_name["node-catalog"])
    (catalog_of_the_stream,) = [
        stream_catalog
        for stream_catalog in catalog["streams"]
        if stream_catalog["stream"] == stream_name
    ]
    catalog_paths = [entry["type"] for entry in catalog["nodes"] + catalog_of_the_stream["nodes"]]
    inserted_type = "processors.bag_marking_effect:BagMarkingEffect"

    # The prompt's three arguments, bound by the names the listing gave them:
    # the stream, the link's id, and a catalog import path.
    argument_names = [argument.name for argument in insert_prompt.arguments or []]
    assert "stream" in argument_names, argument_names
    link_argument = next(name for name in argument_names if name.startswith("link"))
    type_argument = next(name for name in argument_names if name not in ("stream", link_argument))
    recipe_text = client.prompt_text(
        insert_prompt.name,
        {"stream": stream_name, link_argument: replaced_link["id"], type_argument: inserted_type},
    )
    steps = numbered_steps(recipe_text)
    assert steps, f"the recipe lists no steps:\n{recipe_text}"
    assert {tool_name for tool_name, _ in steps} <= served_tool_names, recipe_text
    # The new links go up before the replaced one comes down, so nothing the
    # sink was being fed stops. Only a target port that takes a single inbound
    # link buys the disconnect-first order, and this sink declares no window
    # contract under either profile.
    assert [
        tool_name for tool_name, _ in steps if tool_name in ("connect", "disconnect")
    ] == ["connect", "connect", "disconnect"], recipe_text

    # Dispatch each step as written. An argument the text spells in backticks
    # is passed verbatim; the rest are what an earlier step answered.
    added_node_name = None
    added_node_ports: "dict[str, str]" = {}
    returned_link_ids: "list[str]" = []
    graph_after: "dict[str, Any]" = {}
    for tool_name, instruction in steps:
        spelled = dict(EXPLICIT_ARGUMENT.findall(instruction))
        if tool_name == "add_node":
            added_node_name = client.call_tool(
                "add_node", {"stream": stream_name, "type": spelled["type"]}
            )["name"]
        elif tool_name == "graph":
            graph_after = client.call_tool("graph", {"stream": stream_name})
            if added_node_name is not None and not added_node_ports:
                added_node = next(n for n in graph_after["nodes"] if n["name"] == added_node_name)
                (added_input,) = added_node["ports"]["inputs"]
                (added_output,) = added_node["ports"]["outputs"]
                added_node_ports = {"to_port": added_input["name"], "from_port": added_output["name"]}
        elif tool_name == "connect":
            arguments = {
                "stream": stream_name,
                "from_node": spelled.get("from_node", added_node_name),
                "from_port": spelled.get("from_port", added_node_ports["from_port"]),
                "to_node": spelled.get("to_node", added_node_name),
                "to_port": spelled.get("to_port", added_node_ports["to_port"]),
            }
            returned_link_ids.append(client.call_tool("connect", arguments)["link_id"])
        elif tool_name == "disconnect":
            client.call_tool("disconnect", {"stream": stream_name, "link_id": spelled["link_id"]})
        else:
            pytest.fail(f"the recipe calls `{tool_name}`, which this client was not asked to follow")

    assert added_node_name is not None, recipe_text
    links_by_id = {link["id"]: link for link in graph_after["links"]}
    assert replaced_link["id"] not in links_by_id, "the replaced link must be gone"
    assert len(returned_link_ids) == 2, returned_link_ids
    # Both new links land on helper-placed nodes, so each returns
    # `pending` and reaches `wired` only when that helper answers that it opened
    # its port — which is what the recipe the client just followed tells it to
    # read `graph` again for.
    for link_id in returned_link_ids:
        assert links_by_id[link_id]["state"] in ("pending", "wired"), links_by_id.get(link_id)
        assert await_link_state(client, stream_name, link_id, "wired") == "wired", (
            "the helper's own answer is what makes the link wired; a link stuck "
            "pending is a helper that never opened its port, and one in error "
            "carries the helper's reason"
        )
    links_by_id = {
        link["id"]: link for link in client.call_tool("graph", {"stream": stream_name})["links"]
    }
    upstream_link, downstream_link = (links_by_id[link_id] for link_id in returned_link_ids)
    assert upstream_link["source"] == replaced_link["source"]
    assert upstream_link["target"]["node"] == added_node_name
    assert downstream_link["source"]["node"] == added_node_name
    assert downstream_link["target"] == replaced_link["target"]
    assert await_added_node_state(client, stream_name, added_node_name, "Running") == "Running", (
        "the splice is only carrying bags once the inserted node runs; a node "
        "stuck Idle is a helper that never started it"
    )

    # The sink announces from its own processor interpreter only once a bag
    # carrying the inserted effect's mark reaches it: frames really pass
    # through the splice.
    tatolabd.await_marker("SINK_RECEIVED_A_MARKED_BAG", timeout=FIRST_MARKED_BAG_TIMEOUT_SECONDS)

    # The virtual camera recipe names a type this node's catalog actually holds.
    source_endpoint = replaced_link["source"]
    camera_prompt = prompts_by_name["show_channel_on_virtual_camera"]
    camera_recipe_text = client.prompt_text(
        camera_prompt.name,
        # Its required arguments name the stream, a node, then one of its output ports.
        dict(
            zip(
                [argument.name for argument in camera_prompt.arguments or [] if argument.required],
                [stream_name, source_endpoint["node"], source_endpoint["port"]],
                strict=True,
            )
        ),
    )
    camera_add_step = next(
        instruction for tool_name, instruction in numbered_steps(camera_recipe_text)
        if tool_name == "add_node"
    )
    assert dict(EXPLICIT_ARGUMENT.findall(camera_add_step))["type"] in catalog_paths

    tatolab_run.interrupt()
    assert tatolab_run.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, tatolab_run.recent_stderr()

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A scripted client follows a node's own prompt to a spliced live graph.

The client is a script, not a model. What it brings is the names of the two
resources and the one prompt it picks from the node's listings, and the short
class name of the effect it wants inserted. Everything else comes from the
server: the resource URIs and the prompt's argument names from
`resources/list` and `prompts/list`, the link from the graph resource, the
import path from the catalog resource, and the tool order, ids and ports from
the recipe text, whose numbered steps it dispatches as written. The graph it
leaves is then checked through `graph`, and the frames through the processor it
inserted.

Booting initializes a GPU context, so the whole module needs a device.
"""

import json
import re
import urllib.request
from pathlib import Path
from typing import Any

import pytest

from test_cli_launch import (  # noqa: F401 — the two fixtures are used by name
    NODE_READY_TIMEOUT_SECONDS,
    await_sole_registry_entry,
    free_port,
    isolated_runtime_directory,
    launch_node,
)

pytestmark = pytest.mark.requires_gpu

FIRST_MARKED_BAG_TIMEOUT_SECONDS = 30.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
JSON_RPC_TIMEOUT_SECONDS = 30.0

APP_WITH_A_SOURCE_LINKED_TO_A_SINK = '''\
from streamlib import Runtime, TestPatternSource

# Imported and never added: its decorator is what puts it in the catalog.
from processors.bag_marking_effect import BagMarkingEffect  # noqa: F401
from processors.marked_bag_sink import MarkedBagSink


def setup(rt: Runtime) -> None:
    source = rt.add(TestPatternSource, config={"width": 320, "height": 180}, display_name="pattern")
    sink = rt.add(MarkedBagSink, display_name="sink")
    rt.connect(source.output("video"), sink.input("bags_from_upstream"))
'''

# Both `{delivery_profile}` slots are filled per run, so each way the effect's
# input can relate to the sink's — the same profile, a shallower one, a deeper
# one — meets a live node.
BAG_MARKING_EFFECT_SOURCE_TEMPLATE = '''\
from streamlib import RuntimeContextLimitedAccess, input, output, processor


@processor
class BagMarkingEffect:
    """Forwards every bag with one key added, so a consumer can tell it passed through."""

    @input(delivery_profile="{delivery_profile}")
    def bags_from_upstream(self) -> None: ...

    @output()
    def marked_bags_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("bags_from_upstream")
        if bag is not None:
            ctx.outputs.write("marked_bags_to_downstream", {{**bag, "marked_by_inserted_effect": True}})
'''

MARKED_BAG_SINK_SOURCE_TEMPLATE = '''\
from streamlib import RuntimeContextLimitedAccess, input, log, processor


@processor
class MarkedBagSink:
    """Says so once when a bag arrives carrying the inserted effect's mark."""

    def __init__(self) -> None:
        self.announced = False

    @input(delivery_profile="{delivery_profile}")
    def bags_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("bags_from_upstream")
        if bag is not None and bag.get("marked_by_inserted_effect") and not self.announced:
            self.announced = True
            log.info("MARKER:SINK_RECEIVED_A_MARKED_BAG")
'''

NUMBERED_STEP = re.compile(r"^\d+\. `([a-z_]+)` — (.*)$")
EXPLICIT_ARGUMENT = re.compile(r"`([a-z_]+)`: `([^`]+)`")


class ScriptedMcpClient:
    """Plain JSON-RPC over `POST /mcp`, and nothing streamlib-specific."""

    def __init__(self, control_url: str) -> None:
        self.endpoint = f"{control_url.rstrip('/')}/mcp"
        self.next_request_id = 0

    def request(self, method: str, params: "dict[str, Any]") -> Any:
        envelope = self.envelope(method, params)
        assert "error" not in envelope, f"{method} was refused: {envelope['error']}"
        return envelope["result"]

    def refusal(self, method: str, params: "dict[str, Any]") -> "dict[str, Any]":
        envelope = self.envelope(method, params)
        assert "error" in envelope, f"{method} was expected to be refused: {envelope}"
        return envelope["error"]

    def envelope(self, method: str, params: "dict[str, Any]") -> "dict[str, Any]":
        self.next_request_id += 1
        body = json.dumps(
            {"jsonrpc": "2.0", "id": self.next_request_id, "method": method, "params": params}
        ).encode("utf-8")
        request = urllib.request.Request(
            self.endpoint, data=body, method="POST", headers={"content-type": "application/json"}
        )
        with urllib.request.urlopen(request, timeout=JSON_RPC_TIMEOUT_SECONDS) as response:
            return json.loads(response.read())

    def read_json_resource(self, uri: str) -> Any:
        contents = self.request("resources/read", {"uri": uri})["contents"]
        return json.loads(contents[0]["text"])

    def call_tool(self, tool_name: str, arguments: "dict[str, Any]") -> Any:
        result = self.request("tools/call", {"name": tool_name, "arguments": arguments})
        assert result["isError"] is False, f"`{tool_name}` failed: {result['content']}"
        return json.loads(result["content"][0]["text"])


def numbered_steps(prompt_text: str) -> "list[tuple[str, str]]":
    return [
        (match.group(1), match.group(2))
        for match in map(NUMBERED_STEP.match, prompt_text.splitlines())
        if match is not None
    ]


@pytest.mark.parametrize(
    ("sink_input_delivery_profile", "inserted_input_delivery_profile", "expected_wiring_order"),
    [
        ("newest", "newest", ["connect", "connect", "disconnect"]),
        ("ordered", "newest", ["disconnect", "connect", "connect"]),
        # A live channel keeps the buffer depth it was opened with, so the
        # recipe refuses rather than hand the agent a `connect` that fails.
        ("newest", "ordered", None),
    ],
)
def test_a_client_following_the_insert_prompt_splices_a_processor_into_a_live_link(
    tmp_path: Path,
    isolated_runtime_directory: Path,
    launch_node,
    sink_input_delivery_profile: str,
    inserted_input_delivery_profile: str,
    expected_wiring_order: "list[str] | None",
):
    app_directory = tmp_path / "app"
    (app_directory / "processors").mkdir(parents=True)
    (app_directory / "processors" / "__init__.py").write_text("")
    (app_directory / "processors" / "bag_marking_effect.py").write_text(
        BAG_MARKING_EFFECT_SOURCE_TEMPLATE.format(delivery_profile=inserted_input_delivery_profile)
    )
    (app_directory / "processors" / "marked_bag_sink.py").write_text(
        MARKED_BAG_SINK_SOURCE_TEMPLATE.format(delivery_profile=sink_input_delivery_profile)
    )
    (app_directory / "app.py").write_text(APP_WITH_A_SOURCE_LINKED_TO_A_SINK)

    node = launch_node("run", app_directory, free_port(), capture_output=True)
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)
    node.await_captured_output_containing("[start] Runtime started", NODE_READY_TIMEOUT_SECONDS)
    client = ScriptedMcpClient(entry["control_url"])

    capabilities = client.request(
        "initialize",
        {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "scripted", "version": "0"}},
    )["capabilities"]
    assert {"tools", "resources", "prompts"} <= capabilities.keys(), capabilities
    served_tool_names = {tool["name"] for tool in client.request("tools/list", {})["tools"]}

    resource_uris_by_name = {
        resource["name"]: resource["uri"] for resource in client.request("resources/list", {})["resources"]
    }
    prompts_by_name = {prompt["name"]: prompt for prompt in client.request("prompts/list", {})["prompts"]}
    insert_prompt = prompts_by_name["insert_processor_between_linked_processors"]

    catalog = client.read_json_resource(resource_uris_by_name["processor-catalog"])
    catalog_paths = [entry["processor_class_import_path"] for entry in catalog["processors"]]
    inserted_type = next(
        (path for path in catalog_paths if path.endswith(":BagMarkingEffect")), None
    )
    assert inserted_type is not None, (
        f"a class the app imported and never added must be in the catalog: {catalog_paths}"
    )

    graph_before = client.read_json_resource(resource_uris_by_name["graph"])
    assert len(graph_before["links"]) == 1, graph_before["links"]
    replaced_link = graph_before["links"][0]

    # The prompt's two arguments, bound by the names the listing gave them: one
    # takes the link's id, the other a catalog import path.
    argument_names = [argument["name"] for argument in insert_prompt["arguments"]]
    link_argument = next(name for name in argument_names if name.startswith("link"))
    type_argument = next(name for name in argument_names if name != link_argument)
    insert_request = {
        "name": insert_prompt["name"],
        "arguments": {link_argument: replaced_link["id"], type_argument: inserted_type},
    }
    if expected_wiring_order is None:
        refusal = client.refusal("prompts/get", insert_request)
        assert all(
            profile in refusal["message"]
            for profile in (sink_input_delivery_profile, inserted_input_delivery_profile)
        ), refusal
        assert [link["id"] for link in client.call_tool("graph", {})["links"]] == [
            replaced_link["id"]
        ], "a refused recipe must leave the running graph as it was"
        node.interrupt()
        assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, node.recent_output()
        return
    recipe = client.request("prompts/get", insert_request)
    recipe_text = recipe["messages"][0]["content"]["text"]
    steps = numbered_steps(recipe_text)
    assert steps, f"the recipe lists no steps:\n{recipe_text}"
    assert {tool_name for tool_name, _ in steps} <= served_tool_names, recipe_text
    assert [
        tool_name for tool_name, _ in steps if tool_name in ("connect", "disconnect")
    ] == expected_wiring_order, recipe_text

    # Dispatch each step as written. An argument the text spells in backticks
    # is passed verbatim; the rest are what an earlier step answered.
    added_processor_id = None
    added_node_ports: "dict[str, str]" = {}
    returned_link_ids: "list[str]" = []
    graph_after: "dict[str, Any]" = {}
    for tool_name, instruction in steps:
        spelled = dict(EXPLICIT_ARGUMENT.findall(instruction))
        if tool_name == "add_processor":
            added_processor_id = client.call_tool("add_processor", {"type": spelled["type"]})[
                "processor_id"
            ]
        elif tool_name == "graph":
            graph_after = client.call_tool("graph", {})
            if added_processor_id is not None and not added_node_ports:
                added_node = next(n for n in graph_after["nodes"] if n["id"] == added_processor_id)
                (added_input,) = added_node["ports"]["inputs"]
                (added_output,) = added_node["ports"]["outputs"]
                added_node_ports = {"to_port": added_input["name"], "from_port": added_output["name"]}
        elif tool_name == "connect":
            arguments = {
                "from_processor_id": spelled.get("from_processor_id", added_processor_id),
                "from_port": spelled.get("from_port", added_node_ports["from_port"]),
                "to_processor_id": spelled.get("to_processor_id", added_processor_id),
                "to_port": spelled.get("to_port", added_node_ports["to_port"]),
            }
            returned_link_ids.append(client.call_tool("connect", arguments)["link_id"])
        elif tool_name == "disconnect":
            client.call_tool("disconnect", {"link_id": spelled["link_id"]})
        else:
            pytest.fail(f"the recipe calls `{tool_name}`, which this client was not asked to follow")

    assert added_processor_id is not None, recipe_text
    links_by_id = {link["id"]: link for link in graph_after["links"]}
    assert replaced_link["id"] not in links_by_id, "the replaced link must be gone"
    assert len(returned_link_ids) == 2, returned_link_ids
    for link_id in returned_link_ids:
        assert links_by_id[link_id]["state"] == "wired", links_by_id.get(link_id)
    upstream_link, downstream_link = (links_by_id[link_id] for link_id in returned_link_ids)
    assert upstream_link["source"] == replaced_link["source"]
    assert upstream_link["target"]["processor_id"] == added_processor_id
    assert downstream_link["source"]["processor_id"] == added_processor_id
    assert downstream_link["target"] == replaced_link["target"]
    added_node = next(n for n in graph_after["nodes"] if n["id"] == added_processor_id)
    assert added_node["components"]["state"] == "Running"

    # The sink announces from its own helper only once a bag carrying the
    # inserted effect's mark reaches it: frames really pass through the splice.
    node.await_captured_output_containing(
        "MARKER:SINK_RECEIVED_A_MARKED_BAG", FIRST_MARKED_BAG_TIMEOUT_SECONDS
    )

    # The virtual camera recipe names a type this node's catalog actually holds.
    source_endpoint = replaced_link["source"]
    camera_prompt = prompts_by_name["show_channel_on_virtual_camera"]
    camera_recipe_text = client.request(
        "prompts/get",
        {
            "name": camera_prompt["name"],
            # Its required arguments name a processor, then one of its output ports.
            "arguments": dict(
                zip(
                    [argument["name"] for argument in camera_prompt["arguments"] if argument["required"]],
                    [source_endpoint["processor_id"], source_endpoint["port_name"]],
                )
            ),
        },
    )["messages"][0]["content"]["text"]
    camera_add_step = next(
        instruction for tool_name, instruction in numbered_steps(camera_recipe_text)
        if tool_name == "add_processor"
    )
    assert dict(EXPLICIT_ARGUMENT.findall(camera_add_step))["type"] in catalog_paths

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, node.recent_output()

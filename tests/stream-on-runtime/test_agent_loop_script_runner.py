# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The agent loop script runner's rules, with no runtime: every script on disk is well formed,
and each one replays against a stand-in session that lists, discovers and describes exactly
what the committed `contract.json` says the runtime serves to agents."""

from __future__ import annotations

import asyncio
import base64
import copy
import hashlib
import json
import uuid
from pathlib import Path
from typing import Any

import pytest
from jsonschema import Draft202012Validator
from mcp.types import CallToolResult, ListToolsResult, TextContent, Tool

from agent_loop_script_runner import (
    CALL_OPERATION_TOOL_NAME,
    AgentLoopScript,
    AgentLoopScriptIsMalformed,
    AgentLoopScriptRunFailed,
    agent_loop_script_from,
    every_agent_loop_script_path,
    load_agent_loop_script,
    play_agent_loop_script,
)
from runtime_unit_under_test import REPOSITORY_ROOT

CONTROL_CONTRACT_FILE = REPOSITORY_ROOT / "runtime" / "streamlib-control-contract" / "contract.json"
COMPONENT_SCHEMA_REFERENCE_PREFIX = "#/components/schemas/"
BUILD_A_GRAPH_SCRIPT_NAME = "build_a_graph"


def a_json_object(text: str) -> "dict[str, Any]":
    """`text` parsed, which must be one JSON object."""
    parsed = json.loads(text)
    assert isinstance(parsed, dict), parsed
    return parsed


def the_committed_control_contract() -> "dict[str, Any]":
    return a_json_object(CONTROL_CONTRACT_FILE.read_text(encoding="utf-8"))


class StandInRefusal(Exception):
    """A call the stand-in session refuses, answered as a tool error as the runtime answers one."""


def with_references_into_defs(schema: Any) -> Any:
    """`schema` with each `#/components/schemas/X` reference rewritten to `#/$defs/X`."""
    if isinstance(schema, dict):
        return {
            key: (
                "#/$defs/" + value.removeprefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
                if key == "$ref" and isinstance(value, str)
                else with_references_into_defs(value)
            )
            for key, value in schema.items()
        }
    if isinstance(schema, list):
        return [with_references_into_defs(value) for value in schema]
    return schema


class ContractBackedAgentSession:
    """A stand-in MCP session: its tool list, groups and schemas are the committed contract's,
    and it holds one running stream whose graph `transact` and `undo` edit.

    The answers it gives are checked against the contract's result schemas, so the script is
    replayed against shapes the runtime declares.
    """

    def __init__(self, contract: "dict[str, Any]") -> None:
        self.methods = {method["name"]: method for method in contract["methods"]}
        self.definitions = with_references_into_defs(contract["components"]["schemas"])
        self.listed_tool_names = [
            method["name"]
            for method in contract["methods"]
            if "agent" in method["x-exposures"]
            and method["x-agent-tool-listing"] == "listed_as_a_tool"
        ] + [tool["name"] for tool in contract["x-agent-discovery-tools"]]
        self.agent_operation_names = [
            method["name"] for method in contract["methods"] if "agent" in method["x-exposures"]
        ]
        self.stream_id = "main-0a1b2c"
        self.stream_asset = str(uuid.uuid4())
        self.project_directory = "/projects/app"
        self.nodes: "list[dict[str, Any]]" = [
            {"id": str(uuid.uuid4()), "name": "testpatternsource",
             "type": "tatolab.testpattern:TestPatternSource"}
        ]
        self.links: "list[dict[str, Any]]" = []
        #: The inputs a link from a picture output connects to; `check_link` refuses any other.
        self.picture_inputs = {"video_from_upstream"}
        self.transactions: "list[tuple[str, list[dict[str, Any]], list[dict[str, Any]]]]" = []
        self.tool_calls: "list[tuple[str, dict[str, Any]]]" = []
        self.refused_operations: "set[str]" = set()
        self.operations_left_undescribed: "set[str]" = set()
        self.undo_reverses_nothing = False
        #: The project's files by path, as the file operations read and write them.
        self.project_files: "dict[str, str]" = {}

    def a_schema(self, component_reference: Any) -> Any:
        """A contract component reference as a document of its own, with `$defs`."""
        if component_reference is True:
            return True
        return {**with_references_into_defs(component_reference), "$defs": self.definitions}

    def checked_against_the_result_schema(self, operation: str, answer: Any) -> Any:
        result_schema = self.a_schema(self.methods[operation]["result"]["schema"])
        if result_schema is not True:
            Draft202012Validator(result_schema).validate(answer)
        return answer

    async def list_tools(self, *, cursor: "str | None" = None) -> ListToolsResult:
        tools = [Tool(name=name, input_schema={"type": "object"}) for name in self.listed_tool_names]
        return ListToolsResult(tools=tools)

    async def call_tool(
        self, name: str, arguments: "dict[str, Any] | None" = None
    ) -> CallToolResult:
        sent: "dict[str, Any]" = arguments or {}
        self.tool_calls.append((name, sent))
        try:
            if name == CALL_OPERATION_TOOL_NAME:
                name, sent = sent["operation"], sent.get("params") or {}
                if name not in self.agent_operation_names:
                    raise StandInRefusal(f"`{name}` is not an operation served to agents")
            elif name not in self.listed_tool_names:
                raise StandInRefusal(f"the session lists no tool `{name}`")
            if name in self.refused_operations:
                raise StandInRefusal(f"`{name}` was refused")
            answer = self.answer(name, sent)
        except StandInRefusal as refused:
            refusal = {"refusal_code": "refused", "action": name, "runtime_words": str(refused)}
            return CallToolResult(
                content=[TextContent(text=json.dumps(refusal, indent=2))],
                structured_content={"refusal": refusal},
                is_error=True,
            )
        return CallToolResult(content=[TextContent(text=json.dumps(answer))])

    def answer(self, operation: str, arguments: "dict[str, Any]") -> Any:
        if operation == "list_operation_groups":
            return {"groups": [{"group": "every", "operations": [
                {"name": name, "summary": "", "listed_as_a_tool": name in self.listed_tool_names}
                for name in self.agent_operation_names
            ]}]}
        if operation == "describe_operations":
            for name in arguments["operations"]:
                if name not in self.agent_operation_names:
                    raise StandInRefusal(f"`{name}` is not an operation served to agents")
            return {"operations": [
                {"name": name, "params_schema": self.a_schema(
                    self.methods[name]["params"][0]["schema"]
                )}
                for name in arguments["operations"]
                if name not in self.operations_left_undescribed
            ]}
        if operation in FILE_OPERATIONS:
            assert arguments["project_directory"] == self.project_directory, arguments
            return self.checked_against_the_result_schema(
                operation, self.answered_on_the_project_files(operation, arguments)
            )
        if operation == "list_streams":
            return self.checked_against_the_result_schema(operation, {"streams": [{
                "stream": self.stream_id, "display_name": "main",
                "location": {"project_directory": self.project_directory,
                             "project": str(uuid.uuid4()), "stream_asset": self.stream_asset},
                "state": "running", "node_count": len(self.nodes), "failed_because": None,
                "capture_devices_whose_access_was_never_asked_for": [],
                "action": None, "step": None,
            }]})
        if operation in ("transact", "add_node"):
            requested = arguments["operations"] if operation == "transact" else [
                {**arguments, "operation": "add_node"}
            ]
            return self.checked_against_the_result_schema(operation, self.applied(requested))
        if operation == "check_link":
            assert arguments["stream"] == self.stream_id, arguments
            source, target = (
                self.the_node_of_the_id(arguments[end]["node"]) for end in ("source", "target")
            )
            if arguments["target"]["port"] in self.picture_inputs:
                return self.checked_against_the_result_schema(operation, {"outcome": "connects"})
            return self.checked_against_the_result_schema(operation, {
                "outcome": "refused",
                "refusal": {
                    "refusal_code": "link_pin_types_do_not_connect", "action": "connect",
                    "refused_operation_index": 0,
                    "refused_link": {
                        "source": {"node": source["id"], "port": arguments["source"]["port"]},
                        "target": {"node": target["id"], "port": arguments["target"]["port"]},
                    },
                    "runtime_words": "a Texture output does not connect to a Float input",
                },
            })
        if operation == "graph":
            assert arguments.get("stream") == self.stream_id, arguments
            return {"stream": self.stream_id, "nodes": self.nodes, "links": self.links}
        if operation == "undo":
            assert arguments["project_directory"] == self.project_directory, arguments
            transaction, added_nodes, added_links = self.transactions.pop()
            if not self.undo_reverses_nothing:
                self.nodes = [node for node in self.nodes if node not in added_nodes]
                self.links = [link for link in self.links if link not in added_links]
            return self.checked_against_the_result_schema(operation, {
                "format_version": 2, "transaction": str(uuid.uuid4()), "commit": "c2",
                "author_kind": "agent", "author_name": "script", "operations": [],
                "inverse_operations": [], "undoes": transaction,
                "applied_at": "2026-01-01T00:00:00Z",
            })
        raise AssertionError(f"the stand-in session does not answer `{operation}`")

    def answered_on_the_project_files(
        self, operation: str, arguments: "dict[str, Any]"
    ) -> "dict[str, Any]":
        """A file operation on the stand-in project, whose declaration check refuses a typed
        input written with no default, as the declaration reader does: at the line of the
        decorator above it, where the refusal is raised."""
        if operation == "check_project_code":
            paths = arguments.get("paths") or sorted(self.project_files)
            findings = [
                {"check": "python_node_declarations", "finding_code": "value_input_has_no_default",
                 "severity": "error", "words": "`level` has no default. To fix: give it one.",
                 "path": path, "line": line_number - 1, "node_type": the_node_type_at(path, "Tremolo"),
                 "port": "level"}
                for path in paths
                for line_number, line in enumerate(self.project_files[path].splitlines(), start=1)
                if "(level: float," in line
            ]
            return {
                "verdict": "failed" if findings else "passed",
                "checks": [{"check": "python_node_declarations",
                            "state": "failed" if findings else "passed"}],
                "findings": findings,
                "files_checked": [
                    {"path": path, "content_sha256": sha256_of(self.project_files[path])}
                    for path in paths
                ],
            }
        path = arguments["path"]
        if operation == "create_node_file":
            if path in self.project_files:
                raise StandInRefusal(f"`{path}` already holds a file")
            self.project_files[path] = "# the runtime's node template\n"
            return {"path": path, "node_type": the_node_type_at(path, "Template"),
                    "transaction": str(uuid.uuid4()), "commit": "c0"}
        if path not in self.project_files:
            raise StandInRefusal(f"the project holds no file `{path}`")
        if operation == "read_project_file":
            content = self.project_files[path].encode("utf-8")
            return {"path": path, "byte_len": len(content),
                    "content_base64": base64.b64encode(content).decode("ascii"),
                    "content_sha256": sha256_of(self.project_files[path])}
        expected_sha256 = arguments.get("expected_sha256")
        if expected_sha256 is not None and expected_sha256 != sha256_of(self.project_files[path]):
            raise StandInRefusal(f"`{path}` changed since it was read")
        self.project_files[path] = arguments["content"]
        return {"path": path, "content_sha256": sha256_of(arguments["content"]),
                "transaction": str(uuid.uuid4()), "commit": "c1"}

    def the_node_of_the_id(self, node_id: str) -> "dict[str, Any]":
        [node] = [node for node in self.nodes if node["id"] == node_id]
        return node

    def applied(self, requested: "list[dict[str, Any]]") -> "dict[str, Any]":
        node_by_label: "dict[str, dict[str, Any]]" = {}
        added_nodes: "list[dict[str, Any]]" = []
        added_links: "list[dict[str, Any]]" = []
        answer: "dict[str, Any]" = {
            "mode": "commit", "transaction": str(uuid.uuid4()), "commit": "c1",
            "operations": requested, "added_nodes": [], "connected_links": [],
        }
        for each in requested:
            assert each["stream"] == self.stream_id, each
            if each["operation"] == "add_node":
                node = {"id": str(uuid.uuid4()), "name": each.get("as") or "node",
                        "type": each["type"]}
                node_by_label[each.get("as") or node["id"]] = node
                added_nodes.append(node)
                answer["added_nodes"].append({"stream": self.stream_id, "node": node["id"],
                                              "name": node["name"], "as": each.get("as")})
            elif each["operation"] == "connect":
                source, target = (
                    node_by_label.get(each[end]["node"]) or self.the_node_of_the_id(each[end]["node"])
                    for end in ("source", "target")
                )
                added_links.append({
                    "id": str(uuid.uuid4()),
                    "source": {"node": source["name"], "port": each["source"]["port"]},
                    "target": {"node": target["name"], "port": each["target"]["port"]},
                })
                answer["connected_links"].append({
                    "stream": self.stream_id,
                    "source": {"node": source["id"], "port": each["source"]["port"]},
                    "target": {"node": target["id"], "port": each["target"]["port"]},
                    "state": "pending",
                })
        self.nodes += added_nodes
        self.links += added_links
        self.transactions.append((answer["transaction"], added_nodes, added_links))
        return answer


#: The project file operations the stand-in session answers on its own files.
FILE_OPERATIONS = ("create_node_file", "read_project_file", "write_project_file", "check_project_code")


def sha256_of(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()


def the_node_type_at(path: str, name: str) -> str:
    """The node type a node `name` declared in the module at `path` is named by."""
    return f"{path.removesuffix('.py').replace('/', '.')}:{name}"


def build_a_graph_document() -> "dict[str, Any]":
    [path] = [path for path in every_agent_loop_script_path() if path.stem == BUILD_A_GRAPH_SCRIPT_NAME]
    return a_json_object(path.read_text(encoding="utf-8"))


def played(script: AgentLoopScript, session: ContractBackedAgentSession) -> "dict[str, Any]":
    return asyncio.run(play_agent_loop_script(session, script))


def test_there_is_a_script_for_the_first_loop() -> None:
    assert BUILD_A_GRAPH_SCRIPT_NAME in [path.stem for path in every_agent_loop_script_path()]


@pytest.mark.parametrize(
    "script_path", every_agent_loop_script_path(), ids=lambda script_path: script_path.stem
)
def test_every_script_is_well_formed_and_named_by_its_file(script_path: Path) -> None:
    assert load_agent_loop_script(script_path).script == script_path.stem


@pytest.mark.parametrize(
    "script_path", every_agent_loop_script_path(), ids=lambda script_path: script_path.stem
)
def test_every_script_plays_through_on_the_committed_contracts_tools_and_schemas(script_path: Path) -> None:
    script = load_agent_loop_script(script_path)
    session = ContractBackedAgentSession(the_committed_control_contract())

    answers = played(script, session)

    assert list(answers) == [step.name for step in script.steps]


def test_build_a_graph_calls_each_listed_tool_by_name_after_describing_it() -> None:
    session = ContractBackedAgentSession(the_committed_control_contract())

    played(agent_loop_script_from(build_a_graph_document()), session)

    called = [name for name, _ in session.tool_calls]
    assert called[:2] == ["list_operation_groups", "describe_operations"]
    assert session.tool_calls[1][1] == {
        "operations": ["list_streams", "transact", "check_link", "graph", "undo"]
    }
    assert called[2:] == [
        "list_streams", "transact", CALL_OPERATION_TOOL_NAME, CALL_OPERATION_TOOL_NAME,
        "transact", "graph", "undo", "undo", "graph",
    ]
    checked = [arguments["operation"] for name, arguments in session.tool_calls
               if name == CALL_OPERATION_TOOL_NAME]
    assert checked == ["check_link", "check_link"]


def test_an_operation_that_is_not_a_listed_tool_is_called_through_call_operation() -> None:
    document = {
        "script": "add_one_node",
        "task": {"statement": "Add a sink.", "givens": {"sink_node_type": "nodes.frame_sink:FrameSink"}},
        "steps": [
            {"name": "streams", "tool": "list_streams", "arguments": {}},
            {"name": "added", "tool": "add_node", "arguments": {
                "stream": {"$read": {"step": "streams", "pointer": "/streams/0/stream"}},
                "type": {"$given": "sink_node_type"},
            }, "expect": [{"at": "/added_nodes", "has_length": 1}]},
        ],
    }
    session = ContractBackedAgentSession(the_committed_control_contract())

    played(agent_loop_script_from(document), session)

    assert session.tool_calls[-1][0] == CALL_OPERATION_TOOL_NAME
    assert session.tool_calls[-1][1]["operation"] == "add_node"


def test_a_tool_the_session_neither_listed_nor_discovered_fails_the_run() -> None:
    contract = the_committed_control_contract()
    # `discover` is in the contract, served on JSON-RPC alone: never listed, never discovered.
    [discover] = [method for method in contract["methods"] if method["name"] == "discover"]
    assert "agent" not in discover["x-exposures"], discover
    document = build_a_graph_document()
    document["steps"][0]["tool"] = "discover"

    with pytest.raises(AgentLoopScriptRunFailed, match="`discover` was neither in `tools/list`"):
        played(agent_loop_script_from(document), ContractBackedAgentSession(contract))


def test_an_operation_describe_operations_left_out_fails_the_run() -> None:
    session = ContractBackedAgentSession(the_committed_control_contract())
    session.operations_left_undescribed.add("undo")

    with pytest.raises(AgentLoopScriptRunFailed, match="did not describe `undo`"):
        played(agent_loop_script_from(build_a_graph_document()), session)


def test_a_literal_neither_given_nor_declared_by_the_schema_fails_the_run() -> None:
    document = build_a_graph_document()
    document["steps"][1]["arguments"]["operations"][0]["type"] = "tatolab.testpattern:TestPatternSource"

    with pytest.raises(AgentLoopScriptRunFailed, match="neither a given"):
        played(agent_loop_script_from(document), ContractBackedAgentSession(the_committed_control_contract()))


def test_a_literal_the_schema_declares_at_its_own_position_is_allowed() -> None:
    document = build_a_graph_document()
    document["steps"][1]["arguments"]["mode"] = "commit"
    session = ContractBackedAgentSession(the_committed_control_contract())

    played(agent_loop_script_from(document), session)

    transact_arguments = [arguments for name, arguments in session.tool_calls if name == "transact"]
    assert transact_arguments[0]["mode"] == "commit"


def test_a_literal_the_schema_declares_only_at_another_position_fails_the_run() -> None:
    document = build_a_graph_document()
    # `live` is a constant of `transact`'s `mode`, not of a node's `as`, where it is valid too.
    document["steps"][1]["arguments"]["operations"][0]["as"] = "live"

    with pytest.raises(AgentLoopScriptRunFailed, match="at `/operations/0/as`"):
        played(agent_loop_script_from(document), ContractBackedAgentSession(the_committed_control_contract()))


def test_arguments_the_params_schema_refuses_fail_the_run() -> None:
    document = build_a_graph_document()
    document["steps"][1]["arguments"]["operations"][0]["colour"] = {"$given": "source_label"}

    with pytest.raises(AgentLoopScriptRunFailed, match="params schema"):
        played(agent_loop_script_from(document), ContractBackedAgentSession(the_committed_control_contract()))


def test_an_unexpected_refusal_fails_the_run() -> None:
    session = ContractBackedAgentSession(the_committed_control_contract())
    session.refused_operations.add("transact")

    with pytest.raises(
        AgentLoopScriptRunFailed, match=r"^`transact` was refused \(refused\): `transact` was refused$"
    ):
        played(agent_loop_script_from(build_a_graph_document()), session)


def test_an_answer_that_misses_an_expectation_fails_the_run() -> None:
    session = ContractBackedAgentSession(the_committed_control_contract())
    session.undo_reverses_nothing = True

    with pytest.raises(AgentLoopScriptRunFailed, match="graph_after_the_undo.*holds an item where"):
        played(agent_loop_script_from(build_a_graph_document()), session)


def test_a_read_from_a_step_not_yet_answered_is_malformed() -> None:
    document = build_a_graph_document()
    document["steps"][0]["arguments"] = {"stream": {"$read": {"step": "edit", "pointer": "/x"}}}

    with pytest.raises(AgentLoopScriptIsMalformed, match="not a step answered before it"):
        agent_loop_script_from(document)


def test_a_given_the_task_does_not_state_is_malformed() -> None:
    document = copy.deepcopy(build_a_graph_document())
    del document["task"]["givens"]["sink_input"]

    with pytest.raises(AgentLoopScriptIsMalformed, match="`sink_input` is not among the task's givens"):
        agent_loop_script_from(document)


def test_an_expectation_of_no_known_kind_is_malformed() -> None:
    document = build_a_graph_document()
    document["steps"][0]["expect"] = [{"at": "/streams", "mentions": "running"}]

    with pytest.raises(AgentLoopScriptIsMalformed, match="an expectation is `at` and one of"):
        agent_loop_script_from(document)


def test_a_check_that_refuses_the_link_the_script_makes_fails_the_run() -> None:
    session = ContractBackedAgentSession(the_committed_control_contract())
    session.picture_inputs = set()

    with pytest.raises(AgentLoopScriptRunFailed, match="link_checked_into_the_sink.*`/outcome`"):
        played(agent_loop_script_from(build_a_graph_document()), session)

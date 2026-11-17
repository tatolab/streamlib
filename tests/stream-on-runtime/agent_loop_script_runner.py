# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Replays an agent loop script over one MCP session, as an agent with nothing but that session.

A script is data, in `agent_loop_scripts/`: a task statement with its named givens, then steps.
Each step names a tool, the arguments it is called with and expectations on typed fields of
its answer. An argument value is a constant its params schema declares at that position, a
`{"$given": name}` from the task, or a `{"$read": {"step", "pointer"}}` JSON pointer into an
earlier step's answer; nothing else may appear, so a script can only pass what it was told or
what the runtime said in the same session.

The session opens as an agent's does: `tools/list`, then `list_operation_groups` and
`describe_operations` for every operation the script calls. A tool `tools/list` holds is
called by name; any other operation `list_operation_groups` lists is called through
`call_operation`; anything else fails the run. Every argument object is checked against the
`params_schema` `describe_operations` answered, and any refusal fails the run.
"""

from __future__ import annotations

import json
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from jsonschema import Draft202012Validator
from mcp.types import CallToolResult, ListToolsResult, TextContent

#: Where the scripts live, one JSON file each.
AGENT_LOOP_SCRIPTS_DIRECTORY = Path(__file__).resolve().parent / "agent_loop_scripts"

LIST_OPERATION_GROUPS_TOOL_NAME = "list_operation_groups"
DESCRIBE_OPERATIONS_TOOL_NAME = "describe_operations"
CALL_OPERATION_TOOL_NAME = "call_operation"

GIVEN_REFERENCE_KEY = "$given"
READ_REFERENCE_KEY = "$read"

#: The expectation kinds a step may state, each on the value its `at` pointer reaches.
EXPECTATION_KINDS = ("equals", "is_one_of", "has_length", "holds_an_item_where", "holds_no_item_where")


class AgentLoopScriptIsMalformed(Exception):
    """A script breaks a rule that holds before any runtime answers."""


class AgentLoopScriptRunFailed(AssertionError):
    """A script's replay met a refusal, an undiscovered tool, an argument its schema refuses or
    an expectation its answer does not meet."""


class AgentToolClient(Protocol):
    """What the runner needs of an MCP client: the official SDK's `Client` has both."""

    async def list_tools(self, *, cursor: "str | None" = None) -> ListToolsResult: ...

    async def call_tool(
        self, name: str, arguments: "dict[str, Any] | None" = None
    ) -> CallToolResult: ...


@dataclass(frozen=True)
class AgentLoopStep:
    """One call of a script: its name, which later steps read its answer by, the tool, the
    arguments as written and the expectations on its answer."""

    name: str
    tool: str
    arguments: "dict[str, Any]"
    expectations: "list[dict[str, Any]]"


@dataclass(frozen=True)
class AgentLoopScript:
    """A task statement, its named givens, and the steps that carry it out."""

    script: str
    task_statement: str
    givens: "dict[str, Any]"
    steps: "list[AgentLoopStep]"

    def operations_called(self) -> "list[str]":
        """Each tool the steps call, once, in the order first called."""
        return list(dict.fromkeys(step.tool for step in self.steps))


def every_agent_loop_script_path() -> "list[Path]":
    """Every script in `agent_loop_scripts/`, by file name."""
    return sorted(AGENT_LOOP_SCRIPTS_DIRECTORY.glob("*.json"))


def references_in(template: Any) -> "Iterator[dict[str, Any]]":
    """Each `$given` or `$read` reference anywhere in `template`."""
    if isinstance(template, dict):
        if GIVEN_REFERENCE_KEY in template or READ_REFERENCE_KEY in template:
            yield template
            return
        for value in template.values():
            yield from references_in(value)
    elif isinstance(template, list):
        for value in template:
            yield from references_in(value)


def check_the_references_of(
    template: Any, givens: "Mapping[str, Any]", readable_steps: "Sequence[str]", where: str
) -> None:
    """Refuse a reference that names no given, or no step before this one, or is shaped wrong."""
    for reference in references_in(template):
        if set(reference) == {GIVEN_REFERENCE_KEY}:
            if reference[GIVEN_REFERENCE_KEY] not in givens:
                raise AgentLoopScriptIsMalformed(
                    f"{where}: `{reference[GIVEN_REFERENCE_KEY]}` is not among the task's givens "
                    f"{sorted(givens)}"
                )
        elif set(reference) == {READ_REFERENCE_KEY}:
            read = reference[READ_REFERENCE_KEY]
            if not isinstance(read, dict) or set(read) != {"step", "pointer"}:
                raise AgentLoopScriptIsMalformed(
                    f"{where}: a `$read` names exactly a `step` and a `pointer`: {reference}"
                )
            if read["step"] not in readable_steps:
                raise AgentLoopScriptIsMalformed(
                    f"{where}: `$read` names `{read['step']}`, which is not a step answered "
                    f"before it ({list(readable_steps)})"
                )
            json_pointer_tokens(read["pointer"])
        else:
            raise AgentLoopScriptIsMalformed(
                f"{where}: a reference holds `$given` or `$read` alone: {reference}"
            )


def agent_loop_script_from(document: "dict[str, Any]") -> AgentLoopScript:
    """The script `document` states, once every rule that holds without a runtime is met."""
    task = document["task"]
    givens: "dict[str, Any]" = task["givens"]
    steps: "list[AgentLoopStep]" = []
    for written in document["steps"]:
        name = written["name"]
        if name in (step.name for step in steps):
            raise AgentLoopScriptIsMalformed(f"two steps are named `{name}`")
        step = AgentLoopStep(
            name=name,
            tool=written["tool"],
            arguments=written["arguments"],
            expectations=written.get("expect", []),
        )
        answered_before = [earlier.name for earlier in steps]
        check_the_references_of(
            step.arguments, givens, answered_before, f"step `{name}`'s arguments"
        )
        for expectation in step.expectations:
            kinds = [kind for kind in EXPECTATION_KINDS if kind in expectation]
            if set(expectation) != {"at", *kinds} or len(kinds) != 1:
                raise AgentLoopScriptIsMalformed(
                    f"step `{name}`: an expectation is `at` and one of {EXPECTATION_KINDS}: "
                    f"{expectation}"
                )
            json_pointer_tokens(expectation["at"])
            check_the_references_of(
                expectation, givens, [*answered_before, name], f"step `{name}`'s expectations"
            )
        steps.append(step)
    return AgentLoopScript(
        script=document["script"], task_statement=task["statement"], givens=givens, steps=steps
    )


def load_agent_loop_script(path: Path) -> AgentLoopScript:
    """The script at `path`."""
    return agent_loop_script_from(json.loads(path.read_text(encoding="utf-8")))


def json_pointer_tokens(pointer: str) -> "list[str]":
    """The reference tokens of an RFC 6901 JSON pointer."""
    if pointer == "":
        return []
    if not pointer.startswith("/"):
        raise AgentLoopScriptIsMalformed(f"a JSON pointer starts with `/`: {pointer!r}")
    return [token.replace("~1", "/").replace("~0", "~") for token in pointer[1:].split("/")]


def value_at(document: Any, pointer: str) -> Any:
    """The value `pointer` reaches in `document`; a pointer that reaches nothing fails the run."""
    value = document
    for token in json_pointer_tokens(pointer):
        if isinstance(value, dict) and token in value:
            value = value[token]
        elif isinstance(value, list) and token.isdigit() and int(token) < len(value):
            value = value[int(token)]
        else:
            raise AgentLoopScriptRunFailed(f"`{pointer}` reaches nothing in {document}")
    return value


def subschemas_in_force(schemas: "Sequence[Any]", root: Any) -> "list[dict[str, Any]]":
    """`schemas` with each `$ref` into the root followed and each `allOf`, `anyOf` and `oneOf`
    member added, so every subschema that can apply at one position is listed."""
    in_force: "list[dict[str, Any]]" = []
    pending = list(schemas)
    while pending:
        schema = pending.pop(0)
        if not isinstance(schema, dict) or any(schema is seen for seen in in_force):
            continue
        in_force.append(schema)
        reference = schema.get("$ref")
        if isinstance(reference, str) and reference.startswith("#"):
            pending.append(value_at(root, reference[1:]))
        for combinator in ("allOf", "anyOf", "oneOf"):
            pending.extend(schema.get(combinator, []))
    return in_force


def constants_declared_at(params_schema: Any, path: "Sequence[str | int]") -> "list[Any]":
    """Each `const` and `enum` member the params schema declares for the value at `path` in the
    arguments: a literal is allowed only where the schema itself fixes it."""
    schemas = subschemas_in_force([params_schema], params_schema)
    for token in path:
        reached: "list[Any]" = []
        for schema in schemas:
            if isinstance(token, str):
                properties = schema.get("properties", {})
                if token in properties:
                    reached.append(properties[token])
                elif isinstance(schema.get("additionalProperties"), dict):
                    reached.append(schema["additionalProperties"])
            else:
                prefix_items = schema.get("prefixItems", [])
                if token < len(prefix_items):
                    reached.append(prefix_items[token])
                elif isinstance(schema.get("items"), dict):
                    reached.append(schema["items"])
        schemas = subschemas_in_force(reached, params_schema)
    declared: "list[Any]" = []
    for schema in schemas:
        if "const" in schema:
            declared.append(schema["const"])
        declared.extend(schema.get("enum", []))
    return declared


def resolved(
    template: Any,
    givens: "Mapping[str, Any]",
    answers: "Mapping[str, Any]",
    params_schema: Any,
    where: str,
    path: "tuple[str | int, ...]" = (),
) -> Any:
    """`template` with each reference replaced by the given or the answered value it names.

    With a `params_schema`, a scalar written in the script itself must be a constant that schema
    declares at the scalar's own position: what an argument holds came from the task, an answer
    or the operation's own schema.
    """
    if isinstance(template, dict):
        if set(template) == {GIVEN_REFERENCE_KEY}:
            return givens[template[GIVEN_REFERENCE_KEY]]
        if set(template) == {READ_REFERENCE_KEY}:
            read = template[READ_REFERENCE_KEY]
            return value_at(answers[read["step"]], read["pointer"])
        return {
            key: resolved(value, givens, answers, params_schema, where, (*path, key))
            for key, value in template.items()
        }
    if isinstance(template, list):
        return [
            resolved(value, givens, answers, params_schema, where, (*path, index))
            for index, value in enumerate(template)
        ]
    if params_schema is not None and not any(
        type(template) is type(allowed) and template == allowed
        for allowed in constants_declared_at(params_schema, path)
    ):
        position = "/" + "/".join(str(token) for token in path)
        raise AgentLoopScriptRunFailed(
            f"{where}: {template!r} is written in the script at `{position}`, but is neither a "
            f"given, nor read from an answer, nor a constant the operation's params schema "
            f"declares there"
        )
    return template


def failed_expectation(
    expectation: "Mapping[str, Any]",
    answer: Any,
    givens: "Mapping[str, Any]",
    answers: "Mapping[str, Any]",
) -> "str | None":
    """Why `answer` does not meet `expectation`, or `None` when it does."""
    expected = resolved(
        {key: value for key, value in expectation.items() if key != "at"},
        givens,
        answers,
        None,
        "an expectation",
    )
    at = expectation["at"]
    found = value_at(answer, at)
    if "equals" in expected and found != expected["equals"]:
        return f"`{at}` is {found!r}, not {expected['equals']!r}"
    if "is_one_of" in expected and found not in expected["is_one_of"]:
        return f"`{at}` is {found!r}, not one of {expected['is_one_of']!r}"
    if "has_length" in expected and (
        not isinstance(found, list) or len(found) != expected["has_length"]
    ):
        return f"`{at}` is {found!r}, not a list of {expected['has_length']}"
    for kind, wanted_present in (("holds_an_item_where", True), ("holds_no_item_where", False)):
        if kind not in expected:
            continue
        if not isinstance(found, list):
            return f"`{at}` is {found!r}, not a list"
        fields: "dict[str, Any]" = expected[kind]
        matching = [item for item in found if an_item_has_the_fields(item, fields)]
        if wanted_present and not matching:
            return f"`{at}` holds no item where {fields!r}: {found!r}"
        if not wanted_present and matching:
            return f"`{at}` holds an item where {fields!r}: {matching!r}"
    return None


def an_item_has_the_fields(item: Any, fields: "Mapping[str, Any]") -> bool:
    """Whether every pointer in `fields` reaches its value in `item`."""
    for pointer, wanted in fields.items():
        try:
            if value_at(item, pointer) != wanted:
                return False
        except AgentLoopScriptRunFailed:
            return False
    return True


def the_json_a_tool_answered(tool: str, result: CallToolResult) -> Any:
    """The JSON a successful tool call answered; a refusal fails the run with its words."""
    texts = [content.text for content in result.content if isinstance(content, TextContent)]
    if result.is_error:
        raise AgentLoopScriptRunFailed(f"`{tool}` was refused: {' '.join(texts)}")
    if not texts:
        raise AgentLoopScriptRunFailed(f"`{tool}` answered no text: {result.content}")
    return json.loads(texts[0])


async def a_tool_answer(client: AgentToolClient, tool: str, arguments: "dict[str, Any]") -> Any:
    """`tool` called with `arguments`; a refusal, as a tool error or a protocol error, fails the
    run."""
    try:
        result = await client.call_tool(tool, arguments)
    except Exception as raised:
        raise AgentLoopScriptRunFailed(f"`{tool}` was refused: {raised!r}") from raised
    return the_json_a_tool_answered(tool, result)


async def every_listed_tool_name(client: AgentToolClient) -> "list[str]":
    """Every tool `tools/list` holds, through each page."""
    names: "list[str]" = []
    cursor: "str | None" = None
    while True:
        listed = await client.list_tools(cursor=cursor)
        names.extend(tool.name for tool in listed.tools)
        cursor = listed.next_cursor
        if cursor is None:
            return names


@dataclass
class AgentLoopSession:
    """What one session learned before its first step: the tools it lists, the operations it
    discovered, each called operation's params schema, and each step's answer so far."""

    listed_tool_names: "list[str]"
    discovered_operation_names: "list[str]"
    params_schemas: "dict[str, Any]"
    answers: "dict[str, Any]"

    def the_call_of(self, tool: str, arguments: "dict[str, Any]") -> "tuple[str, dict[str, Any]]":
        """The MCP tool and arguments that call `tool`: itself when listed, `call_operation` when
        discovered."""
        if tool in self.listed_tool_names:
            return tool, arguments
        if tool in self.discovered_operation_names:
            return CALL_OPERATION_TOOL_NAME, {"operation": tool, "params": arguments}
        raise AgentLoopScriptRunFailed(
            f"`{tool}` was neither in `tools/list` ({self.listed_tool_names}) nor listed by "
            f"`{LIST_OPERATION_GROUPS_TOOL_NAME}` in this session"
        )


async def an_opened_agent_loop_session(
    client: AgentToolClient, script: AgentLoopScript
) -> AgentLoopSession:
    """The session as an agent opens it: the tool list, the operation groups, then the
    description of every operation the script calls."""
    listed_tool_names = await every_listed_tool_name(client)
    for discovery_tool in (LIST_OPERATION_GROUPS_TOOL_NAME, DESCRIBE_OPERATIONS_TOOL_NAME):
        if discovery_tool not in listed_tool_names:
            raise AgentLoopScriptRunFailed(
                f"`tools/list` holds no `{discovery_tool}`: {listed_tool_names}"
            )
    groups = await a_tool_answer(client, LIST_OPERATION_GROUPS_TOOL_NAME, {})
    discovered_operation_names = [
        operation["name"] for group in groups["groups"] for operation in group["operations"]
    ]
    session = AgentLoopSession(
        listed_tool_names=listed_tool_names,
        discovered_operation_names=discovered_operation_names,
        params_schemas={},
        answers={},
    )
    for tool in script.operations_called():
        session.the_call_of(tool, {})
    described = await a_tool_answer(
        client, DESCRIBE_OPERATIONS_TOOL_NAME, {"operations": script.operations_called()}
    )
    session.params_schemas = {
        operation["name"]: operation["params_schema"] for operation in described["operations"]
    }
    for tool in script.operations_called():
        if tool not in session.params_schemas:
            raise AgentLoopScriptRunFailed(
                f"`{DESCRIBE_OPERATIONS_TOOL_NAME}` did not describe `{tool}`: "
                f"{sorted(session.params_schemas)}"
            )
    return session


async def play_agent_loop_script(client: AgentToolClient, script: AgentLoopScript) -> "dict[str, Any]":
    """Replay `script` on `client`'s one session, and return each step's answer by step name."""
    session = await an_opened_agent_loop_session(client, script)
    for step in script.steps:
        params_schema = session.params_schemas[step.tool]
        where = f"`{script.script}` step `{step.name}` (`{step.tool}`)"
        arguments = resolved(
            step.arguments,
            script.givens,
            session.answers,
            params_schema,
            where,
        )
        refusals = sorted(
            Draft202012Validator(params_schema).iter_errors(arguments),
            key=lambda error: [str(token) for token in error.path],
        )
        if refusals:
            raise AgentLoopScriptRunFailed(
                f"{where}: the arguments do not meet the params schema `describe_operations` "
                f"answered: {[f'{list(error.path)}: {error.message}' for error in refusals]}"
            )
        answer = await a_tool_answer(client, *session.the_call_of(step.tool, arguments))
        session.answers[step.name] = answer
        for expectation in step.expectations:
            why = failed_expectation(expectation, answer, script.givens, session.answers)
            if why is not None:
                raise AgentLoopScriptRunFailed(f"{where}: {why}")
    return session.answers

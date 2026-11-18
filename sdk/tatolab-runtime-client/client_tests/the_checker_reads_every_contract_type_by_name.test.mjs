// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import {
  CONTROL_CONTRACT_SCHEMAS,
  controlContractIssues,
  isControlContractValue,
  readControlContractValue,
} from "../contract.js";

const STREAM = "6f1c2a4e-0b7d-4c3e-9a51-2d8e7f60b913";

test("a key the contract does not name is an issue in what is sent and ignored in what is read", () => {
  const request = { stream: STREAM, type: "nodes.grayscale_effect:GrayscaleEffect", colour: "red" };
  assert.deepEqual(
    controlContractIssues("AddNodeRequest", request, "sent").map((issue) => [issue.code, issue.path]),
    [["unknown_key", ["colour"]]],
  );
  assert.deepEqual(controlContractIssues("AddNodeRequest", request, "read"), []);
  const answer = { version: "0.68.82", build_id: "stand-in", up_for_ms: 3, added_after_this_client: true };
  assert.equal(isControlContractValue("DescribeRuntimeToolResult", answer), true);
});

test("an open map takes any key both ways", () => {
  assert.deepEqual(controlContractIssues("AddNodeRequest", { stream: STREAM, type: "t", pins: { anything: 1 } }, "sent"), []);
});

test("a tagged union is read by its tag, and a wrong field names the alternative its tag chose", () => {
  assert.equal(isControlContractValue("PinType", { type: "boolean" }), true);
  assert.equal(isControlContractValue("PinType", { type: "no_such_type" }), false);
  const issues = controlContractIssues("PinType", { type: "integer", exact_size: "huge" }, "read");
  assert.ok(issues.length > 0);
  assert.deepEqual(issues[0].path, ["exact_size"]);
  assert.deepEqual(
    controlContractIssues("PinType", { type: "boolean", extra: 1 }, "sent").map((issue) => issue.code),
    ["unknown_key"],
  );
});

test("a required field that is absent, a wrong type and a malformed UUID each name where", () => {
  assert.deepEqual(
    controlContractIssues("DescribeRuntimeToolResult", { version: 1, build_id: "b" }, "read").map((issue) => [issue.code, issue.path]),
    [["invalid_type", ["version"]], ["missing", ["up_for_ms"]]],
  );
  assert.deepEqual(
    controlContractIssues("AddNodeRequest", { stream: "main-0a1b2c", level: ["not-a-uuid"], type: "t" }, "sent").map((issue) => [issue.code, issue.path]),
    [["invalid_format", ["level", 0]]],
  );
});

test("a wide integer takes a bigint, a narrow one refuses it, and a format bounds its range", () => {
  assert.deepEqual(controlContractIssues("DescribeRuntimeToolResult", { version: "v", build_id: "b", up_for_ms: 18446744073709551615n }, "read"), []);
  assert.deepEqual(
    controlContractIssues("DescribeRuntimeToolResult", { version: "v", build_id: "b", up_for_ms: 18446744073709551616n }, "read").map((issue) => issue.code),
    ["out_of_range"],
  );
  assert.deepEqual(
    controlContractIssues("DescribeRuntimeToolResult", { version: "v", build_id: "b", up_for_ms: -1 }, "read").map((issue) => issue.code),
    ["too_small", "out_of_range"],
  );
  assert.deepEqual(controlContractIssues("DiscoverRequest", { contract_version: 4294967296 }, "sent").map((issue) => issue.code), ["out_of_range"]);
  assert.deepEqual(controlContractIssues("DiscoverRequest", { contract_version: 3n }, "sent").map((issue) => issue.code), ["invalid_type"]);
});

test("a set that may grow reads a member it does not name as its fallback, and the guard holds only the value as it stands", () => {
  const refusal = { refusal_code: "a_code_added_after_this_client", action: "open_project", runtime_words: "refused" };
  assert.deepEqual(readControlContractValue("ProjectActionRefusalOnTheWire", refusal), { read: true, value: { ...refusal, refusal_code: "refused" } });
  assert.equal(isControlContractValue("ProjectActionRefusalOnTheWire", refusal), false);
  const withNoCode = { action: "open_project", runtime_words: "refused" };
  assert.deepEqual(readControlContractValue("ProjectActionRefusalOnTheWire", withNoCode), { read: true, value: { ...withNoCode, refusal_code: "refused" } });
  assert.equal(isControlContractValue("ProjectActionRefusalOnTheWire", { ...withNoCode, refusal_code: "refused" }), true);
  const notAString = readControlContractValue("ProjectActionRefusalOnTheWire", { ...withNoCode, refusal_code: 42 });
  assert.equal(notAString.read, false);
  assert.deepEqual(controlContractIssues("ControlOperationRefusalCode", "a_code_added_after_this_client", "sent").map((issue) => issue.code), ["no_alternative"]);
});

test("reading leaves the value it was given untouched", () => {
  const refusal = Object.freeze({ action: "open_project", runtime_words: "refused" });
  readControlContractValue("ProjectActionRefusalOnTheWire", refusal);
  assert.equal("refusal_code" in refusal, false);
});

test("the schema data holds every component ControlContractComponents names, and no other", async () => {
  const declarations = await readFile(new URL("../control_contract.d.ts", import.meta.url), "utf8");
  const [, body] = declarations.match(/export interface ControlContractComponents \{\n([^}]*)\n\}/) ?? [];
  assert.ok(body !== undefined, "control_contract.d.ts declares ControlContractComponents");
  const declaredNames = body.split("\n").map((line) => line.trim().split(":")[0]).filter((name) => name !== "");
  assert.ok(declaredNames.length > 0);
  assert.deepEqual([...declaredNames].sort(), Object.keys(CONTROL_CONTRACT_SCHEMAS.components).sort());
});

const VALID_VALUES_BY_COMPONENT = {
  PinType: { type: "boolean" },
  AddNodeRequest: { stream: STREAM, type: "nodes.grayscale_effect:GrayscaleEffect" },
  DescribeRuntimeToolResult: { version: "0.68.82", build_id: "stand-in", up_for_ms: 3 },
  ProjectActionRefusalOnTheWire: { action: "open_project", runtime_words: "refused", refusal_code: "refused" },
  ProjectChangeFeedEntryOnTheWire: {
    status: "applied",
    format_version: 1,
    transaction: "t1",
    commit: "c1",
    author_kind: "person",
    author_name: "someone",
    operations: [],
    inverse_operations: [],
    applied_at: "2026-11-17T00:00:00Z",
  },
  NodeTypeOfOneProjectSource: { type: "nodes.grayscale_effect:GrayscaleEffect", description: "Grays a frame.", source: "dependency", project: "p", folder: "f", version: "1.0.0" },
};

test("a representative valid value of each kind of component reads clean both ways", () => {
  for (const [name, value] of Object.entries(VALID_VALUES_BY_COMPONENT)) {
    assert.deepEqual(controlContractIssues(name, value, "sent"), [], `${name} sent`);
    assert.deepEqual(controlContractIssues(name, value, "read"), [], `${name} read`);
    assert.equal(isControlContractValue(name, value), true, name);
  }
  const unknownBesideOneOf = { ...VALID_VALUES_BY_COMPONENT.NodeTypeOfOneProjectSource, colour: "red" };
  assert.deepEqual(controlContractIssues("NodeTypeOfOneProjectSource", unknownBesideOneOf, "sent").map((issue) => [issue.code, issue.path]), [["unknown_key", ["colour"]]]);
  const unknownInAFeedEntry = { ...VALID_VALUES_BY_COMPONENT.ProjectChangeFeedEntryOnTheWire, colour: "red" };
  assert.deepEqual(controlContractIssues("ProjectChangeFeedEntryOnTheWire", unknownInAFeedEntry, "sent").map((issue) => [issue.code, issue.path]), [["unknown_key", ["colour"]]]);
  assert.deepEqual(controlContractIssues("ProjectChangeFeedEntryOnTheWire", unknownInAFeedEntry, "read"), []);
});

test("a wrong field in a feed entry names that field, under the alternative its tag beside a $ref chose", () => {
  const entry = { ...VALID_VALUES_BY_COMPONENT.ProjectChangeFeedEntryOnTheWire, commit: 7 };
  for (const direction of ["sent", "read"]) {
    assert.deepEqual(controlContractIssues("ProjectChangeFeedEntryOnTheWire", entry, direction).map((issue) => [issue.code, issue.path]), [["invalid_type", ["commit"]]], direction);
  }
  const { applied_at: _, ...withNoTime } = VALID_VALUES_BY_COMPONENT.ProjectChangeFeedEntryOnTheWire;
  assert.deepEqual(controlContractIssues("ProjectChangeFeedEntryOnTheWire", withNoTime, "read").map((issue) => [issue.code, issue.path]), [["missing", ["applied_at"]]]);
});

test("a bigint on a narrow integer field is refused as a bigint, whatever its size", () => {
  const [issue] = controlContractIssues("NodeTypeOfOneProjectSource", { ...VALID_VALUES_BY_COMPONENT.NodeTypeOfOneProjectSource, line: 3n }, "read");
  assert.deepEqual(issue.path, ["line"]);
  assert.doesNotMatch(issue.message, /2\^53/);
  assert.match(issue.message, /bigint/);
});

test("every component the schema data holds is read by its name, and a name it does not hold is refused", () => {
  for (const name of Object.keys(CONTROL_CONTRACT_SCHEMAS.components)) {
    assert.ok(Array.isArray(controlContractIssues(name, null, "read")), name);
  }
  assert.throws(() => controlContractIssues("#/components/schemas/PinType", {}, "read"), TypeError);
  assert.ok(Object.isFrozen(CONTROL_CONTRACT_SCHEMAS.components.PinType));
  assert.throws(() => controlContractIssues("NoSuchComponent", {}, "read"), TypeError);
  assert.throws(() => controlContractIssues("PinType", {}, "both"), TypeError);
});

test("a number with an exclusive minimum refuses that minimum and takes anything above it", () => {
  assert.deepEqual(controlContractIssues("StreamClockSpec", { mode: "scaled", scale: 0.5 }, "read"), []);
  assert.deepEqual(
    controlContractIssues("StreamClockSpec", { mode: "stepped", step_ns: 1000, pace: 0 }, "read").map((issue) => issue.code),
    ["too_small"],
  );
  assert.deepEqual(controlContractIssues("StreamClockSpec", { mode: "stepped", step_ns: 1000, pace: null }, "read"), []);
});

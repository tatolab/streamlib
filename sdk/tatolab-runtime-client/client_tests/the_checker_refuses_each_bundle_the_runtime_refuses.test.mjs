// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import { controlContractIssues } from "../contract.js";

const examples = JSON.parse(
  await readFile(new URL("./bundle_pin_types_taken_and_refused.json", import.meta.url), "utf8"),
);

test("the checker takes each bundle the runtime takes", () => {
  for (const { why, pin_type: pinType } of examples.taken) {
    assert.deepEqual(controlContractIssues("PinType", pinType, "sent"), [], why);
  }
});

test("the checker refuses each bundle the runtime refuses, where a client sends one", () => {
  for (const { why, pin_type: pinType } of examples.refused) {
    assert.notDeepEqual(controlContractIssues("PinType", pinType, "sent"), [], why);
  }
});

test("a member named twice is refused where the second one is named", () => {
  const twice = examples.refused.find(({ why }) => why === "a member named twice").pin_type;
  assert.deepEqual(
    controlContractIssues("PinType", twice, "sent").map((issue) => [issue.code, issue.path]),
    [["not_unique", ["members", 1, "name"]]],
  );
});

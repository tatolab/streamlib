// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { test } from "node:test";

import { controlContractIssues, readControlContractValue } from "../contract.js";

const A_PIN_TYPE_NO_MEMBER_NAMES = { type: "a_type_a_later_runtime_adds", among: ["x"] };

test("a pin type a later runtime adds is read as it is", () => {
  assert.deepEqual(readControlContractValue("PinType", A_PIN_TYPE_NO_MEMBER_NAMES), {
    read: true,
    value: A_PIN_TYPE_NO_MEMBER_NAMES,
  });
});

test("a pin type a later runtime adds is still refused where a client sends one", () => {
  assert.notDeepEqual(controlContractIssues("PinType", A_PIN_TYPE_NO_MEMBER_NAMES, "sent"), []);
});

test("a known pin type that breaks its member's shape is still refused", () => {
  const read = readControlContractValue("PinType", { type: "float", exact_size: "wide" });
  assert.equal(read.read, false);
});

test("a pin type with no tag is still refused", () => {
  assert.equal(readControlContractValue("PinType", { among: ["x"] }).read, false);
});

const A_BUNDLE_HOLDING_A_LATER_TYPE = {
  type: "bundle",
  members: [
    { name: "gain", member_type: { type: "float" } },
    { name: "later", member_type: A_PIN_TYPE_NO_MEMBER_NAMES },
    { name: "laters", member_type: { type: "list_of", element_type: A_PIN_TYPE_NO_MEMBER_NAMES } },
  ],
};

test("a bundle holding a member of a type a later runtime adds is read as it is", () => {
  assert.deepEqual(readControlContractValue("PinType", A_BUNDLE_HOLDING_A_LATER_TYPE), {
    read: true,
    value: A_BUNDLE_HOLDING_A_LATER_TYPE,
  });
});

test("a bundle holding a member of a type a later runtime adds is still refused where a client sends one", () => {
  assert.notDeepEqual(controlContractIssues("PinType", A_BUNDLE_HOLDING_A_LATER_TYPE, "sent"), []);
});

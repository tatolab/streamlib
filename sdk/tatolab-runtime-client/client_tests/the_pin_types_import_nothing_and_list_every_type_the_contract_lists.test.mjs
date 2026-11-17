// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import { BUILT_IN_PIN_TYPES } from "../pin_types.js";

const packageFile = (fileName) => new URL(`../${fileName}`, import.meta.url);

test("the pin types entry imports nothing, so a browser page loads it", async () => {
  const packageJson = JSON.parse(await readFile(packageFile("package.json"), "utf8"));
  assert.deepEqual(packageJson.exports["./pin_types"], {
    types: "./pin_types.d.ts",
    default: "./pin_types.js",
  });
  for (const fileName of ["pin_types.js", "pin_types.d.ts"]) {
    assert.ok(packageJson.files.includes(fileName), fileName);
    const source = await readFile(packageFile(fileName), "utf8");
    assert.doesNotMatch(source, /^\s*import\b/m, fileName);
    assert.doesNotMatch(source, /\bfrom\s*["']/, fileName);
    assert.doesNotMatch(source, /\bimport\s*\(/, fileName);
  }
});

test("every built-in pin type the contract lists is listed once, frozen, in its order", async () => {
  const contract = JSON.parse(await readFile(packageFile("contract.json"), "utf8"));
  assert.deepEqual(
    BUILT_IN_PIN_TYPES.map((row) => row.name),
    contract["x-pin-types"].map((row) => row.name),
  );
  assert.ok(Object.isFrozen(BUILT_IN_PIN_TYPES));
  for (const row of BUILT_IN_PIN_TYPES) {
    assert.ok(Object.isFrozen(row) && Object.isFrozen(row.flows), row.name);
  }
  const color = BUILT_IN_PIN_TYPES.find((row) => row.name === "color");
  assert.equal(color.value_component, "ColorPinValue");
  const texture = BUILT_IN_PIN_TYPES.find((row) => row.name === "texture");
  assert.equal("value_component" in texture, false);
});

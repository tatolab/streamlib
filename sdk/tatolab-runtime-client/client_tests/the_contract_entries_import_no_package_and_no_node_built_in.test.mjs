// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const PACKAGE_DIRECTORY = new URL("../", import.meta.url);
const ENTRIES_A_BROWSER_PAGE_LOADS = ["./contract", "./pin_types", "./node_ui_manifest"];
const IMPORT_SPECIFIER_PATTERNS = [
  /\bimport\s+(?:type\s+)?(?:[\w*{}\s,$]+?\s+from\s+)?["']([^"']+)["']/g,
  /\bexport\s+(?:type\s+)?(?:\*(?:\s+as\s+\w+)?|\{[^}]*\})\s+from\s+["']([^"']+)["']/g,
  /\bimport\s*\(\s*["']([^"']+)["']\s*\)/g,
  /@import\s+(?:[\w*{}\s,$]+?\s+from\s+)?["']([^"']+)["']/g,
];

function importSpecifiersOf(source) {
  return IMPORT_SPECIFIER_PATTERNS.flatMap((pattern) => [...source.matchAll(pattern)].map((match) => match[1]));
}

/** Every file `entryFiles` reach through relative imports, and each specifier among them that is not relative. */
async function whatTheEntriesReach(entryFiles) {
  const reached = new Set();
  const notRelative = [];
  const toVisit = [...entryFiles];
  while (toVisit.length > 0) {
    const fileName = toVisit.pop();
    if (reached.has(fileName)) continue;
    reached.add(fileName);
    const source = await readFile(new URL(fileName, PACKAGE_DIRECTORY), "utf8");
    for (const specifier of importSpecifiersOf(source)) {
      if (!specifier.startsWith("./")) {
        notRelative.push(`${fileName} imports ${specifier}`);
        continue;
      }
      const imported = specifier.slice(2);
      toVisit.push(imported);
      if (fileName.endsWith(".d.ts") && imported.endsWith(".js")) {
        toVisit.push(imported.replace(/\.js$/, ".d.ts"));
      }
    }
  }
  return { reached, notRelative };
}

test("the import walk sees a package and a node built-in", () => {
  assert.deepEqual(importSpecifiersOf('import { z } from "zod";\nexport * from "node:http";\nimport type { A } from "./a.js";'), [
    "zod",
    "./a.js",
    "node:http",
  ]);
});

test("the package depends on nothing, so the runtime unit's copy of it loads with no install", async () => {
  const packageJson = JSON.parse(await readFile(new URL("package.json", PACKAGE_DIRECTORY), "utf8"));
  for (const field of ["dependencies", "peerDependencies", "optionalDependencies"]) {
    assert.equal(packageJson[field], undefined, field);
  }
});

test("the entries a browser page loads import no package and no node built-in, through any file they reach", async () => {
  const packageJson = JSON.parse(await readFile(new URL("package.json", PACKAGE_DIRECTORY), "utf8"));
  const entryFiles = ENTRIES_A_BROWSER_PAGE_LOADS.flatMap((entry) => Object.values(packageJson.exports[entry])).map((file) => file.slice(2));
  const { reached, notRelative } = await whatTheEntriesReach(entryFiles);
  assert.deepEqual(notRelative, []);
  assert.ok(reached.has("control_contract_check.js") && reached.has("control_contract_schemas.js"), [...reached].join(", "));
  for (const fileName of reached) {
    assert.ok(packageJson.files.includes(fileName), `${fileName} is reached but not published`);
  }
});

test("the whole client imports no package either", async () => {
  const packageJson = JSON.parse(await readFile(new URL("package.json", PACKAGE_DIRECTORY), "utf8"));
  const { notRelative } = await whatTheEntriesReach(Object.values(packageJson.exports["."]).map((file) => file.slice(2)));
  assert.deepEqual(
    notRelative.filter((imported) => !/ imports node:/.test(imported)),
    [],
  );
});

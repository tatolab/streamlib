// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Golden-extraction tests for the import-and-enumerate processor extractor.
 *
 * Mirrors the Python `test_processor_extraction.py` and Rust
 * `golden_extraction_over_a_fixture_crate` shape: a fixture package with
 * several processors across several modules under `processors/` (plus a
 * non-processor module, a `_test.ts` module, and a module beside the
 * `streamlib.yaml` — all of which must be ignored), extracted by importing and
 * enumerating the registry rather than reading the manifest's `processors:`
 * list. Identity, execution mode, and ports are declared in code — the
 * decorator reads no `streamlib.yaml`.
 */

import { assert, assertEquals } from "@std/assert";
import { dirname, join } from "@std/path";

import {
  extractProcessorsFromDir,
  toManifestJson,
} from "./extract_processors.ts";
import { SchemaIdent } from "./schema_ident.ts";

function moduleHeader(): string {
  const decoratorsUrl = new URL("./decorators.ts", import.meta.url).href;
  const schemaIdentUrl = new URL("./schema_ident.ts", import.meta.url).href;
  return (
    `import { input, output, processor } from "${decoratorsUrl}";\n` +
    `import { SchemaIdent } from "${schemaIdentUrl}";\n`
  );
}

/** Write `body` at `relativePath` under `dir`, creating parent directories. */
async function writeFixtureModule(
  dir: string,
  relativePath: string,
  body: string,
): Promise<void> {
  const target = join(dir, ...relativePath.split("/"));
  await Deno.mkdir(dirname(target), { recursive: true });
  await Deno.writeTextFile(target, body);
}

// Three processors across three modules under `processors/` (one nested, to pin
// the recursive walk); a nested port declaration on one; a module that declares
// no processor; a `_test.ts` module and a module beside the manifest, both of
// which declare a processor that must NOT be discovered. No streamlib.yaml is
// needed — identity is declared in code, version-free.
async function makeFixturePackage(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "streamlib-extract-" });
  await writeFixtureModule(
    dir,
    "processors/blur.ts",
    moduleHeader() +
      `const VIDEO = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");\n` +
      `@processor("@tatolab/demo-pack/Blur", { execution: "reactive" })\n` +
      `export default class Blur {\n` +
      `  @input({ name: "frames_in", schema: VIDEO })\n` +
      `  handleIn() {}\n` +
      `  @output({ name: "frames_out", schema: VIDEO })\n` +
      `  handleOut() {}\n` +
      `}\n`,
  );
  await writeFixtureModule(
    dir,
    "processors/camera.ts",
    moduleHeader() +
      `@processor("@tatolab/demo-pack/Camera", { execution: "manual" })\n` +
      `export default class Camera {}\n`,
  );
  await writeFixtureModule(
    dir,
    "processors/nested/deep.ts",
    moduleHeader() +
      `@processor("@tatolab/demo-pack/Deep", { execution: "manual" })\n` +
      `export default class Deep {}\n`,
  );
  await writeFixtureModule(
    dir,
    "processors/not_a_processor.ts",
    `export class JustAHelper {}\n`,
  );
  await writeFixtureModule(
    dir,
    "processors/helper_test.ts",
    moduleHeader() +
      `@processor("@tatolab/demo-pack/TestOnly", { execution: "manual" })\n` +
      `export default class TestOnly {}\n`,
  );
  await writeFixtureModule(
    dir,
    "top_level.ts",
    moduleHeader() +
      `@processor("@tatolab/demo-pack/TopLevel", { execution: "manual" })\n` +
      `export default class TopLevel {}\n`,
  );
  return dir;
}

Deno.test("golden extraction over a fixture package", async () => {
  const dir = await makeFixturePackage();
  try {
    const procs = await extractProcessorsFromDir(dir);
    const names = procs.map((p) => p.shortName);
    // Deterministic: sorted by joined schema-ident string. `TopLevel` sits
    // beside the manifest and `TestOnly` in a `_test.ts` module — neither is a
    // processor module, so neither is discovered.
    assertEquals(names, ["Blur", "Camera", "Deep"]);

    const blur = procs.find((p) => p.shortName === "Blur")!;
    assert(blur.schemaIdent instanceof SchemaIdent);
    // Version-free identity: the extracted ident carries the 0.0.0 sentinel;
    // the concrete version is derived at package-build time (#1409).
    assertEquals(String(blur.schemaIdent), "@tatolab/demo-pack/Blur@0.0.0");
    assertEquals(blur.execution, "reactive");
    assertEquals(blur.inputs.map((port) => port.name), ["frames_in"]);
    assertEquals(blur.outputs.map((port) => port.name), ["frames_out"]);
    assertEquals(blur.inputs[0].schema!.type, "VideoFrame");

    const camera = procs.find((p) => p.shortName === "Camera")!;
    assertEquals(String(camera.schemaIdent), "@tatolab/demo-pack/Camera@0.0.0");
    assertEquals(camera.execution, "manual");
    assertEquals(camera.inputs.length, 0);

    const deep = procs.find((p) => p.shortName === "Deep")!;
    assertEquals(String(deep.schemaIdent), "@tatolab/demo-pack/Deep@0.0.0");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a module beside the manifest is not discovered", async () => {
  // `processors/` is the one discovery root: a processor authored beside the
  // `streamlib.yaml` is invisible, with no fallback to the old top-level glob.
  const dir = await Deno.makeTempDir({ prefix: "streamlib-extract-" });
  try {
    await writeFixtureModule(
      dir,
      "top_level.ts",
      moduleHeader() +
        `@processor("@tatolab/demo-pack/TopLevel", { execution: "manual" })\n` +
        `export default class TopLevel {}\n`,
    );
    await writeFixtureModule(
      dir,
      "processors/keep.ts",
      `export class JustAHelper {}\n`,
    );
    assertEquals((await extractProcessorsFromDir(dir)).length, 0);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("extractor emits manifest JSON pkg build consumes", async () => {
  const dir = await makeFixturePackage();
  try {
    const procs = await extractProcessorsFromDir(dir);
    const payload = JSON.parse(toManifestJson(procs)) as Array<{
      name: string;
      schema_ident: Record<string, string>;
      execution: unknown;
      scheduling: unknown;
      description: unknown;
      inputs: Array<{ name: string; schema: Record<string, string> | null }>;
    }>;
    assertEquals(payload.map((e) => e.name), ["Blur", "Camera", "Deep"]);
    const blur = payload.find((e) => e.name === "Blur")!;
    assertEquals(blur.schema_ident, {
      org: "tatolab",
      package: "demo-pack",
      type: "Blur",
      version: "0.0.0",
    });
    assertEquals(blur.execution, "reactive");
    assertEquals(blur.scheduling, null);
    assertEquals(blur.description, null);
    assertEquals(blur.inputs[0].name, "frames_in");
    assertEquals(blur.inputs[0].schema!.type, "VideoFrame");
    const camera = payload.find((e) => e.name === "Camera")!;
    assertEquals(camera.execution, "manual");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("repeated calls over the same dir are isolated", async () => {
  // Deno caches dynamic imports by URL: without forced re-evaluation the
  // second extraction over the same dir would re-run no decorators and return
  // []. The extractor must re-register per call and yield the same set.
  const dir = await makeFixturePackage();
  try {
    const first = (await extractProcessorsFromDir(dir)).map((p) => p.shortName);
    const second = (await extractProcessorsFromDir(dir)).map((p) =>
      p.shortName
    );
    assertEquals(first, ["Blur", "Camera", "Deep"]);
    assertEquals(second, first);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("package without a processors dir yields no processors", async () => {
  // A schema-only package declares no processors and has no `processors/` —
  // that is not an error.
  const dir = await Deno.makeTempDir({ prefix: "streamlib-extract-" });
  try {
    await Deno.writeTextFile(
      join(dir, "types.ts"),
      `export class JustAType {}\n`,
    );
    const procs = await extractProcessorsFromDir(dir);
    assertEquals(procs.length, 0);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

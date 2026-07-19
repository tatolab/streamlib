// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Golden-extraction tests for the import-and-enumerate processor extractor.
 *
 * Mirrors the Python `test_processor_extraction.py` and Rust
 * `golden_extraction_over_a_fixture_crate` shape: a fixture package with
 * several processors across several modules (plus a non-processor module that
 * must be ignored), extracted by importing and enumerating the registry rather
 * than reading the manifest's `processors:` list.
 */

import { assert, assertEquals } from "@std/assert";
import { join } from "@std/path";

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

async function makeFixturePackage(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "streamlib-extract-" });
  await Deno.writeTextFile(
    join(dir, "streamlib.yaml"),
    [
      "package:",
      "  org: tatolab",
      "  name: demo-pack",
      "  version: 0.2.0",
      "",
    ].join("\n"),
  );
  await Deno.writeTextFile(
    join(dir, "blur.ts"),
    moduleHeader() +
      `const VIDEO = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");\n` +
      `@processor("Blur", import.meta.url)\n` +
      `export default class Blur {\n` +
      `  @input({ name: "frames_in", schema: VIDEO })\n` +
      `  handleIn() {}\n` +
      `  @output({ name: "frames_out", schema: VIDEO })\n` +
      `  handleOut() {}\n` +
      `}\n`,
  );
  await Deno.writeTextFile(
    join(dir, "camera.ts"),
    moduleHeader() +
      `@processor("Camera", import.meta.url)\n` +
      `export default class Camera {}\n`,
  );
  await Deno.writeTextFile(
    join(dir, "not_a_processor.ts"),
    `export class JustAHelper {}\n`,
  );
  return dir;
}

Deno.test("golden extraction over a fixture package", async () => {
  const dir = await makeFixturePackage();
  try {
    const procs = await extractProcessorsFromDir(dir);
    const names = procs.map((p) => p.shortName);
    // Deterministic: sorted by joined schema-ident string.
    assertEquals(names, ["Blur", "Camera"]);

    const blur = procs.find((p) => p.shortName === "Blur")!;
    assert(blur.schemaIdent instanceof SchemaIdent);
    assertEquals(String(blur.schemaIdent), "@tatolab/demo-pack/Blur@0.2.0");
    assertEquals(blur.inputs.map((port) => port.name), ["frames_in"]);
    assertEquals(blur.outputs.map((port) => port.name), ["frames_out"]);
    assertEquals(blur.inputs[0].schema!.type, "VideoFrame");

    const camera = procs.find((p) => p.shortName === "Camera")!;
    assertEquals(String(camera.schemaIdent), "@tatolab/demo-pack/Camera@0.2.0");
    assertEquals(camera.inputs.length, 0);
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
      inputs: Array<{ name: string; schema: Record<string, string> | null }>;
    }>;
    assertEquals(payload.map((e) => e.name), ["Blur", "Camera"]);
    const blur = payload.find((e) => e.name === "Blur")!;
    assertEquals(blur.schema_ident, {
      org: "tatolab",
      package: "demo-pack",
      type: "Blur",
      version: "0.2.0",
    });
    assertEquals(blur.inputs[0].name, "frames_in");
    assertEquals(blur.inputs[0].schema!.type, "VideoFrame");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("schema-only package yields no processors", async () => {
  const dir = await Deno.makeTempDir({ prefix: "streamlib-extract-" });
  try {
    await Deno.writeTextFile(
      join(dir, "streamlib.yaml"),
      [
        "package:",
        "  org: tatolab",
        "  name: schema-only",
        "  version: 1.0.0",
        "",
      ].join("\n"),
    );
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

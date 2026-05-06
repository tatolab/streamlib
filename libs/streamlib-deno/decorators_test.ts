// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Tests for `@processor("PascalCase", import.meta.url)` and structured
 * `SchemaIdent` parity. Mirrors
 * `libs/streamlib-python/python/streamlib/tests/test_processor_decorator.py`
 * shape so a reviewer can diff them line-for-line.
 */

import {
  assert,
  assertEquals,
  assertThrows,
} from "@std/assert";
import { dirname, fromFileUrl, join } from "@std/path";

import { SchemaIdent } from "./schema_ident.ts";
import {
  input,
  output,
  type PortMetadata,
  processor,
  type StreamlibClassMetadata,
} from "./decorators.ts";

// =============================================================================
// SchemaIdent class
// =============================================================================

Deno.test("SchemaIdent constructs with valid segments", () => {
  const ident = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");
  assertEquals(ident.org, "tatolab");
  assertEquals(ident.package, "core");
  assertEquals(ident.type, "VideoFrame");
  assertEquals(ident.version, "1.0.0");
});

Deno.test("SchemaIdent toString renders joined form", () => {
  const ident = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");
  assertEquals(String(ident), "@tatolab/core/VideoFrame@1.0.0");
});

Deno.test("SchemaIdent toWireObject uses 'type' key", () => {
  const ident = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");
  assertEquals(ident.toWireObject(), {
    org: "tatolab",
    package: "core",
    type: "VideoFrame",
    version: "1.0.0",
  });
});

Deno.test("SchemaIdent is frozen", () => {
  const ident = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");
  assertThrows(
    () => {
      // deno-lint-ignore no-explicit-any
      (ident as any).org = "other";
    },
    TypeError,
  );
});

Deno.test("SchemaIdent rejects uppercase org", () => {
  assertThrows(
    () => new SchemaIdent("Tatolab", "core", "VideoFrame", "1.0.0"),
    Error,
    "invalid org",
  );
});

Deno.test("SchemaIdent rejects uppercase package", () => {
  assertThrows(
    () => new SchemaIdent("tatolab", "Core", "VideoFrame", "1.0.0"),
    Error,
    "invalid package",
  );
});

Deno.test("SchemaIdent rejects lowercase type", () => {
  assertThrows(
    () => new SchemaIdent("tatolab", "core", "videoFrame", "1.0.0"),
    Error,
    "invalid type",
  );
});

Deno.test("SchemaIdent rejects underscore in org", () => {
  assertThrows(
    () => new SchemaIdent("tato_lab", "core", "VideoFrame", "1.0.0"),
    Error,
    "invalid org",
  );
});

Deno.test("SchemaIdent rejects malformed version", () => {
  assertThrows(
    () => new SchemaIdent("tatolab", "core", "VideoFrame", "1.0"),
    Error,
    "invalid version",
  );
});

Deno.test("SchemaIdent accepts hyphen in package", () => {
  const ident = new SchemaIdent(
    "tatolab",
    "camera-deno-subprocess",
    "Foo",
    "0.1.0",
  );
  assertEquals(ident.package, "camera-deno-subprocess");
});

Deno.test("SchemaIdent.equals returns true on structural match", () => {
  const a = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");
  const b = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");
  assert(a.equals(b));
  const c = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.1");
  assert(!a.equals(c));
});

// =============================================================================
// @processor decorator — manifest-driven structured-ident emission
// =============================================================================
//
// Each test writes a `streamlib.yaml` + `<fixture>.ts` pair into a
// fresh tmp dir, then dynamic-imports the fixture and inspects its
// emitted class metadata. Mirrors `_import_class_from_dir` from the
// Python test suite.

interface FixturePaths {
  dir: string;
  manifestPath: string;
  modulePath: string;
}

async function makeFixture(
  manifestBody: string,
  moduleBody: string,
): Promise<FixturePaths> {
  const dir = await Deno.makeTempDir({ prefix: "streamlib-decorator-" });
  const manifestPath = join(dir, "streamlib.yaml");
  const modulePath = join(dir, "fixture.ts");
  await Deno.writeTextFile(manifestPath, manifestBody);
  await Deno.writeTextFile(modulePath, moduleBody);
  return { dir, manifestPath, modulePath };
}

const SDK_DIR = dirname(fromFileUrl(import.meta.url));

function moduleHeader(): string {
  const decoratorsUrl = new URL("./decorators.ts", import.meta.url).href;
  const schemaIdentUrl = new URL("./schema_ident.ts", import.meta.url).href;
  return (
    `import { input, output, processor } from "${decoratorsUrl}";\n` +
    `import { SchemaIdent } from "${schemaIdentUrl}";\n`
  );
}

Deno.test("@processor attaches structured SchemaIdent from manifest", async () => {
  const fixture = await makeFixture(
    [
      "package:",
      "  org: tatolab",
      "  name: cyberpunk-processor",
      "  version: 0.1.0",
      "",
      "processors:",
      "  - name: CyberpunkProcessor",
      "    runtime: deno",
      "    execution: reactive",
      "",
    ].join("\n"),
    moduleHeader() +
      `@processor("CyberpunkProcessor", import.meta.url)\n` +
      `export default class CyberpunkProcessor {}\n`,
  );
  try {
    const mod = await import(`file://${fixture.modulePath}`);
    const cls = mod.default as unknown as StreamlibClassMetadata;
    const ident = cls.streamlibSchemaIdent;
    assert(ident instanceof SchemaIdent);
    assertEquals(ident.org, "tatolab");
    assertEquals(ident.package, "cyberpunk-processor");
    assertEquals(ident.type, "CyberpunkProcessor");
    assertEquals(ident.version, "0.1.0");
    assertEquals(String(ident), "@tatolab/cyberpunk-processor/CyberpunkProcessor@0.1.0");
    assertEquals(cls.streamlibPorts.inputs.length, 0);
    assertEquals(cls.streamlibPorts.outputs.length, 0);
  } finally {
    await Deno.remove(fixture.dir, { recursive: true });
  }
});

Deno.test("@processor errors with expected path when manifest missing", async () => {
  const dir = await Deno.makeTempDir({ prefix: "streamlib-decorator-" });
  const modulePath = join(dir, "fixture.ts");
  // No streamlib.yaml in dir.
  await Deno.writeTextFile(
    modulePath,
    moduleHeader() +
      `@processor("Anything", import.meta.url)\n` +
      `export default class Anything {}\n`,
  );
  try {
    let caught: unknown = null;
    try {
      await import(`file://${modulePath}`);
    } catch (e) {
      caught = e;
    }
    assert(caught instanceof Error, "expected an error");
    const msg = (caught as Error).message;
    assert(
      msg.includes("streamlib.yaml not found"),
      `expected 'streamlib.yaml not found' in error, got: ${msg}`,
    );
    assert(
      msg.includes(join(dir, "streamlib.yaml")),
      `expected expected manifest path in error, got: ${msg}`,
    );
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("@processor short-name not in manifest lists available", async () => {
  const fixture = await makeFixture(
    [
      "package:",
      "  org: tatolab",
      "  name: example",
      "  version: 0.1.0",
      "",
      "processors:",
      "  - name: Camera",
      "  - name: Display",
      "",
    ].join("\n"),
    moduleHeader() +
      `@processor("MissingProcessor", import.meta.url)\n` +
      `export default class MissingProcessor {}\n`,
  );
  try {
    let caught: unknown = null;
    try {
      await import(`file://${fixture.modulePath}`);
    } catch (e) {
      caught = e;
    }
    assert(caught instanceof Error);
    const msg = (caught as Error).message;
    assert(msg.includes("Available processors"), msg);
    assert(msg.includes("Camera"), msg);
    assert(msg.includes("Display"), msg);
  } finally {
    await Deno.remove(fixture.dir, { recursive: true });
  }
});

Deno.test("@processor errors when manifest missing required package fields", async () => {
  const fixture = await makeFixture(
    [
      "package:",
      "  name: example",
      "  version: 0.1.0",
      "",
      "processors:",
      "  - name: Foo",
      "",
    ].join("\n"),
    moduleHeader() +
      `@processor("Foo", import.meta.url)\n` +
      `export default class Foo {}\n`,
  );
  try {
    let caught: unknown = null;
    try {
      await import(`file://${fixture.modulePath}`);
    } catch (e) {
      caught = e;
    }
    assert(caught instanceof Error);
    const msg = (caught as Error).message;
    assert(
      msg.includes("missing required `package:` field"),
      `expected missing-fields message, got: ${msg}`,
    );
    assert(msg.includes("org"), msg);
  } finally {
    await Deno.remove(fixture.dir, { recursive: true });
  }
});

Deno.test("@processor rejects non-string short name", () => {
  assertThrows(
    () => {
      // deno-lint-ignore no-explicit-any
      (processor as any)(123, "file:///dummy.ts");
    },
    TypeError,
    "PascalCase short name",
  );
});

Deno.test("@processor rejects missing module URL", () => {
  assertThrows(
    () => {
      // deno-lint-ignore no-explicit-any
      (processor as any)("Camera");
    },
    TypeError,
    "import.meta.url",
  );
});

// =============================================================================
// @input / @output schema validation
// =============================================================================

Deno.test("@input accepts a SchemaIdent instance", () => {
  const ident = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");

  class P {
    @input({ schema: ident })
    videoIn() {}
  }
  // deno-lint-ignore no-explicit-any
  const meta = (P.prototype as any).videoIn.streamlibInputPort as PortMetadata;
  assertEquals(meta.name, "videoIn");
  assert(meta.schema instanceof SchemaIdent);
  assert(meta.schema!.equals(ident));
});

Deno.test("@output accepts a class carrying streamlibSchemaIdent", () => {
  class MySchema {
    static streamlibSchemaIdent = new SchemaIdent(
      "tatolab",
      "core",
      "VideoFrame",
      "1.0.0",
    );
  }

  class P {
    @output({ schema: MySchema })
    videoOut() {}
  }
  // deno-lint-ignore no-explicit-any
  const meta = (P.prototype as any).videoOut.streamlibOutputPort as PortMetadata;
  assert(meta.schema instanceof SchemaIdent);
  assertEquals(meta.schema!.type, "VideoFrame");
});

Deno.test("@input rejects bare-string schema", () => {
  assertThrows(
    () => {
      class P {
        // deno-lint-ignore no-explicit-any
        @input({ schema: "VideoFrame" as any })
        videoIn() {}
      }
      // Force decorator evaluation via reference.
      void P;
    },
    TypeError,
    "string schema references are no longer accepted",
  );
});

Deno.test("@output rejects joined-string schema", () => {
  assertThrows(
    () => {
      class P {
        // deno-lint-ignore no-explicit-any
        @output({ schema: "@tatolab/core/VideoFrame@1.0.0" as any })
        videoOut() {}
      }
      void P;
    },
    TypeError,
    "string schema references",
  );
});

Deno.test("@input rejects class without schema metadata", () => {
  class Plain {}
  assertThrows(
    () => {
      class P {
        // deno-lint-ignore no-explicit-any
        @input({ schema: Plain as any })
        videoIn() {}
      }
      void P;
    },
    TypeError,
    "does not expose a structured",
  );
});

Deno.test("@input with no schema attaches null schema", () => {
  class P {
    @input()
    control() {}
  }
  // deno-lint-ignore no-explicit-any
  const meta = (P.prototype as any).control.streamlibInputPort as PortMetadata;
  assertEquals(meta.schema, null);
  assertEquals(meta.name, "control");
});

Deno.test("@input port name override", () => {
  class P {
    @input({ name: "video_in" })
    handler() {}
  }
  // deno-lint-ignore no-explicit-any
  const meta = (P.prototype as any).handler.streamlibInputPort as PortMetadata;
  assertEquals(meta.name, "video_in");
});

Deno.test("@processor collects @input + @output port metadata", async () => {
  const fixture = await makeFixture(
    [
      "package:",
      "  org: tatolab",
      "  name: ports-fixture",
      "  version: 0.1.0",
      "",
      "processors:",
      "  - name: PortsFixture",
      "    runtime: deno",
      "    execution: reactive",
      "",
    ].join("\n"),
    moduleHeader() +
      `const VIDEO = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");\n` +
      `@processor("PortsFixture", import.meta.url)\n` +
      `export default class PortsFixture {\n` +
      `  @input({ name: "video_in", schema: VIDEO, description: "trigger" })\n` +
      `  handleIn() {}\n` +
      `  @output({ name: "video_out", schema: VIDEO })\n` +
      `  handleOut() {}\n` +
      `}\n`,
  );
  try {
    const mod = await import(`file://${fixture.modulePath}`);
    const cls = mod.default as unknown as StreamlibClassMetadata;
    assertEquals(cls.streamlibPorts.inputs.length, 1);
    assertEquals(cls.streamlibPorts.outputs.length, 1);
    assertEquals(cls.streamlibPorts.inputs[0].name, "video_in");
    assertEquals(cls.streamlibPorts.inputs[0].description, "trigger");
    assert(cls.streamlibPorts.inputs[0].schema instanceof SchemaIdent);
    assertEquals(cls.streamlibPorts.inputs[0].schema!.type, "VideoFrame");
    assertEquals(cls.streamlibPorts.outputs[0].name, "video_out");
  } finally {
    await Deno.remove(fixture.dir, { recursive: true });
  }
});

// Anchor the SDK_DIR test — silences unused-var lint for the constant.
Deno.test("SDK_DIR is the directory holding decorators.ts", () => {
  const expected = fromFileUrl(new URL("./", import.meta.url));
  // join() normalizes trailing slashes; compare via dirname round-trip.
  assertEquals(SDK_DIR + "/", expected);
});

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Tests for `@processor("@org/package/Type", { execution: ... })` — identity,
 * mode, and ports declared in code (no decoration-time `streamlib.yaml` read).
 * Mirrors
 * `sdk/streamlib-python/python/streamlib/tests/test_processor_decorator.py`
 * shape so a reviewer can diff them section-for-section.
 */

import { assert, assertEquals, assertThrows } from "@std/assert";

import { SchemaIdent } from "./schema_ident.ts";
import { getRegisteredProcessors } from "./_processor_registry.ts";
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
// @processor decorator — in-code identity, version-free sentinel
// =============================================================================

Deno.test("@processor attaches structured SchemaIdent from code", () => {
  @processor("@tatolab/camera/Camera", { execution: "manual" })
  class Camera {}

  const ident = (Camera as unknown as StreamlibClassMetadata)
    .streamlibSchemaIdent;
  assert(ident instanceof SchemaIdent);
  assertEquals(ident.org, "tatolab");
  assertEquals(ident.package, "camera");
  assertEquals(ident.type, "Camera");
  // Version-free identity synthesizes the 0.0.0 sentinel; the concrete
  // version is derived at package-build time (#1409).
  assertEquals(ident.version, "0.0.0");
  assertEquals(String(ident), "@tatolab/camera/Camera@0.0.0");
});

Deno.test("@processor accepts hyphenated org and package", () => {
  @processor("@tatolab/camera-deno-subprocess/CyberpunkProcessor", {
    execution: "reactive",
  })
  class CyberpunkProcessor {}

  const ident = (CyberpunkProcessor as unknown as StreamlibClassMetadata)
    .streamlibSchemaIdent;
  assertEquals(ident.package, "camera-deno-subprocess");
  assertEquals(ident.type, "CyberpunkProcessor");
});

Deno.test("@processor omitted identity synthesizes @app/local", () => {
  // A bare module with no streamlib.yaml defines a working local processor:
  // identity synthesizes @app/local/<ClassName>.
  @processor({ execution: "reactive" })
  class LocalFilter {}

  const ident = (LocalFilter as unknown as StreamlibClassMetadata)
    .streamlibSchemaIdent;
  assertEquals(ident.org, "app");
  assertEquals(ident.package, "local");
  assertEquals(ident.type, "LocalFilter");
  assertEquals(ident.version, "0.0.0");
});

Deno.test("@processor app/local synth rejects non-PascalCase class name", () => {
  const decorate = processor({ execution: "reactive" });
  assertThrows(
    () => {
      class lowercaseName {}
      decorate(
        lowercaseName,
        { kind: "class", name: "lowercaseName" } as ClassDecoratorContext,
      );
    },
    Error,
    "cannot synthesize an `@app/local`",
  );
});

Deno.test("@processor rejects a versioned identity", () => {
  // The grammar is version-free (#1409): a hand-authored `@<version>` is
  // rejected. Mentally revert the version-free `parseIdentityStr` and this
  // passes when it must fail.
  assertThrows(
    () => {
      @processor("@tatolab/camera/Camera@1.0.0", { execution: "manual" })
      class Camera {}
      void Camera;
    },
    Error,
    "must be version-free",
  );
});

Deno.test("@processor rejects identity without @ prefix", () => {
  assertThrows(
    () => {
      @processor("tatolab/camera/Camera", { execution: "manual" })
      class Camera {}
      void Camera;
    },
    Error,
    "must start with `@`",
  );
});

Deno.test("@processor rejects identity with wrong segment count", () => {
  assertThrows(
    () => {
      @processor("@tatolab/Camera", { execution: "manual" })
      class Camera {}
      void Camera;
    },
    Error,
    "three `/`-separated segments",
  );
});

Deno.test("@processor rejects a non-string, non-options identity", () => {
  assertThrows(
    () => {
      // deno-lint-ignore no-explicit-any
      (processor as any)(123, { execution: "manual" });
    },
    TypeError,
    "identity string",
  );
});

// =============================================================================
// @processor decorator — execution + scheduling declared in code
// =============================================================================

Deno.test("@processor reactive execution is a bare string", () => {
  @processor("@tatolab/demo/Reactive", { execution: "reactive" })
  class Reactive {}

  assertEquals(
    (Reactive as unknown as StreamlibClassMetadata).streamlibExecution,
    "reactive",
  );
});

Deno.test("@processor manual execution is a bare string", () => {
  @processor("@tatolab/demo/Manual", { execution: "manual" })
  class Manual {}

  assertEquals(
    (Manual as unknown as StreamlibClassMetadata).streamlibExecution,
    "manual",
  );
});

Deno.test("@processor continuous execution carries interval", () => {
  @processor("@tatolab/demo/Loop", { execution: "continuous", intervalMs: 16 })
  class Loop {}

  assertEquals(
    (Loop as unknown as StreamlibClassMetadata).streamlibExecution,
    { type: "continuous", interval_ms: 16 },
  );
});

Deno.test("@processor continuous defaults interval to zero", () => {
  @processor("@tatolab/demo/Loop", { execution: "continuous" })
  class Loop {}

  assertEquals(
    (Loop as unknown as StreamlibClassMetadata).streamlibExecution,
    { type: "continuous", interval_ms: 0 },
  );
});

Deno.test("@processor execution is required", () => {
  assertThrows(
    () => {
      // deno-lint-ignore no-explicit-any
      (processor as any)("@tatolab/demo/NoMode");
    },
    TypeError,
    "requires an options object",
  );
});

Deno.test("@processor rejects unknown execution mode", () => {
  assertThrows(
    () => {
      // deno-lint-ignore no-explicit-any
      processor("@tatolab/demo/Bad", { execution: "sideways" as any });
    },
    Error,
    "invalid execution",
  );
});

Deno.test("@processor scheduling projects to priority mapping", () => {
  @processor("@tatolab/demo/Scheduled", {
    execution: "manual",
    scheduling: "high",
  })
  class Scheduled {}
  void Scheduled;

  const entry = getRegisteredProcessors().find(
    (e) => e.shortName === "Scheduled",
  )!;
  assertEquals(entry.scheduling, { priority: "high" });
});

Deno.test("@processor rejects unknown scheduling priority", () => {
  assertThrows(
    () => {
      processor("@tatolab/demo/Bad", {
        execution: "manual",
        // deno-lint-ignore no-explicit-any
        scheduling: "turbo" as any,
      });
    },
    Error,
    "invalid scheduling",
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

Deno.test("@processor collects @input + @output ports declared in code", () => {
  const VIDEO = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");

  @processor("@tatolab/demo/Ports", { execution: "reactive" })
  class Ports {
    @input({ name: "video_in", schema: VIDEO, description: "frames" })
    handleIn() {}
    @output({ name: "video_out", schema: VIDEO })
    handleOut() {}
  }

  const meta = (Ports as unknown as StreamlibClassMetadata).streamlibPorts;
  assertEquals(meta.inputs.map((p) => p.name), ["video_in"]);
  assertEquals(meta.outputs.map((p) => p.name), ["video_out"]);
  assertEquals(meta.inputs[0].description, "frames");
  assert(meta.inputs[0].schema instanceof SchemaIdent);
  assertEquals(meta.inputs[0].schema!.type, "VideoFrame");
});

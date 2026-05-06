// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Decorators for defining StreamLib processors in Deno.
 *
 * `@processor("PascalCase", import.meta.url)` mirrors Rust's
 * `#[streamlib::processor("Camera")]` proc-macro and Python's
 * `@processor("Camera")`: a positional PascalCase short name resolved
 * against the sibling `streamlib.yaml`'s `package: { org, name, version }`
 * block at decoration time. The result is a structured
 * {@linkcode SchemaIdent} attached to the class as
 * `streamlibSchemaIdent`.
 *
 * Deno has no `inspect.getfile` equivalent — TC39 stage-3 decorators
 * don't surface the source URL in the decorator context. The caller
 * passes `import.meta.url` explicitly as the second argument; the
 * decorator resolves it to a filesystem path and looks for a sibling
 * `streamlib.yaml`.
 *
 * Schema references in port declarations (`@input(...)` / `@output(...)`)
 * are cross-package by definition. The only accepted forms are a
 * {@linkcode SchemaIdent} instance or a class carrying
 * `streamlibSchemaIdent` as a static field. String forms (bare type
 * name or joined `@org/pkg/Type@v`) are rejected — schemas have no
 * shorthand. See `docs/architecture/schema-identity-and-packaging.md`.
 *
 * Timestamps
 * ----------
 *
 * For any timestamp that crosses the host/subprocess boundary or is
 * compared against another runtime's stamps — frame stamps, log
 * correlation, escalate request IDs, anything similar — use
 * `monotonicNowNs()` from `mod.ts`. It calls
 * `clock_gettime(CLOCK_MONOTONIC)`, the same kernel syscall the host
 * Rust runtime and the Python SDK make, so values share a system-wide
 * epoch and are directly comparable.
 *
 * Do NOT use `Date.now()` or `performance.now()` for cross-process
 * timestamps. Wall-clock APIs remain appropriate for ISO8601
 * formatting and other genuinely human-facing display.
 *
 * @module
 */

import { dirname, fromFileUrl, join } from "@std/path";

import {
  type ManifestSummary,
  ManifestParseError,
  readManifestSummary,
} from "./_manifest.ts";
import { SchemaIdent } from "./schema_ident.ts";

// =============================================================================
// Public types
// =============================================================================

/** Carrier shape for a class that has been processed by `@processor`. */
export interface StreamlibClassMetadata {
  readonly streamlibSchemaIdent: SchemaIdent;
  readonly streamlibPorts: {
    readonly inputs: readonly PortMetadata[];
    readonly outputs: readonly PortMetadata[];
  };
}

/** Per-port metadata attached by `@input` / `@output`. */
export interface PortMetadata {
  readonly name: string;
  readonly schema: SchemaIdent | null;
  readonly description: string;
}

/**
 * Schema carrier accepted by `@input` / `@output`.
 *
 * Either a {@linkcode SchemaIdent} instance, or a class that carries
 * `streamlibSchemaIdent` as a static field (produced by
 * `streamlib generate` from the package's JTD/YAML schemas).
 */
export type SchemaCarrier =
  | SchemaIdent
  | { streamlibSchemaIdent: SchemaIdent };

/** Options accepted by `input()` / `output()`. */
export interface PortOptions {
  /** Port name. Defaults to the method name. */
  name?: string;
  /** Structured schema carrier — strings are rejected. */
  schema?: SchemaCarrier | null;
  /** Human-readable description for introspection. */
  description?: string;
}

// =============================================================================
// Processor class decorator
// =============================================================================

// deno-lint-ignore no-explicit-any
type ClassConstructor = new (...args: any[]) => any;

/**
 * Mark a class as a StreamLib processor (PascalCase positional short
 * name).
 *
 * Mirrors Rust's `#[streamlib::processor("Camera")]` macro and Python's
 * `@processor("Camera")`. At decoration time, the decorator locates the
 * sibling `streamlib.yaml` (next to the file containing
 * `import.meta.url`), reads its `package: { org, name, version }` block,
 * validates that `shortName` appears in the manifest's `processors:`
 * list, and constructs a structured {@linkcode SchemaIdent} attached to
 * the class as `streamlibSchemaIdent`. Method-level port metadata
 * declared by {@linkcode input}/{@linkcode output} is collected onto
 * `streamlibPorts`.
 *
 * @param shortName PascalCase type name. Must match an entry in the
 *   sibling manifest's `processors:` list.
 * @param moduleUrl Pass `import.meta.url` from the file containing the
 *   decorated class. Required because Deno's TC39 decorator context
 *   does not expose the source URL.
 *
 * @example
 * ```ts
 * import { processor } from "<deno-streamlib>";
 *
 * @processor("CyberpunkProcessor", import.meta.url)
 * export default class CyberpunkProcessor { … }
 * ```
 *
 * Wire format and IPC always carry the full structured `SchemaIdent`;
 * the short-name positional is an authoring convenience for the
 * processor's own identity declaration only — schema references in
 * port declarations have no analogous shorthand and require a
 * structured carrier.
 */
export function processor(shortName: string, moduleUrl: string) {
  if (typeof shortName !== "string") {
    throw new TypeError(
      `@processor() takes a positional PascalCase short name (string); got ` +
        `${typeof shortName}. Pass the type name as the first argument: ` +
        `@processor("Camera", import.meta.url).`,
    );
  }
  if (typeof moduleUrl !== "string") {
    throw new TypeError(
      `@processor() requires the module's import.meta.url as the second ` +
        `argument; got ${typeof moduleUrl}. Pass import.meta.url explicitly: ` +
        `@processor("Camera", import.meta.url).`,
    );
  }

  return <T extends ClassConstructor>(
    target: T,
    _context: ClassDecoratorContext,
  ): T => {
    const manifestPath = locateSiblingManifest(moduleUrl, shortName);
    const summary = loadManifestSummary(manifestPath, shortName);

    if (!summary.processorNames.includes(shortName)) {
      const available = summary.processorNames.length > 0
        ? summary.processorNames.join("\n    ")
        : "(none declared)";
      throw new Error(
        `@processor(${JSON.stringify(shortName)}): short name not declared ` +
          `in ${manifestPath}'s \`processors:\` list. Available processors:\n    ` +
          `${available}`,
      );
    }

    const ident = new SchemaIdent(
      summary.package.org,
      summary.package.name,
      shortName,
      summary.package.version,
    );

    // Attach as static fields. Mirrors Python's
    // `cls.__streamlib_schema_ident__` / `cls.__streamlib_ports__`.
    Object.defineProperty(target, "streamlibSchemaIdent", {
      value: ident,
      writable: false,
      enumerable: true,
      configurable: false,
    });

    const inputs: PortMetadata[] = [];
    const outputs: PortMetadata[] = [];
    const prototype = target.prototype as Record<string, unknown>;
    if (prototype) {
      for (const key of Object.getOwnPropertyNames(prototype)) {
        const value = prototype[key];
        if (typeof value !== "function") continue;
        // deno-lint-ignore no-explicit-any
        const fn = value as any;
        if (fn.streamlibInputPort) inputs.push(fn.streamlibInputPort);
        if (fn.streamlibOutputPort) outputs.push(fn.streamlibOutputPort);
      }
    }
    Object.defineProperty(target, "streamlibPorts", {
      value: Object.freeze({
        inputs: Object.freeze(inputs),
        outputs: Object.freeze(outputs),
      }),
      writable: false,
      enumerable: true,
      configurable: false,
    });

    return target;
  };
}

// =============================================================================
// Port method decorators
// =============================================================================

/**
 * Mark a method as defining an input port.
 *
 * @example
 * ```ts
 * import { input } from "<deno-streamlib>";
 * import { VideoFrame } from "<deno-streamlib>/_generated_/tatolab__core/video_frame.ts";
 *
 * class MyProcessor {
 *   @input({ schema: VideoFrame, description: "RGB video input" })
 *   videoIn() {}
 * }
 * ```
 */
export function input(opts: PortOptions = {}) {
  return (
    // deno-lint-ignore ban-types
    target: Function,
    context: ClassMethodDecoratorContext,
  ): void => {
    if (context.kind !== "method") {
      throw new Error("@input must be applied to a method");
    }
    const portName = opts.name ??
      (typeof context.name === "string" ? context.name : String(context.name));
    // deno-lint-ignore no-explicit-any
    (target as any).streamlibInputPort = {
      name: portName,
      schema: resolveSchemaIdent(opts.schema ?? null),
      description: opts.description ?? "",
    } as PortMetadata;
  };
}

/**
 * Mark a method as defining an output port. Same shape as
 * {@linkcode input}.
 */
export function output(opts: PortOptions = {}) {
  return (
    // deno-lint-ignore ban-types
    target: Function,
    context: ClassMethodDecoratorContext,
  ): void => {
    if (context.kind !== "method") {
      throw new Error("@output must be applied to a method");
    }
    const portName = opts.name ??
      (typeof context.name === "string" ? context.name : String(context.name));
    // deno-lint-ignore no-explicit-any
    (target as any).streamlibOutputPort = {
      name: portName,
      schema: resolveSchemaIdent(opts.schema ?? null),
      description: opts.description ?? "",
    } as PortMetadata;
  };
}

// =============================================================================
// Internal helpers
// =============================================================================

function locateSiblingManifest(
  moduleUrl: string,
  shortName: string,
): string {
  if (!moduleUrl.startsWith("file://")) {
    throw new Error(
      `@processor(${JSON.stringify(shortName)}): module URL must be a ` +
        `file:// URL (got ${moduleUrl}). Pass import.meta.url from a ` +
        `regular .ts module file.`,
    );
  }
  const filePath = fromFileUrl(moduleUrl);
  return join(dirname(filePath), "streamlib.yaml");
}

function loadManifestSummary(
  manifestPath: string,
  shortName: string,
): ManifestSummary {
  try {
    return readManifestSummary(manifestPath);
  } catch (e) {
    if (e instanceof Deno.errors.NotFound) {
      throw new Error(
        `streamlib.yaml not found at ${manifestPath}. ` +
          `@processor(${JSON.stringify(shortName)}) requires a sibling ` +
          `streamlib.yaml with a \`package: { org, name, version }\` block ` +
          `and a matching \`processors:\` entry.`,
      );
    }
    if (e instanceof ManifestParseError) {
      throw e;
    }
    throw e;
  }
}

function resolveSchemaIdent(
  arg: SchemaCarrier | null | undefined,
): SchemaIdent | null {
  if (arg === null || arg === undefined) return null;
  if (arg instanceof SchemaIdent) return arg;
  if (typeof arg === "string") {
    throw new TypeError(
      `schema=${JSON.stringify(arg)}: string schema references are no ` +
        `longer accepted. Pass a structured \`SchemaIdent(org, package, type, version)\` ` +
        `instance instead. Joined-string forms like ` +
        `'@tatolab/core/VideoFrame@1.0.0' and bare type names like ` +
        `'VideoFrame' are both rejected — schemas are cross-package ` +
        `references by definition and have no shorthand. See ` +
        `docs/architecture/schema-identity-and-packaging.md.`,
    );
  }
  if (typeof arg === "function" || typeof arg === "object") {
    // deno-lint-ignore no-explicit-any
    const candidate = (arg as any).streamlibSchemaIdent;
    if (candidate instanceof SchemaIdent) return candidate;
    throw new TypeError(
      `schema=${
        // deno-lint-ignore no-explicit-any
        ((arg as any).name ?? String(arg))
      }: carrier does not expose a structured \`streamlibSchemaIdent\`. ` +
        `Import a codegen-emitted class from your generated bindings, or ` +
        `pass a \`SchemaIdent\` instance directly.`,
    );
  }
  throw new TypeError(
    `schema=${JSON.stringify(arg)}: unsupported type ${typeof arg}. Pass a ` +
      `\`SchemaIdent\` instance or a codegen-emitted schema class.`,
  );
}

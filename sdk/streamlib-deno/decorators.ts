// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Decorators for defining StreamLib processors in Deno.
 *
 * `@processor("@org/package/Type", { execution: ... })` mirrors Rust's
 * `#[processor("@org/package/Type", execution = ...)]` proc-macro
 * (`sdk/streamlib-processor-extract/src/grammar.rs`) and Python's
 * `@processor("@org/package/Type", execution=...)`: identity, execution mode,
 * and scheduling are declared **in code**, never read from a sibling
 * `streamlib.yaml` at decoration time. The identity string is **version-free**
 * (`@org/package/Type`, no `@version`) — a schema ref is an identity the runtime
 * binds version-blind, and the concrete version is derived at package-build
 * time, never hand-authored (#1409). The decorator synthesizes the `SemVer
 * 0.0.0` version-free sentinel and attaches a structured {@linkcode SchemaIdent}
 * to the class as `streamlibSchemaIdent`, registering the processor in the
 * module-global registry (`_processor_registry.ts`).
 *
 * Omitting the identity synthesizes `@app/local/<ClassName>` — a bare `.ts`
 * module with no `streamlib.yaml` defines a working local processor. Because
 * nothing is read from disk, the decorator no longer needs `import.meta.url`:
 * the second positional argument is gone.
 *
 * The decorator is the manifest truth-source for the `processors:` set: a
 * package's processors are derived by *importing* its modules and enumerating
 * what `@processor` registered — never by reading a hand-authored
 * `processors:` list, and never by reading `package:` identity out of
 * `streamlib.yaml`. This is the Deno analogue of the Rust `syn` source-scan in
 * `sdk/streamlib-processor-extract` (there the scan reads the AST without
 * running it; here extraction is import). See `extract_processors.ts`.
 *
 * Schema references in port declarations follow the two-door descriptor
 * model (`docs/architecture/zero-ceremony-authoring.md`). A port needs
 * **no** schema to move data: the wire is self-describing (msgpack named
 * maps / `Bag`), so send and receive work with zero type. When a port
 * *does* declare a schema — for validation, the visual builder, or
 * opt-in typed views — the reference is cross-package by definition, and
 * the only accepted forms are a {@linkcode SchemaIdent} instance or a
 * class carrying `streamlibSchemaIdent` as a static field (produced by
 * the opt-in `streamlib generate`). String forms (bare type name or
 * joined `@org/pkg/Type@v`) are rejected — schemas have no shorthand.
 * `streamlib generate` typed views are sugar consumed as data;
 * JTD-in-YAML remains the authored source for a shared vocabulary type.
 * See `docs/architecture/schema-identity-and-packaging.md`.
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

import { registerProcessor } from "./_processor_registry.ts";
import { SchemaIdent } from "./schema_ident.ts";

// =============================================================================
// Public types
// =============================================================================

/**
 * Manifest-shaped execution mode carried by the decorator. A bare string for
 * `reactive` / `manual`, or the `{ type: "continuous", interval_ms: N }`
 * mapping the Rust `ProcessorSchemaExecution` serializer emits for continuous.
 */
export type ExecutionSpec =
  | "reactive"
  | "manual"
  | { readonly type: "continuous"; readonly interval_ms: number };

/**
 * Manifest-shaped `scheduling:` block (`{ priority: <realtime|high|normal> }`),
 * or `null` when the processor declares no scheduling.
 */
export type SchedulingSpec = { readonly priority: string } | null;

/** Options declaring a processor's execution mode and scheduling in code. */
export interface ProcessorOptions {
  /**
   * `"reactive"`, `"manual"`, or `"continuous"`. Required — the execution
   * mode is authored in code, mirroring the Rust grammar.
   */
  readonly execution: "reactive" | "manual" | "continuous";
  /**
   * Minimum interval between `process()` calls in milliseconds, only
   * meaningful for `execution: "continuous"`. Defaults to `0`.
   */
  readonly intervalMs?: number;
  /** `"realtime"`, `"high"`, or `"normal"`; omit for the default. */
  readonly scheduling?: "realtime" | "high" | "normal";
  /** Human-readable processor description for introspection. */
  readonly description?: string;
}

/** Carrier shape for a class that has been processed by `@processor`. */
export interface StreamlibClassMetadata {
  readonly streamlibSchemaIdent: SchemaIdent;
  readonly streamlibExecution: ExecutionSpec;
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

// The version-free sentinel every code-declared identity carries. The concrete
// release version is derived at package-build time (#1409); the runtime schema
// registry stores and looks up unversioned, so `0.0.0` is an inert placeholder.
const VERSION_FREE_SENTINEL = "0.0.0";

const EXECUTION_MODES = ["reactive", "manual", "continuous"] as const;
const SCHEDULING_PRIORITIES = ["realtime", "high", "normal"] as const;

// =============================================================================
// Processor class decorator
// =============================================================================

// deno-lint-ignore no-explicit-any
type ClassConstructor = new (...args: any[]) => any;

/**
 * Mark a class as a StreamLib processor — identity and mode declared in code.
 *
 * Mirrors Rust's `#[processor("@org/package/Type", execution = ...)]` macro and
 * Python's `@processor("@org/package/Type", execution=...)`. Nothing is read
 * from disk: the version-free `@org/package/Type` identity, the execution mode,
 * and the scheduling priority all come from the arguments. The decorator
 * synthesizes the `0.0.0` version-free sentinel, attaches a structured
 * {@linkcode SchemaIdent} as `streamlibSchemaIdent`, and registers the
 * processor so the import-and-enumerate extractor can derive the package's
 * `processors:` set from code. Method-level port metadata declared by
 * {@linkcode input}/{@linkcode output} is collected onto `streamlibPorts`.
 *
 * @param identity Version-free `@org/package/Type` string. Omit (pass the
 *   options object as the sole argument) to synthesize `@app/local/<ClassName>`
 *   — a bare module with no `streamlib.yaml` still defines a working local
 *   processor.
 * @param options Execution mode (required), interval, scheduling, description.
 *
 * @example
 * ```ts
 * import { processor } from "streamlib";
 *
 * @processor("@tatolab/camera/Camera", { execution: "manual", scheduling: "high" })
 * export default class Camera { … }
 *
 * @processor({ execution: "reactive" }) // → @app/local/LocalFilter
 * export default class LocalFilter { … }
 * ```
 */
export function processor(
  identity: string,
  options: ProcessorOptions,
): <T extends ClassConstructor>(target: T, context: ClassDecoratorContext) => T;
export function processor(
  options: ProcessorOptions,
): <T extends ClassConstructor>(target: T, context: ClassDecoratorContext) => T;
export function processor(
  identityOrOptions: string | ProcessorOptions,
  maybeOptions?: ProcessorOptions,
) {
  let identity: string | null;
  let options: ProcessorOptions;
  if (typeof identityOrOptions === "string") {
    identity = identityOrOptions;
    if (maybeOptions === undefined) {
      throw new TypeError(
        `@processor(${JSON.stringify(identityOrOptions)}) requires an options ` +
          `object declaring the execution mode: ` +
          `@processor("@org/package/Type", { execution: "reactive" }).`,
      );
    }
    options = maybeOptions;
  } else if (
    identityOrOptions !== null && typeof identityOrOptions === "object"
  ) {
    identity = null;
    options = identityOrOptions;
  } else {
    throw new TypeError(
      `@processor() takes a version-free \`@org/package/Type\` identity string ` +
        `plus an options object, or just the options object (for ` +
        `\`@app/local/<ClassName>\`); got ${typeof identityOrOptions}.`,
    );
  }

  const executionSpec = normalizeExecution(options.execution, options.intervalMs);
  const schedulingSpec = normalizeScheduling(options.scheduling);
  const description = options.description ?? null;

  return <T extends ClassConstructor>(
    target: T,
    _context: ClassDecoratorContext,
  ): T => {
    const ident = resolveProcessorIdentity(identity, target);

    // Attach as static fields. Mirrors Python's
    // `cls.__streamlib_schema_ident__` / `cls.__streamlib_execution__` /
    // `cls.__streamlib_ports__`.
    Object.defineProperty(target, "streamlibSchemaIdent", {
      value: ident,
      writable: false,
      enumerable: true,
      configurable: false,
    });
    Object.defineProperty(target, "streamlibExecution", {
      value: executionSpec,
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

    registerProcessor({
      shortName: ident.type,
      schemaIdent: ident,
      execution: executionSpec,
      scheduling: schedulingSpec,
      description,
      inputs: Object.freeze(inputs.slice()),
      outputs: Object.freeze(outputs.slice()),
      className: target.name,
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
 * import { input } from "streamlib";
 * import { VideoFrame } from "./_generated_/tatolab__core/video_frame.ts";
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

/**
 * Version-free identity grammar: `@<org>/<package>/<Type>`, no trailing
 * `@<version>`. Mirrors Rust's `parse_schema_ident_str` in
 * `sdk/streamlib-processor-extract/src/grammar.rs`.
 */
const IDENTITY_PATTERN = /^@([^/@]+)\/([^/@]+)\/([^/@]+)$/;

/** Resolve the declared identity, or synthesize `@app/local/<ClassName>`. */
function resolveProcessorIdentity(
  identity: string | null,
  target: ClassConstructor,
): SchemaIdent {
  if (identity === null) {
    const typeName = target.name;
    try {
      return new SchemaIdent("app", "local", typeName, VERSION_FREE_SENTINEL);
    } catch (e) {
      throw new Error(
        `cannot synthesize an \`@app/local\` identity for class ` +
          `${JSON.stringify(typeName)}: ${e instanceof Error ? e.message : String(e)}. ` +
          `Declare an explicit \`@org/package/Type\` identity, or give the ` +
          `class a PascalCase name.`,
      );
    }
  }
  return parseIdentityStr(identity);
}

/**
 * Parse a version-free `@org/package/Type` string into a `SchemaIdent`.
 *
 * The grammar is version-free (#1409): a trailing `@<version>` is rejected —
 * a schema ref is an identity the runtime binds version-blind, and versions
 * are derived at package-build time. The synthesized `SchemaIdent` carries
 * the `0.0.0` version-free sentinel. Mirrors Rust's `parse_schema_ident_str`.
 */
function parseIdentityStr(raw: string): SchemaIdent {
  if (!raw.startsWith("@")) {
    throw new Error(
      `schema identity ${JSON.stringify(raw)} must start with \`@\` ` +
        `(e.g. \`@tatolab/core/VideoFrame\`)`,
    );
  }
  if (raw.slice(1).includes("@")) {
    throw new Error(
      `schema identity ${JSON.stringify(raw)} must be version-free ` +
        `\`@<org>/<package>/<Type>\` with no \`@<version>\` — a schema ref is ` +
        `an identity the runtime binds version-blind; versions are derived at ` +
        `package-build time, never hand-authored (#1409)`,
    );
  }
  const match = IDENTITY_PATTERN.exec(raw);
  if (match === null) {
    throw new Error(
      `schema identity ${JSON.stringify(raw)} must be ` +
        `\`@<org>/<package>/<Type>\` (exactly three \`/\`-separated segments)`,
    );
  }
  const [, org, pkg, type] = match;
  return new SchemaIdent(org, pkg, type, VERSION_FREE_SENTINEL);
}

/**
 * Project the `execution` / `intervalMs` options onto the manifest shape.
 *
 * `reactive` / `manual` render as bare strings; `continuous` renders as the
 * `{ type: "continuous", interval_ms: N }` mapping the Rust
 * `ProcessorSchemaExecution` serializer emits.
 */
function normalizeExecution(
  execution: string,
  intervalMs: number | undefined,
): ExecutionSpec {
  if (
    typeof execution !== "string" ||
    !(EXECUTION_MODES as readonly string[]).includes(execution)
  ) {
    throw new Error(
      `invalid execution ${JSON.stringify(execution)}: must be one of ` +
        `${EXECUTION_MODES.join(", ")}`,
    );
  }
  if (execution === "continuous") {
    const interval = intervalMs ?? 0;
    if (!Number.isInteger(interval) || interval < 0) {
      throw new Error(
        `invalid intervalMs ${JSON.stringify(intervalMs)}: must be a ` +
          `non-negative integer`,
      );
    }
    return { type: "continuous", interval_ms: interval };
  }
  return execution as "reactive" | "manual";
}

/** Project the `scheduling` option onto the manifest `{ priority }` shape. */
function normalizeScheduling(
  scheduling: string | undefined,
): SchedulingSpec {
  if (scheduling === undefined || scheduling === null) {
    return null;
  }
  if (
    typeof scheduling !== "string" ||
    !(SCHEDULING_PRIORITIES as readonly string[]).includes(scheduling)
  ) {
    throw new Error(
      `invalid scheduling ${JSON.stringify(scheduling)}: must be one of ` +
        `${SCHEDULING_PRIORITIES.join(", ")}`,
    );
  }
  return { priority: scheduling };
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

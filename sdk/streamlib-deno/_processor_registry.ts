// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Module-level registry the `@processor` decorator appends to.
 *
 * The decorator is the manifest truth-source: applying `@processor(...)` at
 * import time registers the processor's structured identity here, so a
 * downstream extractor can enumerate a package's processors by *importing* its
 * modules and reading this registry — never by trusting a hand-authored
 * `processors:` list in `streamlib.yaml`. This is the Deno analogue of the
 * Rust `syn` source-scan in `sdk/streamlib-processor-extract` and Python's
 * `_processor_registry`; there the scan reads the AST without running it, here
 * extraction *is* import.
 *
 * The registry is module-global and append-only during a normal import. An
 * extractor that wants a package's processors in isolation calls
 * {@linkcode clearRegisteredProcessors} before importing that package's
 * modules.
 *
 * @module
 */

import type { SchemaIdent } from "./schema_ident.ts";
import type { PortMetadata } from "./decorators.ts";

/**
 * One processor derived from a `@processor(...)` decorator at import time.
 *
 * Mirrors Rust's `streamlib_processor_extract::ExtractedProcessor` and
 * Python's `RegisteredProcessor`.
 */
export interface RegisteredProcessor {
  readonly shortName: string;
  readonly schemaIdent: SchemaIdent;
  readonly inputs: readonly PortMetadata[];
  readonly outputs: readonly PortMetadata[];
  readonly className: string;
}

const registeredProcessors: RegisteredProcessor[] = [];

/** Append a decorator-derived processor to the module-global registry. */
export function registerProcessor(entry: RegisteredProcessor): void {
  registeredProcessors.push(entry);
}

/** Snapshot the processors registered so far, in registration order. */
export function getRegisteredProcessors(): readonly RegisteredProcessor[] {
  return registeredProcessors.slice();
}

/**
 * Empty the registry.
 *
 * Used by the import-and-enumerate extractor to isolate one package's
 * processors from anything already imported in the same process.
 */
export function clearRegisteredProcessors(): void {
  registeredProcessors.length = 0;
}

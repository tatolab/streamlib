// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
// streamlib:lint-logging:allow-file — pkg-build subprocess CLI; emits the manifest JSON on stdout and usage/errors on stderr with no log pipeline installed

/**
 * Import-and-enumerate processor extractor for a Deno package directory.
 *
 * The Deno analogue of Rust's `streamlib_processor_extract` and Python's
 * `streamlib.extract_processors`: derive a package's `processors:` manifest
 * section from code rather than a hand-authored list. Where the Rust
 * capability parses source without running it, here extraction *is* import —
 * every processor module is dynamic-imported, which runs the `@processor`
 * decorators, which register into `_processor_registry.ts`; the registered set
 * is then emitted.
 *
 * Once the pkg-build truth-flip lands, `streamlib pkg build` will invoke this in
 * a fresh subprocess (`deno run --allow-read <this> <package_dir>`), read the
 * JSON on stdout, and write the manifest `processors:` section — the same shape
 * the Rust extractor feeds the catalog. Running in a fresh process guarantees an
 * empty registry to start; the in-process {@linkcode extractProcessorsFromDir}
 * entrypoint clears the registry and forces a fresh module evaluation per call,
 * so repeated calls (including over the same dir) stay isolated despite Deno
 * caching dynamic imports by URL.
 *
 * `processors/` is the discovery root, the polyglot analogue of the Rust
 * extractor's `src/`: every `*.ts` under `<packageDir>/processors/`, walked
 * recursively. A `*.ts` beside the `streamlib.yaml` is NOT a processor module,
 * and a package with no `processors/` directory yields no processors (a
 * schema-only package is legitimate). Test scaffolding is skipped — `*_test.ts`
 * and `*.test.ts` (both of `deno test`'s own conventions), `*.d.ts`, and any
 * `tests/` or `__tests__/` directory, the same skip set the Python extractor
 * applies. Each collected path is relative to the PACKAGE ROOT, so it is
 * exactly the module half of the `entrypoint:` a built manifest carries
 * (`processors/blur.ts:default`).
 *
 * The root governs DISCOVERY, not registration: a `@processor` declared in a
 * module outside `processors/` still registers if a discovered module imports
 * it, and the per-call isolation guarantee below covers only the modules under
 * `processors/` — a transitively-imported module stays in Deno's module cache
 * and its decorators do not re-run on a second call.
 *
 * Modules are imported in sorted path-segment order (matching Python's
 * `Path.parts` ordering, so both runtimes evaluate a nested tree in the same
 * sequence); the emitted list is then sorted by joined schema-ident codepoint
 * order, so output is deterministic regardless of import order and identical
 * across host locales.
 *
 * @module
 */

import { join, toFileUrl } from "@std/path";

import {
  clearRegisteredProcessors,
  getRegisteredProcessors,
  type RegisteredProcessor,
} from "./_processor_registry.ts";

/** Raised when a package directory cannot be scanned for processors. */
export class ProcessorExtractionError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProcessorExtractionError";
  }
}

let extractionGeneration = 0;

/**
 * The one directory, relative to the package root, processor modules are
 * discovered under. Mirrors the Rust extractor's `src/` root.
 */
const PROCESSOR_SOURCE_DIR_NAME = "processors";

/**
 * Directory names under `processors/` that hold test scaffolding, never
 * processor modules. Mirrors the Python extractor's directory skip set.
 */
const TEST_SCAFFOLDING_DIR_NAMES = ["tests", "__tests__"];

/**
 * Whether a `*.ts` under `processors/` is a module extraction should import.
 *
 * `deno test` collects BOTH `*_test.ts` and `*.test.ts`; a module matching
 * either is test scaffolding, and importing it would register its fixture
 * processors into the emitted manifest.
 */
function isExtractableProcessorModuleFile(fileName: string): boolean {
  if (!fileName.endsWith(".ts")) return false;
  if (fileName.endsWith(".d.ts")) return false;
  return !fileName.endsWith("_test.ts") && !fileName.endsWith(".test.ts");
}

/**
 * Collect every extractable `*.ts` under `dir`, as paths relative to the
 * PACKAGE ROOT (`processors/blur.ts`), recursing into subdirectories.
 */
function collectProcessorModuleRelativePaths(
  dir: string,
  relativePathPrefix: string,
  out: string[],
): void {
  for (const entry of Deno.readDirSync(dir)) {
    const relativePath = `${relativePathPrefix}/${entry.name}`;
    if (entry.isDirectory) {
      if (TEST_SCAFFOLDING_DIR_NAMES.includes(entry.name)) continue;
      collectProcessorModuleRelativePaths(
        join(dir, entry.name),
        relativePath,
        out,
      );
      continue;
    }
    if (!entry.isFile) continue;
    if (!isExtractableProcessorModuleFile(entry.name)) continue;
    out.push(relativePath);
  }
}

/**
 * Order two package-root-relative module paths by path segment, the ordering
 * Python's `Path.parts` sort produces — so a nested processor tree is imported
 * in the same sequence under both runtimes. Raw string order would not:
 * `.` (0x2E) sorts before `/` (0x2F), putting `nested.ts` ahead of
 * `nested/deep.ts` while Python puts the directory first.
 */
function compareProcessorModuleRelativePaths(left: string, right: string): number {
  const leftSegments = left.split("/");
  const rightSegments = right.split("/");
  const sharedDepth = Math.min(leftSegments.length, rightSegments.length);
  for (let depth = 0; depth < sharedDepth; depth++) {
    if (leftSegments[depth] === rightSegments[depth]) continue;
    return leftSegments[depth] < rightSegments[depth] ? -1 : 1;
  }
  return leftSegments.length - rightSegments.length;
}

/**
 * Import every module under `<packageDir>/processors/` and enumerate processors.
 *
 * Returns the processors registered by `@processor` during import, sorted by
 * joined schema-ident string. The registry is cleared first and every module is
 * re-evaluated under a per-call generation token, so repeated calls in one
 * process — including repeated calls over the same directory — are isolated. A
 * package with no `processors/` directory yields `[]` — a schema-only package
 * declares no processors.
 *
 * Throws {@linkcode ProcessorExtractionError} if `packageDir` is not a
 * directory.
 */
export async function extractProcessorsFromDir(
  packageDir: string,
): Promise<readonly RegisteredProcessor[]> {
  let stat: Deno.FileInfo;
  try {
    stat = Deno.statSync(packageDir);
  } catch {
    throw new ProcessorExtractionError(
      `not a directory: ${packageDir} — nothing to scan for processors`,
    );
  }
  if (!stat.isDirectory) {
    throw new ProcessorExtractionError(
      `not a directory: ${packageDir} — nothing to scan for processors`,
    );
  }

  clearRegisteredProcessors();

  const processorSourceDir = join(packageDir, PROCESSOR_SOURCE_DIR_NAME);
  let processorSourceDirIsPresent: boolean;
  try {
    processorSourceDirIsPresent = Deno.statSync(processorSourceDir).isDirectory;
  } catch {
    processorSourceDirIsPresent = false;
  }
  if (!processorSourceDirIsPresent) return [];

  const moduleRelativePaths: string[] = [];
  collectProcessorModuleRelativePaths(
    processorSourceDir,
    PROCESSOR_SOURCE_DIR_NAME,
    moduleRelativePaths,
  );
  moduleRelativePaths.sort(compareProcessorModuleRelativePaths);

  // Deno caches dynamic imports by URL, so a second call over the same dir
  // would re-import nothing and re-run no `@processor` decorators. Append a
  // per-call generation token to the module URL so each extraction forces a
  // fresh evaluation of the processor module and re-registers its processors.
  // Sibling relative imports (the SDK, the shared registry) drop the query and
  // resolve to their canonical URLs, so the registry stays a single instance.
  const generation = ++extractionGeneration;
  for (const relativePath of moduleRelativePaths) {
    const href = toFileUrl(join(packageDir, ...relativePath.split("/"))).href;
    await import(`${href}?streamlib_extract=${generation}`);
  }

  // Codepoint order, matching the Python and Rust extractors.
  // `String.localeCompare` would order by the host's ICU collation — a
  // machine-dependent, case-insensitive-at-the-primary-level result.
  const procs = getRegisteredProcessors().slice();
  procs.sort((a, b) => {
    const left = String(a.schemaIdent);
    const right = String(b.schemaIdent);
    return left < right ? -1 : left > right ? 1 : 0;
  });
  return procs;
}

/** Render extracted processors as the JSON `pkg build` consumes on stdout. */
export function toManifestJson(procs: readonly RegisteredProcessor[]): string {
  const payload = procs.map((entry) => ({
    name: entry.shortName,
    schema_ident: entry.schemaIdent.toWireObject(),
    execution: entry.execution,
    scheduling: entry.scheduling,
    description: entry.description,
    inputs: entry.inputs.map((port) => ({
      name: port.name,
      schema: port.schema === null ? null : port.schema.toWireObject(),
      description: port.description,
      delivery_profile: port.deliveryProfile,
    })),
    outputs: entry.outputs.map((port) => ({
      name: port.name,
      schema: port.schema === null ? null : port.schema.toWireObject(),
      description: port.description,
    })),
  }));
  return JSON.stringify(payload, null, 2);
}

/** CLI entrypoint: `deno run --allow-read extract_processors.ts <package_dir>`. */
export async function main(args: string[]): Promise<number> {
  if (args.length !== 1) {
    console.error(
      "usage: deno run --allow-read extract_processors.ts <package_dir>",
    );
    return 2;
  }
  let procs: readonly RegisteredProcessor[];
  try {
    procs = await extractProcessorsFromDir(args[0]);
  } catch (e) {
    console.error(e instanceof Error ? e.message : String(e));
    return 1;
  }
  console.log(toManifestJson(procs));
  return 0;
}

if (import.meta.main) {
  Deno.exit(await main(Deno.args));
}

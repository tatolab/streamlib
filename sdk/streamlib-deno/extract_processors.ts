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
 * every top-level module is dynamic-imported, which runs the `@processor`
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
 * Discovery matches the Rust scan's `collect_rs_files` + sort: every top-level
 * `*.ts` beside the `streamlib.yaml`, imported in sorted filename order (test
 * files are skipped). The emitted list is sorted by joined schema-ident string
 * so output is deterministic regardless of import order.
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
 * Import every top-level module under `packageDir` and enumerate processors.
 *
 * Returns the processors registered by `@processor` during import, sorted by
 * joined schema-ident string. The registry is cleared first and every module is
 * re-evaluated under a per-call generation token, so repeated calls in one
 * process — including repeated calls over the same directory — are isolated.
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

  const tsFiles: string[] = [];
  for (const entry of Deno.readDirSync(packageDir)) {
    if (!entry.isFile) continue;
    const name = entry.name;
    if (!name.endsWith(".ts")) continue;
    if (name.endsWith("_test.ts") || name.endsWith(".d.ts")) continue;
    tsFiles.push(name);
  }
  tsFiles.sort();

  clearRegisteredProcessors();
  // Deno caches dynamic imports by URL, so a second call over the same dir
  // would re-import nothing and re-run no `@processor` decorators. Append a
  // per-call generation token to the module URL so each extraction forces a
  // fresh evaluation of the top-level module and re-registers its processors.
  // Sibling relative imports (the SDK, the shared registry) drop the query and
  // resolve to their canonical URLs, so the registry stays a single instance.
  const generation = ++extractionGeneration;
  for (const name of tsFiles) {
    const href = toFileUrl(join(packageDir, name)).href;
    await import(`${href}?streamlib_extract=${generation}`);
  }

  const procs = getRegisteredProcessors().slice();
  procs.sort((a, b) =>
    String(a.schemaIdent).localeCompare(String(b.schemaIdent))
  );
  return procs;
}

/** Render extracted processors as the JSON `pkg build` consumes on stdout. */
export function toManifestJson(procs: readonly RegisteredProcessor[]): string {
  const payload = procs.map((entry) => ({
    name: entry.shortName,
    schema_ident: entry.schemaIdent.toWireObject(),
    inputs: entry.inputs.map((port) => ({
      name: port.name,
      schema: port.schema === null ? null : port.schema.toWireObject(),
      description: port.description,
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

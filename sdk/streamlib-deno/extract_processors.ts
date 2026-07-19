// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
 * `streamlib pkg build` invokes this in a fresh subprocess
 * (`deno run --allow-read <this> <package_dir>`), reads the JSON on stdout, and
 * writes the manifest `processors:` section — the same shape the Rust extractor
 * feeds the catalog. Running in a fresh process guarantees an empty registry to
 * start; the in-process {@linkcode extractProcessorsFromDir} entrypoint clears
 * the registry itself so it is safe to call repeatedly (over distinct dirs —
 * Deno caches dynamic imports by URL).
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

/**
 * Import every top-level module under `packageDir` and enumerate processors.
 *
 * Returns the processors registered by `@processor` during import, sorted by
 * joined schema-ident string. The registry is cleared first, so repeated calls
 * over distinct directories in one process are isolated.
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
  for (const name of tsFiles) {
    const href = toFileUrl(join(packageDir, name)).href;
    await import(href);
  }

  const procs = getRegisteredProcessors().slice();
  procs.sort((a, b) => String(a.schemaIdent).localeCompare(String(b.schemaIdent)));
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

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Decorator-side reader for `streamlib.yaml` package metadata.
 *
 * Reads only the top-level `package: { org, name, version, ... }` block — the
 * package identity the `@processor` decorator needs to compose a structured
 * `SchemaIdent`.
 *
 * The `processors:` list is NOT read here: the decorator is the truth-source
 * for which processors a package declares (extraction is import — see
 * `extract_processors.ts`). Only the package identity lives in the manifest;
 * the processor set is derived from `@processor` usage in code.
 *
 * The full manifest is parsed by the Rust runtime via `serde_yaml` when the
 * host loads it; this reader only needs the package identity fields.
 *
 * Uses the npm `yaml` parser (declared as a registry dependency) rather
 * than a `jsr:` import: the SDK is published to npm via `deno pack`, and
 * Deno cannot resolve `jsr:` specifiers from inside a published npm
 * package — so the wire-vocabulary deps must be plain npm / `node:`.
 *
 * @module
 */

import { parse as parseYaml } from "yaml";

/** Resolved `package:` block from streamlib.yaml. */
export interface ManifestPackage {
  readonly org: string;
  readonly name: string;
  readonly version: string;
}

/** Raised when a streamlib.yaml cannot be parsed for decorator use. */
export class ManifestParseError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ManifestParseError";
  }
}

/**
 * Parse the `package:` block from a streamlib.yaml at `path`.
 *
 * Throws `Deno.errors.NotFound` if the file does not exist, and
 * `ManifestParseError` if the manifest is missing `package:`, missing
 * any of `org`/`name`/`version`, or otherwise malformed.
 */
export function readPackageBlock(path: string): ManifestPackage {
  let text: string;
  try {
    text = Deno.readTextFileSync(path);
  } catch (e) {
    if (e instanceof Deno.errors.NotFound) {
      throw e;
    }
    throw new ManifestParseError(
      `failed to read ${path}: ${e instanceof Error ? e.message : String(e)}`,
    );
  }

  let parsed: unknown;
  try {
    parsed = parseYaml(text);
  } catch (e) {
    throw new ManifestParseError(
      `failed to parse ${path}: ${e instanceof Error ? e.message : String(e)}`,
    );
  }

  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new ManifestParseError(
      `streamlib.yaml at ${path} must be a YAML mapping at the top level`,
    );
  }
  const root = parsed as Record<string, unknown>;

  const pkgRaw = root["package"];
  if (
    pkgRaw === null || pkgRaw === undefined ||
    typeof pkgRaw !== "object" || Array.isArray(pkgRaw)
  ) {
    throw new ManifestParseError(
      `streamlib.yaml at ${path} is missing required \`package:\` block. ` +
        `The decorator requires \`package: { org, name, version }\` to ` +
        `construct a structured SchemaIdent.`,
    );
  }
  const pkg = pkgRaw as Record<string, unknown>;

  const org = pkg["org"];
  const name = pkg["name"];
  const version = pkg["version"];
  const missing: string[] = [];
  if (typeof org !== "string") missing.push("org");
  if (typeof name !== "string") missing.push("name");
  if (typeof version !== "string") missing.push("version");
  if (missing.length > 0) {
    throw new ManifestParseError(
      `streamlib.yaml at ${path} is missing required \`package:\` field(s): ` +
        `${missing.join(", ")}. The decorator requires ` +
        `\`package: { org, name, version }\` to construct a structured SchemaIdent.`,
    );
  }

  return {
    org: org as string,
    name: name as string,
    version: version as string,
  };
}

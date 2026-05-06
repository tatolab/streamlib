// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Structured schema identity for the streamlib Deno SDK.
 *
 * Mirrors Rust's `streamlib_idents::SchemaIdent` and Python's
 * `streamlib.schema_ident.SchemaIdent`: 4 validating fields (`org`,
 * `package`, `type`, `version`) constructed directly. There is no
 * parse / from-string API — joined-string representations like
 * `@org/pkg/Type@v` are render-only (`toString()`) and never round-trip
 * through a parser.
 *
 * Distinct from the wire-side `SchemaIdentEnvelope` /
 * `SchemaIdentSegments` in `subprocess_runner.ts`, which parse the
 * envelope arriving over inbound compiler IPC. This class is the
 * source-side authoring surface used by the `@processor` decorator and
 * by codegen-emitted classes that carry their identity as a static
 * field.
 *
 * @module
 */

// Validation patterns mirror Rust's `streamlib_idents` newtypes.
const ORG_PATTERN = /^[a-z][a-z0-9-]*$/;
const PACKAGE_PATTERN = /^[a-z][a-z0-9-]*$/;
const TYPE_PATTERN = /^[A-Z][A-Za-z0-9]*$/;
const VERSION_PATTERN = /^\d+\.\d+\.\d+$/;

/** IPC wire-format dict shape. The `type` key is unquoted on the wire. */
export interface SchemaIdentWire {
  org: string;
  package: string;
  type: string;
  version: string;
}

/**
 * Structured schema identifier — 4 fields, validated at construction,
 * frozen.
 */
export class SchemaIdent {
  readonly org: string;
  readonly package: string;
  readonly type: string;
  readonly version: string;

  constructor(org: string, pkg: string, type: string, version: string) {
    if (typeof org !== "string" || !ORG_PATTERN.test(org)) {
      throw new Error(
        `invalid org ${JSON.stringify(org)}: must match [a-z][a-z0-9-]*`,
      );
    }
    if (typeof pkg !== "string" || !PACKAGE_PATTERN.test(pkg)) {
      throw new Error(
        `invalid package ${JSON.stringify(pkg)}: must match [a-z][a-z0-9-]*`,
      );
    }
    if (typeof type !== "string" || !TYPE_PATTERN.test(type)) {
      throw new Error(
        `invalid type ${JSON.stringify(type)}: must match [A-Z][A-Za-z0-9]* (PascalCase)`,
      );
    }
    if (typeof version !== "string" || !VERSION_PATTERN.test(version)) {
      throw new Error(
        `invalid version ${JSON.stringify(version)}: must match major.minor.patch`,
      );
    }
    this.org = org;
    this.package = pkg;
    this.type = type;
    this.version = version;
    Object.freeze(this);
  }

  /** Render as the human-facing joined form `@org/package/Type@version`. */
  toString(): string {
    return `@${this.org}/${this.package}/${this.type}@${this.version}`;
  }

  /**
   * Render as the IPC wire-format dict.
   *
   * Matches the shape emitted by the Rust serializer
   * (`#[serde(rename = "type")]` on `SchemaIdent::r#type`) and the
   * `SchemaIdent.to_wire_dict()` method on the Python side.
   */
  toWireObject(): SchemaIdentWire {
    return {
      org: this.org,
      package: this.package,
      type: this.type,
      version: this.version,
    };
  }

  /** Structural equality across two idents. */
  equals(other: SchemaIdent): boolean {
    return (
      this.org === other.org &&
      this.package === other.package &&
      this.type === other.type &&
      this.version === other.version
    );
  }
}

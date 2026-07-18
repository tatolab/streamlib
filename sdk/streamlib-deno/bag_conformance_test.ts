// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Cross-language dynamic-bag wire conformance (issue #1407).
 *
 * The Deno SDK has no dedicated `Bag` type — `inputs.read` already returns a
 * native object and `outputs.write` accepts one, since a StreamLib payload is
 * a msgpack named map. This test proves that native object decodes the *same*
 * committed fixture bytes the Rust `Bag` and the Python dict do, across every
 * value class, and that a Deno-encoded equivalent is itself a decodable named
 * map (the write direction).
 *
 * The fixture is authored by Rust (source of truth) at
 * `sdk/streamlib-plugin-sdk/tests/fixtures/bag_conformance.msgpack` and read
 * identically by all three runtimes — a wire disagreement fails a test rather
 * than silently corrupting a payload.
 */

import { assert, assertEquals } from "@std/assert";
import * as msgpack from "@msgpack/msgpack";

// bag_conformance_test.ts is in sdk/streamlib-deno; the fixture is two levels
// up under sdk/streamlib-plugin-sdk.
const FIXTURE_URL = new URL(
  "../streamlib-plugin-sdk/tests/fixtures/bag_conformance.msgpack",
  import.meta.url,
);

/**
 * The canonical value-class-complete map every runtime's conformance test
 * mirrors. `blob` is a msgpack `bin` (Deno `Uint8Array`); everything else is
 * a plain JSON-shaped value.
 */
function canonical(): Record<string, unknown> {
  return {
    nil: null,
    flag: true,
    count: -7,
    big: 4_000_000_000,
    ratio: 1.5,
    name: "streamlib",
    list: [1, 2, 3],
    nested: { inner: "value" },
    blob: new Uint8Array([0xde, 0xad, 0xbe, 0xef]),
  };
}

Deno.test("fixture decodes to canonical values", () => {
  const raw = Deno.readFileSync(FIXTURE_URL);
  const decoded = msgpack.decode(raw) as Record<string, unknown>;
  assertEquals(decoded, canonical());
  // `blob` must arrive as `bin` (Uint8Array), never an array of numbers.
  assert(decoded.blob instanceof Uint8Array);
});

Deno.test("deno encoding is a decodable named map", () => {
  // The write direction: a Deno object encoded by @msgpack/msgpack round-trips
  // to the same logical map — i.e. it is a named map the Rust `Bag` and the
  // Python dict can read.
  const encoded = msgpack.encode(canonical());
  const decoded = msgpack.decode(encoded) as Record<string, unknown>;
  assertEquals(decoded, canonical());
});

Deno.test("tolerant missing and unexpected fields", () => {
  const raw = Deno.readFileSync(FIXTURE_URL);
  const decoded = msgpack.decode(raw) as Record<string, unknown>;
  // A field a consumer doesn't know about is simply present and ignorable.
  assert("nested" in decoded);
  // A field the producer never sent is a plain miss, not a throw.
  assertEquals(decoded.frame_rate ?? null, null);
});

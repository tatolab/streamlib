// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import { assert, assertEquals, assertThrows } from "@std/assert";
import {
  assertEngineCompatible,
  ENGINE_PROTOCOL_ENV,
  engineProtocolFromEnv,
  MIN_ENGINE_PROTOCOL,
  PROTOCOL_VERSION,
  ProtocolMismatchError,
} from "./_protocol.ts";

// The handshake exists so an incompatible installed SDK is refused before it
// runs, not deep in the FFI/escalate path. Mentally revert `assertEngineCompatible`
// to a no-op and the too-new / too-old assertions below go green for the wrong
// reason — so these lock the gate, not just exercise it.

Deno.test("PROTOCOL_VERSION and MIN are a coherent integer range", () => {
  assert(Number.isInteger(PROTOCOL_VERSION));
  assert(Number.isInteger(MIN_ENGINE_PROTOCOL));
  assert(MIN_ENGINE_PROTOCOL <= PROTOCOL_VERSION);
});

Deno.test("engineProtocolFromEnv reads the advertised version", () => {
  const prev = Deno.env.get(ENGINE_PROTOCOL_ENV);
  try {
    Deno.env.set(ENGINE_PROTOCOL_ENV, String(PROTOCOL_VERSION));
    assertEquals(engineProtocolFromEnv(), PROTOCOL_VERSION);
  } finally {
    if (prev === undefined) Deno.env.delete(ENGINE_PROTOCOL_ENV);
    else Deno.env.set(ENGINE_PROTOCOL_ENV, prev);
  }
});

Deno.test("engineProtocolFromEnv throws when the env var is unset", () => {
  const prev = Deno.env.get(ENGINE_PROTOCOL_ENV);
  try {
    Deno.env.delete(ENGINE_PROTOCOL_ENV);
    assertThrows(() => engineProtocolFromEnv(), ProtocolMismatchError);
  } finally {
    if (prev !== undefined) Deno.env.set(ENGINE_PROTOCOL_ENV, prev);
  }
});

Deno.test("engineProtocolFromEnv throws on a non-integer version", () => {
  const prev = Deno.env.get(ENGINE_PROTOCOL_ENV);
  try {
    Deno.env.set(ENGINE_PROTOCOL_ENV, "not-a-number");
    assertThrows(() => engineProtocolFromEnv(), ProtocolMismatchError);
  } finally {
    if (prev === undefined) Deno.env.delete(ENGINE_PROTOCOL_ENV);
    else Deno.env.set(ENGINE_PROTOCOL_ENV, prev);
  }
});

Deno.test("assertEngineCompatible accepts versions in range", () => {
  assertEngineCompatible(PROTOCOL_VERSION);
  assertEngineCompatible(MIN_ENGINE_PROTOCOL);
});

Deno.test("assertEngineCompatible refuses an engine newer than this SDK", () => {
  assertThrows(
    () => assertEngineCompatible(PROTOCOL_VERSION + 1),
    ProtocolMismatchError,
  );
});

Deno.test("assertEngineCompatible refuses an engine older than the minimum", () => {
  assertThrows(
    () => assertEngineCompatible(MIN_ENGINE_PROTOCOL - 1),
    ProtocolMismatchError,
  );
});

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Engine↔SDK subprocess protocol-version handshake.
 *
 * The single coordinate the engine and this SDK agree on so a
 * `@tatolab/streamlib-deno` resolved from a registry *by version* refuses to
 * run against an incompatible engine — instead of failing deep in the FFI /
 * escalate / lifecycle path with a cryptic crash. Mirrors the engine-side
 * `STREAMLIB_SUBPROCESS_PROTOCOL_VERSION`; covers the three lockstep runtime
 * surfaces (native-lib FFI symbol contract, escalate IPC schema,
 * stdin/stdout lifecycle-command protocol). This is the Deno-subprocess
 * analogue of the cdylib plugin ABI's `STREAMLIB_ABI_VERSION`, and the twin
 * of the Python SDK's `streamlib._protocol`.
 *
 * The contract is a **monotonic range, not strict equality** (the Cloudflare
 * `compatibility_date` shape): this SDK can speak any engine protocol in
 * `[MIN_ENGINE_PROTOCOL, PROTOCOL_VERSION]`, so a newer SDK keeps working
 * against a range of older engines. Bump `PROTOCOL_VERSION` (in lockstep with
 * the engine constant) when any of the three surfaces changes incompatibly;
 * raise `MIN_ENGINE_PROTOCOL` only when dropping support for an old engine
 * protocol.
 *
 * @module
 */

/** The subprocess protocol version this SDK implements. */
export const PROTOCOL_VERSION = 1;

/** Oldest engine protocol version this SDK can still speak. */
export const MIN_ENGINE_PROTOCOL = 1;

/** Env var the engine sets to advertise its protocol version to the subprocess. */
export const ENGINE_PROTOCOL_ENV = "STREAMLIB_PROTOCOL_VERSION";

/** The engine's subprocess protocol version is one this SDK can't speak. */
export class ProtocolMismatchError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProtocolMismatchError";
  }
}

/**
 * Read the engine's advertised protocol version from the environment.
 *
 * Throws [`ProtocolMismatchError`] when unset (the host doesn't speak the
 * streamlib subprocess protocol — an engine older than this SDK, or a process
 * not launched by streamlib) or non-integer.
 */
export function engineProtocolFromEnv(): number {
  const raw = Deno.env.get(ENGINE_PROTOCOL_ENV);
  if (!raw) {
    throw new ProtocolMismatchError(
      `${ENGINE_PROTOCOL_ENV} not set — the host does not speak the streamlib ` +
        `subprocess protocol (this SDK implements v${PROTOCOL_VERSION}). The ` +
        `engine is older than this streamlib, or this process was not launched ` +
        `by a streamlib runtime.`,
    );
  }
  const parsed = Number(raw);
  if (!Number.isInteger(parsed)) {
    throw new ProtocolMismatchError(
      `${ENGINE_PROTOCOL_ENV}=${JSON.stringify(raw)} is not an integer protocol version`,
    );
  }
  return parsed;
}

/** Fail loud when the engine's protocol version is outside this SDK's range. */
export function assertEngineCompatible(engineVersion: number): void {
  if (
    !(MIN_ENGINE_PROTOCOL <= engineVersion &&
      engineVersion <= PROTOCOL_VERSION)
  ) {
    throw new ProtocolMismatchError(
      `engine speaks subprocess protocol v${engineVersion}, this ` +
        `streamlib SDK speaks v${MIN_ENGINE_PROTOCOL}..v${PROTOCOL_VERSION}. ` +
        `The installed streamlib is incompatible with this engine — align ` +
        `the package's declared streamlib version to the engine's.`,
    );
  }
}

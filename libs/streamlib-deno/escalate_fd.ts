// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Length-prefixed JSON framing over a raw, inherited file descriptor.
 *
 * Used by `subprocess_runner.ts` to speak the escalate IPC protocol over
 * the dedicated `AF_UNIX` socketpair advertised by the host via
 * `STREAMLIB_ESCALATE_FD`. Previously the framing rode on
 * `Deno.stdin` / `Deno.stdout`; moving it off fd0/fd1 frees those for
 * host-side `intercepted` log capture (see #451).
 *
 * Deno has no stable API to wrap an existing fd as a duplex stream, so
 * we bind the libc `read(2)` / `write(2)` syscalls via FFI. Nonblocking
 * mode lets the calls run on Deno's FFI worker threads without stalling
 * the event loop.
 */

const LIBC_SYMBOLS = {
  read: {
    parameters: ["i32", "buffer", "usize"],
    result: "isize",
    nonblocking: true,
  },
  write: {
    parameters: ["i32", "buffer", "usize"],
    result: "isize",
    nonblocking: true,
  },
} as const;

type LibcHandle = Deno.DynamicLibrary<typeof LIBC_SYMBOLS>;

let _libc: LibcHandle | null = null;

function libcPath(): string {
  // Linux is the only supported target for subprocess-host polyglot
  // work today. macOS support will land with the macOS milestone; add
  // the `libc.dylib` branch then.
  if (Deno.build.os === "darwin") return "libc.dylib";
  return "libc.so.6";
}

function libc(): LibcHandle {
  if (_libc) return _libc;
  _libc = Deno.dlopen(libcPath(), LIBC_SYMBOLS);
  return _libc;
}

/**
 * Resolve the inherited escalate socketpair fd from
 * `STREAMLIB_ESCALATE_FD`. Throws when the env var is missing or
 * unparseable — the host always sets it, so a missing value indicates
 * a spawn-path regression.
 */
export function resolveEscalateFd(): number {
  const raw = Deno.env.get("STREAMLIB_ESCALATE_FD");
  if (!raw) {
    throw new Error(
      "STREAMLIB_ESCALATE_FD not set — escalate IPC transport unavailable",
    );
  }
  const fd = Number.parseInt(raw, 10);
  if (!Number.isFinite(fd) || fd < 0) {
    throw new Error(`STREAMLIB_ESCALATE_FD is not a valid fd: ${raw}`);
  }
  return fd;
}

async function readExact(
  fd: number,
  buf: Uint8Array<ArrayBuffer>,
): Promise<void> {
  const sym = libc().symbols.read;
  let offset = 0;
  while (offset < buf.byteLength) {
    const slice = buf.subarray(offset) as Uint8Array<ArrayBuffer>;
    const n = Number(await sym(fd, slice, BigInt(slice.byteLength)));
    if (n === 0) {
      throw new Error("escalate fd closed");
    }
    if (n < 0) {
      throw new Error(`escalate fd read failed (errno-style: ${n})`);
    }
    offset += n;
  }
}

async function writeAll(
  fd: number,
  buf: Uint8Array<ArrayBuffer>,
): Promise<void> {
  const sym = libc().symbols.write;
  let offset = 0;
  while (offset < buf.byteLength) {
    const slice = buf.subarray(offset) as Uint8Array<ArrayBuffer>;
    const n = Number(await sym(fd, slice, BigInt(slice.byteLength)));
    if (n < 0) {
      throw new Error(`escalate fd write failed (errno-style: ${n})`);
    }
    offset += n;
  }
}

/**
 * Read one length-prefixed JSON frame from `fd`. Throws on short read
 * (fd closed mid-frame) or malformed JSON.
 */
export async function readFrame(fd: number): Promise<Record<string, unknown>> {
  const lenBuf = new Uint8Array(new ArrayBuffer(4));
  await readExact(fd, lenBuf);
  const len = new DataView(lenBuf.buffer).getUint32(0, false);
  const msgBuf = new Uint8Array(new ArrayBuffer(len));
  await readExact(fd, msgBuf);
  const text = new TextDecoder().decode(msgBuf);
  return JSON.parse(text);
}

/**
 * Write one length-prefixed JSON frame to `fd`. The caller must hold
 * the shared write lock so concurrent writes can't interleave framing
 * bytes.
 */
export async function writeFrame(
  fd: number,
  msg: Record<string, unknown>,
): Promise<void> {
  const text = JSON.stringify(msg);
  const encodedRaw = new TextEncoder().encode(text);
  const encoded = new Uint8Array(new ArrayBuffer(encodedRaw.byteLength));
  encoded.set(encodedRaw);
  const lenBuf = new Uint8Array(new ArrayBuffer(4));
  new DataView(lenBuf.buffer).setUint32(0, encoded.byteLength, false);
  await writeAll(fd, lenBuf);
  await writeAll(fd, encoded);
}

/**
 * Close the libc FFI handle. Safe to call multiple times; used during
 * subprocess shutdown.
 */
export function closeLibcHandle(): void {
  if (_libc) {
    _libc.close();
    _libc = null;
  }
}

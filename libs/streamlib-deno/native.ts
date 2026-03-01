// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Deno FFI bindings to libstreamlib_deno_native.
 *
 * Wraps all `sldn_*` C ABI functions with TypeScript-friendly APIs.
 */

// FFI symbol definitions
const symbols = {
  // Context
  sldn_context_create: {
    parameters: ["buffer"] as const,
    result: "pointer" as const,
  },
  sldn_context_destroy: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  sldn_context_time_ns: {
    parameters: ["pointer"] as const,
    result: "i64" as const,
  },

  // Input
  sldn_input_subscribe: {
    parameters: ["pointer", "buffer"] as const,
    result: "i32" as const,
  },
  sldn_input_poll: {
    parameters: ["pointer"] as const,
    result: "i32" as const,
  },
  sldn_input_read: {
    parameters: ["pointer", "buffer", "pointer", "u32", "pointer", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_input_set_read_mode: {
    parameters: ["pointer", "buffer", "i32"] as const,
    result: "i32" as const,
  },

  // Output
  sldn_output_publish: {
    parameters: ["pointer", "buffer", "buffer", "buffer", "buffer"] as const,
    result: "i32" as const,
  },
  sldn_output_write: {
    parameters: ["pointer", "buffer", "pointer", "u32", "i64"] as const,
    result: "i32" as const,
  },

  // GPU Surface
  sldn_gpu_surface_lookup: {
    parameters: ["u32"] as const,
    result: "pointer" as const,
  },
  sldn_gpu_surface_lock: {
    parameters: ["pointer", "i32"] as const,
    result: "i32" as const,
  },
  sldn_gpu_surface_unlock: {
    parameters: ["pointer", "i32"] as const,
    result: "i32" as const,
  },
  sldn_gpu_surface_base_address: {
    parameters: ["pointer"] as const,
    result: "pointer" as const,
  },
  sldn_gpu_surface_width: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_height: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_bytes_per_row: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_create: {
    parameters: ["u32", "u32", "u32"] as const,
    result: "pointer" as const,
  },
  sldn_gpu_surface_get_id: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_release: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },

  // Broker XPC client
  sldn_broker_connect: {
    parameters: ["buffer"] as const,
    result: "pointer" as const,
  },
  sldn_broker_disconnect: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  sldn_broker_resolve_surface: {
    parameters: ["pointer", "buffer"] as const,
    result: "pointer" as const,
  },
  sldn_broker_acquire_surface: {
    parameters: ["pointer", "u32", "u32", "u32", "pointer", "u32"] as const,
    result: "pointer" as const,
  },
} as const;

export type NativeLib = Deno.DynamicLibrary<typeof symbols>;

/**
 * Load the native StreamLib Deno library.
 */
export function loadNativeLib(path: string): NativeLib {
  return Deno.dlopen(path, symbols);
}

/**
 * Encode a string to a null-terminated UTF-8 buffer.
 */
export function cString(str: string): Uint8Array {
  const encoder = new TextEncoder();
  const encoded = encoder.encode(str);
  const buf = new Uint8Array(encoded.length + 1);
  buf.set(encoded);
  buf[encoded.length] = 0; // null terminator
  return buf;
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Subprocess runner — spawned by the Rust DenoSubprocessHostProcessor.
 *
 * Reads lifecycle commands from stdin, manages the TypeScript processor
 * lifecycle, and uses FFI for direct iceoryx2 I/O.
 *
 * Environment variables:
 * - STREAMLIB_ENTRYPOINT: "module.ts:export" (e.g., "grayscale_processor.ts:default")
 * - STREAMLIB_PROJECT_PATH: Absolute path to Deno project directory
 * - STREAMLIB_NATIVE_LIB_PATH: Path to libstreamlib_deno_native.dylib
 * - STREAMLIB_PROCESSOR_ID: Unique processor ID
 * - STREAMLIB_EXECUTION_MODE: "reactive", "continuous", or "manual"
 */

import { loadNativeLib, cString, type NativeLib } from "./native.ts";
import { NativeProcessorContext } from "./context.ts";
import type {
  ReactiveProcessor,
  ContinuousProcessor,
  ManualProcessor,
  ProcessorLifecycle,
} from "./types.ts";

// ============================================================================
// Bridge protocol (length-prefixed JSON over stdin/stdout)
// ============================================================================

const stdin = Deno.stdin;
const stdout = Deno.stdout;

async function bridgeReadJson(): Promise<Record<string, unknown>> {
  const lenBuf = new Uint8Array(4);
  let bytesRead = 0;
  while (bytesRead < 4) {
    const n = await stdin.read(lenBuf.subarray(bytesRead));
    if (n === null) throw new Error("stdin closed");
    bytesRead += n;
  }

  const view = new DataView(lenBuf.buffer);
  const len = view.getUint32(0, false); // big-endian

  const msgBuf = new Uint8Array(len);
  bytesRead = 0;
  while (bytesRead < len) {
    const n = await stdin.read(msgBuf.subarray(bytesRead));
    if (n === null) throw new Error("stdin closed");
    bytesRead += n;
  }

  const text = new TextDecoder().decode(msgBuf);
  return JSON.parse(text);
}

async function bridgeSendJson(msg: Record<string, unknown>): Promise<void> {
  const text = JSON.stringify(msg);
  const encoded = new TextEncoder().encode(text);
  const lenBuf = new Uint8Array(4);
  const view = new DataView(lenBuf.buffer);
  view.setUint32(0, encoded.length, false); // big-endian

  await stdout.write(lenBuf);
  await stdout.write(encoded);
}

// ============================================================================
// Main subprocess loop
// ============================================================================

async function main(): Promise<void> {
  const entrypoint = Deno.env.get("STREAMLIB_ENTRYPOINT") ?? "";
  const projectPath = Deno.env.get("STREAMLIB_PROJECT_PATH") ?? "";
  const nativeLibPath = Deno.env.get("STREAMLIB_NATIVE_LIB_PATH") ?? "";
  const processorId = Deno.env.get("STREAMLIB_PROCESSOR_ID") ?? "unknown";
  const executionMode = Deno.env.get("STREAMLIB_EXECUTION_MODE") ?? "reactive";

  console.error(`[subprocess_runner:${processorId}] Starting`);
  console.error(`  entrypoint: ${entrypoint}`);
  console.error(`  project_path: ${projectPath}`);
  console.error(`  native_lib: ${nativeLibPath}`);
  console.error(`  execution_mode: ${executionMode}`);

  // Load native library
  let lib: NativeLib;
  try {
    lib = loadNativeLib(nativeLibPath);
  } catch (e) {
    console.error(`[subprocess_runner:${processorId}] Failed to load native lib: ${e}`);
    await bridgeSendJson({ rpc: "error", error: `Failed to load native lib: ${e}` });
    Deno.exit(1);
  }

  // Create native context
  const processorIdBuf = cString(processorId);
  const ctxPtr = lib.symbols.sldn_context_create(processorIdBuf);
  if (ctxPtr === null) {
    console.error(`[subprocess_runner:${processorId}] Failed to create native context`);
    await bridgeSendJson({ rpc: "error", error: "Failed to create native context" });
    Deno.exit(1);
  }

  // Connect to broker for surface resolution (if XPC service name is set)
  const xpcServiceName = Deno.env.get("STREAMLIB_XPC_SERVICE_NAME") ?? "";
  let brokerPtr: Deno.PointerObject | null = null;
  if (xpcServiceName) {
    const serviceNameBuf = cString(xpcServiceName);
    brokerPtr = lib.symbols.sldn_broker_connect(serviceNameBuf);
    if (brokerPtr === null) {
      console.error(`[subprocess_runner:${processorId}] Warning: broker connect failed for '${xpcServiceName}'`);
    } else {
      console.error(`[subprocess_runner:${processorId}] Connected to broker '${xpcServiceName}'`);
    }
  } else {
    console.error(`[subprocess_runner:${processorId}] No STREAMLIB_XPC_SERVICE_NAME set, broker resolution disabled`);
  }

  let processor: ProcessorLifecycle | null = null;
  let ctx: NativeProcessorContext | null = null;
  let running = false;

  // Command loop
  try {
    while (true) {
      const msg = await bridgeReadJson();
      const cmd = msg.cmd as string;

      switch (cmd) {
        case "setup": {
          const config = (msg.config as Record<string, unknown>) ?? {};
          const ports = (msg.ports as {
            inputs?: { name: string; service_name: string; read_mode?: string }[];
            outputs?: { name: string; dest_port: string; dest_service_name: string; schema_name: string }[];
          }) ?? { inputs: [], outputs: [] };

          // Subscribe to input iceoryx2 services
          const inputPorts = ports.inputs ?? [];
          for (const input of inputPorts) {
            const readMode = input.read_mode ?? "skip_to_latest";
            console.error(
              `[subprocess_runner:${processorId}] Subscribing to input: port='${input.name}', service='${input.service_name}', read_mode='${readMode}'`,
            );
            const result = lib.symbols.sldn_input_subscribe(
              ctxPtr,
              cString(input.service_name),
            );
            if (result !== 0) {
              console.error(
                `[subprocess_runner:${processorId}] Failed to subscribe to '${input.service_name}'`,
              );
            }
            // Configure per-port read mode (0 = skip_to_latest, 1 = read_next_in_order)
            const modeInt = readMode === "skip_to_latest" ? 0 : 1;
            lib.symbols.sldn_input_set_read_mode(ctxPtr, cString(input.name), modeInt);
          }

          // Create publishers for output iceoryx2 services
          const outputPorts = ports.outputs ?? [];
          for (const output of outputPorts) {
            console.error(
              `[subprocess_runner:${processorId}] Publishing to output: port='${output.name}', dest_port='${output.dest_port}', service='${output.dest_service_name}', schema='${output.schema_name}'`,
            );
            const result = lib.symbols.sldn_output_publish(
              ctxPtr,
              cString(output.dest_service_name),
              cString(output.name),
              cString(output.dest_port),
              cString(output.schema_name),
            );
            if (result !== 0) {
              console.error(
                `[subprocess_runner:${processorId}] Failed to create publisher for '${output.dest_service_name}'`,
              );
            }
          }

          // Parse entrypoint: "module.ts:export_name"
          const [modulePath, exportName] = parseEntrypoint(entrypoint);
          const fullModulePath = projectPath
            ? `${projectPath}/${modulePath}`
            : modulePath;

          console.error(
            `[subprocess_runner:${processorId}] Importing ${fullModulePath}:${exportName}`,
          );

          try {
            const module = await import(`file://${fullModulePath}`);
            const ProcessorClass = module[exportName];
            if (!ProcessorClass) {
              throw new Error(
                `Export '${exportName}' not found in ${fullModulePath}`,
              );
            }

            // Instantiate processor
            if (typeof ProcessorClass === "function") {
              processor = new ProcessorClass() as ProcessorLifecycle;
            } else {
              // Already an instance (default export of an object)
              processor = ProcessorClass as ProcessorLifecycle;
            }

            // Create context (with broker for surface resolution)
            ctx = new NativeProcessorContext(lib, ctxPtr, config, brokerPtr);

            // Call setup
            if (processor.setup) {
              await processor.setup(ctx);
            }

            await bridgeSendJson({ rpc: "ready" });
          } catch (e) {
            console.error(
              `[subprocess_runner:${processorId}] Setup failed: ${e}`,
            );
            await bridgeSendJson({ rpc: "error", error: String(e) });
          }
          break;
        }

        case "run": {
          if (!processor || !ctx) {
            console.error(
              `[subprocess_runner:${processorId}] run before setup`,
            );
            break;
          }

          running = true;
          console.error(
            `[subprocess_runner:${processorId}] Entering ${executionMode} loop`,
          );

          if (executionMode === "manual") {
            // Manual mode: start() returns, outer loop handles stop/teardown
            const manualProc = processor as ManualProcessor;
            try {
              await manualProc.start(ctx);
            } catch (e) {
              console.error(
                `[subprocess_runner:${processorId}] start() error: ${e}`,
              );
            }
            break;
          }

          // Reactive/continuous: the processing loop blocks the outer command
          // loop. Read stdin commands concurrently so teardown can be received
          // during processing. The processing loop yields at await points,
          // allowing the event loop to progress the stdin reader.
          let teardownReceived = false;
          const _stdinReader = (async () => {
            try {
              while (running) {
                const nextMsg = await bridgeReadJson();
                const nextCmd = nextMsg.cmd as string;
                if (nextCmd === "teardown") {
                  running = false;
                  teardownReceived = true;
                  return;
                }
                if (nextCmd === "stop") {
                  running = false;
                  try {
                    await bridgeSendJson({ rpc: "stopped" });
                  } catch {
                    // stdout may be closed
                  }
                  return;
                }
                if (nextCmd === "on_pause" && processor?.onPause && ctx) {
                  await processor.onPause(ctx);
                  await bridgeSendJson({ rpc: "ok" });
                } else if (nextCmd === "on_resume" && processor?.onResume && ctx) {
                  await processor.onResume(ctx);
                  await bridgeSendJson({ rpc: "ok" });
                } else if (nextCmd === "update_config" && processor?.updateConfig) {
                  const config = (nextMsg.config as Record<string, unknown>) ?? {};
                  await processor.updateConfig(config);
                  await bridgeSendJson({ rpc: "ok" });
                } else {
                  console.error(
                    `[subprocess_runner:${processorId}] Unknown command during run: ${nextCmd}`,
                  );
                }
              }
            } catch {
              // stdin closed (pipe broken) — treat as shutdown signal
              running = false;
              teardownReceived = true;
            }
          })();

          // Enter execution loop based on mode
          if (executionMode === "reactive") {
            const reactiveProc = processor as ReactiveProcessor;
            let pollCount = 0;
            let dataCount = 0;
            // Poll iceoryx2 via FFI, call process() on data
            while (running) {
              const hasData = lib.symbols.sldn_input_poll(ctxPtr);
              pollCount++;
              if (hasData === 1) {
                dataCount++;
                if (dataCount <= 3 || dataCount % 60 === 0) {
                  console.error(
                    `[subprocess_runner:${processorId}] poll: data received (frame #${dataCount})`,
                  );
                }
                try {
                  await reactiveProc.process(ctx);
                } catch (e) {
                  console.error(
                    `[subprocess_runner:${processorId}] process() error: ${e}`,
                  );
                }
              } else {
                if (pollCount === 100) {
                  console.error(
                    `[subprocess_runner:${processorId}] poll: no data after ${pollCount} polls (${dataCount} frames so far)`,
                  );
                }
                // No data, yield to event loop briefly
                await new Promise((resolve) => setTimeout(resolve, 1));
              }
            }
          } else if (executionMode === "continuous") {
            const continuousProc = processor as ContinuousProcessor;
            const intervalMs = (msg.interval_ms as number) ?? 0;
            while (running) {
              // Poll for any available input data
              lib.symbols.sldn_input_poll(ctxPtr);
              try {
                await continuousProc.process(ctx);
              } catch (e) {
                console.error(
                  `[subprocess_runner:${processorId}] process() error: ${e}`,
                );
              }
              if (intervalMs > 0) {
                await new Promise((resolve) =>
                  setTimeout(resolve, intervalMs),
                );
              } else {
                // Yield to event loop
                await new Promise((resolve) => setTimeout(resolve, 0));
              }
            }
          }

          // Processing loop exited because running = false (teardown or EOF)
          if (teardownReceived) {
            if (processor?.teardown && ctx) {
              try {
                await processor.teardown(ctx);
              } catch (e) {
                console.error(
                  `[subprocess_runner:${processorId}] teardown() error: ${e}`,
                );
              }
            }
            try {
              await bridgeSendJson({ rpc: "done" });
            } catch {
              // stdout may be closed if stdin EOF triggered shutdown
            }
            lib.symbols.sldn_context_destroy(ctxPtr);
            lib.close();
            Deno.exit(0);
          }
          break;
        }

        case "stop": {
          running = false;
          if (processor && ctx) {
            const manualProc = processor as ManualProcessor;
            if (manualProc.stop) {
              try {
                await manualProc.stop(ctx);
              } catch (e) {
                console.error(
                  `[subprocess_runner:${processorId}] stop() error: ${e}`,
                );
              }
            }
          }
          await bridgeSendJson({ rpc: "stopped" });
          break;
        }

        case "on_pause": {
          if (processor?.onPause && ctx) {
            await processor.onPause(ctx);
          }
          await bridgeSendJson({ rpc: "ok" });
          break;
        }

        case "on_resume": {
          if (processor?.onResume && ctx) {
            await processor.onResume(ctx);
          }
          await bridgeSendJson({ rpc: "ok" });
          break;
        }

        case "update_config": {
          if (processor?.updateConfig) {
            const config = (msg.config as Record<string, unknown>) ?? {};
            await processor.updateConfig(config);
          }
          await bridgeSendJson({ rpc: "ok" });
          break;
        }

        case "teardown": {
          running = false;
          if (processor?.teardown && ctx) {
            try {
              await processor.teardown(ctx);
            } catch (e) {
              console.error(
                `[subprocess_runner:${processorId}] teardown() error: ${e}`,
              );
            }
          }
          await bridgeSendJson({ rpc: "done" });

          // Cleanup native context
          lib.symbols.sldn_context_destroy(ctxPtr);
          lib.close();
          Deno.exit(0);
        }

        default: {
          console.error(
            `[subprocess_runner:${processorId}] Unknown command: ${cmd}`,
          );
          break;
        }
      }
    }
  } catch (e) {
    const isStdinClosed = e instanceof Error && e.message === "stdin closed";
    if (isStdinClosed) {
      console.error(`[subprocess_runner:${processorId}] stdin closed, shutting down`);
    } else {
      console.error(`[subprocess_runner:${processorId}] Fatal error: ${e}`);
    }
    if (processor?.teardown && ctx) {
      try {
        await processor.teardown(ctx);
      } catch {
        // ignore teardown errors during shutdown
      }
    }
    lib.symbols.sldn_context_destroy(ctxPtr);
    lib.close();
    Deno.exit(isStdinClosed ? 0 : 1);
  }
}

/**
 * Parse entrypoint string "module.ts:export" into [modulePath, exportName].
 */
function parseEntrypoint(entrypoint: string): [string, string] {
  const colonIdx = entrypoint.lastIndexOf(":");
  if (colonIdx === -1) {
    return [entrypoint, "default"];
  }
  return [entrypoint.substring(0, colonIdx), entrypoint.substring(colonIdx + 1)];
}

// Run
main();

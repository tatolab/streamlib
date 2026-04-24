// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
// streamlib:lint-logging:allow-file — subprocess bootstrap; writes to Deno.stderr before the log pipeline is installed

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

import { cString, loadNativeLib, type NativeLib } from "./native.ts";
import {
  computeReadBufBytes,
  NativeProcessorState,
  NativeRuntimeContextFullAccess,
  NativeRuntimeContextLimitedAccess,
} from "./context.ts";
import { EscalateChannel } from "./escalate.ts";
import {
  closeLibcHandle,
  readFrame as readEscalateFrame,
  resolveEscalateFd,
  writeFrame as writeEscalateFrame,
} from "./escalate_fd.ts";
import * as log from "./log.ts";
import type {
  ContinuousProcessor,
  ManualProcessor,
  ProcessorLifecycle,
  ReactiveProcessor,
} from "./types.ts";

/**
 * Pre-install fatal — escalate channel not up yet, so fall back to raw
 * stderr. The host's fd2 reader will still capture this line and surface
 * it as `intercepted=true, channel="fd2", source="deno"`.
 */
function fatalPreInstall(message: string): never {
  const text = `[streamlib-deno] ${message}\n`;
  try {
    Deno.stderr.writeSync(new TextEncoder().encode(text));
  } catch {
    // Even raw stderr broken; nothing else to try.
  }
  Deno.exit(1);
}

// ============================================================================
// Bridge protocol — length-prefixed JSON over the dedicated
// `STREAMLIB_ESCALATE_FD` socketpair. fd0/fd1 stay free for log capture
// (see #451).
// ============================================================================

let _escalateFd = -1;

function escalateFd(): number {
  if (_escalateFd < 0) _escalateFd = resolveEscalateFd();
  return _escalateFd;
}

async function bridgeReadJson(): Promise<Record<string, unknown>> {
  return await readEscalateFrame(escalateFd());
}

async function bridgeSendJson(msg: Record<string, unknown>): Promise<void> {
  // Serialize concurrent writes (lifecycle replies + escalate requests)
  // so the length prefix and payload aren't interleaved across async
  // tasks sharing the same fd.
  await writeLock;
  let release: () => void;
  writeLock = new Promise<void>((resolve) => {
    release = resolve;
  });
  try {
    await writeEscalateFrame(escalateFd(), msg);
  } finally {
    release!();
  }
}

let writeLock: Promise<void> = Promise.resolve();

/**
 * Validate that the wire-format capability field matches what the lifecycle
 * method expects. The Rust host is the source of truth; mismatches indicate
 * a wire-format drift bug and are logged but not fatal (belt-and-braces).
 */
function assertCapability(
  processorId: string,
  cmd: string,
  msg: Record<string, unknown>,
  expected: "full" | "limited",
): void {
  const actual = msg.capability as string | undefined;
  if (actual !== undefined && actual !== expected) {
    log.error("capability mismatch", {
      processor_id: processorId,
      cmd,
      expected,
      actual,
    });
  }
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

  if (!entrypoint) {
    fatalPreInstall("STREAMLIB_ENTRYPOINT not set");
  }

  // Escalate channel — requests from the TS processor go out on stdout,
  // host responses arrive on stdin and are routed here from every stdin
  // read site (outer loop + run-phase concurrent reader). Constructed
  // BEFORE installing logging so `log.install` has a channel to drain to.
  const escalateChannel = new EscalateChannel(bridgeSendJson);

  // Install unified logging: writer task + console / Deno.stdout/stderr
  // interceptors. After this point, `console.*` and `Deno.stdout.write`
  // route through `streamlib.log.*` with `intercepted: true`. fd1 is the
  // IPC channel and is NOT intercepted; raw fd1 writes would corrupt
  // bridge framing — see #444 / #451.
  log.setProcessorContext({ processorId });
  await log.install(escalateChannel);

  log.info("Subprocess runner starting", {
    processor_id: processorId,
    entrypoint,
    project_path: projectPath,
    native_lib: nativeLibPath,
    execution_mode: executionMode,
  });

  // Load native library
  let lib: NativeLib;
  try {
    lib = loadNativeLib(nativeLibPath);
  } catch (e) {
    log.error("Failed to load native lib", {
      processor_id: processorId,
      error: String(e),
    });
    await bridgeSendJson({ rpc: "error", error: `Failed to load native lib: ${e}` });
    closeLibcHandle();
    await log.shutdown();
    Deno.exit(1);
  }

  // Create native context
  const processorIdBuf = cString(processorId);
  const ctxPtr = lib.symbols.sldn_context_create(processorIdBuf);
  if (ctxPtr === null) {
    log.error("Failed to create native context", { processor_id: processorId });
    await bridgeSendJson({ rpc: "error", error: "Failed to create native context" });
    closeLibcHandle();
    await log.shutdown();
    Deno.exit(1);
  }

  // Connect to broker for surface resolution.
  //
  // macOS: STREAMLIB_XPC_SERVICE_NAME is the launchd mach-service name.
  // Linux: STREAMLIB_BROKER_SOCKET is the Unix-socket path the broker
  //        listens on. Both values funnel through the same FFI entry
  //        (`sldn_broker_connect`) — the native lib's platform-specific
  //        broker_macos / broker_linux module interprets the C string
  //        accordingly.
  const isDarwin = Deno.build.os === "darwin";
  const brokerEndpoint = isDarwin
    ? (Deno.env.get("STREAMLIB_XPC_SERVICE_NAME") ?? "")
    : (Deno.env.get("STREAMLIB_BROKER_SOCKET") ?? "");
  const brokerEndpointDesc = isDarwin ? "xpc_service_name" : "broker_socket";
  let brokerPtr: Deno.PointerObject | null = null;
  if (brokerEndpoint) {
    const endpointBuf = cString(brokerEndpoint);
    brokerPtr = lib.symbols.sldn_broker_connect(endpointBuf);
    if (brokerPtr === null) {
      log.warn("Broker connect failed", {
        endpoint_kind: brokerEndpointDesc,
        endpoint: brokerEndpoint,
      });
    } else {
      log.info("Connected to broker", {
        endpoint_kind: brokerEndpointDesc,
        endpoint: brokerEndpoint,
      });
    }
  } else {
    const envName = isDarwin ? "STREAMLIB_XPC_SERVICE_NAME" : "STREAMLIB_BROKER_SOCKET";
    log.info("Broker resolution disabled", { missing_env: envName });
  }

  let processor: ProcessorLifecycle | null = null;
  let state: NativeProcessorState | null = null;
  let fullCtx: NativeRuntimeContextFullAccess | null = null;
  let limitedCtx: NativeRuntimeContextLimitedAccess | null = null;
  let running = false;

  // Command loop
  try {
    while (true) {
      const msg = await bridgeReadJson();
      // Drop escalate responses at the outer level too — defensive: the
      // run-phase concurrent reader is normally the only site that sees
      // them, but a late-arriving response after `running` flips to
      // false should still be routed to the channel (it may reject a
      // pending request so subprocess teardown isn't blocked).
      if (escalateChannel.handleIncoming(msg)) {
        continue;
      }
      const cmd = msg.cmd as string;

      switch (cmd) {
        case "setup": {
          assertCapability(processorId, cmd, msg, "full");
          const config = (msg.config as Record<string, unknown>) ?? {};
          const ports = (msg.ports as {
            inputs?: {
              name: string;
              service_name: string;
              read_mode?: string;
              max_payload_bytes?: number;
            }[];
            outputs?: {
              name: string;
              dest_port: string;
              dest_service_name: string;
              schema_name: string;
              max_payload_bytes?: number;
            }[];
          }) ?? { inputs: [], outputs: [] };

          // Subscribe to input iceoryx2 services
          const inputPorts = ports.inputs ?? [];
          for (const input of inputPorts) {
            const readMode = input.read_mode ?? "skip_to_latest";
            log.info("Subscribing to input", {
              port: input.name,
              service: input.service_name,
              read_mode: readMode,
              max_payload_bytes: input.max_payload_bytes ?? null,
            });
            const result = lib.symbols.sldn_input_subscribe(
              ctxPtr,
              cString(input.service_name),
            );
            if (result !== 0) {
              log.error("Failed to subscribe to input", {
                service: input.service_name,
              });
            }
            // Configure per-port read mode (0 = skip_to_latest, 1 = read_next_in_order)
            const modeInt = readMode === "skip_to_latest" ? 0 : 1;
            lib.symbols.sldn_input_set_read_mode(ctxPtr, cString(input.name), modeInt);
          }

          // Create publishers for output iceoryx2 services
          const outputPorts = ports.outputs ?? [];
          for (const output of outputPorts) {
            log.info("Publishing to output", {
              port: output.name,
              dest_port: output.dest_port,
              service: output.dest_service_name,
              schema: output.schema_name,
            });
            const result = lib.symbols.sldn_output_publish(
              ctxPtr,
              cString(output.dest_service_name),
              cString(output.name),
              cString(output.dest_port),
              cString(output.schema_name),
              BigInt(output.max_payload_bytes ?? 65536),
            );
            if (result !== 0) {
              log.error("Failed to create publisher", {
                service: output.dest_service_name,
              });
            }
          }

          // Parse entrypoint: "module.ts:export_name"
          const [modulePath, exportName] = parseEntrypoint(entrypoint);
          const fullModulePath = projectPath
            ? `${projectPath}/${modulePath}`
            : modulePath;

          log.info("Importing processor module", {
            module: fullModulePath,
            export: exportName,
          });

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

            const readBufBytes = computeReadBufBytes(inputPorts);

            // Build shared FFI state and the two capability views once per
            // lifecycle. Each view wraps the same underlying FFI ctx, so
            // input/output ports and timing are shared; the capability split
            // is enforced purely by what each view exposes.
            state = new NativeProcessorState(
              lib,
              ctxPtr,
              config,
              brokerPtr,
              escalateChannel,
              readBufBytes,
            );
            fullCtx = new NativeRuntimeContextFullAccess(state);
            limitedCtx = new NativeRuntimeContextLimitedAccess(state);

            // setup() — privileged, receives full-access ctx
            if (processor.setup) {
              await processor.setup(fullCtx);
            }

            await bridgeSendJson({ rpc: "ready" });
          } catch (e) {
            log.error("Setup failed", { error: String(e) });
            await bridgeSendJson({ rpc: "error", error: String(e) });
          }
          break;
        }

        case "run": {
          if (!processor || !state || !fullCtx || !limitedCtx) {
            log.warn("run before setup");
            break;
          }

          running = true;
          log.info("Entering execution loop", { mode: executionMode });

          if (executionMode === "manual") {
            // Manual mode: start() is a resource-lifecycle op → full access
            const manualProc = processor as ManualProcessor;
            try {
              await manualProc.start(fullCtx);
            } catch (e) {
              log.error("start() error", { error: String(e) });
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
                // Demultiplex escalate responses out of the lifecycle path.
                if (escalateChannel.handleIncoming(nextMsg)) {
                  continue;
                }
                const nextCmd = nextMsg.cmd as string;
                if (nextCmd === "teardown") {
                  assertCapability(processorId, nextCmd, nextMsg, "full");
                  running = false;
                  teardownReceived = true;
                  return;
                }
                if (nextCmd === "stop") {
                  assertCapability(processorId, nextCmd, nextMsg, "full");
                  running = false;
                  try {
                    await bridgeSendJson({ rpc: "stopped" });
                  } catch {
                    // stdout may be closed
                  }
                  return;
                }
                if (nextCmd === "on_pause" && processor?.onPause && limitedCtx) {
                  assertCapability(processorId, nextCmd, nextMsg, "limited");
                  await processor.onPause(limitedCtx);
                  await bridgeSendJson({ rpc: "ok" });
                } else if (nextCmd === "on_resume" && processor?.onResume && limitedCtx) {
                  assertCapability(processorId, nextCmd, nextMsg, "limited");
                  await processor.onResume(limitedCtx);
                  await bridgeSendJson({ rpc: "ok" });
                } else if (nextCmd === "update_config" && processor?.updateConfig) {
                  const config = (nextMsg.config as Record<string, unknown>) ?? {};
                  await processor.updateConfig(config);
                  await bridgeSendJson({ rpc: "ok" });
                } else {
                  log.warn("Unknown command during run", { cmd: nextCmd });
                }
              }
            } catch {
              // stdin closed (pipe broken) — treat as shutdown signal
              running = false;
              teardownReceived = true;
            }
          })();

          // Enter execution loop based on mode. process() always receives
          // the limited ctx — no path in the hot loop can reach full access.
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
                  log.debug("poll: data received", { frame_index: dataCount });
                }
                try {
                  await reactiveProc.process(limitedCtx);
                } catch (e) {
                  log.error("process() error", { error: String(e) });
                }
              } else {
                if (pollCount === 100) {
                  log.debug("poll: no data", {
                    polls: pollCount,
                    frames_so_far: dataCount,
                  });
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
                await continuousProc.process(limitedCtx);
              } catch (e) {
                log.error("process() error", { error: String(e) });
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
            if (processor?.teardown && fullCtx) {
              try {
                await processor.teardown(fullCtx);
              } catch (e) {
                log.error("teardown() error", { error: String(e) });
              }
            }
            try {
              await bridgeSendJson({ rpc: "done" });
            } catch {
              // stdout may be closed if stdin EOF triggered shutdown
            }
            lib.symbols.sldn_context_destroy(ctxPtr);
            lib.close();
            closeLibcHandle();
            await log.shutdown();
            Deno.exit(0);
          }
          break;
        }

        case "stop": {
          assertCapability(processorId, cmd, msg, "full");
          running = false;
          if (processor && fullCtx) {
            const manualProc = processor as ManualProcessor;
            if (manualProc.stop) {
              try {
                await manualProc.stop(fullCtx);
              } catch (e) {
                log.error("stop() error", { error: String(e) });
              }
            }
          }
          await bridgeSendJson({ rpc: "stopped" });
          break;
        }

        case "on_pause": {
          assertCapability(processorId, cmd, msg, "limited");
          if (processor?.onPause && limitedCtx) {
            await processor.onPause(limitedCtx);
          }
          await bridgeSendJson({ rpc: "ok" });
          break;
        }

        case "on_resume": {
          assertCapability(processorId, cmd, msg, "limited");
          if (processor?.onResume && limitedCtx) {
            await processor.onResume(limitedCtx);
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
          assertCapability(processorId, cmd, msg, "full");
          running = false;
          if (processor?.teardown && fullCtx) {
            try {
              await processor.teardown(fullCtx);
            } catch (e) {
              log.error("teardown() error", { error: String(e) });
            }
          }
          await bridgeSendJson({ rpc: "done" });

          // Cleanup native context
          lib.symbols.sldn_context_destroy(ctxPtr);
          lib.close();
          closeLibcHandle();
          await log.shutdown();
          Deno.exit(0);
        }

        default: {
          log.warn("Unknown command", { cmd });
          break;
        }
      }
    }
  } catch (e) {
    const isEscalateClosed = e instanceof Error &&
      e.message === "escalate fd closed";
    if (isEscalateClosed) {
      log.info("escalate fd closed, shutting down");
    } else {
      log.error("Fatal error", { error: String(e) });
    }
    if (processor?.teardown && fullCtx) {
      try {
        await processor.teardown(fullCtx);
      } catch {
        // ignore teardown errors during shutdown
      }
    }
    escalateChannel.cancelAll("subprocess shutting down");
    lib.symbols.sldn_context_destroy(ctxPtr);
    lib.close();
    closeLibcHandle();
    await log.shutdown();
    Deno.exit(isEscalateClosed ? 0 : 1);
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

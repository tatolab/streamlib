// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot GPU escalation channel — subprocess → host IPC for Deno.
 *
 * Mirrors the Python SDK's `escalate.py`. The subprocess sees a limited
 * GPU capability; anything that needs the privileged surface routes
 * through the Rust host via an `escalate_request` on stdout. The host
 * runs the op inside `GpuContextLimitedAccess::escalate` and replies
 * with `escalate_response` on stdin.
 *
 * Wire types (`EscalateRequest`, `EscalateResponse`, …) are generated
 * from `schemas/com.streamlib.escalate_{request,response}@1.0.0.yaml`
 * via `cargo xtask generate-schemas --runtime typescript`. This file
 * owns only the channel coordination logic (request-id bookkeeping,
 * lifecycle-message deferral, promise plumbing).
 */

import type {
  EscalateRequest,
  EscalateRequestAcquirePixelBuffer,
  EscalateRequestAcquireTexture,
  EscalateRequestLog,
  EscalateRequestReleaseHandle,
  EscalateRequestRunCpuReadbackCopy,
  EscalateRequestTryRunCpuReadbackCopy,
} from "./_generated_/com_streamlib_escalate_request.ts";
import {
  EscalateRequestRunCpuReadbackCopyDirection,
  EscalateRequestTryRunCpuReadbackCopyDirection,
} from "./_generated_/com_streamlib_escalate_request.ts";
import type {
  EscalateResponse,
  EscalateResponseContended,
  EscalateResponseErr,
  EscalateResponseOk,
} from "./_generated_/com_streamlib_escalate_response.ts";

export type {
  EscalateRequest,
  EscalateRequestAcquirePixelBuffer,
  EscalateRequestAcquireTexture,
  EscalateRequestLog,
  EscalateRequestReleaseHandle,
  EscalateRequestRunCpuReadbackCopy,
  EscalateRequestTryRunCpuReadbackCopy,
  EscalateResponse,
  EscalateResponseContended,
  EscalateResponseErr,
  EscalateResponseOk,
};
export {
  EscalateRequestRunCpuReadbackCopyDirection,
  EscalateRequestTryRunCpuReadbackCopyDirection,
};

/** Backwards-compat alias for the `ok` variant of [`EscalateResponse`]. */
export type EscalateOkResponse = EscalateResponseOk;
/** Backwards-compat alias for the `err` variant of [`EscalateResponse`]. */
export type EscalateErrResponse = EscalateResponseErr;

export const ESCALATE_REQUEST_RPC = "escalate_request";
export const ESCALATE_RESPONSE_RPC = "escalate_response";

/**
 * Caller-facing payload — the discriminator variants of
 * [`EscalateRequest`] with `request_id` stripped. The channel injects
 * `request_id` when serializing onto the wire.
 */
export type EscalateOpPayload =
  | Omit<EscalateRequestAcquirePixelBuffer, "request_id">
  | Omit<EscalateRequestAcquireTexture, "request_id">
  | Omit<EscalateRequestReleaseHandle, "request_id">
  | Omit<EscalateRequestRunCpuReadbackCopy, "request_id">
  | Omit<EscalateRequestTryRunCpuReadbackCopy, "request_id">;

export class EscalateError extends Error {}

/** Sentinel resolved value for an escalate request that opted into the
 * contended-skip shape (e.g. `try_acquire_cpu_readback`) and that the
 * host responded to with `result: "contended"`. The
 * [`EscalateChannel.tryAcquireCpuReadback`] caller branches on
 * `null` vs. an [`EscalateOkResponse`] payload. */
export const ESCALATE_CONTENDED = null;

type Pending = {
  resolve: (value: EscalateOkResponse | null) => void;
  reject: (err: Error) => void;
  allowContended: boolean;
};

/**
 * Bidirectional escalate channel wired into the subprocess_runner's
 * stdin demux. Call `handleIncoming(msg)` from the central stdin
 * reader for every message tagged `rpc: "escalate_response"`.
 */
export class EscalateChannel {
  private pending = new Map<string, Pending>();
  private counter = 0;
  private writer: (msg: Record<string, unknown>) => Promise<void>;

  constructor(writer: (msg: Record<string, unknown>) => Promise<void>) {
    this.writer = writer;
  }

  async acquirePixelBuffer(
    width: number,
    height: number,
    format = "bgra",
  ): Promise<EscalateOkResponse> {
    return await this.request({
      op: "acquire_pixel_buffer",
      width,
      height,
      format,
    }) as EscalateOkResponse;
  }

  async acquireTexture(
    width: number,
    height: number,
    format: string,
    usage: readonly string[],
  ): Promise<EscalateOkResponse> {
    if (usage.length === 0) {
      throw new EscalateError("acquireTexture: usage must not be empty");
    }
    return await this.request({
      op: "acquire_texture",
      width,
      height,
      format,
      usage: [...usage],
    }) as EscalateOkResponse;
  }

  /**
   * Trigger the host-side cpu-readback copy for an already-registered
   * surface. `direction` selects `image_to_buffer` (host runs
   * `vkCmdCopyImageToBuffer`) or `buffer_to_image` (host runs the
   * inverse on write release). The host signals a new value on the
   * surface's shared timeline and returns the value in `timeline_value`
   * (decimal-string `u64`); the consumer waits on its imported
   * `ConsumerVulkanTimelineSemaphore` at that value before reading or
   * after writing the staging buffers' mapped bytes.
   */
  async runCpuReadbackCopy(
    surfaceId: bigint | number,
    direction: "image_to_buffer" | "buffer_to_image",
  ): Promise<EscalateOkResponse> {
    if (direction !== "image_to_buffer" && direction !== "buffer_to_image") {
      throw new EscalateError(
        `runCpuReadbackCopy: direction must be 'image_to_buffer' or 'buffer_to_image', got ${
          JSON.stringify(direction)
        }`,
      );
    }
    const wireDirection = direction === "image_to_buffer"
      ? EscalateRequestRunCpuReadbackCopyDirection.ImageToBuffer
      : EscalateRequestRunCpuReadbackCopyDirection.BufferToImage;
    return await this.request({
      op: "run_cpu_readback_copy",
      surface_id: typeof surfaceId === "bigint"
        ? surfaceId.toString()
        : Math.trunc(surfaceId).toString(),
      direction: wireDirection,
    }) as EscalateOkResponse;
  }

  /**
   * Non-blocking variant of [`runCpuReadbackCopy`]. Resolves to the
   * same `ok`-payload on success or to `null` on contention; rejects
   * on hard errors.
   */
  async tryRunCpuReadbackCopy(
    surfaceId: bigint | number,
    direction: "image_to_buffer" | "buffer_to_image",
  ): Promise<EscalateOkResponse | null> {
    if (direction !== "image_to_buffer" && direction !== "buffer_to_image") {
      throw new EscalateError(
        `tryRunCpuReadbackCopy: direction must be 'image_to_buffer' or 'buffer_to_image', got ${
          JSON.stringify(direction)
        }`,
      );
    }
    const wireDirection = direction === "image_to_buffer"
      ? EscalateRequestTryRunCpuReadbackCopyDirection.ImageToBuffer
      : EscalateRequestTryRunCpuReadbackCopyDirection.BufferToImage;
    return await this.request(
      {
        op: "try_run_cpu_readback_copy",
        surface_id: typeof surfaceId === "bigint"
          ? surfaceId.toString()
          : Math.trunc(surfaceId).toString(),
        direction: wireDirection,
      },
      { allowContended: true },
    );
  }

  async releaseHandle(handleId: string): Promise<EscalateOkResponse> {
    return await this.request({ op: "release_handle", handle_id: handleId }) as
      EscalateOkResponse;
  }

  /**
   * Send a fire-and-forget `log` op. The host enqueues the record into
   * the unified JSONL pipeline and returns nothing — no `request_id`,
   * no correlated response. The frame goes through the same writer lock
   * as request/response traffic so length-prefix and payload stay
   * contiguous on the wire.
   */
  async logFireAndForget(payload: EscalateRequestLog): Promise<void> {
    const msg = {
      rpc: ESCALATE_REQUEST_RPC,
      ...payload,
    } as Record<string, unknown>;
    await this.writer(msg);
  }

  async request(
    op: EscalateOpPayload,
    options: { allowContended?: boolean } = {},
  ): Promise<EscalateOkResponse | null> {
    const allowContended = options.allowContended === true;
    const requestId = this.nextRequestId();
    const msg = {
      rpc: ESCALATE_REQUEST_RPC,
      request_id: requestId,
      ...op,
    } as Record<string, unknown>;
    const promise = new Promise<EscalateOkResponse | null>(
      (resolve, reject) => {
        this.pending.set(requestId, { resolve, reject, allowContended });
      },
    );
    try {
      await this.writer(msg);
    } catch (e) {
      const p = this.pending.get(requestId);
      if (p) {
        this.pending.delete(requestId);
        p.reject(e as Error);
      }
    }
    return promise;
  }

  /**
   * Consume an escalate_response. Returns true if the message was
   * recognised as an escalate response (so the caller can skip
   * lifecycle dispatch for it).
   */
  handleIncoming(msg: Record<string, unknown>): boolean {
    if (msg.rpc !== ESCALATE_RESPONSE_RPC) return false;
    const requestId = msg.request_id as string | undefined;
    if (!requestId) return true; // malformed; eaten
    const pending = this.pending.get(requestId);
    if (!pending) return true;
    this.pending.delete(requestId);
    if (msg.result === "ok") {
      pending.resolve(msg as unknown as EscalateOkResponse);
    } else if (msg.result === "contended") {
      if (pending.allowContended) {
        pending.resolve(ESCALATE_CONTENDED);
      } else {
        pending.reject(
          new EscalateError(
            "escalate returned contended for an op that does not allow it",
          ),
        );
      }
    } else {
      const err = new EscalateError(
        (msg.message as string | undefined) ?? "escalate failed",
      );
      pending.reject(err);
    }
    return true;
  }

  /** Reject all in-flight requests (e.g. on shutdown). */
  cancelAll(reason = "subprocess shutting down"): void {
    for (const [id, pending] of this.pending.entries()) {
      pending.reject(new EscalateError(reason));
      this.pending.delete(id);
    }
  }

  private nextRequestId(): string {
    this.counter += 1;
    // Short correlation id is enough — request_id only has to be unique
    // within this subprocess's escalate channel.
    return `dn-${Date.now().toString(36)}-${this.counter}`;
  }
}

/**
 * Process-wide escalate channel singleton — mirror of Python's
 * `streamlib.escalate.channel()`. Subprocess runner installs it after
 * wiring the bridge stdio pipes; processor code (and SDK helpers like
 * `CpuReadbackContext.fromRuntime`) reach for it via `getChannel()`.
 *
 * Throws if the channel hasn't been installed — that only happens when
 * processor code runs outside the normal subprocess_runner lifecycle
 * (e.g. bare unit tests without a host).
 */
let _channelSingleton: EscalateChannel | null = null;

export function installChannel(channel: EscalateChannel): void {
  _channelSingleton = channel;
}

export function getChannel(): EscalateChannel {
  if (_channelSingleton === null) {
    throw new EscalateError(
      "escalate channel not installed — getChannel() is only available inside the subprocess lifecycle",
    );
  }
  return _channelSingleton;
}

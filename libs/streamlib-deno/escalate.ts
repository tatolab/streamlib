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
  EscalateRequestReleaseHandle,
} from "./_generated_/com_streamlib_escalate_request.ts";
import type {
  EscalateResponse,
  EscalateResponseErr,
  EscalateResponseOk,
} from "./_generated_/com_streamlib_escalate_response.ts";

export type {
  EscalateRequest,
  EscalateRequestAcquirePixelBuffer,
  EscalateRequestReleaseHandle,
  EscalateResponse,
  EscalateResponseErr,
  EscalateResponseOk,
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
  | Omit<EscalateRequestReleaseHandle, "request_id">;

export class EscalateError extends Error {}

type Pending = {
  resolve: (value: EscalateOkResponse) => void;
  reject: (err: Error) => void;
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
    return this.request({
      op: "acquire_pixel_buffer",
      width,
      height,
      format,
    });
  }

  async releaseHandle(handleId: string): Promise<EscalateOkResponse> {
    return this.request({ op: "release_handle", handle_id: handleId });
  }

  async request(op: EscalateOpPayload): Promise<EscalateOkResponse> {
    const requestId = this.nextRequestId();
    const msg = {
      rpc: ESCALATE_REQUEST_RPC,
      request_id: requestId,
      ...op,
    } as Record<string, unknown>;
    const promise = new Promise<EscalateOkResponse>((resolve, reject) => {
      this.pending.set(requestId, { resolve, reject });
    });
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

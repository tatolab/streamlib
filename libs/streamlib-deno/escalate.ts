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
 * The channel is designed to be multiplexed into the existing stdin
 * reader loop in `subprocess_runner.ts`. Outstanding requests are
 * keyed by `request_id`; `handleIncoming(msg)` consumes an escalate
 * response and resolves the corresponding promise.
 */

export const ESCALATE_REQUEST_RPC = "escalate_request";
export const ESCALATE_RESPONSE_RPC = "escalate_response";

export type EscalateOpPayload =
  | {
    op: "acquire_pixel_buffer";
    width: number;
    height: number;
    format: string;
  }
  | {
    op: "release_handle";
    handle_id: string;
  };

export interface EscalateOkResponse {
  result: "ok";
  request_id: string;
  handle_id: string;
  width?: number;
  height?: number;
  format?: string;
}

export interface EscalateErrResponse {
  result: "err";
  request_id: string;
  message: string;
}

export type EscalateResponse = EscalateOkResponse | EscalateErrResponse;

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

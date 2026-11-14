// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

import type { ControlRequestMessage } from "./control_messages.js";

/** The parts of an `AbortSignal` the client listens to; a DOM or Node `AbortSignal` is one. */
export interface ControlOperationAbortSignal {
  readonly aborted: boolean;
  readonly reason: unknown;
  addEventListener(type: "abort", listener: () => void, options?: { once?: boolean }): void;
  removeEventListener(type: "abort", listener: () => void): void;
}

/** How a transport waits for one answer. */
export interface ControlTransportSendOptions {
  /** Aborting it rejects the send with the signal's reason and drops a late answer. */
  signal?: ControlOperationAbortSignal;
}

/** Carries the client's JSON-RPC messages to a runtime and the runtime's back; the client checks each against the contract. */
export interface ControlTransport {
  /** Send one request and resolve the message that answers it, parsed with {@link controlMessageOfText}. */
  sendRequest(requestMessage: ControlRequestMessage, sendOptions?: ControlTransportSendOptions): Promise<unknown>;
  /** Hear each notification message the runtime sends, where the transport carries notifications; the returned function stops it. */
  onNotificationMessage?(listener: (notificationMessage: unknown) => void): () => void;
  /** Resolves once the transport has closed, by either side, where it holds a connection. */
  readonly closed?: Promise<void>;
  /** Close the transport; a request still unanswered rejects. */
  close(): Promise<void>;
}

/** How to reach a runtime's local API and who the client is. */
export interface ControlLocalApiSocketAddress {
  /** The runtime's local API socket. */
  socketPath: string;
  /** The client's name, the author of a change it sends with no author. */
  clientName: string;
}

/** One `POST /control` per request on the runtime's local API socket; it carries no notifications. */
export declare class ControlPostTransport implements ControlTransport {
  constructor(address: ControlLocalApiSocketAddress);
  sendRequest(requestMessage: ControlRequestMessage, sendOptions?: ControlTransportSendOptions): Promise<unknown>;
  close(): Promise<void>;
}

/** One WebSocket opened at `/control/ws` on the runtime's local API socket, one JSON-RPC message per text frame, carrying requests, answers and notifications. */
export declare class ControlWebSocketTransport implements ControlTransport {
  /** Open the WebSocket; aborting `signal` before it is open closes it and rejects with the signal's reason. */
  static open(address: ControlLocalApiSocketAddress & { signal?: ControlOperationAbortSignal }): Promise<ControlWebSocketTransport>;
  sendRequest(requestMessage: ControlRequestMessage, sendOptions?: ControlTransportSendOptions): Promise<unknown>;
  onNotificationMessage(listener: (notificationMessage: unknown) => void): () => void;
  readonly closed: Promise<void>;
  close(): Promise<void>;
}

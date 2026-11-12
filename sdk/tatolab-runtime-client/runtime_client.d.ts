// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import type {
  ControlNotificationMethodName,
  ControlNotifications,
  ControlOperationMethodName,
  ControlOperations,
  DiscoverResult,
} from "./control_contract.js";
import type { ControlNotificationInvalid } from "./control_messages.js";
import type { ControlOperationAbortSignal, ControlTransport } from "./control_transports.js";

/** How one call waits for its answer. */
export interface ControlCallOptions {
  /** Aborting it rejects the call with the signal's reason and drops a late answer. */
  signal?: ControlOperationAbortSignal;
}

/** How to reach a runtime's local API and who the client is. */
export interface TatolabRuntimeClientConnectOptions {
  /** The runtime's local API socket. */
  socketPath: string;
  /** The client's name, the author of a change it sends with no author. */
  clientName: string;
  /** Aborting it before the connection is open closes it and rejects with the signal's reason; it has no effect after. */
  signal?: ControlOperationAbortSignal;
}

/** The calls every generated method makes: each request is checked against the contract before it is sent, and each answer and notification before it is handed on. */
export declare class TatolabRuntimeClientCallsOverATransport {
  /** A client over `transport`, which it closes on {@link close}. */
  constructor(transport: ControlTransport);
  /** What `discover` answered, when the client was opened with `connect`. */
  discovered: DiscoverResult;
  /** Call one operation: rejects with `ControlRequestInvalid` before sending params that do not match the contract, `ControlOperationError` for the runtime's refusal or failure, and `ControlAnswerInvalid` for an answer that does not match. */
  call<Method extends ControlOperationMethodName>(
    method: Method,
    params: ControlOperations[Method]["params"],
    callOptions?: ControlCallOptions,
  ): Promise<ControlOperations[Method]["result"]>;
  /** Hear each notification of `method` checked against the contract; the returned function stops it. Throws when the transport carries no notifications. */
  onNotification<Method extends ControlNotificationMethodName>(
    method: Method,
    listener: (params: ControlNotifications[Method]) => void,
  ): () => void;
  /** Hear each notification that did not match the contract, which no listener heard; the returned function stops it. */
  onNotificationInvalid(listener: (notificationInvalid: ControlNotificationInvalid) => void): () => void;
  /** Resolves once the transport has closed, by either side; never, for a transport that holds no connection. */
  readonly closed: Promise<void>;
  /** Close the transport; a call still unanswered rejects. */
  close(): Promise<void>;
}

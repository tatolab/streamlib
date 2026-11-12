// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import type {
  ControlNotificationMethodName,
  ControlNotifications,
  ControlOperationMethodName,
  ControlOperations,
  DiscoverResult,
  ProjectActionRefusalOnTheWire,
} from "./control_contract.js";

/** The parts of an `AbortSignal` the client listens to; a DOM or Node `AbortSignal` is one. */
export interface ControlOperationAbortSignal {
  readonly aborted: boolean;
  readonly reason: unknown;
  addEventListener(type: "abort", listener: () => void, options?: { once?: boolean }): void;
  removeEventListener(type: "abort", listener: () => void): void;
}

/** The source text `JSON.parse` hands a reviver beside a primitive value. */
export interface ControlOperationResultReviverContext {
  readonly source?: string;
}

/** How one call waits and reads its answer. */
export interface ControlOperationCallOptions {
  /** Aborting it rejects the call with the signal's reason and drops a late answer. */
  signal?: ControlOperationAbortSignal;
  /** Parses the answer with this `JSON.parse` reviver, so a caller can keep a number's digits as written; each value in the result is what the reviver returned for it. */
  resultReviver?: (key: string, value: unknown, context?: ControlOperationResultReviverContext) => unknown;
}

/** How to reach a runtime's local API and who the client is. */
export interface TatolabRuntimeControlClientConnectOptions {
  /** The runtime's local API socket. */
  socketPath: string;
  /** The client's name, the author of a change it sends with no author. */
  clientName: string;
  /** Aborting it before the connection is open closes it and rejects with the signal's reason; it has no effect after. */
  signal?: ControlOperationAbortSignal;
}

/** A JSON-RPC error the runtime answered a call with, in the runtime's own words. */
export declare class ControlOperationError extends Error {
  /** The method the call was to. */
  readonly method: string;
  /** The JSON-RPC error code, one of the contract's `CONTROL_*_ERROR_CODE` or a JSON-RPC one. */
  readonly code: number;
  /** The error's `data`, as the runtime sent it. */
  readonly data: unknown;
  /** The refusal, when the runtime refused the operation. */
  readonly refusal: ProjectActionRefusalOnTheWire | undefined;
}

/** Call one operation in one `POST` to the runtime's control route; a subscription needs a {@link TatolabRuntimeControlClient}. */
export declare function callOneControlOperation<Method extends ControlOperationMethodName>(
  connectOptions: TatolabRuntimeControlClientConnectOptions,
  method: Method,
  params: ControlOperations[Method]["params"],
  callOptions?: ControlOperationCallOptions,
): Promise<ControlOperations[Method]["result"]>;

/** One connection to a runtime's control API, upgraded on its local API socket. */
export declare class TatolabRuntimeControlClient {
  /** Upgrade a connection on the socket and `discover` over it; refused when the runtime serves another contract version. */
  static connect(options: TatolabRuntimeControlClientConnectOptions): Promise<TatolabRuntimeControlClient>;
  /** What `discover` answered when the connection opened. */
  readonly discovered: DiscoverResult;
  /** Call one operation; rejects with a {@link ControlOperationError} when the runtime answers an error. */
  call<Method extends ControlOperationMethodName>(
    method: Method,
    params: ControlOperations[Method]["params"],
    callOptions?: ControlOperationCallOptions,
  ): Promise<ControlOperations[Method]["result"]>;
  /** Hear each notification of `method` the connection is sent; the returned function stops it. */
  onNotification<Method extends ControlNotificationMethodName>(
    method: Method,
    listener: (params: ControlNotifications[Method]) => void,
  ): () => void;
  /** Resolves once the connection has closed, by either side. */
  readonly closed: Promise<void>;
  /** Close the connection; a call still unanswered rejects. */
  close(): Promise<void>;
}

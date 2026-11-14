// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

import type {
  ControlNotificationMethodName,
  ControlNotifications,
  ControlOperationMethodName,
  ControlOperations,
  ProjectActionRefusalOnTheWire,
} from "./control_contract.js";

/** One way a value does not match the control contract: where in the value, and what is wrong there. */
export interface ControlContractIssue {
  readonly code: string;
  readonly path: readonly PropertyKey[];
  readonly message: string;
}

/** A JSON-RPC request the client sends, its params checked against the contract. */
export interface ControlRequestMessage<Method extends ControlOperationMethodName = ControlOperationMethodName> {
  readonly jsonrpc: "2.0";
  readonly id: number;
  readonly method: Method;
  readonly params: ControlOperations[Method]["params"];
}

/** A JSON-RPC message as the runtime sent it, before it is checked against the contract. */
export interface ControlMessageFromTheRuntime {
  readonly [field: string]: unknown;
}

/** A notification checked against the contract. */
export interface ControlNotificationChecked<Method extends ControlNotificationMethodName = ControlNotificationMethodName> {
  readonly method: Method;
  readonly params: ControlNotifications[Method];
}

/** A JSON-RPC error the runtime answered a call with, in the runtime's own words. */
export declare class ControlOperationError extends Error {
  /** The method the call was to. */
  readonly method: string;
  /** The JSON-RPC error code, one of the contract's `CONTROL_*_ERROR_CODE` or a JSON-RPC one. */
  readonly code: number;
  /** The error's `data`, as the runtime sent it. */
  readonly data: unknown;
  /** The refusal, when the runtime refused the operation and its `data` matches the contract's refusal; `data` keeps what the runtime sent either way. */
  readonly refusal: ProjectActionRefusalOnTheWire | undefined;
}

/** A request whose params do not match the contract; it was never sent. */
export declare class ControlRequestInvalid extends Error {
  /** The method the request was to. */
  readonly method: string;
  /** Each way the params do not match. */
  readonly issues: readonly ControlContractIssue[];
}

/** An answer from the runtime that does not match the contract. */
export declare class ControlAnswerInvalid extends Error {
  /** The method the answer was to. */
  readonly method: string;
  /** Each way the answer does not match. */
  readonly issues: readonly ControlContractIssue[];
}

/** A notification from the runtime that does not match the contract; no listener heard it. */
export declare class ControlNotificationInvalid extends Error {
  /** The notification's method, or `(no method)`. */
  readonly method: string;
  /** Each way the notification does not match. */
  readonly issues: readonly ControlContractIssue[];
}

/** A message's JSON text, a `bigint` written as its digits. */
export declare function controlMessageText(message: unknown): string;

/** A message parsed from its JSON text, an integer past `Number.MAX_SAFE_INTEGER` read as a `bigint`. */
export declare function controlMessageOfText(messageText: string): unknown;

/** The request to `method` with `params` under `id`; throws {@link ControlRequestInvalid} when the params do not match the contract. */
export declare function controlRequestMessage<Method extends ControlOperationMethodName>(
  method: Method,
  params: ControlOperations[Method]["params"],
  id: number,
): ControlRequestMessage<Method>;

/** The result an answer to `method` carries, checked against the contract; throws {@link ControlOperationError} for an error answer and {@link ControlAnswerInvalid} for one that does not match. */
export declare function controlAnswerResult<Method extends ControlOperationMethodName>(
  method: Method,
  answerMessage: unknown,
): ControlOperations[Method]["result"];

/** A notification checked against the contract; throws {@link ControlNotificationInvalid} when it does not match. */
export declare function controlNotificationParams(notificationMessage: unknown): ControlNotificationChecked;

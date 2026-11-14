// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import { CONTROL_OPERATION_REFUSED_ERROR_CODE } from "./control_contract.js";
import { ControlSchemas } from "./control_schemas.js";

const WRITTEN_INTEGER_PATTERN = /^-?\d+$/;

export class ControlOperationError extends Error {
  constructor(method, jsonRpcError) {
    super(jsonRpcError.message);
    this.name = "ControlOperationError";
    this.method = method;
    this.code = jsonRpcError.code;
    this.data = jsonRpcError.data;
  }

  get refusal() {
    if (this.code !== CONTROL_OPERATION_REFUSED_ERROR_CODE) {
      return undefined;
    }
    const checkedRefusal = ControlSchemas.components.ProjectActionRefusalOnTheWire.safeParse(this.data);
    return checkedRefusal.success ? checkedRefusal.data : undefined;
  }
}

function describeTheIssues(issues) {
  return issues.map((issue) => `${issue.path.length === 0 ? "(the whole value)" : issue.path.join(".")}: ${issue.message}`).join("; ");
}

export class ControlRequestInvalid extends Error {
  constructor(method, issues) {
    super(`\`${method}\` was not sent: its params do not match the control contract: ${describeTheIssues(issues)}`);
    this.name = "ControlRequestInvalid";
    this.method = method;
    this.issues = issues;
  }
}

export class ControlAnswerInvalid extends Error {
  constructor(method, issues) {
    super(`the runtime's answer to \`${method}\` does not match the control contract: ${describeTheIssues(issues)}`);
    this.name = "ControlAnswerInvalid";
    this.method = method;
    this.issues = issues;
  }
}

export class ControlNotificationInvalid extends Error {
  constructor(method, issues) {
    super(`the runtime's \`${method}\` notification does not match the control contract: ${describeTheIssues(issues)}`);
    this.name = "ControlNotificationInvalid";
    this.method = method;
    this.issues = issues;
  }
}

function anIssue(message) {
  return [{ code: "custom", path: [], message }];
}

export function controlMessageText(message) {
  return JSON.stringify(message, (_key, value) => (typeof value === "bigint" ? JSON.rawJSON(value.toString()) : value));
}

export function controlMessageOfText(messageText) {
  return JSON.parse(messageText, (_key, value, context) =>
    typeof value === "number" && !Number.isSafeInteger(value) && WRITTEN_INTEGER_PATTERN.test(context?.source ?? "")
      ? BigInt(context.source)
      : value,
  );
}

export function controlRequestMessage(method, params, id) {
  const schemasOfTheMethod = Object.hasOwn(ControlSchemas.operations, method) ? ControlSchemas.operations[method] : undefined;
  if (schemasOfTheMethod === undefined) {
    throw new ControlRequestInvalid(method, anIssue("the control contract declares no such method"));
  }
  const checkedParams = schemasOfTheMethod.params.safeParse(params);
  if (!checkedParams.success) {
    throw new ControlRequestInvalid(method, checkedParams.error.issues);
  }
  return { jsonrpc: "2.0", id, method, params: checkedParams.data };
}

export function controlAnswerResult(method, answerMessage) {
  if (typeof answerMessage !== "object" || answerMessage === null) {
    throw new ControlAnswerInvalid(method, anIssue("the answer is not a JSON-RPC message"));
  }
  if (answerMessage.error !== undefined) {
    const { error } = answerMessage;
    if (typeof error !== "object" || error === null || typeof error.code !== "number" || typeof error.message !== "string") {
      throw new ControlAnswerInvalid(method, anIssue("the answer's `error` is not a JSON-RPC error"));
    }
    throw new ControlOperationError(method, error);
  }
  if (!Object.hasOwn(answerMessage, "result")) {
    throw new ControlAnswerInvalid(method, anIssue("the answer carries neither `result` nor `error`"));
  }
  const schemasOfTheMethod = Object.hasOwn(ControlSchemas.operations, method) ? ControlSchemas.operations[method] : undefined;
  if (schemasOfTheMethod === undefined) {
    throw new ControlAnswerInvalid(method, anIssue("the control contract declares no such method"));
  }
  const checkedResult = schemasOfTheMethod.result.safeParse(answerMessage.result);
  if (!checkedResult.success) {
    throw new ControlAnswerInvalid(method, checkedResult.error.issues);
  }
  return checkedResult.data;
}

export function controlNotificationParams(notificationMessage) {
  const method = typeof notificationMessage?.method === "string" ? notificationMessage.method : "(no method)";
  const paramsSchema = Object.hasOwn(ControlSchemas.notifications, method) ? ControlSchemas.notifications[method] : undefined;
  if (paramsSchema === undefined) {
    throw new ControlNotificationInvalid(method, anIssue("the control contract declares no such notification"));
  }
  const checkedParams = paramsSchema.safeParse(notificationMessage.params);
  if (!checkedParams.success) {
    throw new ControlNotificationInvalid(method, checkedParams.error.issues);
  }
  return { method, params: checkedParams.data };
}

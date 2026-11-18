// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import {
  CONTROL_CONTRACT_REVISION,
  CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE,
  CONTROL_EXPERIMENTAL_OPERATION_METHOD_NAMES,
} from "./control_contract.js";
import { readAgainstControlContractSchema, readControlContractValue } from "./control_contract_check.js";
import { CONTROL_CONTRACT_SCHEMAS } from "./control_contract_schemas.js";

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
    const refusalReading = readControlContractValue("ProjectActionRefusalOnTheWire", this.data);
    return refusalReading.read ? refusalReading.value : undefined;
  }
}

function wordsOfAContractMismatch(mismatch) {
  const theClients = `version ${mismatch.client_contract_version} revision ${mismatch.client_contract_revision}`;
  const theRuntimes = `version ${mismatch.runtime_contract_version} revision ${mismatch.runtime_contract_revision}`;
  return mismatch.older_side === "runtime"
    ? `the runtime is older than this client: it serves control contract ${theRuntimes} (release ${mismatch.runtime_version}), and this client was built against ${theClients}; update the runtime`
    : `this client is older than the runtime: it was built against control contract ${theClients}, and the runtime (release ${mismatch.runtime_version}) serves ${theRuntimes}; take the client the runtime carries`;
}

export class ControlContractMismatch extends Error {
  constructor(mismatch) {
    super(wordsOfAContractMismatch(mismatch));
    this.name = "ControlContractMismatch";
    this.mismatch = mismatch;
  }

  get olderSide() {
    return this.mismatch.older_side;
  }
}

export function controlContractMismatchOf(value) {
  const mismatchReading = readControlContractValue("ControlContractVersionMismatch", value);
  return mismatchReading.read ? new ControlContractMismatch(mismatchReading.value) : undefined;
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

function whyAnExperimentalAnswerMayDiffer(method, runtimeContractRevision) {
  return CONTROL_EXPERIMENTAL_OPERATION_METHOD_NAMES.includes(method) &&
    runtimeContractRevision !== undefined &&
    runtimeContractRevision !== CONTROL_CONTRACT_REVISION
    ? `; \`${method}\` is experimental, and this client was built against control contract revision ${CONTROL_CONTRACT_REVISION} while the runtime serves revision ${runtimeContractRevision}, so its shape may have changed between them`
    : "";
}

export class ControlAnswerInvalid extends Error {
  constructor(method, issues, runtimeContractRevision) {
    super(
      `the runtime's answer to \`${method}\` does not match the control contract: ${describeTheIssues(issues)}${whyAnExperimentalAnswerMayDiffer(method, runtimeContractRevision)}`,
    );
    this.name = "ControlAnswerInvalid";
    this.method = method;
    this.issues = issues;
    this.runtimeContractRevision = runtimeContractRevision;
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

function schemasOfTheMethod(method) {
  return Object.hasOwn(CONTROL_CONTRACT_SCHEMAS.operations, method) ? CONTROL_CONTRACT_SCHEMAS.operations[method] : undefined;
}

export function controlRequestMessage(method, params, id) {
  const schemas = schemasOfTheMethod(method);
  if (schemas === undefined) {
    throw new ControlRequestInvalid(method, anIssue("the control contract declares no such method"));
  }
  const checkedParams = readAgainstControlContractSchema(schemas.params, params, "sent");
  if (checkedParams.issues.length > 0) {
    throw new ControlRequestInvalid(method, checkedParams.issues);
  }
  return { jsonrpc: "2.0", id, method, params };
}

export function controlAnswerResult(method, answerMessage, runtimeContractRevision) {
  if (typeof answerMessage !== "object" || answerMessage === null) {
    throw new ControlAnswerInvalid(method, anIssue("the answer is not a JSON-RPC message"));
  }
  if (answerMessage.error !== undefined) {
    const { error } = answerMessage;
    if (typeof error !== "object" || error === null || typeof error.code !== "number" || typeof error.message !== "string") {
      throw new ControlAnswerInvalid(method, anIssue("the answer's `error` is not a JSON-RPC error"));
    }
    if (error.code === CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE) {
      const contractMismatch = controlContractMismatchOf(error.data);
      if (contractMismatch !== undefined) {
        throw contractMismatch;
      }
    }
    throw new ControlOperationError(method, error);
  }
  if (!Object.hasOwn(answerMessage, "result")) {
    throw new ControlAnswerInvalid(method, anIssue("the answer carries neither `result` nor `error`"));
  }
  const schemas = schemasOfTheMethod(method);
  if (schemas === undefined) {
    throw new ControlAnswerInvalid(method, anIssue("the control contract declares no such method"));
  }
  const checkedResult = readAgainstControlContractSchema(schemas.result, answerMessage.result, "read");
  if (checkedResult.issues.length > 0) {
    throw new ControlAnswerInvalid(method, checkedResult.issues, runtimeContractRevision);
  }
  return checkedResult.value;
}

export function controlNotificationParams(notificationMessage) {
  const method = typeof notificationMessage?.method === "string" ? notificationMessage.method : "(no method)";
  const paramsSchema = Object.hasOwn(CONTROL_CONTRACT_SCHEMAS.notifications, method) ? CONTROL_CONTRACT_SCHEMAS.notifications[method] : undefined;
  if (paramsSchema === undefined) {
    throw new ControlNotificationInvalid(method, anIssue("the control contract declares no such notification"));
  }
  const checkedParams = readAgainstControlContractSchema(paramsSchema, notificationMessage.params, "read");
  if (checkedParams.issues.length > 0) {
    throw new ControlNotificationInvalid(method, checkedParams.issues);
  }
  return { method, params: checkedParams.value };
}

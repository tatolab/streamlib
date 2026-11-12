// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import { request as sendAnHttpRequest } from "node:http";

import {
  CONTROL_CLIENT_NAME_HEADER_NAME,
  CONTROL_CONTRACT_VERSION,
  CONTROL_CONTRACT_VERSION_HEADER_NAME,
  CONTROL_JSON_RPC_ROUTE_PATH,
  CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN,
  CONTROL_JSON_RPC_STREAM_UPGRADE_REQUEST_TARGET,
  CONTROL_OPERATION_REFUSED_ERROR_CODE,
} from "./control_contract.js";

export class ControlOperationError extends Error {
  constructor(method, jsonRpcError) {
    super(jsonRpcError.message);
    this.name = "ControlOperationError";
    this.method = method;
    this.code = jsonRpcError.code;
    this.data = jsonRpcError.data;
  }

  get refusal() {
    return this.code === CONTROL_OPERATION_REFUSED_ERROR_CODE ? this.data : undefined;
  }
}

export class TatolabRuntimeControlClient {
  #upgradedSocket;
  #nextRequestId = 1;
  #callsAwaitingAnAnswerById = new Map();
  #notificationListenersByMethod = new Map();
  #unfinishedLineFromTheRuntime = "";
  #whyTheConnectionClosed = undefined;

  static async connect({ socketPath, clientName, signal }) {
    const upgradedSocket = await upgradeAControlStreamConnection(socketPath, clientName, signal);
    const client = new TatolabRuntimeControlClient(upgradedSocket.socket, upgradedSocket.bytesAlreadyRead);
    try {
      client.discovered = await client.call("discover", { contract_version: CONTROL_CONTRACT_VERSION }, { signal });
    } catch (discoverRefusal) {
      await client.close();
      throw discoverRefusal;
    }
    return client;
  }

  constructor(upgradedSocket, bytesAlreadyRead) {
    this.#upgradedSocket = upgradedSocket;
    this.closed = new Promise((resolveClosed) => {
      upgradedSocket.once("close", () => {
        this.#rejectEveryCallAwaitingAnAnswer(
          this.#whyTheConnectionClosed ?? new Error("the runtime closed the control connection"),
        );
        resolveClosed();
      });
    });
    upgradedSocket.on("error", (socketError) => {
      this.#whyTheConnectionClosed = socketError;
    });
    upgradedSocket.setEncoding("utf8");
    upgradedSocket.on("data", (textFromTheRuntime) => this.#readTextFromTheRuntime(textFromTheRuntime));
    if (bytesAlreadyRead.length > 0) {
      this.#readTextFromTheRuntime(bytesAlreadyRead.toString("utf8"));
    }
  }

  call(method, params, { signal, resultReviver } = {}) {
    if (this.#upgradedSocket.destroyed || this.#upgradedSocket.writableEnded) {
      return Promise.reject(new Error(`the control connection is closed; \`${method}\` was not sent`));
    }
    if (signal?.aborted) {
      return Promise.reject(signal.reason);
    }
    const requestId = this.#nextRequestId++;
    return new Promise((resolveResult, rejectResult) => {
      const forgetTheCallWhenAborted = () => {
        this.#callsAwaitingAnAnswerById.delete(requestId);
        rejectResult(signal.reason);
      };
      const stopListeningForAnAbort = () => signal?.removeEventListener("abort", forgetTheCallWhenAborted);
      signal?.addEventListener("abort", forgetTheCallWhenAborted, { once: true });
      this.#callsAwaitingAnAnswerById.set(requestId, {
        method,
        resultReviver,
        resolveResult: (result) => {
          stopListeningForAnAbort();
          resolveResult(result);
        },
        rejectResult: (whyNoResult) => {
          stopListeningForAnAbort();
          rejectResult(whyNoResult);
        },
      });
      this.#upgradedSocket.write(`${JSON.stringify({ jsonrpc: "2.0", id: requestId, method, params })}\n`);
    });
  }

  onNotification(method, listener) {
    const listeners = this.#notificationListenersByMethod.get(method) ?? new Set();
    listeners.add(listener);
    this.#notificationListenersByMethod.set(method, listeners);
    return () => listeners.delete(listener);
  }

  async close() {
    if (!this.#upgradedSocket.destroyed) {
      this.#upgradedSocket.end();
    }
    await this.closed;
  }

  #readTextFromTheRuntime(textFromTheRuntime) {
    const lines = (this.#unfinishedLineFromTheRuntime + textFromTheRuntime).split("\n");
    this.#unfinishedLineFromTheRuntime = lines.pop() ?? "";
    for (const line of lines) {
      if (line.trim().length > 0) {
        this.#readOneMessageFromTheRuntime(line);
      }
    }
  }

  #readOneMessageFromTheRuntime(line) {
    let message;
    try {
      message = JSON.parse(line);
    } catch (notJson) {
      this.#whyTheConnectionClosed = new Error(`the runtime sent a line that is not JSON: ${notJson.message}`);
      this.#upgradedSocket.destroy();
      return;
    }
    if (message.id === undefined || message.id === null) {
      if (typeof message.method === "string") {
        for (const listener of this.#notificationListenersByMethod.get(message.method) ?? []) {
          listener(message.params);
        }
      }
      return;
    }
    const callAwaitingThisAnswer = this.#callsAwaitingAnAnswerById.get(message.id);
    if (callAwaitingThisAnswer === undefined) {
      return;
    }
    this.#callsAwaitingAnAnswerById.delete(message.id);
    if (message.error !== undefined) {
      callAwaitingThisAnswer.rejectResult(new ControlOperationError(callAwaitingThisAnswer.method, message.error));
    } else if (callAwaitingThisAnswer.resultReviver === undefined) {
      callAwaitingThisAnswer.resolveResult(message.result);
    } else {
      callAwaitingThisAnswer.resolveResult(resultOfTheAnswerRevivedWith(line, callAwaitingThisAnswer.resultReviver));
    }
  }

  #rejectEveryCallAwaitingAnAnswer(whyNoAnswer) {
    for (const callAwaitingAnAnswer of this.#callsAwaitingAnAnswerById.values()) {
      callAwaitingAnAnswer.rejectResult(whyNoAnswer);
    }
    this.#callsAwaitingAnAnswerById.clear();
  }
}

function resultOfTheAnswerRevivedWith(answerText, resultReviver) {
  return JSON.parse(answerText, resultReviver).result;
}

export function callOneControlOperation({ socketPath, clientName }, method, params, { signal, resultReviver } = {}) {
  const requestBody = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  return new Promise((resolveResult, rejectResult) => {
    const postRequest = sendAnHttpRequest(
      {
        socketPath,
        path: CONTROL_JSON_RPC_ROUTE_PATH,
        method: "POST",
        signal,
        headers: {
          "Content-Type": "application/json",
          "Content-Length": Buffer.byteLength(requestBody),
          [CONTROL_CONTRACT_VERSION_HEADER_NAME]: String(CONTROL_CONTRACT_VERSION),
          [CONTROL_CLIENT_NAME_HEADER_NAME]: clientName,
        },
      },
      (postResponse) => {
        postResponse.once("error", (responseError) => rejectResult(signal?.aborted ? signal.reason : responseError));
        let answerText = "";
        postResponse.setEncoding("utf8");
        postResponse.on("data", (answerTextPart) => {
          answerText += answerTextPart;
        });
        postResponse.once("end", () => {
          let answer;
          try {
            answer = JSON.parse(answerText);
          } catch {
            rejectResult(
              new Error(`the runtime answered \`${method}\` with HTTP ${postResponse.statusCode}: ${answerText}`),
            );
            return;
          }
          if (answer.error !== undefined) {
            rejectResult(new ControlOperationError(method, answer.error));
          } else {
            resolveResult(resultReviver === undefined ? answer.result : resultOfTheAnswerRevivedWith(answerText, resultReviver));
          }
        });
      },
    );
    postRequest.once("error", (requestError) => rejectResult(signal?.aborted ? signal.reason : requestError));
    postRequest.end(requestBody);
  });
}

function upgradeAControlStreamConnection(socketPath, clientName, signal) {
  return new Promise((resolveUpgradedSocket, rejectUpgrade) => {
    const upgradeRequest = sendAnHttpRequest({
      socketPath,
      path: CONTROL_JSON_RPC_STREAM_UPGRADE_REQUEST_TARGET,
      method: "GET",
      headers: {
        Connection: "Upgrade",
        Upgrade: CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN,
        [CONTROL_CONTRACT_VERSION_HEADER_NAME]: String(CONTROL_CONTRACT_VERSION),
        [CONTROL_CLIENT_NAME_HEADER_NAME]: clientName,
      },
    });
    if (signal?.aborted) {
      rejectUpgrade(signal.reason);
      return;
    }
    const giveUpTheUpgradeWhenAborted = () => upgradeRequest.destroy();
    signal?.addEventListener("abort", giveUpTheUpgradeWhenAborted, { once: true });
    const stopListeningForAnAbort = () => signal?.removeEventListener("abort", giveUpTheUpgradeWhenAborted);
    upgradeRequest.once("upgrade", (_upgradeResponse, socket, bytesAlreadyRead) => {
      stopListeningForAnAbort();
      resolveUpgradedSocket({ socket, bytesAlreadyRead });
    });
    upgradeRequest.once("response", (refusalResponse) => {
      stopListeningForAnAbort();
      let refusalWords = "";
      refusalResponse.setEncoding("utf8");
      refusalResponse.on("data", (refusalText) => {
        refusalWords += refusalText;
      });
      refusalResponse.once("end", () => {
        rejectUpgrade(
          new Error(
            `the runtime refused the control connection (HTTP ${refusalResponse.statusCode}): ${refusalWords}`,
          ),
        );
      });
    });
    upgradeRequest.once("error", (upgradeError) => {
      stopListeningForAnAbort();
      rejectUpgrade(signal?.aborted ? signal.reason : upgradeError);
    });
    upgradeRequest.end();
  });
}

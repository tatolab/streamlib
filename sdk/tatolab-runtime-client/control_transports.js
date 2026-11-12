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
} from "./control_contract.js";
import { controlMessageOfText, controlMessageText } from "./control_messages.js";

function controlClientHeaders(clientName) {
  return {
    [CONTROL_CONTRACT_VERSION_HEADER_NAME]: String(CONTROL_CONTRACT_VERSION),
    [CONTROL_CLIENT_NAME_HEADER_NAME]: clientName,
  };
}

export class ControlPostTransport {
  #socketPath;
  #clientName;

  constructor({ socketPath, clientName }) {
    this.#socketPath = socketPath;
    this.#clientName = clientName;
  }

  sendRequest(requestMessage, { signal } = {}) {
    if (signal?.aborted) {
      return Promise.reject(signal.reason);
    }
    const requestBody = controlMessageText(requestMessage);
    return new Promise((resolveAnswer, rejectAnswer) => {
      const postRequest = sendAnHttpRequest(
        {
          socketPath: this.#socketPath,
          path: CONTROL_JSON_RPC_ROUTE_PATH,
          method: "POST",
          signal,
          headers: {
            "Content-Type": "application/json",
            "Content-Length": Buffer.byteLength(requestBody),
            ...controlClientHeaders(this.#clientName),
          },
        },
        (postResponse) => {
          postResponse.once("error", (responseError) => rejectAnswer(signal?.aborted ? signal.reason : responseError));
          let answerText = "";
          postResponse.setEncoding("utf8");
          postResponse.on("data", (answerTextPart) => {
            answerText += answerTextPart;
          });
          postResponse.once("end", () => {
            try {
              resolveAnswer(controlMessageOfText(answerText));
            } catch {
              rejectAnswer(
                new Error(`the runtime answered \`${requestMessage.method}\` with HTTP ${postResponse.statusCode}: ${answerText}`),
              );
            }
          });
        },
      );
      postRequest.once("error", (requestError) => rejectAnswer(signal?.aborted ? signal.reason : requestError));
      postRequest.end(requestBody);
    });
  }

  async close() {}
}

export class ControlStreamTransport {
  #upgradedSocket;
  #answersAwaitedById = new Map();
  #notificationMessageListeners = new Set();
  #unfinishedLineFromTheRuntime = "";
  #whyTheConnectionClosed = undefined;

  static async open({ socketPath, clientName, signal }) {
    const { socket, bytesAlreadyRead } = await upgradeAControlStreamConnection(socketPath, clientName, signal);
    return new ControlStreamTransport(socket, bytesAlreadyRead);
  }

  constructor(upgradedSocket, bytesAlreadyRead) {
    this.#upgradedSocket = upgradedSocket;
    this.closed = new Promise((resolveClosed) => {
      upgradedSocket.once("close", () => {
        const whyNoAnswer = this.#whyTheConnectionClosed ?? new Error("the runtime closed the control connection");
        for (const answerAwaited of this.#answersAwaitedById.values()) {
          answerAwaited.rejectAnswer(whyNoAnswer);
        }
        this.#answersAwaitedById.clear();
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

  sendRequest(requestMessage, { signal } = {}) {
    if (this.#upgradedSocket.destroyed || this.#upgradedSocket.writableEnded) {
      return Promise.reject(new Error(`the control connection is closed; \`${requestMessage.method}\` was not sent`));
    }
    if (signal?.aborted) {
      return Promise.reject(signal.reason);
    }
    const requestId = requestMessage.id;
    return new Promise((resolveAnswer, rejectAnswer) => {
      const forgetTheAnswerWhenAborted = () => {
        this.#answersAwaitedById.delete(requestId);
        rejectAnswer(signal.reason);
      };
      const stopListeningForAnAbort = () => signal?.removeEventListener("abort", forgetTheAnswerWhenAborted);
      signal?.addEventListener("abort", forgetTheAnswerWhenAborted, { once: true });
      this.#answersAwaitedById.set(requestId, {
        resolveAnswer: (answerMessage) => {
          stopListeningForAnAbort();
          resolveAnswer(answerMessage);
        },
        rejectAnswer: (whyNoAnswer) => {
          stopListeningForAnAbort();
          rejectAnswer(whyNoAnswer);
        },
      });
      this.#upgradedSocket.write(`${controlMessageText(requestMessage)}\n`);
    });
  }

  onNotificationMessage(listener) {
    this.#notificationMessageListeners.add(listener);
    return () => this.#notificationMessageListeners.delete(listener);
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
      message = controlMessageOfText(line);
    } catch (notJson) {
      this.#whyTheConnectionClosed = new Error(`the runtime sent a line that is not JSON: ${notJson.message}`);
      this.#upgradedSocket.destroy();
      return;
    }
    if (typeof message !== "object" || message === null) {
      return;
    }
    if (message.id === undefined || message.id === null) {
      if (typeof message.method === "string") {
        for (const listener of this.#notificationMessageListeners) {
          listener(message);
        }
      }
      return;
    }
    const answerAwaited = this.#answersAwaitedById.get(message.id);
    if (answerAwaited !== undefined) {
      this.#answersAwaitedById.delete(message.id);
      answerAwaited.resolveAnswer(message);
    }
  }
}

function upgradeAControlStreamConnection(socketPath, clientName, signal) {
  return new Promise((resolveUpgradedSocket, rejectUpgrade) => {
    if (signal?.aborted) {
      rejectUpgrade(signal.reason);
      return;
    }
    const upgradeRequest = sendAnHttpRequest({
      socketPath,
      path: CONTROL_JSON_RPC_STREAM_UPGRADE_REQUEST_TARGET,
      method: "GET",
      headers: {
        Connection: "Upgrade",
        Upgrade: CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN,
        ...controlClientHeaders(clientName),
      },
    });
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
        rejectUpgrade(new Error(`the runtime refused the control connection (HTTP ${refusalResponse.statusCode}): ${refusalWords}`));
      });
    });
    upgradeRequest.once("error", (upgradeError) => {
      stopListeningForAnAbort();
      rejectUpgrade(signal?.aborted ? signal.reason : upgradeError);
    });
    upgradeRequest.end();
  });
}

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import { createHash, randomBytes } from "node:crypto";
import { request as sendAnHttpRequest } from "node:http";

import {
  CONTROL_CLIENT_NAME_HEADER_NAME,
  CONTROL_CONTRACT_VERSION,
  CONTROL_CONTRACT_VERSION_HEADER_NAME,
  CONTROL_JSON_RPC_ROUTE_PATH,
  CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
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
  #postRequestsUnanswered = new Set();
  #closed = false;

  constructor({ socketPath, clientName }) {
    this.#socketPath = socketPath;
    this.#clientName = clientName;
  }

  sendRequest(requestMessage, { signal } = {}) {
    if (this.#closed) {
      return Promise.reject(new Error(`the control transport is closed; \`${requestMessage.method}\` was not sent`));
    }
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
            const theRuntimeRefusedTheHttpRequest = new Error(
              `the runtime answered \`${requestMessage.method}\` with HTTP ${postResponse.statusCode}: ${answerText}`,
            );
            if (postResponse.statusCode < 200 || postResponse.statusCode > 299) {
              rejectAnswer(theRuntimeRefusedTheHttpRequest);
              return;
            }
            try {
              resolveAnswer(controlMessageOfText(answerText));
            } catch {
              rejectAnswer(theRuntimeRefusedTheHttpRequest);
            }
          });
        },
      );
      this.#postRequestsUnanswered.add(postRequest);
      postRequest.once("close", () => this.#postRequestsUnanswered.delete(postRequest));
      postRequest.once("error", (requestError) =>
        rejectAnswer(signal?.aborted ? signal.reason : this.#closed ? new Error(`the control transport closed before the runtime answered \`${requestMessage.method}\``) : requestError),
      );
      postRequest.end(requestBody);
    });
  }

  async close() {
    this.#closed = true;
    for (const postRequest of this.#postRequestsUnanswered) {
      postRequest.destroy();
    }
    this.#postRequestsUnanswered.clear();
  }
}

const WEBSOCKET_ACCEPT_KEY_SUFFIX = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const WEBSOCKET_CONTINUATION_OPCODE = 0x0;
const WEBSOCKET_TEXT_OPCODE = 0x1;
const WEBSOCKET_BINARY_OPCODE = 0x2;
const WEBSOCKET_CLOSE_OPCODE = 0x8;
const WEBSOCKET_PING_OPCODE = 0x9;
const WEBSOCKET_PONG_OPCODE = 0xa;
const WEBSOCKET_NORMAL_CLOSURE_CODE = 1000;

export class ControlWebSocketTransport {
  #socket;
  #answersAwaitedById = new Map();
  #notificationMessageListeners = new Set();
  #bytesNotYetFramed = Buffer.alloc(0);
  #fragmentsOfTheTextMessage = undefined;
  #closeFrameSent = false;
  #whyTheConnectionClosed = undefined;

  static async open({ socketPath, clientName, signal }) {
    const { socket, bytesAlreadyRead } = await openAControlWebSocket(socketPath, clientName, signal);
    return new ControlWebSocketTransport(socket, bytesAlreadyRead);
  }

  constructor(socket, bytesAlreadyRead) {
    this.#socket = socket;
    this.closed = new Promise((resolveClosed) => {
      socket.once("close", () => {
        this.#rejectEveryAnswerAwaited(this.#whyTheConnectionClosed ?? new Error("the runtime closed the control WebSocket"));
        resolveClosed();
      });
    });
    socket.on("error", (socketError) => {
      this.#whyTheConnectionClosed = socketError;
    });
    socket.on("data", (bytes) => this.#readBytesFromTheRuntime(bytes));
    if (bytesAlreadyRead.length > 0) {
      this.#readBytesFromTheRuntime(bytesAlreadyRead);
    }
  }

  sendRequest(requestMessage, { signal } = {}) {
    if (this.#socket.destroyed || this.#closeFrameSent) {
      return Promise.reject(new Error(`the control WebSocket is closed; \`${requestMessage.method}\` was not sent`));
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
      this.#sendAFrame(WEBSOCKET_TEXT_OPCODE, Buffer.from(controlMessageText(requestMessage), "utf8"));
    });
  }

  onNotificationMessage(listener) {
    this.#notificationMessageListeners.add(listener);
    return () => this.#notificationMessageListeners.delete(listener);
  }

  async close() {
    this.#rejectEveryAnswerAwaited(new Error("the control WebSocket was closed before the runtime answered"));
    if (!this.#socket.destroyed && !this.#closeFrameSent) {
      this.#sendTheCloseFrame(WEBSOCKET_NORMAL_CLOSURE_CODE);
      this.#socket.end();
    }
    await this.closed;
  }

  #rejectEveryAnswerAwaited(whyNoAnswer) {
    for (const answerAwaited of this.#answersAwaitedById.values()) {
      answerAwaited.rejectAnswer(whyNoAnswer);
    }
    this.#answersAwaitedById.clear();
  }

  #sendTheCloseFrame(closeCode) {
    const closeCodeBytes = Buffer.alloc(2);
    closeCodeBytes.writeUInt16BE(closeCode);
    this.#sendAFrame(WEBSOCKET_CLOSE_OPCODE, closeCodeBytes);
    this.#closeFrameSent = true;
  }

  #sendAFrame(opcode, payload) {
    this.#socket.write(aMaskedClientFrame(opcode, payload));
  }

  #readBytesFromTheRuntime(bytes) {
    this.#bytesNotYetFramed = Buffer.concat([this.#bytesNotYetFramed, bytes]);
    for (;;) {
      const frame = theFirstFrameOf(this.#bytesNotYetFramed);
      if (frame === undefined) {
        return;
      }
      this.#bytesNotYetFramed = this.#bytesNotYetFramed.subarray(frame.byteLength);
      if (frame.isMasked) {
        this.#failTheConnection("the runtime sent a masked frame, which a server never sends");
        return;
      }
      this.#readOneFrame(frame);
    }
  }

  #readOneFrame({ isTheFinalFragment, opcode, payload }) {
    switch (opcode) {
      case WEBSOCKET_TEXT_OPCODE:
        this.#fragmentsOfTheTextMessage = [payload];
        break;
      case WEBSOCKET_CONTINUATION_OPCODE:
        if (this.#fragmentsOfTheTextMessage === undefined) {
          this.#failTheConnection("the runtime continued a message it never started");
          return;
        }
        this.#fragmentsOfTheTextMessage.push(payload);
        break;
      case WEBSOCKET_PING_OPCODE:
        this.#sendAFrame(WEBSOCKET_PONG_OPCODE, payload);
        return;
      case WEBSOCKET_PONG_OPCODE:
        return;
      case WEBSOCKET_CLOSE_OPCODE:
        if (!this.#closeFrameSent) {
          this.#sendTheCloseFrame(WEBSOCKET_NORMAL_CLOSURE_CODE);
        }
        this.#socket.end();
        return;
      case WEBSOCKET_BINARY_OPCODE:
      default:
        this.#failTheConnection(`the runtime sent a frame of opcode ${opcode}, which carries no control message`);
        return;
    }
    if (isTheFinalFragment) {
      const messageText = Buffer.concat(this.#fragmentsOfTheTextMessage).toString("utf8");
      this.#fragmentsOfTheTextMessage = undefined;
      this.#readOneMessageFromTheRuntime(messageText);
    }
  }

  #failTheConnection(why) {
    this.#whyTheConnectionClosed = new Error(why);
    this.#socket.destroy();
  }

  #readOneMessageFromTheRuntime(messageText) {
    let message;
    try {
      message = controlMessageOfText(messageText);
    } catch (notJson) {
      this.#failTheConnection(`the runtime sent a message that is not JSON: ${notJson.message}`);
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

function aMaskedClientFrame(opcode, payload) {
  const lengthBytes =
    payload.length < 126 ? 0 : payload.length <= 0xffff ? 2 : 8;
  const head = Buffer.alloc(2 + lengthBytes + 4);
  head[0] = 0x80 | opcode;
  if (lengthBytes === 0) {
    head[1] = 0x80 | payload.length;
  } else if (lengthBytes === 2) {
    head[1] = 0x80 | 126;
    head.writeUInt16BE(payload.length, 2);
  } else {
    head[1] = 0x80 | 127;
    head.writeBigUInt64BE(BigInt(payload.length), 2);
  }
  const maskingKey = randomBytes(4);
  maskingKey.copy(head, 2 + lengthBytes);
  const maskedPayload = Buffer.alloc(payload.length);
  for (let index = 0; index < payload.length; index++) {
    maskedPayload[index] = payload[index] ^ maskingKey[index % 4];
  }
  return Buffer.concat([head, maskedPayload]);
}

function theFirstFrameOf(bytes) {
  if (bytes.length < 2) {
    return undefined;
  }
  const isTheFinalFragment = (bytes[0] & 0x80) !== 0;
  const opcode = bytes[0] & 0x0f;
  const isMasked = (bytes[1] & 0x80) !== 0;
  let payloadLength = bytes[1] & 0x7f;
  let headLength = 2;
  if (payloadLength === 126) {
    if (bytes.length < 4) return undefined;
    payloadLength = bytes.readUInt16BE(2);
    headLength = 4;
  } else if (payloadLength === 127) {
    if (bytes.length < 10) return undefined;
    payloadLength = Number(bytes.readBigUInt64BE(2));
    headLength = 10;
  }
  const maskingKeyLength = isMasked ? 4 : 0;
  const byteLength = headLength + maskingKeyLength + payloadLength;
  if (bytes.length < byteLength) {
    return undefined;
  }
  const payload = bytes.subarray(headLength + maskingKeyLength, byteLength);
  return { isTheFinalFragment, opcode, isMasked, payload, byteLength };
}

function openAControlWebSocket(socketPath, clientName, signal) {
  return new Promise((resolveOpenedSocket, rejectOpening) => {
    if (signal?.aborted) {
      rejectOpening(signal.reason);
      return;
    }
    const websocketKey = randomBytes(16).toString("base64");
    const upgradeRequest = sendAnHttpRequest({
      socketPath,
      path: CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
      method: "GET",
      headers: {
        Connection: "Upgrade",
        Upgrade: "websocket",
        "Sec-WebSocket-Version": "13",
        "Sec-WebSocket-Key": websocketKey,
        ...controlClientHeaders(clientName),
      },
    });
    const giveUpTheOpeningWhenAborted = () => upgradeRequest.destroy();
    signal?.addEventListener("abort", giveUpTheOpeningWhenAborted, { once: true });
    const stopListeningForAnAbort = () => signal?.removeEventListener("abort", giveUpTheOpeningWhenAborted);
    upgradeRequest.once("upgrade", (upgradeResponse, socket, bytesAlreadyRead) => {
      stopListeningForAnAbort();
      const acceptKeyExpected = createHash("sha1").update(websocketKey + WEBSOCKET_ACCEPT_KEY_SUFFIX).digest("base64");
      if (upgradeResponse.headers["sec-websocket-accept"] !== acceptKeyExpected) {
        socket.destroy();
        rejectOpening(new Error("the runtime answered the control WebSocket with the wrong `Sec-WebSocket-Accept`"));
        return;
      }
      resolveOpenedSocket({ socket, bytesAlreadyRead });
    });
    upgradeRequest.once("response", (refusalResponse) => {
      let refusalWords = "";
      let refusalReadToTheEnd = false;
      refusalResponse.setEncoding("utf8");
      refusalResponse.on("data", (refusalText) => {
        refusalWords += refusalText;
      });
      refusalResponse.once("end", () => {
        refusalReadToTheEnd = true;
        stopListeningForAnAbort();
        rejectOpening(new Error(`the runtime refused the control WebSocket (HTTP ${refusalResponse.statusCode}): ${refusalWords}`));
      });
      refusalResponse.once("error", (refusalError) => {
        stopListeningForAnAbort();
        rejectOpening(signal?.aborted ? signal.reason : refusalError);
      });
      refusalResponse.once("close", () => {
        if (refusalReadToTheEnd) {
          return;
        }
        stopListeningForAnAbort();
        rejectOpening(
          signal?.aborted
            ? signal.reason
            : new Error(`the runtime refused the control WebSocket (HTTP ${refusalResponse.statusCode}) and closed before its reason: ${refusalWords}`),
        );
      });
    });
    upgradeRequest.once("error", (upgradeError) => {
      stopListeningForAnAbort();
      rejectOpening(signal?.aborted ? signal.reason : upgradeError);
    });
    upgradeRequest.end();
  });
}

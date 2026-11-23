// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { CONTROL_CONTRACT_REVISION, CONTROL_CONTRACT_VERSION, CONTROL_OPERATION_METHOD_NAMES } from "../index.js";

/** What `discover` answers, in the shape `DiscoverResult` takes. */
export const DISCOVERED = { contract_version: CONTROL_CONTRACT_VERSION, contract_revision: CONTROL_CONTRACT_REVISION, runtime_version: "0.68.76", build_id: "stand-in", operations: [...CONTROL_OPERATION_METHOD_NAMES] };

/** A `describe_runtime` result naming `version`, in the shape `DescribeRuntimeToolResult` takes. */
export function describedRuntime(version) {
  return { version, build_id: "stand-in", up_for_ms: 1, machine: "rig" };
}

/** One unmasked text frame, as a server sends it. */
function aServerTextFrame(messageText) {
  const payload = Buffer.from(messageText, "utf8");
  const head = payload.length < 126 ? Buffer.from([0x81, payload.length]) : Buffer.from([0x81, 126, payload.length >> 8, payload.length & 0xff]);
  return Buffer.concat([head, payload]);
}

/** Each complete masked client frame at the start of `bytes`, and the bytes after them. */
function theClientFramesOf(bytes) {
  const frames = [];
  for (;;) {
    if (bytes.length < 2) break;
    let payloadLength = bytes[1] & 0x7f;
    let headLength = 2;
    if (payloadLength === 126) {
      if (bytes.length < 4) break;
      payloadLength = bytes.readUInt16BE(2);
      headLength = 4;
    }
    if (bytes.length < headLength + 4 + payloadLength) break;
    const maskingKey = bytes.subarray(headLength, headLength + 4);
    const payload = Buffer.from(bytes.subarray(headLength + 4, headLength + 4 + payloadLength)).map((byte, index) => byte ^ maskingKey[index % 4]);
    frames.push({ opcode: bytes[0] & 0x0f, payload });
    bytes = bytes.subarray(headLength + 4 + payloadLength);
  }
  return { frames, bytesLeft: bytes };
}

/** A local API stand-in on a Unix socket: it answers `discover` on a WebSocket with `discovered`, or with `discoverError` when one is given, and every other request on it or in a POST only when told to; `refusesTheUpgradeWith`, a status and a body, refuses the WebSocket instead. */
export async function startALocalApiThatAnswersOnlyWhenTold({
  answersTheUpgrade = true,
  endsTheConnectionWhenTheClientDoes = true,
  discovered = DISCOVERED,
  discoverError = undefined,
  refusesTheUpgradeWith = undefined,
} = {}) {
  const socketDirectory = await mkdtemp(join(tmpdir(), "runtime-client-test-"));
  const socketPath = join(socketDirectory, "local-api.sock");
  const requestsHeardOnTheConnection = [];
  const upgradedSockets = [];
  const upgradeRequestsHeard = [];
  const discoverParamsHeard = [];
  const postRequestsHeard = [];
  const postResponsesUnsent = [];
  const localApi = createServer((postRequest, postResponse) => {
    postRequestsHeard.push(postRequest);
    postResponsesUnsent.push(postResponse);
  });
  localApi.on("upgrade", (upgradeRequest, socket) => {
    upgradedSockets.push(socket);
    upgradeRequestsHeard.push(upgradeRequest);
    if (refusesTheUpgradeWith !== undefined) {
      const { statusCode, body } = refusesTheUpgradeWith;
      socket.end(`HTTP/1.1 ${statusCode} Refused\r\nContent-Type: application/json\r\nContent-Length: ${Buffer.byteLength(body)}\r\nConnection: close\r\n\r\n${body}`);
      return;
    }
    if (!answersTheUpgrade) return;
    const acceptKey = createHash("sha1").update(`${upgradeRequest.headers["sec-websocket-key"]}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest("base64");
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: ${acceptKey}\r\n\r\n`);
    if (endsTheConnectionWhenTheClientDoes) socket.once("end", () => socket.end());
    let bytesNotYetFramed = Buffer.alloc(0);
    socket.on("data", (bytes) => {
      const { frames, bytesLeft } = theClientFramesOf(Buffer.concat([bytesNotYetFramed, bytes]));
      bytesNotYetFramed = bytesLeft;
      for (const frame of frames) {
        if (frame.opcode === 0x8) {
          if (endsTheConnectionWhenTheClientDoes) socket.end(Buffer.from([0x88, 0]));
          return;
        }
        const message = JSON.parse(frame.payload.toString("utf8"));
        if (message.method === "discover") {
          discoverParamsHeard.push(message.params);
          const discoverAnswer = discoverError === undefined ? { result: discovered } : { error: discoverError };
          socket.write(aServerTextFrame(JSON.stringify({ jsonrpc: "2.0", id: message.id, ...discoverAnswer })));
        } else {
          requestsHeardOnTheConnection.push(message);
        }
      }
    });
  });
  await new Promise((listening) => localApi.listen(socketPath, listening));
  return {
    socketPath,
    requestsHeardOnTheConnection,
    postRequestsHeard,
    upgradeRequestsHeard,
    discoverParamsHeard,
    writeOnTheConnection(messageText) {
      upgradedSockets[0].write(aServerTextFrame(messageText));
    },
    answerOnTheConnection(requestId, result) {
      upgradedSockets[0].write(aServerTextFrame(JSON.stringify({ jsonrpc: "2.0", id: requestId, result })));
    },
    answerTheNextPostWithTheMessageText(messageText, statusCode = 200) {
      const postResponse = postResponsesUnsent.shift();
      postResponse.statusCode = statusCode;
      postResponse.setHeader("Content-Type", "application/json");
      postResponse.end(messageText);
    },
    async stop() {
      for (const socket of upgradedSockets) socket.destroy();
      for (const postRequest of postRequestsHeard) postRequest.socket.destroy();
      await new Promise((closed) => localApi.close(closed));
      await rm(socketDirectory, { recursive: true, force: true });
    },
  };
}

export async function untilTrue(condition) {
  while (!condition()) await new Promise((later) => setImmediate(later));
}

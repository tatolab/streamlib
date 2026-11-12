// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import { createServer } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { CONTROL_CONTRACT_VERSION } from "../index.js";

/** A local API stand-in on a Unix socket: it answers `discover` on an upgraded connection, `describe_runtime` only when told to, and a POST only when told to. */
export async function startALocalApiThatAnswersOnlyWhenTold({ answersTheUpgrade = true } = {}) {
  const socketDirectory = await mkdtemp(join(tmpdir(), "runtime-client-test-"));
  const socketPath = join(socketDirectory, "local-api.sock");
  const describeRuntimeRequestIdsHeard = [];
  const upgradedSockets = [];
  const postRequestsHeard = [];
  const postResponsesUnsent = [];
  const localApi = createServer((postRequest, postResponse) => {
    postRequestsHeard.push(postRequest);
    postResponsesUnsent.push(postResponse);
  });
  localApi.on("upgrade", (upgradeRequest, socket) => {
    upgradedSockets.push(socket);
    if (!answersTheUpgrade) return;
    socket.write("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: control-stream\r\n\r\n");
    socket.once("end", () => socket.end());
    let unfinishedLine = "";
    socket.on("data", (bytes) => {
      const lines = (unfinishedLine + bytes.toString("utf8")).split("\n");
      unfinishedLine = lines.pop() ?? "";
      for (const line of lines.filter((line) => line.length > 0)) {
        const message = JSON.parse(line);
        if (message.method === "discover") {
          socket.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { contract_version: CONTROL_CONTRACT_VERSION } })}\n`);
        } else if (message.method === "describe_runtime") {
          describeRuntimeRequestIdsHeard.push(message.id);
        }
      }
    });
  });
  await new Promise((listening) => localApi.listen(socketPath, listening));
  return {
    socketPath,
    describeRuntimeRequestIdsHeard,
    postRequestsHeard,
    answerDescribeRuntime(requestId, version) {
      upgradedSockets[0].write(`${JSON.stringify({ jsonrpc: "2.0", id: requestId, result: { version } })}\n`);
    },
    answerDescribeRuntimeWithTheResultText(requestId, resultText) {
      upgradedSockets[0].write(`{"jsonrpc":"2.0","id":${requestId},"result":${resultText}}\n`);
    },
    answerTheNextPostWithTheResultText(resultText) {
      const postResponse = postResponsesUnsent.shift();
      postResponse.setHeader("Content-Type", "application/json");
      postResponse.end(`{"jsonrpc":"2.0","id":1,"result":${resultText}}`);
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


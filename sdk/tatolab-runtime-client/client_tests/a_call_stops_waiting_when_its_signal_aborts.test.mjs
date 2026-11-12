// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { CONTROL_CONTRACT_VERSION, TatolabRuntimeControlClient, callOneControlOperation } from "../index.js";

const CLIENT_NAME = "abort test";

/** A local API stand-in on a Unix socket: it answers `discover` on an upgraded connection, `describe_runtime` only when told to, and never answers a POST. */
async function startALocalApiThatAnswersOnlyWhenTold({ answersTheUpgrade = true } = {}) {
  const socketDirectory = await mkdtemp(join(tmpdir(), "runtime-client-abort-"));
  const socketPath = join(socketDirectory, "local-api.sock");
  const describeRuntimeRequestIdsHeard = [];
  const upgradedSockets = [];
  const postRequestsHeard = [];
  const localApi = createServer((postRequest) => {
    postRequestsHeard.push(postRequest);
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
    async stop() {
      for (const socket of upgradedSockets) socket.destroy();
      for (const postRequest of postRequestsHeard) postRequest.socket.destroy();
      await new Promise((closed) => localApi.close(closed));
      await rm(socketDirectory, { recursive: true, force: true });
    },
  };
}

async function untilTrue(condition) {
  while (!condition()) await new Promise((later) => setImmediate(later));
}

test("a POST the runtime never answers rejects with the signal's reason once it aborts", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const abortController = new AbortController();
    const unansweredCall = callOneControlOperation({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }, "describe_runtime", {}, {
      signal: abortController.signal,
    });
    await untilTrue(() => localApi.postRequestsHeard.length === 1);
    const whyItStopped = new Error("gave up after 65 s");
    abortController.abort(whyItStopped);
    await assert.rejects(unansweredCall, (rejection) => rejection === whyItStopped);
  } finally {
    await localApi.stop();
  }
});

test("a call on the connection stops waiting when aborted, drops its late answer, and the connection keeps answering", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeControlClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const abortController = new AbortController();
    const abortedCall = client.call("describe_runtime", {}, { signal: abortController.signal });
    await untilTrue(() => localApi.describeRuntimeRequestIdsHeard.length === 1);
    const whyItStopped = new Error("gave up");
    abortController.abort(whyItStopped);
    await assert.rejects(abortedCall, (rejection) => rejection === whyItStopped);

    const laterCall = client.call("describe_runtime", {});
    await untilTrue(() => localApi.describeRuntimeRequestIdsHeard.length === 2);
    const [abortedRequestId, laterRequestId] = localApi.describeRuntimeRequestIdsHeard;
    localApi.answerDescribeRuntime(abortedRequestId, "late");
    localApi.answerDescribeRuntime(laterRequestId, "0.68.75");
    assert.deepEqual(await laterCall, { version: "0.68.75" });
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("a signal already aborted rejects a call without sending it", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeControlClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const whyItStopped = new Error("already given up");
    await assert.rejects(client.call("describe_runtime", {}, { signal: AbortSignal.abort(whyItStopped) }), (rejection) => rejection === whyItStopped);
    await client.close();
    assert.equal(localApi.describeRuntimeRequestIdsHeard.length, 0);
  } finally {
    await localApi.stop();
  }
});

test("a connect whose upgrade is never answered rejects with the signal's reason once it aborts", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({ answersTheUpgrade: false });
  try {
    const abortController = new AbortController();
    const unansweredConnect = TatolabRuntimeControlClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME, signal: abortController.signal });
    const whyItStopped = new Error("no runtime answered the upgrade");
    setTimeout(() => abortController.abort(whyItStopped), 20);
    await assert.rejects(unansweredConnect, (rejection) => rejection === whyItStopped);
  } finally {
    await localApi.stop();
  }
});

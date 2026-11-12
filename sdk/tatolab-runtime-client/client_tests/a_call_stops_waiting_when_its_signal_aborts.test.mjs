// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import { test } from "node:test";

import { TatolabRuntimeControlClient, callOneControlOperation } from "../index.js";
import { startALocalApiThatAnswersOnlyWhenTold, untilTrue } from "./local_api_stand_in.mjs";

const CLIENT_NAME = "abort test";

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

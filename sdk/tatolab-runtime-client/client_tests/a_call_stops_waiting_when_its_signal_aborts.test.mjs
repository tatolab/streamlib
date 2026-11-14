// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { test } from "node:test";

import { ControlPostTransport, TatolabRuntimeClient } from "../index.js";
import { describedRuntime, startALocalApiThatAnswersOnlyWhenTold, untilTrue } from "./local_api_stand_in.mjs";

const CLIENT_NAME = "abort test";

test("a POST the runtime never answers rejects with the signal's reason once it aborts", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const abortController = new AbortController();
    const client = new TatolabRuntimeClient(new ControlPostTransport({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }));
    const unansweredCall = client.describe_runtime({}, { signal: abortController.signal });
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
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const abortController = new AbortController();
    const abortedCall = client.describe_runtime({}, { signal: abortController.signal });
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    const whyItStopped = new Error("gave up");
    abortController.abort(whyItStopped);
    await assert.rejects(abortedCall, (rejection) => rejection === whyItStopped);

    const laterCall = client.describe_runtime();
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 2);
    const [abortedRequest, laterRequest] = localApi.requestsHeardOnTheConnection;
    localApi.answerOnTheConnection(abortedRequest.id, describedRuntime("late"));
    localApi.answerOnTheConnection(laterRequest.id, describedRuntime("0.68.76"));
    assert.deepEqual(await laterCall, describedRuntime("0.68.76"));
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("a signal already aborted rejects a call without sending it", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const whyItStopped = new Error("already given up");
    await assert.rejects(client.describe_runtime({}, { signal: AbortSignal.abort(whyItStopped) }), (rejection) => rejection === whyItStopped);
    await client.close();
    assert.equal(localApi.requestsHeardOnTheConnection.length, 0);
  } finally {
    await localApi.stop();
  }
});

test("a connect whose upgrade is never answered rejects with the signal's reason once it aborts", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({ answersTheUpgrade: false });
  try {
    const abortController = new AbortController();
    const unansweredConnect = TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME, signal: abortController.signal });
    const whyItStopped = new Error("no runtime answered the upgrade");
    setTimeout(() => abortController.abort(whyItStopped), 20);
    await assert.rejects(unansweredConnect, (rejection) => rejection === whyItStopped);
  } finally {
    await localApi.stop();
  }
});

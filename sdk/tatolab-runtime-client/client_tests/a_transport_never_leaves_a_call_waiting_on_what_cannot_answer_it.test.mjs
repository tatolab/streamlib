// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { test } from "node:test";

import { ControlOperationError, ControlPostTransport, TatolabRuntimeClient } from "../index.js";
import { describedRuntime, startALocalApiThatAnswersOnlyWhenTold, untilTrue } from "./local_api_stand_in.mjs";

const CLIENT_NAME = "transport test";

test("a POST answered with an HTTP error rejects naming the status, even when its body is a JSON-RPC answer", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = new TatolabRuntimeClient(new ControlPostTransport({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }));
    const described = client.describe_runtime();
    await untilTrue(() => localApi.postRequestsHeard.length === 1);
    localApi.answerTheNextPostWithTheMessageText(JSON.stringify({ jsonrpc: "2.0", id: 1, result: describedRuntime("0.68.76") }), 502);
    await assert.rejects(described, /HTTP 502/);
  } finally {
    await localApi.stop();
  }
});

test("closing a POST transport rejects the call it has not had answered, and refuses the next", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = new TatolabRuntimeClient(new ControlPostTransport({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }));
    const unanswered = client.describe_runtime();
    await untilTrue(() => localApi.postRequestsHeard.length === 1);
    await client.close();
    await assert.rejects(unanswered, /closed before the runtime answered `describe_runtime`/);
    await assert.rejects(client.describe_runtime(), /closed; `describe_runtime` was not sent/);
  } finally {
    await localApi.stop();
  }
});

test("closing a connection rejects the call it has not had answered without waiting for the runtime to close its side", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({ endsTheConnectionWhenTheClientDoes: false });
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const unanswered = client.describe_runtime();
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    let closeFinished = false;
    void client.close().then(() => {
      closeFinished = true;
    });
    await assert.rejects(unanswered, /closed before the runtime answered/);
    assert.equal(closeFinished, false);
  } finally {
    await localApi.stop();
  }
});

test("a refusal whose data does not match the contract's refusal keeps the data but offers no refusal", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const opened = client.open_project({ project_directory: "relative" });
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    const notARefusal = { action: 7 };
    localApi.writeOnTheConnection(
      JSON.stringify({ jsonrpc: "2.0", id: localApi.requestsHeardOnTheConnection[0].id, error: { code: 1, message: "refused", data: notARefusal } }),
    );
    await assert.rejects(opened, (rejection) => {
      assert.ok(rejection instanceof ControlOperationError);
      assert.deepEqual(rejection.data, notARefusal);
      assert.equal(rejection.refusal, undefined);
      return true;
    });
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("a notification that does not match is reported once, however many listeners the client has", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const invalidNotificationsHeard = [];
    client.onNotificationInvalid((notificationInvalid) => invalidNotificationsHeard.push(notificationInvalid));
    localApi.writeOnTheConnection(JSON.stringify({ jsonrpc: "2.0", method: "project_changed", params: { project: "not-a-uuid", change: "missed" } }));
    await untilTrue(() => invalidNotificationsHeard.length === 1);
    const changesHeard = [];
    client.on_project_changed((change) => changesHeard.push(change));
    client.on_project_changed((change) => changesHeard.push(change));
    localApi.writeOnTheConnection(JSON.stringify({ jsonrpc: "2.0", method: "project_changed", params: { project: 7 } }));
    await untilTrue(() => invalidNotificationsHeard.length === 2);
    await new Promise((later) => setImmediate(later));
    assert.equal(invalidNotificationsHeard.length, 2);
    assert.equal(changesHeard.length, 0);
    await client.close();
  } finally {
    await localApi.stop();
  }
});

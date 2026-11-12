// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ControlAnswerInvalid,
  ControlNotificationInvalid,
  ControlOperationError,
  ControlPostTransport,
  ControlRequestInvalid,
  TatolabRuntimeClient,
  controlMessageText,
} from "../index.js";
import { startALocalApiThatAnswersOnlyWhenTold, untilTrue } from "./local_api_stand_in.mjs";

const CLIENT_NAME = "contract test";

test("params that do not match the contract reject naming the field, and nothing is sent", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    await assert.rejects(client.open_project({ project_directory: 7 }), (rejection) => {
      assert.ok(rejection instanceof ControlRequestInvalid);
      assert.equal(rejection.method, "open_project");
      assert.deepEqual(rejection.issues[0].path, ["project_directory"]);
      return true;
    });
    await assert.rejects(client.describe_runtime({ verbose: true }), ControlRequestInvalid);
    await assert.rejects(client.call("no_such_method", {}), ControlRequestInvalid);
    await client.close();
    assert.equal(localApi.requestsHeardOnTheConnection.length, 0);
  } finally {
    await localApi.stop();
  }
});

test("a method builds its own request and resolves the answer it checked", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const opened = client.open_project({ project_directory: "/projects/arm" });
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    const [openRequest] = localApi.requestsHeardOnTheConnection;
    assert.deepEqual(openRequest, { jsonrpc: "2.0", id: openRequest.id, method: "open_project", params: { project_directory: "/projects/arm" } });
    const queued = { action: "a1", project_directory: "/projects/arm" };
    localApi.answerOnTheConnection(openRequest.id, queued);
    assert.deepEqual(await opened, queued);
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("an answer that does not match the contract rejects naming what does not", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const described = client.describe_runtime();
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    localApi.answerOnTheConnection(localApi.requestsHeardOnTheConnection[0].id, { version: "0.68.76", build_id: "stand-in" });
    await assert.rejects(described, (rejection) => {
      assert.ok(rejection instanceof ControlAnswerInvalid);
      assert.deepEqual(rejection.issues[0].path, ["up_for_ms"]);
      return true;
    });
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("the runtime's refusal rejects in its own words with the structured refusal", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const opened = client.open_project({ project_directory: "relative" });
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    const refusal = { action: "open_project", runtime_words: "`relative` is not an absolute directory" };
    localApi.writeOnTheConnection(
      JSON.stringify({ jsonrpc: "2.0", id: localApi.requestsHeardOnTheConnection[0].id, error: { code: 1, message: refusal.runtime_words, data: refusal } }),
    );
    await assert.rejects(opened, (rejection) => {
      assert.ok(rejection instanceof ControlOperationError);
      assert.equal(rejection.message, refusal.runtime_words);
      assert.deepEqual(rejection.refusal, refusal);
      return true;
    });
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("an integer past Number.MAX_SAFE_INTEGER keeps its digits as a bigint, both ways", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = new TatolabRuntimeClient(new ControlPostTransport({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }));
    const described = client.describe_runtime();
    await untilTrue(() => localApi.postRequestsHeard.length === 1);
    localApi.answerTheNextPostWithTheMessageText('{"jsonrpc":"2.0","id":1,"result":{"version":"0.68.76","build_id":"stand-in","up_for_ms":18446744073709551615}}');
    assert.equal((await described).up_for_ms, 18446744073709551615n);
    assert.equal(controlMessageText({ up_for_ms: 18446744073709551615n }), '{"up_for_ms":18446744073709551615}');
  } finally {
    await localApi.stop();
  }
});

test("a notification is heard checked, and one that does not match reaches only the invalid listener", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const changesHeard = [];
    const invalidNotificationsHeard = [];
    client.on_project_changed((change) => changesHeard.push(change));
    client.onNotificationInvalid((notificationInvalid) => invalidNotificationsHeard.push(notificationInvalid));
    localApi.writeOnTheConnection(JSON.stringify({ jsonrpc: "2.0", method: "project_changed", params: { project: 7 } }));
    await untilTrue(() => invalidNotificationsHeard.length === 1);
    assert.ok(invalidNotificationsHeard[0] instanceof ControlNotificationInvalid);
    assert.equal(changesHeard.length, 0);
    const missedChange = { project: "6f1c2a4e-0b7d-4c3e-9a51-2d8e7f60b913", change: "missed" };
    localApi.writeOnTheConnection(JSON.stringify({ jsonrpc: "2.0", method: "project_changed", params: missedChange }));
    await untilTrue(() => changesHeard.length === 1);
    assert.deepEqual(changesHeard, [missedChange]);
    await client.close();
  } finally {
    await localApi.stop();
  }
});

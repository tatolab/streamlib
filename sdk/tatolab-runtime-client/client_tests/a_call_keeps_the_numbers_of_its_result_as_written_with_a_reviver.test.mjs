// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import { test } from "node:test";

import { TatolabRuntimeControlClient, callOneControlOperation } from "../index.js";
import { startALocalApiThatAnswersOnlyWhenTold, untilTrue } from "./local_api_stand_in.mjs";

const CLIENT_NAME = "reviver test";
const A_RESULT_WITH_A_NUMBER_PAST_DOUBLE_PRECISION = '{"version":"0.68.75","up_for_ms":18446744073709551615}';
const keepTheDigitsAsWritten = (_key, value, context) => (typeof value === "number" ? `digits:${context?.source}` : value);

test("a POST's result is read with the reviver, which sees each number's digits as written", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const answered = callOneControlOperation({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }, "describe_runtime", {}, {
      resultReviver: keepTheDigitsAsWritten,
    });
    await untilTrue(() => localApi.postRequestsHeard.length === 1);
    localApi.answerTheNextPostWithTheResultText(A_RESULT_WITH_A_NUMBER_PAST_DOUBLE_PRECISION);
    assert.deepEqual(await answered, { version: "0.68.75", up_for_ms: "digits:18446744073709551615" });
  } finally {
    await localApi.stop();
  }
});

test("a call on the connection reads its result with its reviver, and a call with none reads plain numbers", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeControlClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const revived = client.call("describe_runtime", {}, { resultReviver: keepTheDigitsAsWritten });
    const plain = client.call("describe_runtime", {});
    await untilTrue(() => localApi.describeRuntimeRequestIdsHeard.length === 2);
    const [revivedRequestId, plainRequestId] = localApi.describeRuntimeRequestIdsHeard;
    localApi.answerDescribeRuntimeWithTheResultText(revivedRequestId, A_RESULT_WITH_A_NUMBER_PAST_DOUBLE_PRECISION);
    localApi.answerDescribeRuntimeWithTheResultText(plainRequestId, '{"version":"0.68.75","up_for_ms":42}');
    assert.deepEqual(await revived, { version: "0.68.75", up_for_ms: "digits:18446744073709551615" });
    assert.deepEqual(await plain, { version: "0.68.75", up_for_ms: 42 });
    await client.close();
  } finally {
    await localApi.stop();
  }
});

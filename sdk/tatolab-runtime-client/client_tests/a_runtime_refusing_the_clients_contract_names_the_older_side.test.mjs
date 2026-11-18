// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CONTROL_CONTRACT_REVISION,
  CONTROL_CONTRACT_REVISION_HEADER_NAME,
  CONTROL_CONTRACT_VERSION,
  CONTROL_CONTRACT_VERSION_HEADER_NAME,
  CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE,
  CONTROL_EXPERIMENTAL_OPERATION_METHOD_NAMES,
  ControlAnswerInvalid,
  ControlContractMismatch,
  ControlOperationError,
  ControlPostTransport,
  TatolabRuntimeClient,
  controlAnswerResult,
} from "../index.js";
import { DISCOVERED, describedRuntime, startALocalApiThatAnswersOnlyWhenTold, untilTrue } from "./local_api_stand_in.mjs";

const CLIENT_NAME = "contract mismatch test";

/** The record a runtime of revision `runtimeRevision` refuses this client with, naming `olderSide`. */
function aMismatch(olderSide, runtimeRevision) {
  return {
    client_contract_version: CONTROL_CONTRACT_VERSION,
    runtime_contract_version: CONTROL_CONTRACT_VERSION,
    client_contract_revision: CONTROL_CONTRACT_REVISION,
    runtime_contract_revision: runtimeRevision,
    older_side: olderSide,
    runtime_version: "0.68.90",
  };
}

const THE_RUNTIME_IS_OLDER = aMismatch("runtime", CONTROL_CONTRACT_REVISION - 1);

function aMismatchAnswer(requestId, mismatch) {
  return JSON.stringify({ jsonrpc: "2.0", id: requestId, error: { code: CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE, message: "refused", data: mismatch } });
}

function assertTheRuntimeIsNamedOlder(refusal) {
  assert.ok(refusal instanceof ControlContractMismatch, `${refusal}`);
  assert.equal(refusal.olderSide, "runtime");
  assert.deepEqual(refusal.mismatch, THE_RUNTIME_IS_OLDER);
  assert.match(refusal.message, /the runtime is older than this client/);
  assert.match(refusal.message, /release 0\.68\.90/);
  assert.match(refusal.message, /update the runtime$/);
  return true;
}

test("a client sends its contract version and revision in the upgrade's headers and in `discover`", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    const { headers } = localApi.upgradeRequestsHeard[0];
    assert.equal(headers[CONTROL_CONTRACT_VERSION_HEADER_NAME], String(CONTROL_CONTRACT_VERSION));
    assert.equal(headers[CONTROL_CONTRACT_REVISION_HEADER_NAME], String(CONTROL_CONTRACT_REVISION));
    assert.deepEqual(localApi.discoverParamsHeard, [{ contract_version: CONTROL_CONTRACT_VERSION, contract_revision: CONTROL_CONTRACT_REVISION }]);
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("a WebSocket upgrade refused with the mismatch as its 409 body rejects with one typed mismatch", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({ refusesTheUpgradeWith: { statusCode: 409, body: JSON.stringify(THE_RUNTIME_IS_OLDER) } });
  try {
    await assert.rejects(TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }), assertTheRuntimeIsNamedOlder);
  } finally {
    await localApi.stop();
  }
});

test("a WebSocket upgrade refused with words alone still rejects naming the status", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({ refusesTheUpgradeWith: { statusCode: 409, body: "not a record" } });
  try {
    await assert.rejects(TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }), (refusal) => {
      assert.ok(!(refusal instanceof ControlContractMismatch));
      assert.match(refusal.message, /HTTP 409\): not a record/);
      return true;
    });
  } finally {
    await localApi.stop();
  }
});

test("a POST refused with the mismatch as error 3's data rejects with the same typed mismatch", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold();
  try {
    const client = new TatolabRuntimeClient(new ControlPostTransport({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }));
    const described = client.describe_runtime();
    await untilTrue(() => localApi.postRequestsHeard.length === 1);
    assert.equal(localApi.postRequestsHeard[0].headers[CONTROL_CONTRACT_REVISION_HEADER_NAME], String(CONTROL_CONTRACT_REVISION));
    localApi.answerTheNextPostWithTheMessageText(aMismatchAnswer(1, THE_RUNTIME_IS_OLDER));
    await assert.rejects(described, assertTheRuntimeIsNamedOlder);
  } finally {
    await localApi.stop();
  }
});

test("`discover` refused with the mismatch rejects with the same typed mismatch, naming the client older when it is", () => {
  const theClientIsOlder = { ...aMismatch("client", CONTROL_CONTRACT_REVISION), runtime_contract_version: CONTROL_CONTRACT_VERSION + 1, runtime_contract_revision: 0 };
  assert.throws(
    () => controlAnswerResult("discover", JSON.parse(aMismatchAnswer(1, theClientIsOlder))),
    (refusal) => {
      assert.ok(refusal instanceof ControlContractMismatch);
      assert.equal(refusal.olderSide, "client");
      assert.match(refusal.message, /this client is older than the runtime/);
      assert.match(refusal.message, /take the client the runtime carries$/);
      return true;
    },
  );
});

test("error 3 whose data is not the mismatch record stays a plain operation error", () => {
  assert.throws(
    () => controlAnswerResult("discover", JSON.parse(aMismatchAnswer(1, { client_contract_version: 2, runtime_contract_version: 3 }))),
    (refusal) => refusal instanceof ControlOperationError && !(refusal instanceof ControlContractMismatch),
  );
});

test("an experimental answer that does not match says so when the runtime serves another revision", () => {
  const [experimentalMethod] = CONTROL_EXPERIMENTAL_OPERATION_METHOD_NAMES;
  const notAnAnswer = { jsonrpc: "2.0", id: 1, result: "not the result's shape" };
  const otherRevision = CONTROL_CONTRACT_REVISION + 1;
  assert.throws(
    () => controlAnswerResult(experimentalMethod, notAnAnswer, otherRevision),
    (invalid) => {
      assert.ok(invalid instanceof ControlAnswerInvalid);
      assert.equal(invalid.runtimeContractRevision, otherRevision);
      assert.match(invalid.message, new RegExp(`\`${experimentalMethod}\` is experimental, and this client was built against control contract revision ${CONTROL_CONTRACT_REVISION} while the runtime serves revision ${otherRevision}`));
      return true;
    },
  );
  for (const [method, runtimeRevision] of [
    [experimentalMethod, CONTROL_CONTRACT_REVISION],
    [experimentalMethod, undefined],
    ["describe_runtime", otherRevision],
  ]) {
    assert.throws(
      () => controlAnswerResult(method, notAnAnswer, runtimeRevision),
      (invalid) => invalid instanceof ControlAnswerInvalid && !invalid.message.includes("is experimental"),
    );
  }
});

test("a connected client names the revision `discover` answered when an experimental answer does not match", async () => {
  const otherRevision = CONTROL_CONTRACT_REVISION + 1;
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({ discovered: { ...DISCOVERED, contract_revision: otherRevision } });
  try {
    const client = await TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    assert.ok(CONTROL_EXPERIMENTAL_OPERATION_METHOD_NAMES.includes("subscribe_project_changes"));
    const subscribed = client.subscribe_project_changes({ projects: ["0b0c8a52-7e0e-4bd3-9a3c-3c7f2f5b9a11"] });
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    localApi.answerOnTheConnection(localApi.requestsHeardOnTheConnection[0].id, { projects: "not a list" });
    await assert.rejects(subscribed, (invalid) => {
      assert.ok(invalid instanceof ControlAnswerInvalid);
      assert.match(invalid.message, new RegExp(`while the runtime serves revision ${otherRevision}`));
      return true;
    });
    await client.close();
  } finally {
    await localApi.stop();
  }
});

test("a runtime built before the contract had a revision, which refuses `contract_revision`, is named the older side", async () => {
  const refusedAsARuntimeBeforeTheRevisionRefusesIt = {
    code: -32602,
    message: "unknown field `contract_revision`, expected `contract_version`",
    data: {
      refusal_code: "params_do_not_match_schema",
      action: "discover",
      runtime_words: "unknown field `contract_revision`, expected `contract_version`",
      refused_params_pointer: "/contract_revision",
    },
  };
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({ discoverError: refusedAsARuntimeBeforeTheRevisionRefusesIt });
  try {
    const connecting = TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME });
    await untilTrue(() => localApi.requestsHeardOnTheConnection.length === 1);
    const describing = localApi.requestsHeardOnTheConnection[0];
    assert.equal(describing.method, "describe_runtime");
    localApi.answerOnTheConnection(describing.id, describedRuntime("0.68.82"));
    await assert.rejects(connecting, (refusal) => {
      assert.ok(refusal instanceof ControlContractMismatch, `${refusal}`);
      assert.deepEqual(refusal.mismatch, {
        client_contract_version: CONTROL_CONTRACT_VERSION,
        runtime_contract_version: CONTROL_CONTRACT_VERSION,
        client_contract_revision: CONTROL_CONTRACT_REVISION,
        runtime_contract_revision: 0,
        older_side: "runtime",
        runtime_version: "0.68.82",
      });
      return true;
    });
  } finally {
    await localApi.stop();
  }
});

test("any other refusal of `discover`'s params is handed on as it came", async () => {
  const localApi = await startALocalApiThatAnswersOnlyWhenTold({
    discoverError: { code: -32602, message: "no", data: { refusal_code: "params_do_not_match_schema", action: "discover", runtime_words: "no", refused_params_pointer: "/other" } },
  });
  try {
    await assert.rejects(TatolabRuntimeClient.connect({ socketPath: localApi.socketPath, clientName: CLIENT_NAME }), (refusal) => refusal instanceof ControlOperationError && refusal.code === -32602);
  } finally {
    await localApi.stop();
  }
});

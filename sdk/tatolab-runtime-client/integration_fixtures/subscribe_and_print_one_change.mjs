// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Connects to the local API socket named by argv[2], calls over the connection and over a POST,
// subscribes to the project named by argv[3], and prints one JSON line per step for the
// runtime's integration test to read.

import { ControlOperationError, TatolabRuntimeControlClient, callOneControlOperation } from "../index.js";

const [socketPath, project] = process.argv.slice(2);
const printOneStep = (step) => process.stdout.write(`${JSON.stringify(step)}\n`);

const client = await TatolabRuntimeControlClient.connect({ socketPath, clientName: "runtime client integration" });
const heardChange = new Promise((resolveHeardChange) => {
  const stopHearing = client.onNotification("project_changed", (params) => {
    stopHearing();
    resolveHeardChange(params);
  });
});

let emptySubscriptionRefusal;
try {
  await client.call("subscribe_project_changes", { projects: [] });
} catch (callError) {
  emptySubscriptionRefusal = {
    is_a_control_operation_error: callError instanceof ControlOperationError,
    method: callError.method,
    code: callError.code,
  };
}

printOneStep({
  discovered: client.discovered,
  listed_streams: await client.call("list_streams", {}),
  listed_streams_over_a_post: await callOneControlOperation(
    { socketPath, clientName: "runtime client integration" },
    "list_streams",
    {},
  ),
  empty_subscription_refusal: emptySubscriptionRefusal,
  subscribed: await client.call("subscribe_project_changes", { projects: [project] }),
});
printOneStep({ heard: await heardChange });
await client.close();
printOneStep({ closed: true });

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

// Connects to the local API socket named by argv[2], calls over the connection and over a POST,
// subscribes to the project named by argv[3], and prints one JSON line per step for the
// runtime's integration test to read.

import { ControlOperationError, ControlPostTransport, TatolabRuntimeClient } from "../index.js";

const [socketPath, project] = process.argv.slice(2);
const printOneStep = (step) => process.stdout.write(`${JSON.stringify(step)}\n`);
const clientName = "runtime client integration";

const client = await TatolabRuntimeClient.connect({ socketPath, clientName });
const clientOverAPost = new TatolabRuntimeClient(new ControlPostTransport({ socketPath, clientName }));
const heardChange = new Promise((resolveHeardChange) => {
  const stopHearing = client.on_project_changed((params) => {
    if (params.change === "files_changed") return;
    stopHearing();
    resolveHeardChange(params);
  });
});

let emptySubscriptionRefusal;
try {
  await client.subscribe_project_changes({ projects: [] });
} catch (callError) {
  emptySubscriptionRefusal = {
    is_a_control_operation_error: callError instanceof ControlOperationError,
    method: callError.method,
    code: callError.code,
  };
}

printOneStep({
  discovered: client.discovered,
  listed_streams: await client.list_streams(),
  listed_streams_over_a_post: await clientOverAPost.list_streams(),
  empty_subscription_refusal: emptySubscriptionRefusal,
  subscribed: await client.subscribe_project_changes({ projects: [project] }),
});
printOneStep({ heard: await heardChange });
await client.close();
printOneStep({ closed: true });

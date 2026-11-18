// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// Connects to the local API socket named by argv[2], reads `graph` of the stream named by
// argv[3] and of every loaded stream through the client, which checks each answer against the
// contract as it reads it, and prints one JSON line, a wide integer as written: each answer, and
// each key either answer carries that the contract does not state.

import { TatolabRuntimeClient, controlContractIssues, controlMessageText } from "../index.js";

const [socketPath, stream] = process.argv.slice(2);
const client = await TatolabRuntimeClient.connect({ socketPath, clientName: "graph answer guard" });
try {
  const oneStream = await client.graph({ stream });
  const everyStream = await client.graph({});
  process.stdout.write(
    `${controlMessageText({
      one_stream: oneStream,
      every_stream: everyStream,
      keys_the_contract_does_not_state: [
        ...controlContractIssues("StreamGraphOnTheWire", oneStream, "sent"),
        ...controlContractIssues("MachineWideGraphOnTheWire", everyStream, "sent"),
      ],
    })}\n`,
  );
} finally {
  await client.close();
}

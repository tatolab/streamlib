// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import {
  CONTROL_OPERATION_METHOD_NAMES,
  ControlAnswerInvalid,
  ControlOperationError,
  ControlPostTransport,
  ControlRequestInvalid,
  ControlSchemas,
  TatolabRuntimeClient,
  controlAnswerResult,
  controlNotificationParams,
  controlRequestMessage,
  type ControlOperationAbortSignal,
  type ControlOperationMethodName,
  type ControlTransport,
  type DescribeRuntimeToolResult,
  type DiscoverResult,
  type GraphTransactionToolResult,
  type ListStreamsToolResult,
  type ProjectChangedNotificationParams,
  type ProjectStreamActionQueuedToolResult,
  type SubscribeProjectChangesResult,
} from "../index.js";

export async function aClientCallingEveryKindOfOperation(
  socketPath: string,
  project: string,
  callAbortSignal: ControlOperationAbortSignal,
  aTransportOfItsOwn: ControlTransport,
): Promise<void> {
  const client = await TatolabRuntimeClient.connect({ socketPath, clientName: "type check" });
  const discovered: DiscoverResult = client.discovered;
  const subscribed: SubscribeProjectChangesResult = await client.subscribe_project_changes({ projects: [project] });
  const described: DescribeRuntimeToolResult = await client.describe_runtime();
  const listedStreams: ListStreamsToolResult = await client.list_streams({});
  const streamAsset: string = listedStreams.streams[0]?.location?.stream_asset ?? "";
  const opened: ProjectStreamActionQueuedToolResult = await client.open_project({ project_directory: "/projects/arm" });
  const transacted: GraphTransactionToolResult = await client.transact({
    operations: [{ operation: "remove_node", stream: streamAsset, node: "a node" }],
    mode: "live",
  });
  const added: GraphTransactionToolResult = await client.add_node({
    stream: streamAsset,
    type: "nodes.grayscale_effect:GrayscaleEffect",
    author: { kind: "person", name: "Dana" },
  });
  const connected: GraphTransactionToolResult = await client.connect({
    stream: streamAsset,
    source: { node: "a node", port: "video_out" },
    target: { node: "another node", port: "video" },
  });
  const describedUntilAborted: DescribeRuntimeToolResult = await client.describe_runtime({}, { signal: callAbortSignal });
  const describedByName: DescribeRuntimeToolResult = await client.call("describe_runtime", {});
  const stopHearing = client.on_project_changed((params: ProjectChangedNotificationParams) => {
    void params.change;
  });
  stopHearing();
  const stopHearingTheInvalid = client.onNotificationInvalid((notificationInvalid) => void notificationInvalid.issues[0]?.path);
  stopHearingTheInvalid();

  const overAPost = new TatolabRuntimeClient(new ControlPostTransport({ socketPath, clientName: "type check" }));
  const describedOverAPost: DescribeRuntimeToolResult = await overAPost.describe_runtime();
  const overItsOwnTransport = new TatolabRuntimeClient(aTransportOfItsOwn);
  const listedOverItsOwnTransport: ListStreamsToolResult = await overItsOwnTransport.list_streams();

  const requestMessage = controlRequestMessage("open_project", { project_directory: "/projects/arm" }, 7);
  const openedFromAnAnswer: ProjectStreamActionQueuedToolResult = controlAnswerResult("open_project", { jsonrpc: "2.0", id: 7, result: {} });
  const heardChange = controlNotificationParams({ jsonrpc: "2.0", method: "project_changed", params: {} });
  const parsedDirectly = ControlSchemas.operations.open_project.result.parse({});
  const parsedByComponent: DiscoverResult = ControlSchemas.components.DiscoverResult.parse({});

  // @ts-expect-error the client has no method the contract does not declare.
  await client.no_such_method({});
  // @ts-expect-error `add_node` takes the node's `type`.
  await client.add_node({ stream: streamAsset });
  // @ts-expect-error `open_project` takes its `project_directory`.
  await client.open_project();
  // @ts-expect-error `list_streams` answers no `nodes`.
  void (await client.list_streams()).nodes;
  // @ts-expect-error a transact operation is tagged by one of the operations the contract names.
  await client.transact({ operations: [{ operation: "rename_stream", stream: streamAsset }] });
  // @ts-expect-error the contract sends no notification by that name.
  client.on_project_opened(() => {});

  try {
    await client.graph({});
  } catch (callError) {
    if (callError instanceof ControlOperationError) {
      const refusedAction: string | undefined = callError.refusal?.action;
      void refusedAction;
    } else if (callError instanceof ControlRequestInvalid || callError instanceof ControlAnswerInvalid) {
      const firstIssueMessage: string | undefined = callError.issues[0]?.message;
      void firstIssueMessage;
    }
  }
  const everyMethodName: readonly ControlOperationMethodName[] = CONTROL_OPERATION_METHOD_NAMES;
  void [discovered, subscribed, described, opened, transacted, added, connected, describedUntilAborted, describedByName];
  void [describedOverAPost, listedOverItsOwnTransport, requestMessage, openedFromAnAnswer, heardChange, parsedDirectly, parsedByComponent];
  void everyMethodName;
  await client.close();
}

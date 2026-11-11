// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import {
  CONTROL_OPERATION_METHOD_NAMES,
  ControlOperationError,
  TatolabRuntimeControlClient,
  callOneControlOperation,
  type ControlOperationMethodName,
  type DescribeRuntimeToolResult,
  type DiscoverResult,
  type GraphTransactionToolResult,
  type ListStreamsToolResult,
  type ProjectChangedNotificationParams,
  type SubscribeProjectChangesResult,
} from "../index.js";

export async function aClientCallingEveryOperation(socketPath: string, project: string): Promise<void> {
  const client = await TatolabRuntimeControlClient.connect({ socketPath, clientName: "type check" });
  const discovered: DiscoverResult = await client.call("discover", { contract_version: 1 });
  const subscribed: SubscribeProjectChangesResult = await client.call("subscribe_project_changes", {
    projects: [project],
  });
  const described: DescribeRuntimeToolResult = await client.call("describe_runtime", {});
  const listedStreams: ListStreamsToolResult = await client.call("list_streams", {});
  const exportedGraph: unknown = await client.call("graph", { stream: null });
  const streamAsset: string = listedStreams.streams[0]?.location?.stream_asset ?? "";
  const transacted: GraphTransactionToolResult = await client.call("transact", {
    operations: [{ operation: "remove_node", stream: streamAsset, node: "a node" }],
    mode: "live",
  });
  const added: GraphTransactionToolResult = await client.call("add_node", {
    stream: streamAsset,
    type: "nodes.grayscale_effect:GrayscaleEffect",
    author: { kind: "person", name: "Dana" },
  });
  const connected: GraphTransactionToolResult = await client.call("connect", {
    stream: streamAsset,
    source: { node: "a node", port: "video_out" },
    target: { node: "another node", port: "video" },
  });
  const describedOverAPost: DescribeRuntimeToolResult = await callOneControlOperation(
    { socketPath, clientName: "type check" },
    "describe_runtime",
    {},
  );
  const stopHearing = client.onNotification("project_changed", (params: ProjectChangedNotificationParams) => {
    void params.change;
  });
  stopHearing();

  // @ts-expect-error a method the contract does not declare is refused.
  await client.call("no_such_method", {});
  // @ts-expect-error `add_node` takes the node's `type`.
  await client.call("add_node", { stream: streamAsset });
  // @ts-expect-error `list_streams` answers no `nodes`.
  void (await client.call("list_streams", {})).nodes;
  // @ts-expect-error a transact operation is tagged by one of the operations the contract names.
  await client.call("transact", { operations: [{ operation: "rename_stream", stream: streamAsset }] });
  // @ts-expect-error the contract sends no notification by that name.
  client.onNotification("project_opened", () => {});

  try {
    await client.call("graph", {});
  } catch (callError) {
    if (callError instanceof ControlOperationError) {
      const refusedAction: string | undefined = callError.refusal?.action;
      void refusedAction;
    }
  }
  const everyMethodName: readonly ControlOperationMethodName[] = CONTROL_OPERATION_METHOD_NAMES;
  void [discovered, subscribed, described, exportedGraph, transacted, added, connected, describedOverAPost];
  void everyMethodName;
  await client.close();
}

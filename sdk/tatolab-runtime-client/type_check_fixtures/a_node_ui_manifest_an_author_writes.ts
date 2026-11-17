// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import type {
  BuiltInPinTypeName,
  NodeUiManifest,
  NodeUiManifestControl,
} from "@tatolab/runtime-client/node_ui_manifest";

export const aModulesNodeUiManifest: NodeUiManifest = {
  contract: "1.1",
  controls: [
    { type: "float", script: "float_number.ts" },
    { type: "float", control: "slider", script: "float_slider.ts" },
    { type: "vector", control: "xy_pad", script: "vector_xy_pad.ts" },
  ],
  node_bodies: [{ node_type: "acme:Crossfade", script: "crossfade_body.ts" }],
};

export const aControlNamesAPinTypeFromTheContractsList: BuiltInPinTypeName =
  aModulesNodeUiManifest.controls?.[0]?.type ?? "float";

export const aControlOfAPinTypeTheContractDoesNotList: NodeUiManifestControl = {
  // @ts-expect-error `flaot` is not a built-in pin type's name.
  type: "flaot",
  script: "float_dial.ts",
};

export const aControlWithAFieldTheManifestDoesNotHave: NodeUiManifestControl = {
  type: "float",
  script: "float_number.ts",
  // @ts-expect-error a control has no `surface`.
  surface: "node",
};

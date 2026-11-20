// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import type {
  NodeUiControlPinType,
  NodeUiManifest,
  NodeUiManifestControl,
} from "@tatolab/runtime-client/node_ui_manifest";

export const aModulesNodeUiManifest: NodeUiManifest = {
  contract: "1.1",
  controls: [
    { type: "float", script: "float_number.ts" },
    { type: "float", control: "slider", script: "float_slider.ts" },
    { type: "vector", control: "xy_pad", script: "vector_xy_pad.ts" },
    { type: "nodes.sweep:Sweep", script: "sweep_on_its_wire.ts" },
  ],
  node_bodies: [{ node_type: "acme:Crossfade", script: "crossfade_body.ts" }],
};

export const aControlNamesABuiltInOrANamedPinType: NodeUiControlPinType =
  aModulesNodeUiManifest.controls?.[3]?.type ?? "float";

export const aControlWhosePinTypeIsNotAName: NodeUiManifestControl = {
  // @ts-expect-error a control's pin type is a built-in type's name or a class's import path.
  type: 3,
  script: "float_dial.ts",
};

export const aControlWithAFieldTheManifestDoesNotHave: NodeUiManifestControl = {
  type: "float",
  script: "float_number.ts",
  // @ts-expect-error a control has no `surface`.
  surface: "node",
};

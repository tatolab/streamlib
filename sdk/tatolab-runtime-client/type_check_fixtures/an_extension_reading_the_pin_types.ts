// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import {
  BUILT_IN_PIN_TYPES,
  type BuiltInPinTypeName,
  type PinDeclaration,
  type PinType,
  type PinValue,
  type PinValues,
} from "../pin_types.js";
import type { PinType as PinTypeFromTheContract } from "../index.js";

export const aColorValue: PinValue<"color"> = { red: 1, green: 0.5, blue: 0, alpha: 1 };

export const aVectorValue: PinValues["vector"] = { x: 0, y: 1, z: 2 };

export const anAssetValue: PinValue<"asset"> = { asset: "1d0a7e52-1c1a-4c43-9c38-0f1f0c8a5a2e" };

export const aListOfFloats: PinType = { type: "list_of", element_type: { type: "float" } };

export const theSamePinTypeThroughTheContract: PinTypeFromTheContract = aListOfFloats;

export const aFloatInput: PinDeclaration = {
  pin_type: { kind: "is", pin_type: { type: "float", exact_size: "float32" } },
  flow: "value",
  annotations: { range: { minimum: 0, maximum: 1 }, unit: "Hz" },
};

export function namesOfTheTypesThatStoreAValue(): readonly BuiltInPinTypeName[] {
  return BUILT_IN_PIN_TYPES.filter((row) => row.value_component !== undefined).map((row) => row.name);
}

// @ts-expect-error A Texture stores no value, so it has no key in `PinValues`.
export type NoTextureValue = PinValue<"texture">;

// @ts-expect-error A Color's value has all four channels.
export const aColorMissingAlpha: PinValue<"color"> = { red: 1, green: 0.5, blue: 0 };

// @ts-expect-error A name the contract does not list is not a built-in pin type.
export const notABuiltInPinType: BuiltInPinTypeName = "image";

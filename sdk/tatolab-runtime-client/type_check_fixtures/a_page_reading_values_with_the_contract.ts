// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import {
  CONTROL_CONTRACT_SCHEMAS,
  controlContractIssues,
  isControlContractValue,
  readControlContractValue,
  type ControlContractComponentName,
  type ControlContractIssue,
  type PinType,
  type ProjectActionRefusalOnTheWire,
} from "../contract.js";

export function aPageReadingValuesWithTheContract(sent: unknown): void {
  if (isControlContractValue("PinType", sent)) {
    const pinType: PinType = sent;
    void pinType.type;
  }
  const refusalReading = readControlContractValue("ProjectActionRefusalOnTheWire", sent);
  if (refusalReading.read) {
    const refusal: ProjectActionRefusalOnTheWire = refusalReading.value;
    void refusal.refusal_code;
  } else {
    const firstIssue: ControlContractIssue | undefined = refusalReading.issues[0];
    void firstIssue?.path;
  }
  const issues: readonly ControlContractIssue[] = controlContractIssues("DiscoverRequest", sent, "sent");
  const componentName: ControlContractComponentName = "PinFlow";
  const pinTypeSchema = CONTROL_CONTRACT_SCHEMAS.components.PinType;
  const openProjectParamsSchema = CONTROL_CONTRACT_SCHEMAS.operations.open_project.params;

  // @ts-expect-error the contract has no component by that name.
  isControlContractValue("NoSuchComponent", sent);
  // @ts-expect-error a value is checked as sent or read.
  controlContractIssues("PinType", sent, "both");
  // @ts-expect-error the schema data is read-only.
  CONTROL_CONTRACT_SCHEMAS.components.PinType = true;
  void [issues, componentName, pinTypeSchema, openProjectParamsSchema];
}

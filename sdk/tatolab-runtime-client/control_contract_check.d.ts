// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import type { ControlContractComponentName, ControlContractComponents } from "./control_contract.js";
import type { ControlContractSchema } from "./control_contract_schemas.js";

/** One way a value does not match the control contract: where in the value, and what is wrong there. */
export interface ControlContractIssue {
  readonly code: string;
  readonly path: readonly PropertyKey[];
  readonly message: string;
}

/**
 * Which way a value travels, which decides how a key the contract does not name is taken: in what
 * is `sent` to the runtime it is an issue, and in what is `read` from it it is ignored, so a field
 * added to an answer never breaks an older client.
 */
export type ControlContractDirection = "sent" | "read";

/** A value read against a contract component: the value as read, or each way it does not match. */
export type ControlContractReading<Value> =
  | { readonly read: true; readonly value: Value }
  | { readonly read: false; readonly issues: readonly ControlContractIssue[] };

/**
 * `value` checked against one schema of the contract: each issue, and the value as read, which is
 * `value` itself unless reading filled in a default or read a member of a set that may grow as its
 * fallback; neither happens to what is `sent`.
 */
export declare function readAgainstControlContractSchema(
  schema: ControlContractSchema,
  value: unknown,
  direction: ControlContractDirection,
): { readonly issues: readonly ControlContractIssue[]; readonly value: unknown };

/** Every place `value` differs from the named component; empty when it matches. */
export declare function controlContractIssues(
  name: ControlContractComponentName,
  value: unknown,
  direction: ControlContractDirection,
): readonly ControlContractIssue[];

/** `value` read from the runtime as the named component, a default or a fallback read into it where the contract states one. */
export declare function readControlContractValue<Name extends ControlContractComponentName>(
  name: Name,
  value: unknown,
): ControlContractReading<ControlContractComponents[Name]>;

/** Whether `value`, as it stands, reads as the named component; one that matches only once a default or fallback is read into it does not. */
export declare function isControlContractValue<Name extends ControlContractComponentName>(
  name: Name,
  value: unknown,
): value is ControlContractComponents[Name];

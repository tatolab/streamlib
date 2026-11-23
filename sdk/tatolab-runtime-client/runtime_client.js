// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import { CONTROL_CONTRACT_REVISION, CONTROL_CONTRACT_VERSION, JSON_RPC_INVALID_PARAMS_ERROR_CODE } from "./control_contract.js";
import {
  ControlContractMismatch,
  ControlOperationError,
  controlAnswerResult,
  controlNotificationParams,
  controlRequestMessage,
} from "./control_messages.js";
import { ControlWebSocketTransport } from "./control_transports.js";

const A_TRANSPORT_THAT_NEVER_CLOSES = new Promise(() => {});

const THE_CONTRACT_REVISION_PARAM_POINTER = "/contract_revision";

/** Whether `discoverRefusal` is a runtime of this client's version refusing `contract_revision` as a param it does not take: one built before the contract had a revision, which serves revision 0. */
function isARuntimeFromBeforeTheRevision(discoverRefusal) {
  return (
    discoverRefusal instanceof ControlOperationError &&
    discoverRefusal.code === JSON_RPC_INVALID_PARAMS_ERROR_CODE &&
    discoverRefusal.refusal?.refused_params_pointer === THE_CONTRACT_REVISION_PARAM_POINTER
  );
}

export class TatolabRuntimeClientCallsOverATransport {
  #transport;
  #nextRequestId = 1;
  #notificationInvalidListeners = new Set();
  #notificationListenersByMethod = new Map();
  #listeningToTheTransportsNotifications = false;

  static async connect({ socketPath, clientName, signal }) {
    const transport = await ControlWebSocketTransport.open({ socketPath, clientName, signal });
    const client = new this(transport);
    try {
      client.discovered = await client.call(
        "discover",
        { contract_version: CONTROL_CONTRACT_VERSION, contract_revision: CONTROL_CONTRACT_REVISION },
        { signal },
      );
    } catch (discoverRefusal) {
      try {
        throw isARuntimeFromBeforeTheRevision(discoverRefusal) ? await client.#theMismatchOfARuntimeFromBeforeTheRevision(signal) : discoverRefusal;
      } finally {
        await client.close();
      }
    }
    return client;
  }

  /** The mismatch naming a runtime from before the revision the older side, with the `version` its `describe_runtime` answers; the rest of that answer is read by no schema, since such a runtime lacks every key added since. */
  async #theMismatchOfARuntimeFromBeforeTheRevision(signal) {
    const answerMessage = await this.#transport.sendRequest(controlRequestMessage("describe_runtime", {}, this.#nextRequestId++), { signal });
    const answeredVersion = answerMessage?.result?.version;
    const version = typeof answeredVersion === "string" ? answeredVersion : controlAnswerResult("describe_runtime", answerMessage).version;
    return new ControlContractMismatch({
      client_contract_version: CONTROL_CONTRACT_VERSION,
      runtime_contract_version: CONTROL_CONTRACT_VERSION,
      client_contract_revision: CONTROL_CONTRACT_REVISION,
      runtime_contract_revision: 0,
      older_side: "runtime",
      runtime_version: version,
    });
  }

  constructor(transport) {
    this.#transport = transport;
    this.closed = transport.closed ?? A_TRANSPORT_THAT_NEVER_CLOSES;
    this.discovered = undefined;
  }

  async call(method, params, { signal } = {}) {
    const requestMessage = controlRequestMessage(method, params, this.#nextRequestId++);
    const answerMessage = await this.#transport.sendRequest(requestMessage, { signal });
    return controlAnswerResult(method, answerMessage, this.discovered?.contract_revision);
  }

  onNotification(method, listener) {
    if (this.#transport.onNotificationMessage === undefined) {
      throw new Error(`this client's transport carries no notifications, so \`${method}\` is never heard`);
    }
    this.#listenToTheTransportsNotificationsOnce();
    const listenersOfTheMethod = this.#notificationListenersByMethod.get(method) ?? new Set();
    this.#notificationListenersByMethod.set(method, listenersOfTheMethod);
    const listenerOfThisCall = (params) => listener(params);
    listenersOfTheMethod.add(listenerOfThisCall);
    return () => listenersOfTheMethod.delete(listenerOfThisCall);
  }

  #listenToTheTransportsNotificationsOnce() {
    if (this.#transport.onNotificationMessage === undefined || this.#listeningToTheTransportsNotifications) {
      return;
    }
    this.#listeningToTheTransportsNotifications = true;
    this.#transport.onNotificationMessage((notificationMessage) => this.#hearOneNotificationMessage(notificationMessage));
  }

  #hearOneNotificationMessage(notificationMessage) {
    let checkedNotification;
    try {
      checkedNotification = controlNotificationParams(notificationMessage);
    } catch (notificationInvalid) {
      for (const notificationInvalidListener of this.#notificationInvalidListeners) {
        notificationInvalidListener(notificationInvalid);
      }
      return;
    }
    for (const listener of this.#notificationListenersByMethod.get(checkedNotification.method) ?? []) {
      listener(checkedNotification.params);
    }
  }

  onNotificationInvalid(listener) {
    this.#listenToTheTransportsNotificationsOnce();
    this.#notificationInvalidListeners.add(listener);
    return () => this.#notificationInvalidListeners.delete(listener);
  }

  close() {
    return this.#transport.close();
  }
}

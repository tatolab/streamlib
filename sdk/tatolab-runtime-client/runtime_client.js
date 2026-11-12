// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

import { CONTROL_CONTRACT_VERSION } from "./control_contract.js";
import { controlAnswerResult, controlNotificationParams, controlRequestMessage } from "./control_messages.js";
import { ControlStreamTransport } from "./control_transports.js";

const A_TRANSPORT_THAT_NEVER_CLOSES = new Promise(() => {});

export class TatolabRuntimeClientCallsOverATransport {
  #transport;
  #nextRequestId = 1;
  #notificationInvalidListeners = new Set();

  static async connect({ socketPath, clientName, signal }) {
    const transport = await ControlStreamTransport.open({ socketPath, clientName, signal });
    const client = new this(transport);
    try {
      client.discovered = await client.call("discover", { contract_version: CONTROL_CONTRACT_VERSION }, { signal });
    } catch (discoverRefusal) {
      await client.close();
      throw discoverRefusal;
    }
    return client;
  }

  constructor(transport) {
    this.#transport = transport;
    this.closed = transport.closed ?? A_TRANSPORT_THAT_NEVER_CLOSES;
  }

  async call(method, params, { signal } = {}) {
    const requestMessage = controlRequestMessage(method, params, this.#nextRequestId++);
    const answerMessage = await this.#transport.sendRequest(requestMessage, { signal });
    return controlAnswerResult(method, answerMessage);
  }

  onNotification(method, listener) {
    if (this.#transport.onNotificationMessage === undefined) {
      throw new Error(`this client's transport carries no notifications, so \`${method}\` is never heard`);
    }
    return this.#transport.onNotificationMessage((notificationMessage) => {
      if (notificationMessage?.method !== method) {
        return;
      }
      let checkedNotification;
      try {
        checkedNotification = controlNotificationParams(notificationMessage);
      } catch (notificationInvalid) {
        for (const notificationInvalidListener of this.#notificationInvalidListeners) {
          notificationInvalidListener(notificationInvalid);
        }
        return;
      }
      listener(checkedNotification.params);
    });
  }

  onNotificationInvalid(listener) {
    this.#notificationInvalidListeners.add(listener);
    return () => this.#notificationInvalidListeners.delete(listener);
  }

  close() {
    return this.#transport.close();
  }
}

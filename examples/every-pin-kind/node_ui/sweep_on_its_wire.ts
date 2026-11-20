// The default control of the sample's own named type, `nodes.sweep:Sweep`. A named type holds no
// value, so the Studio sets `value` to `null`; the control reads the wire into its input instead,
// and says how many bytes of a `Sweep` the last message carried, or why none arrive.

import type { StopWatching, ViewInputEvent } from "@tatolab/tato-app-view-sdk";
import type { ControlledPin, InputControlElement, InputControlState, StoredPinValue } from "@tatolab/tato-app-view-sdk/node_ui";
import controlStyles from "@tatolab/tato-app-view-sdk/node_ui/input_control.css" with { type: "css" };

/** The named type this control is the default of. */
type PinTypeName = "nodes.sweep:Sweep";

/** Shows what reaches its input of the sample's `Sweep` along the wire. */
export default class SweepOnItsWire extends HTMLElement implements InputControlElement<PinTypeName> {
  readonly #words = document.createElement("output");
  #pin: ControlledPin<PinTypeName> | null = null;
  #stopWatching: StopWatching | null = null;

  constructor() {
    super();
    const shadow = this.attachShadow({ mode: "open" });
    shadow.adoptedStyleSheets = [controlStyles];
    this.#words.value = "No sweep has arrived yet";
    shadow.append(this.#words);
  }

  set pin(pin: ControlledPin<PinTypeName>) {
    this.#pin = pin;
    this.#words.setAttribute("aria-label", pin.declaration.label);
  }

  set value(_value: StoredPinValue<PinTypeName>) {
    // A named type holds no value: only the wire says what reaches the input.
  }

  set state(state: InputControlState) {
    this.#words.dataset["state"] = state;
  }

  connectedCallback(): void {
    const pin = this.#pin;
    if (pin === null || this.#stopWatching !== null) return;
    this.#stopWatching = pin.studio.watchWhatIsWiredInto(pin.declaration.input_name, (event) => {
      this.#words.value = wordsForWhatArrived(event);
    });
  }

  disconnectedCallback(): void {
    this.#stopWatching?.();
    this.#stopWatching = null;
  }
}

/** What one event along the wire says of the sweep it carries. */
function wordsForWhatArrived(event: ViewInputEvent): string {
  switch (event.kind) {
    case "message":
      return event.reading.family === "bytes" ? `A sweep of ${String(event.reading.byte_length)} bytes` : "The wire carries no sweep";
    case "image_frame":
      event.video_frame.close();
      return "The wire carries no sweep";
    case "image_pixels":
      return "The wire carries no sweep";
    case "unwired":
    case "reopening":
    case "ended":
      return event.because;
  }
}

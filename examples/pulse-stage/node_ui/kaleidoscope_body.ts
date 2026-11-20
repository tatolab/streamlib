// The body of the sample's Kaleidoscope: a drawing of its mirrored wedges, turned by `spin`, and a
// fader for `spin` that a drag changes live and a release commits. `segments` is drawn as set, or
// as its default while a wire sets it.

import { fireNodeBodyEvent, type DrawnNode, type NodeBodyElement, type NodeBodyInputs } from "@tatolab/tato-app-view-sdk/node_ui";
import controlStyles from "@tatolab/tato-app-view-sdk/node_ui/input_control.css" with { type: "css" };

const DRAWING_SIZE_PIXELS = 72;
const FEWEST_SEGMENTS = 2;
const SEGMENTS_WHILE_UNKNOWN = 6;
const TURN_RADIANS = 2 * Math.PI;

/** The wedges, the turn and a fader for `spin`. */
export default class KaleidoscopeBody extends HTMLElement implements NodeBodyElement<typeof KaleidoscopeBody.inputTypes> {
  static readonly inputTypes = { segments: "float", spin: "float" } as const;
  readonly #drawing = document.createElement("canvas");
  readonly #words = document.createElement("p");
  readonly #spin = document.createElement("input");
  #segments = SEGMENTS_WHILE_UNKNOWN;
  #turns = 0;

  constructor() {
    super();
    const shadow = this.attachShadow({ mode: "open" });
    shadow.adoptedStyleSheets = [controlStyles];
    this.#drawing.width = DRAWING_SIZE_PIXELS;
    this.#drawing.height = DRAWING_SIZE_PIXELS;
    this.#drawing.setAttribute("role", "img");
    this.#words.style.margin = "0";
    this.#spin.type = "range";
    this.#spin.min = "0";
    this.#spin.max = "1";
    this.#spin.step = "0.01";
    this.#spin.style.width = "100%";
    shadow.append(this.#drawing, this.#words, this.#spin);
    this.#spin.addEventListener("input", () => {
      this.#turns = this.#spin.valueAsNumber;
      this.#draw();
      fireNodeBodyEvent(this, "input", "spin", this.#turns);
    });
    this.#spin.addEventListener("change", () => fireNodeBodyEvent(this, "change", "spin", this.#spin.valueAsNumber));
  }

  set node(node: DrawnNode<typeof KaleidoscopeBody.inputTypes>) {
    this.#spin.ariaLabel = node.declarations.spin.label;
  }

  set inputs(inputs: NodeBodyInputs<typeof KaleidoscopeBody.inputTypes>) {
    const { segments, spin } = inputs;
    this.#segments = Math.max(FEWEST_SEGMENTS, Math.floor(segments.value ?? SEGMENTS_WHILE_UNKNOWN));
    this.#spin.disabled = spin.state !== "editable";
    if (spin.value !== null) {
      this.#turns = spin.value;
      this.#spin.valueAsNumber = spin.value;
    }
    this.#draw();
  }

  #draw(): void {
    const words = `${String(this.#segments)} mirrored wedges, turned ${String(Math.round(this.#turns * 360))}°`;
    this.#words.textContent = words;
    this.#drawing.ariaLabel = words;
    const context = this.#drawing.getContext("2d");
    if (context === null) return;
    const centre = DRAWING_SIZE_PIXELS / 2;
    const wedge = TURN_RADIANS / this.#segments;
    context.clearRect(0, 0, DRAWING_SIZE_PIXELS, DRAWING_SIZE_PIXELS);
    for (let index = 0; index < this.#segments; index += 1) {
      const from = this.#turns * TURN_RADIANS + index * wedge;
      context.beginPath();
      context.moveTo(centre, centre);
      context.arc(centre, centre, centre - 2, from, from + wedge);
      context.closePath();
      context.fillStyle = index % 2 === 0 ? "rgb(255 214 140)" : "rgb(120 160 255)";
      context.fill();
    }
  }
}

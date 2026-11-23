// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The Paint Pulse stage, on the view SDK: the painted ball wired into `video`, and the music wired
// into `audio`, played here. Dragging orbits, scrolling zooms, and a click or space bursts; each
// is sent on the `camera` output, which the graph's OrbitCamera folds into the ball's camera. The
// sliders set node inputs live, by the node's name in the stream's graph.

import {
  connectToTatoApp,
  drawThePortViewerFrame,
  drawThePortViewerPixels,
  type JsonValue,
  type ListedStreamsReading,
  type OpenProjectReading,
  type StopWatching,
  type StreamGraphAction,
  type StreamGraphReading,
  type TatoAppViewConnection,
  type ViewInputEvent,
} from "@tatolab/tato-app-view-sdk";
import { StageSound } from "./stage_sound.ts";

/** A live edit is a project transaction, sent at most this often while a slider moves. */
const CONTROL_SEND_EVERY_MS = 60;
/** The camera is sent at most this often while the hand moves it. */
const CAMERA_SEND_EVERY_MS = 33;
const RADIANS_PER_PIXEL = 0.008;
/** A press that moves less than this is a click, which bursts. */
const CLICK_SLOP_PX = 4;

/** One node input a control sets live, by the node's name in the graph, and how the control's value becomes the input's. */
type InputSetLive = readonly [nodeName: string, input: string, shape?: (value: number) => JsonValue];

/** A heading over the controls after it. */
type ControlGroup = { group: string };
/** A slider. */
type SliderControl = { label: string; min: number; max: number; step: number; value: number; sets: readonly InputSetLive[] };
/** A choice among options, its value the chosen option's index. */
type OptionsControl = { label: string; options: readonly string[]; value: number; sets: readonly InputSetLive[] };
/** A switch. */
type ToggleControl = { label: string; toggle: true; value: boolean; sets: readonly InputSetLive[] };
type StageControl = ControlGroup | SliderControl | OptionsControl | ToggleControl;

/** Each control sets one or more node inputs live, by the node's name in the graph. */
const CONTROLS: readonly StageControl[] = [
  { group: "Music" },
  { label: "Source", options: ["Song", "My computer", "Mic"], value: 0, sets: [["source_switch", "source"]] },
  { label: "Volume", min: 0, max: 1, step: 0.01, value: 0.13, sets: [["synth_song", "volume"]] },
  { label: "Sensitivity", min: 0.3, max: 3, step: 0.05, value: 0.5, sets: [["spectrum_listener", "sensitivity"]] },
  { label: "Explosion", min: 0, max: 2, step: 0.05, value: 0.65, sets: [["spectrum_listener", "spread"]] },
  { group: "Ball" },
  { label: "Spin", min: -0.6, max: 0.6, step: 0.01, value: 0.12, sets: [["spin", "rate"]] },
  { label: "Flow", toggle: true, value: true, sets: [["paint_ball", "flow"]] },
  { group: "Camera" },
  { label: "Blown back", min: 1, max: 4, step: 0.05, value: 2.6, sets: [["paint_ball", "pullback"]] },
  { label: "Push out", min: 5, max: 150, step: 1, value: 60, sets: [["paint_ball", "push_out"]] },
  { label: "Spring back", min: 0.5, max: 30, step: 0.5, value: 5, sets: [["paint_ball", "spring_back"]] },
  { label: "Hold", min: 0, max: 1.5, step: 0.05, value: 0.35, sets: [["paint_ball", "hold"]] },
  { group: "Paint" },
  { label: "Brush size", min: 4, max: 30, step: 0.5, value: 11, sets: [["stroke_from_bass", "to_low"], ["stroke_from_bass", "to_high", (value) => value * 2]] },
  { label: "Paint", min: 0, max: 1, step: 0.01, value: 1, sets: [["painterly", "amount"]] },
  { label: "Canvas", min: 0, max: 0.1, step: 0.005, value: 0.03, sets: [["painterly", "canvas"]] },
  { group: "Colour" },
  { label: "Saturation", min: 0.5, max: 2.5, step: 0.05, value: 1.5, sets: [["saturation_from_energy", "to_high"]] },
  { label: "Hue", min: -0.5, max: 0.5, step: 0.01, value: 0, sets: [["colour", "hue_shift"]] },
  { label: "Kick split", min: 0, max: 0.03, step: 0.001, value: 0.008, sets: [["split_from_beat", "to_high"]] },
  { label: "Vignette", min: 0, max: 1, step: 0.01, value: 0.55, sets: [["vignette", "amount"]] },
];

/** The element of the page `selector` finds, which must be a `kind`. */
function theElement<Kind extends Element>(selector: string, kind: abstract new () => Kind): Kind {
  const element = document.querySelector(selector);
  if (!(element instanceof kind)) throw new Error(`the stage page holds no ${selector}`);
  return element;
}

const canvas = theElement(".stage-picture", HTMLCanvasElement);
const notice = theElement(".stage-notice", HTMLElement);
const panelStatus = theElement(".stage-panel-status", HTMLElement);
const panelControls = theElement(".stage-panel-controls", HTMLElement);
const muteButton = theElement(".stage-mute", HTMLButtonElement);

/** Where the stage has been dragged and zoomed to, and how many bursts it has asked for: the record the view sends on `camera`. */
type StageCamera = { yaw: number; pitch: number; zoom: number; burst: number };

const camera: StageCamera = { yaw: 0, pitch: 0, zoom: 1, burst: 0 };
const sound = new StageSound();

/** Where a live edit goes: the project's folder, the stream by its id, and each node's id by its name. */
const target: { projectFolder: string | null; stream: string | null; nodeIds: ReadonlyMap<string, string> } = { projectFolder: null, stream: null, nodeIds: new Map() };
let connection: TatoAppViewConnection | null = null;
let stopWatchingTheGraph: StopWatching | null = null;

// ---------------------------------------------------------------- sending

let cameraSendQueued = false;
let cameraLastSentAt = 0;

/** Send the camera on `camera`, at most every `CAMERA_SEND_EVERY_MS`; a move inside that is sent once it passes. */
function sendTheCamera(): void {
  if (connection === null) return;
  const wait = CAMERA_SEND_EVERY_MS - (performance.now() - cameraLastSentAt);
  if (wait > 0) {
    if (!cameraSendQueued) {
      cameraSendQueued = true;
      setTimeout(() => {
        cameraSendQueued = false;
        sendTheCamera();
      }, wait);
    }
    return;
  }
  cameraLastSentAt = performance.now();
  void connection.sendOutput("camera", { ...camera }).then((answer) => {
    if (answer.kind === "refused") notice.textContent = answer.because;
  });
}

/** Set each input `control` sets to `value`, live, in one transaction. */
function sendTheControl(control: SliderControl | OptionsControl | ToggleControl, value: number | boolean): void {
  const { projectFolder, stream } = target;
  if (connection === null || projectFolder === null || stream === null) return;
  const operations: StreamGraphAction[] = control.sets.flatMap(([nodeName, input, shape]): StreamGraphAction[] => {
    const node = target.nodeIds.get(nodeName);
    if (node === undefined) return [];
    return [{ operation: "set_node_input_value", stream, node, input, value: shape !== undefined && typeof value === "number" ? shape(value) : value }];
  });
  if (operations.length === 0) return;
  void connection.transactInTheProject(projectFolder, operations, { mode: "live", idempotencyKey: null }).then((answer) => {
    if (answer.kind === "refused") panelStatus.textContent = answer.refusal.runtime_words;
    else if (answer.kind === "not_sent") panelStatus.textContent = answer.because;
  });
}

/** `send`, called at most every `CONTROL_SEND_EVERY_MS`, with the latest value once that passes. */
function throttled(send: (value: number) => void): (value: number) => void {
  let lastAt = 0;
  let pending = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  return (value) => {
    pending = value;
    const wait = CONTROL_SEND_EVERY_MS - (performance.now() - lastAt);
    if (wait <= 0) {
      lastAt = performance.now();
      send(pending);
    } else if (timer === null) {
      timer = setTimeout(() => {
        timer = null;
        lastAt = performance.now();
        send(pending);
      }, wait);
    }
  };
}

// ---------------------------------------------------------------- the mouse and keys

function burst(): void {
  camera.burst += 1;
  cameraLastSentAt = 0;
  sendTheCamera();
}

function resetTheView(): void {
  camera.yaw = 0;
  camera.pitch = 0;
  camera.zoom = 1;
  sendTheCamera();
}

let drag: { x: number; y: number; moved: number } | null = null;
canvas.addEventListener("pointerdown", (event) => {
  canvas.setPointerCapture(event.pointerId);
  canvas.focus();
  drag = { x: event.clientX, y: event.clientY, moved: 0 };
  canvas.classList.add("is-dragging");
});
canvas.addEventListener("pointermove", (event) => {
  if (drag === null) return;
  const dx = event.clientX - drag.x;
  const dy = event.clientY - drag.y;
  drag = { x: event.clientX, y: event.clientY, moved: drag.moved + Math.abs(dx) + Math.abs(dy) };
  camera.yaw -= dx * RADIANS_PER_PIXEL;
  camera.pitch = Math.max(-1.3, Math.min(1.3, camera.pitch + dy * RADIANS_PER_PIXEL));
  sendTheCamera();
});
function endTheDrag(event: PointerEvent): void {
  if (drag === null) return;
  const wasAClick = drag.moved < CLICK_SLOP_PX;
  drag = null;
  canvas.classList.remove("is-dragging");
  if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
  if (wasAClick && event.type === "pointerup") burst();
}
canvas.addEventListener("pointerup", endTheDrag);
canvas.addEventListener("pointercancel", endTheDrag);
canvas.addEventListener("dblclick", resetTheView);
canvas.addEventListener(
  "wheel",
  (event) => {
    event.preventDefault();
    camera.zoom = Math.max(0.4, Math.min(2.5, camera.zoom * Math.exp(event.deltaY * 0.0015)));
    sendTheCamera();
  },
  { passive: false },
);
window.addEventListener("keydown", (event) => {
  if (event.code !== "Space" || event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
  event.preventDefault();
  burst();
});
theElement(".stage-burst", HTMLButtonElement).addEventListener("click", burst);
theElement(".stage-reset", HTMLButtonElement).addEventListener("click", resetTheView);

muteButton.addEventListener("click", () => {
  sound.setMuted(!sound.isMuted);
  muteButton.textContent = sound.isMuted ? "Unmute" : "Mute";
  muteButton.setAttribute("aria-pressed", String(sound.isMuted));
});
// Chromium keeps sound suspended until the page has seen a gesture; any press here resumes it.
window.addEventListener("pointerdown", () => sound.resumeAfterAGesture());

// ---------------------------------------------------------------- the panel

function formatted(control: SliderControl, value: number): string {
  if (control.step >= 1) return value.toFixed(0);
  if (control.step >= 0.01) return value.toFixed(2);
  return value.toFixed(3);
}

/** The row of a choice among options. */
function optionsRow(control: OptionsControl, id: string, label: HTMLLabelElement): HTMLElement {
  const row = document.createElement("div");
  row.className = "stage-control is-select";
  const picker = document.createElement("select");
  picker.id = id;
  control.options.forEach((name, optionIndex) => picker.append(new Option(name, String(optionIndex), optionIndex === control.value, optionIndex === control.value)));
  picker.addEventListener("change", () => sendTheControl(control, Number(picker.value)));
  row.append(label, picker);
  return row;
}

/** The row of a switch. */
function toggleRow(control: ToggleControl, id: string, label: HTMLLabelElement): HTMLElement {
  const row = document.createElement("div");
  row.className = "stage-control is-toggle";
  const box = document.createElement("input");
  box.id = id;
  box.type = "checkbox";
  box.checked = control.value;
  box.addEventListener("change", () => sendTheControl(control, box.checked));
  row.append(box, label);
  return row;
}

/** The row of a slider, which a double-click returns to its starting value. */
function sliderRow(control: SliderControl, id: string, label: HTMLLabelElement): HTMLElement {
  const row = document.createElement("div");
  row.className = "stage-control";
  const slider = document.createElement("input");
  slider.id = id;
  slider.type = "range";
  slider.min = String(control.min);
  slider.max = String(control.max);
  slider.step = String(control.step);
  slider.value = String(control.value);
  const shown = document.createElement("output");
  shown.htmlFor.add(id);
  shown.textContent = formatted(control, control.value);
  const send = throttled((value) => sendTheControl(control, value));
  slider.addEventListener("input", () => {
    shown.textContent = formatted(control, Number(slider.value));
    send(Number(slider.value));
  });
  slider.addEventListener("dblclick", () => {
    slider.value = String(control.value);
    shown.textContent = formatted(control, control.value);
    sendTheControl(control, control.value);
  });
  row.append(label, shown, slider);
  return row;
}

function buildThePanel(): void {
  CONTROLS.forEach((control, index) => {
    if ("group" in control) {
      const heading = document.createElement("h3");
      heading.className = "stage-panel-group";
      heading.textContent = control.group;
      panelControls.append(heading);
      return;
    }
    const id = `stage-control-${index}`;
    const label = document.createElement("label");
    label.htmlFor = id;
    label.textContent = control.label;
    if ("options" in control) panelControls.append(optionsRow(control, id, label));
    else if ("toggle" in control) panelControls.append(toggleRow(control, id, label));
    else panelControls.append(sliderRow(control, id, label));
  });
}

// ---------------------------------------------------------------- what the view reads

function onVideo(inputEvent: ViewInputEvent): void {
  switch (inputEvent.kind) {
    case "image_frame":
      drawThePortViewerFrame(canvas, inputEvent.video_frame);
      inputEvent.video_frame.close();
      notice.textContent = "";
      return;
    case "image_pixels":
      notice.textContent = drawThePortViewerPixels(canvas, inputEvent.width, inputEvent.height, inputEvent.format_wire_name, inputEvent.bytes_per_row, inputEvent.pixels)
        ? ""
        : `The stage can't draw ${inputEvent.format_wire_name} pixels.`;
      return;
    case "message":
      notice.textContent = `video carries ${inputEvent.reading.family}, not a picture.`;
      return;
    case "unwired":
    case "reopening":
    case "ended":
      notice.textContent = inputEvent.because;
      return;
  }
}

function onAudio(inputEvent: ViewInputEvent): void {
  if (inputEvent.kind === "message" && inputEvent.reading.family === "channels") sound.play(inputEvent.reading);
  else if (inputEvent.kind === "unwired" || inputEvent.kind === "reopening" || inputEvent.kind === "ended") sound.stop();
}

function onOpenProject(reading: OpenProjectReading): void {
  target.projectFolder = reading.kind === "read" ? reading.open_project.project.folder : null;
  if (target.projectFolder === null) panelStatus.textContent = "No project is open.";
}

function onStreamGraph(reading: StreamGraphReading): void {
  if (reading.kind !== "read") {
    panelStatus.textContent = reading.because;
    return;
  }
  target.nodeIds = new Map(reading.graph.nodes.map((graphNode) => [graphNode.name, graphNode.id]));
  panelStatus.textContent = "Live: changes apply as you drag. Double-click a slider to reset it.";
}

/** Find Paint Pulse's stream among those listed, the open project's first, and watch its graph for the node ids the sliders set. */
function onListedStreams(reading: ListedStreamsReading): void {
  if (reading.kind !== "listed") {
    panelStatus.textContent = reading.because;
    return;
  }
  const ours =
    reading.streams.find((stream) => stream.location !== null && stream.location.project_directory === target.projectFolder) ??
    reading.streams.find((stream) => stream.stream.toLowerCase().startsWith("paint-pulse"));
  if (ours === undefined) {
    panelStatus.textContent = "Paint Pulse's stream isn't listed.";
    return;
  }
  if (target.projectFolder === null && ours.location !== null) target.projectFolder = ours.location.project_directory;
  if (connection === null || ours.stream === target.stream) return;
  target.stream = ours.stream;
  stopWatchingTheGraph?.();
  stopWatchingTheGraph = connection.watchStreamGraph(ours.stream, onStreamGraph, (because) => {
    panelStatus.textContent = because;
  });
}

buildThePanel();
void connectToTatoApp().then((connected) => {
  connection = connected;
  const refused = (because: string) => {
    notice.textContent = because;
  };
  connected.watchInput("video", onVideo, refused);
  connected.watchInput("audio", onAudio);
  connected.watchOpenProject(onOpenProject, refused);
  connected.watchListedStreams(onListedStreams, refused);
  sendTheCamera();
});

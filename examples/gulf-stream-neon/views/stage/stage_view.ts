// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The Gulf Stream Neon stage, on the view SDK: the picture wired into `video`, steered by the hand.
// Dragging orbits, scrolling zooms, WASD and the arrows fly, and a click on the water drops an
// eddy; each move is sent on the `camera` output, which the graph's SmoothCamera glides after. The
// readings wired into `station` fill the live data panel, and each slider sets node inputs live,
// by the node's name in the graph.

import {
  connectToTatoApp,
  drawThePortViewerFrame,
  drawThePortViewerPixels,
  type JsonValue,
  type OpenProjectReading,
  type StreamGraphAction,
  type TatoAppViewConnection,
  type ViewInputEvent,
  wordsOfTheMessageRead,
} from "@tatolab/tato-app-view-sdk";
import { projectedOnThePicture, seaPointAt, stageCameraFrame, worldOf, type StageCamera } from "./stage_camera.ts";
import { NODE_IDS, STAGE_HOME, STREAM_ASSET } from "./stage_graph.ts";

/** A live edit is a project transaction; a few a second keeps the runtime with the hand. */
const CONTROL_SEND_EVERY_MS = 250;
/** The camera is sent at most this often while the hand moves it. */
const CAMERA_SEND_EVERY_MS = 16;
const DEGREES_PER_PIXEL = 0.25;
/** A press that moves less than this is a click, which drops an eddy. */
const CLICK_SLOP_PX = 4;

/** The Tato inks, from tatolab.com. */
const INK = { pink: "#ff3d8b", yellow: "#ffd60a", cream: "#fff4e0", ink: "#14101f", mint: "#2fd6a0" };

/** What a place on the map is, which picks how its label is lettered. */
type PlaceKind = "city" | "place" | "water" | "stream" | "station";

/** Each labelled place: its name, longitude, latitude and kind. */
const PLACES: readonly (readonly [string, number, number, PlaceKind])[] = [
  ["MIAMI", -80.19, 25.775, "city"],
  ["MIAMI BEACH", -80.13, 25.82, "city"],
  ["FORT LAUDERDALE", -80.14, 26.12, "city"],
  ["KEY BISCAYNE", -80.16, 25.69, "place"],
  ["BIMINI", -79.28, 25.73, "place"],
  ["FLORIDA STRAITS", -79.75, 25.05, "water"],
  ["GULF STREAM ↑ 1.5 M/S", -79.84, 26.3, "stream"],
  ["NOAA 8723214", -80.1618, 25.7314, "station"],
  ["UPPER KEYS", -80.45, 25.1, "place"],
];

/** One node input a control sets live, by the node's name in the graph, and how the control's value becomes the input's. */
type InputSetLive = readonly [nodeName: string, input: string, shape?: (value: number) => JsonValue];

/** A heading over the controls after it. */
type ControlGroup = { group: string };
/** A slider. `local` names the view's own copy of its value, which the labels follow. */
type SliderControl = { label: string; min: number; max: number; step: number; value: number; sets: readonly InputSetLive[]; hero?: boolean; local?: "morph" };
/** A choice among options, its value the chosen option's index. */
type OptionsControl = { label: string; options: readonly string[]; value: number; sets: readonly InputSetLive[] };
type StageControl = ControlGroup | SliderControl | OptionsControl;

/** Each control sets one or more node inputs live, by the node's name in the graph. */
const CONTROLS: readonly StageControl[] = [
  { group: "The reveal" },
  { label: "Real map ↔ Neon", hero: true, min: 0, max: 1, step: 0.01, value: 0, sets: [["smooth_morph", "x"]], local: "morph" },
  { group: "Sky" },
  { label: "Time of day", min: 0, max: 1, step: 0.01, value: 0.3, sets: [["smooth_time_of_day", "x"]] },
  { group: "Ocean · live data" },
  { label: "Data", options: ["Logged snapshot (19 Nov 2026)", "Live NOAA feed"], value: 0, sets: [["noaa_virginia_key", "mode", (value) => (value === 1 ? "live" : "replay")]] },
  { label: "Current speed", min: 0.2, max: 3, step: 0.05, value: 1, sets: [["smooth_current_speed", "x"]] },
  { label: "Turbulence", min: 0, max: 4, step: 0.05, value: 1, sets: [["hycom_gulf_stream", "turbulence"]] },
  { label: "Particles (millions)", min: 0.1, max: 4, step: 0.1, value: 2, sets: [["hycom_gulf_stream", "active_particles", (value) => Math.round(value * 1048576)]] },
  { group: "Neon" },
  { label: "Neon intensity", min: 0, max: 2.5, step: 0.05, value: 1, sets: [["smooth_neon", "x"]] },
  { label: "Bloom", min: 0, max: 1.2, step: 0.02, value: 0.25, sets: [["bloom_from_kick", "to_low"], ["bloom_from_kick", "to_high", (value) => value * 2]] },
  { label: "Skyline height", min: 0, max: 0.12, step: 0.002, value: 0.055, sets: [["smooth_skyline_height", "x"]] },
  { label: "Exposure", min: 0.4, max: 1.8, step: 0.02, value: 1, sets: [["exposure_through_drop", "a"], ["exposure_through_drop", "b"]] },
];

/** The element of the page `selector` finds, which must be a `kind`. */
function theElement<Kind extends Element>(selector: string, kind: abstract new () => Kind): Kind {
  const element = document.querySelector(selector);
  if (!(element instanceof kind)) throw new Error(`the stage page holds no ${selector}`);
  return element;
}

/** The 2D drawing of `canvas`. */
function drawingOf(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const drawing = canvas.getContext("2d");
  if (drawing === null) throw new Error("the stage's canvas draws no 2D picture");
  return drawing;
}

const canvas = theElement(".stage-picture", HTMLCanvasElement);
const labels = theElement(".stage-labels", HTMLCanvasElement);
const labelsDrawing = drawingOf(labels);
const notice = theElement(".stage-notice", HTMLElement);
const panelStatus = theElement(".stage-panel-status", HTMLElement);
const panelControls = theElement(".stage-panel-controls", HTMLElement);
const fpsText = theElement(".stage-fps", HTMLElement);

const camera: StageCamera = { ...STAGE_HOME, bursts: 0, burst_x: 0, burst_y: 0 };
/** The view's own copy of the values its labels follow. */
const local = { morph: 0 };
const keysDown = new Set<string>();
let connection: TatoAppViewConnection | null = null;
let projectFolder: string | null = null;

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
function sendTheControl(control: SliderControl | OptionsControl, value: number): void {
  if (connection === null || projectFolder === null) return;
  const operations: StreamGraphAction[] = control.sets.flatMap(([nodeName, input, shape]): StreamGraphAction[] => {
    const node = NODE_IDS[nodeName];
    return node === undefined ? [] : [{ operation: "set_node_input_value", stream: STREAM_ASSET, node, input, value: shape === undefined ? value : shape(value) }];
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

/** Where the picture sits inside the canvas element, letterboxed, and its shape. */
function pictureRect(): { left: number; top: number; width: number; height: number; aspect: number } {
  const box = canvas.getBoundingClientRect();
  const aspect = canvas.width / Math.max(canvas.height, 1);
  let width = box.width;
  let height = width / aspect;
  if (height > box.height) {
    height = box.height;
    width = height * aspect;
  }
  return { left: box.left + (box.width - width) / 2, top: box.top + (box.height - height) / 2, width, height, aspect };
}

/** Each ripple an eddy drop draws, where it was dropped on the picture and when. */
const ripples: { across: number; down: number; born: number }[] = [];

// The hand's motion carries on after release and eases out, and scrolling glides to its zoom.
const spin = { yaw: 0, pitch: 0 };
let zoomTarget = camera.distance;
let drag: { x: number; y: number; moved: number; at: number } | null = null;

canvas.addEventListener("pointerdown", (event) => {
  spin.yaw = 0;
  spin.pitch = 0;
  canvas.setPointerCapture(event.pointerId);
  canvas.focus();
  drag = { x: event.clientX, y: event.clientY, moved: 0, at: performance.now() };
  canvas.classList.add("is-dragging");
});
canvas.addEventListener("pointermove", (event) => {
  if (drag === null) return;
  const dx = event.clientX - drag.x;
  const dy = event.clientY - drag.y;
  const now = performance.now();
  const seconds = Math.max((now - drag.at) / 1000, 1 / 240);
  drag = { x: event.clientX, y: event.clientY, moved: drag.moved + Math.abs(dx) + Math.abs(dy), at: now };
  camera.yaw_degrees -= dx * DEGREES_PER_PIXEL;
  camera.pitch_degrees = Math.max(3, Math.min(89.5, camera.pitch_degrees + dy * DEGREES_PER_PIXEL));
  spin.yaw = spin.yaw * 0.6 + ((-dx * DEGREES_PER_PIXEL) / seconds) * 0.4;
  spin.pitch = spin.pitch * 0.6 + ((dy * DEGREES_PER_PIXEL) / seconds) * 0.4;
  sendTheCamera();
});

/** Drop an eddy on the water under `event`, which the current carries north. */
function dropAnEddy(event: PointerEvent): void {
  const rect = pictureRect();
  const across = (event.clientX - rect.left) / rect.width;
  const down = (event.clientY - rect.top) / rect.height;
  if (across < 0 || across > 1 || down < 0 || down > 1) return;
  const point = seaPointAt(stageCameraFrame(camera), across, down, rect.aspect);
  if (point === null) return;
  [camera.burst_x, camera.burst_y] = point;
  camera.bursts += 1;
  ripples.push({ across, down, born: performance.now() });
  cameraLastSentAt = 0;
  sendTheCamera();
}

function endTheDrag(event: PointerEvent): void {
  if (drag === null) return;
  const wasAClick = drag.moved < CLICK_SLOP_PX;
  drag = null;
  canvas.classList.remove("is-dragging");
  if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
  if (wasAClick && event.type === "pointerup") dropAnEddy(event);
}
canvas.addEventListener("pointerup", endTheDrag);
canvas.addEventListener("pointercancel", endTheDrag);
canvas.addEventListener(
  "wheel",
  (event) => {
    event.preventDefault();
    zoomTarget = Math.max(0.03, Math.min(4.5, zoomTarget * Math.exp(event.deltaY * 0.0015)));
  },
  { passive: false },
);
window.addEventListener("keydown", (event) => {
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
  keysDown.add(event.code);
});
window.addEventListener("keyup", (event) => keysDown.delete(event.code));
window.addEventListener("blur", () => keysDown.clear());

/** Fly the camera's target over the sea with the keys held, for `seconds`. */
function flyWithTheKeys(seconds: number): void {
  const held = (...codes: string[]) => (codes.some((code) => keysDown.has(code)) ? 1 : 0);
  const ahead = held("KeyW", "ArrowUp") - held("KeyS", "ArrowDown");
  const aside = held("KeyD", "ArrowRight") - held("KeyA", "ArrowLeft");
  if (ahead === 0 && aside === 0) return;
  const yaw = (camera.yaw_degrees * Math.PI) / 180;
  const speed = camera.distance * 0.9 * seconds;
  camera.target_x += (Math.sin(yaw) * ahead + Math.cos(yaw) * aside) * speed;
  camera.target_y += (Math.cos(yaw) * ahead - Math.sin(yaw) * aside) * speed;
  sendTheCamera();
}

theElement(".stage-home", HTMLButtonElement).addEventListener("click", () => {
  Object.assign(camera, STAGE_HOME);
  zoomTarget = camera.distance;
  spin.yaw = 0;
  spin.pitch = 0;
  sendTheCamera();
});

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
  row.dataset["control"] = control.label;
  const picker = document.createElement("select");
  picker.id = id;
  control.options.forEach((name, optionIndex) => picker.append(new Option(name, String(optionIndex), optionIndex === control.value, optionIndex === control.value)));
  picker.addEventListener("change", () => sendTheControl(control, Number(picker.value)));
  row.append(label, picker);
  return row;
}

/** The row of a slider, which a double-click returns to its starting value. */
function sliderRow(control: SliderControl, id: string, label: HTMLLabelElement): HTMLElement {
  const row = document.createElement("div");
  row.className = control.hero === true ? "stage-control is-hero" : "stage-control";
  row.dataset["control"] = control.label;
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
  const show = (value: number) => {
    shown.textContent = formatted(control, value);
    if (control.local !== undefined) local[control.local] = value;
  };
  slider.addEventListener("input", () => {
    show(Number(slider.value));
    send(Number(slider.value));
  });
  slider.addEventListener("dblclick", () => {
    slider.value = String(control.value);
    show(control.value);
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
    panelControls.append("options" in control ? optionsRow(control, id, label) : sliderRow(control, id, label));
  });
}

function sendEveryControl(): void {
  for (const control of CONTROLS) if (!("group" in control)) sendTheControl(control, control.value);
}

// ---------------------------------------------------------------- the picture, the labels and the data

const frameTimes: number[] = [];
function countAFrame(): void {
  const now = performance.now();
  frameTimes.push(now);
  while ((frameTimes[0] ?? now) < now - 1000) frameTimes.shift();
  notice.textContent = "";
}

/** Each place's label where the picture shows it, faded out as the chart turns to neon, and each eddy drop's ripple. */
function drawTheLabels(): void {
  const dpr = window.devicePixelRatio || 1;
  const box = labels.getBoundingClientRect();
  if (labels.width !== Math.round(box.width * dpr) || labels.height !== Math.round(box.height * dpr)) {
    labels.width = Math.round(box.width * dpr);
    labels.height = Math.round(box.height * dpr);
  }
  labelsDrawing.setTransform(dpr, 0, 0, dpr, 0, 0);
  labelsDrawing.clearRect(0, 0, box.width, box.height);
  const rect = pictureRect();
  const offsetX = rect.left - box.left;
  const offsetY = rect.top - box.top;
  const frame = stageCameraFrame(camera);
  const opacity = Math.max(0, 1 - local.morph * 1.6);
  if (opacity > 0.01 && canvas.width > 0) {
    labelsDrawing.globalAlpha = opacity;
    labelsDrawing.textBaseline = "middle";
    for (const [name, longitude, latitude, kind] of PLACES) {
      const [x, y] = worldOf(longitude, latitude);
      const at = projectedOnThePicture(frame, [x, y, 0], rect.aspect);
      if (at === null || at[0] < 0.02 || at[0] > 0.98 || at[1] < 0.02 || at[1] > 0.98) continue;
      const pixelX = offsetX + at[0] * rect.width;
      const pixelY = offsetY + at[1] * rect.height;
      const size = Math.max(11, Math.min(20, rect.height / 44));
      const isOnTheWater = kind === "water" || kind === "stream";
      // The Tato look: Tektur sign lettering for places, JetBrains Mono for the station's code.
      labelsDrawing.font = kind === "station" ? `700 ${size * 0.9}px "JetBrains Mono", monospace` : `${isOnTheWater ? 900 : 800} ${size}px Tektur, sans-serif`;
      if (!isOnTheWater) {
        labelsDrawing.fillStyle = kind === "station" ? INK.mint : INK.cream;
        labelsDrawing.beginPath();
        labelsDrawing.arc(pixelX, pixelY, kind === "station" ? 4 : 3, 0, Math.PI * 2);
        labelsDrawing.fill();
      }
      const labelX = pixelX + (isOnTheWater ? -labelsDrawing.measureText(name).width / 2 : 9);
      // An offset ink shadow, like the site's sign lettering.
      labelsDrawing.fillStyle = INK.ink;
      labelsDrawing.fillText(name, labelX + size * 0.07, pixelY + size * 0.08);
      labelsDrawing.fillStyle = kind === "stream" ? INK.yellow : kind === "station" ? INK.mint : INK.cream;
      labelsDrawing.fillText(name, labelX, pixelY);
    }
    labelsDrawing.globalAlpha = 1;
  }
  const now = performance.now();
  for (let index = ripples.length - 1; index >= 0; index -= 1) {
    const ripple = ripples[index];
    if (ripple === undefined) continue;
    const age = (now - ripple.born) / 900;
    if (age > 1) {
      ripples.splice(index, 1);
      continue;
    }
    labelsDrawing.strokeStyle = `rgba(47, 214, 160, ${1 - age})`;
    labelsDrawing.lineWidth = 2;
    labelsDrawing.beginPath();
    labelsDrawing.arc(offsetX + ripple.across * rect.width, offsetY + ripple.down * rect.height, 6 + age * 40, 0, Math.PI * 2);
    labelsDrawing.stroke();
  }
}

/** Carry the hand's spin on after release and glide the zoom toward where the scroll left it, for `seconds`. */
function glide(seconds: number): void {
  let moved = false;
  if (drag === null && Math.abs(spin.yaw) + Math.abs(spin.pitch) > 0.5) {
    camera.yaw_degrees += spin.yaw * seconds;
    camera.pitch_degrees = Math.max(3, Math.min(89.5, camera.pitch_degrees + spin.pitch * seconds));
    const keep = Math.exp(-seconds / 0.35);
    spin.yaw *= keep;
    spin.pitch *= keep;
    moved = true;
  }
  if (Math.abs(zoomTarget - camera.distance) > 1e-4) {
    camera.distance += (zoomTarget - camera.distance) * (1 - Math.exp(-seconds / 0.12));
    moved = true;
  }
  if (moved) sendTheCamera();
}

let lastTick = performance.now();
function tick(now: number): void {
  const seconds = Math.min(0.1, (now - lastTick) / 1000);
  flyWithTheKeys(seconds);
  glide(seconds);
  lastTick = now;
  drawTheLabels();
  fpsText.textContent = `${frameTimes.length} FPS · ${canvas.width}×${canvas.height} · LIVE ON THE GPU`;
  requestAnimationFrame(tick);
}

const COMPASS = ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];

/** The number `record` holds under `name`, 0 where it holds none. */
function numberIn(record: JsonValue, name: string): number {
  if (typeof record !== "object" || record === null || Array.isArray(record)) return 0;
  const value = record[name];
  return typeof value === "number" ? value : 0;
}

/** The station's reading `record`, in the live data panel. */
function showTheStation(record: JsonValue): void {
  const at = new Date(numberIn(record, "observed_at_unix") * 1000).toISOString();
  const show = (selector: string, words: string) => {
    theElement(selector, HTMLElement).textContent = words;
  };
  show(".d-time", `${at.slice(0, 10)} ${at.slice(11, 16)} UTC`);
  show(".d-level", `${numberIn(record, "water_level_m").toFixed(3)} m MLLW`);
  const from = COMPASS[Math.round(numberIn(record, "wind_from_degrees") / 22.5) % 16] ?? "N";
  show(".d-wind", `${numberIn(record, "wind_speed_mps").toFixed(1)} m/s ${from} · gust ${numberIn(record, "wind_gust_mps").toFixed(1)}`);
  show(".d-water", `${numberIn(record, "water_temperature_c").toFixed(1)} °C`);
  show(".d-air", `${numberIn(record, "air_temperature_c").toFixed(1)} °C · ${numberIn(record, "air_pressure_hpa").toFixed(1)} hPa`);
  show(".stage-data-mode", numberIn(record, "live") > 0.5 ? "■ Live · NOAA CO-OPS API" : "Logged snapshot · replaying the last 24 h");
}

function onVideo(inputEvent: ViewInputEvent): void {
  switch (inputEvent.kind) {
    case "image_frame":
      drawThePortViewerFrame(canvas, inputEvent.video_frame);
      inputEvent.video_frame.close();
      countAFrame();
      return;
    case "image_pixels":
      if (drawThePortViewerPixels(canvas, inputEvent.width, inputEvent.height, inputEvent.format_wire_name, inputEvent.bytes_per_row, inputEvent.pixels)) countAFrame();
      else notice.textContent = `The stage can't draw ${inputEvent.format_wire_name} pixels.`;
      return;
    case "message":
      notice.textContent = `video carries ${wordsOfTheMessageRead(inputEvent.reading)}, not a picture.`;
      return;
    case "unwired":
    case "reopening":
    case "ended":
      notice.textContent = inputEvent.because;
      return;
  }
}

function onStation(inputEvent: ViewInputEvent): void {
  if (inputEvent.kind === "message" && inputEvent.reading.message === "records_message") showTheStation(inputEvent.reading.record);
}

function onOpenProject(reading: OpenProjectReading): void {
  projectFolder = reading.kind === "read" ? reading.open_project.project.folder : null;
  if (projectFolder === null) {
    panelStatus.textContent = "No project is open.";
    return;
  }
  panelStatus.textContent = "Live: every slider changes the running stream as you drag.";
  sendEveryControl();
}

buildThePanel();
requestAnimationFrame(tick);
// The canvas labels draw in the bundled fonts once they have loaded.
for (const face of ["800 16px Tektur", "900 16px Tektur", '700 16px "JetBrains Mono"']) void document.fonts.load(face);
void connectToTatoApp().then((connected) => {
  connection = connected;
  const refused = (because: string) => {
    notice.textContent = because;
  };
  connected.watchInput("video", onVideo, refused);
  connected.watchInput("station", onStation);
  connected.watchOpenProject(onOpenProject, (because) => {
    panelStatus.textContent = because;
  });
  sendTheCamera();
});

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// View contract 1.0: Studio hands this frame a MessagePort in a `tatolab_view_port` message,
// says `hello` over it, and answers each `ask` under its `ask_id`. The view watches its
// `video` input to show the picture and its `station` input for the live data panel, sends
// its `camera` Records output with the `send_output` ask, and sets node inputs live, by the
// node's name in the graph, with a live project transaction.
import { NODE_IDS, STREAM_ASSET, STAGE_FOV_DEGREES, STAGE_HOME } from "./stage_graph.js";

const VIEW_PORT_HANDOVER = "tatolab_view_port";
const VIDEO_WATCH_ID = 1;
const OPEN_PROJECT_WATCH_ID = 2;
const STATION_WATCH_ID = 5;
// A live edit is a project transaction; a few a second keeps the runtime with the hand.
const CONTROL_SEND_EVERY_MS = 250;
const SEND_EVERY_MS = 33;
const DEGREES_PER_PIXEL = 0.25;
const CLICK_SLOP_PX = 4;

// The real map's square, as the renderer maps it onto world units from -2.5 to 2.5.
const MAP = { south: 24.3, north: 27.3, west: -81.0, east: -77.67, half: 2.5 };
// The Tato inks, from tatolab.com.
const INK = { blue: "#2a5bff", pink: "#ff3d8b", yellow: "#ffd60a", cream: "#fff4e0", ink: "#14101f", mint: "#2fd6a0", violet: "#5b2ee6", band: "#0d1a4a" };
const PLACES = [
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

// Each control sets one or more node inputs live, by the node's name in the graph.
const CONTROLS = [
  { group: "The reveal" },
  { label: "Real map ↔ Neon", hero: true, min: 0, max: 1, step: 0.01, value: 0, sets: [["neon_horizon", "morph"]], local: "morph" },
  { group: "Sky" },
  { label: "Time of day", min: 0, max: 1, step: 0.01, value: 0.3, sets: [["neon_horizon", "time_of_day"], ["stage_sun", "time_of_day"]] },
  { group: "Ocean · live data" },
  { label: "Data", options: ["Logged snapshot (19 Nov 2026)", "Live NOAA feed"], value: 0, sets: [["noaa_virginia_key", "mode", (v) => (v === 1 ? "live" : "replay")]] },
  { label: "Current speed", min: 0.2, max: 3, step: 0.05, value: 1, sets: [["hycom_gulf_stream", "time_lapse", (v) => v * 9000]] },
  { label: "Turbulence", min: 0, max: 4, step: 0.05, value: 1, sets: [["hycom_gulf_stream", "turbulence"]] },
  { label: "Particles (millions)", min: 0.1, max: 4, step: 0.1, value: 2, sets: [["hycom_gulf_stream", "active_particles", (v) => Math.round(v * 1048576)]] },
  { group: "Neon" },
  { label: "Neon intensity", min: 0, max: 2.5, step: 0.05, value: 1, sets: [["neon_horizon", "glow"]] },
  { label: "Bloom", min: 0, max: 1.2, step: 0.02, value: 0.25, sets: [["bloom_from_kick", "to_low"], ["bloom_from_kick", "to_high", (v) => v * 2]] },
  { label: "Skyline height", min: 0, max: 0.12, step: 0.002, value: 0.055, sets: [["neon_horizon", "skyline_height"]] },
  { label: "Exposure", min: 0.4, max: 1.8, step: 0.02, value: 1, sets: [["exposure_through_drop", "a"], ["exposure_through_drop", "b"]] },
];

const canvas = document.querySelector(".stage-picture");
const labels = document.querySelector(".stage-labels");
const notice = document.querySelector(".stage-notice");
const context2d = canvas.getContext("2d");
const labels2d = labels.getContext("2d");
const panelStatus = document.querySelector(".stage-panel-status");
const panelControls = document.querySelector(".stage-panel-controls");
const fpsText = document.querySelector(".stage-fps");

const target = { projectFolder: null, streamAsset: STREAM_ASSET, nodeIds: new Map(Object.entries(NODE_IDS)) };
const camera = { ...STAGE_HOME, bursts: 0, burst_x: 0, burst_y: 0 };
const local = { morph: 0 };
const keysDown = new Set();
let port = null;
let nextAskId = 1;
let sendQueued = false;
let lastSentAt = 0;

// ---------------------------------------------------------------- the camera, as the renderer flies it

function worldOf(longitude, latitude) {
  const u = (longitude - MAP.west) / (MAP.east - MAP.west);
  const v = (latitude - MAP.south) / (MAP.north - MAP.south);
  return [(u * 2 - 1) * MAP.half, (v * 2 - 1) * MAP.half];
}

function basis(yawDegrees, pitchDegrees) {
  const yaw = (yawDegrees * Math.PI) / 180;
  const pitch = (pitchDegrees * Math.PI) / 180;
  const f = [Math.sin(yaw) * Math.cos(pitch), Math.cos(yaw) * Math.cos(pitch), -Math.sin(pitch)];
  const r = [Math.cos(yaw), -Math.sin(yaw), 0];
  const u = [r[1] * f[2] - r[2] * f[1], r[2] * f[0] - r[0] * f[2], r[0] * f[1] - r[1] * f[0]];
  return { f, r, u };
}

function stageCamera() {
  const pitch = Math.min(Math.max(camera.pitch_degrees, 2), 89.9);
  const distance = Math.min(Math.max(camera.distance, 0.02), 6);
  const b = basis(camera.yaw_degrees, pitch);
  const position = [camera.target_x - b.f[0] * distance, camera.target_y - b.f[1] * distance, Math.max(0.006, -b.f[2] * distance)];
  return { position, ...b, focal: 1 / Math.tan((STAGE_FOV_DEGREES * Math.PI) / 360) };
}

function project(cam, point, aspect) {
  const rel = [point[0] - cam.position[0], point[1] - cam.position[1], point[2] - cam.position[2]];
  const depth = rel[0] * cam.f[0] + rel[1] * cam.f[1] + rel[2] * cam.f[2];
  if (depth <= 1e-6) return null;
  const x = ((rel[0] * cam.r[0] + rel[1] * cam.r[1] + rel[2] * cam.r[2]) / depth) * cam.focal;
  const y = ((rel[0] * cam.u[0] + rel[1] * cam.u[1] + rel[2] * cam.u[2]) / depth) * cam.focal;
  return [0.5 + (0.5 * x) / aspect, 0.5 - 0.5 * y];
}

function seaPointAt(cam, u, v, aspect) {
  const x = (u - 0.5) * 2 * aspect;
  const y = (0.5 - v) * 2;
  const d = [0, 1, 2].map((i) => cam.f[i] * cam.focal + cam.r[i] * x + cam.u[i] * y);
  if (d[2] >= -1e-6) return null;
  const t = cam.position[2] / -d[2];
  return [cam.position[0] + d[0] * t, cam.position[1] + d[1] * t];
}

// Where the picture sits inside the canvas element, letterboxed.
function pictureRect() {
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

// ---------------------------------------------------------------- sending

function sendTheCamera() {
  if (port === null) return;
  const now = performance.now();
  const wait = SEND_EVERY_MS - (now - lastSentAt);
  if (wait > 0) {
    if (!sendQueued) {
      sendQueued = true;
      setTimeout(() => { sendQueued = false; sendTheCamera(); }, wait);
    }
    return;
  }
  lastSentAt = now;
  port.postMessage({ kind: "ask", ask_id: nextAskId++, ask: { kind: "send_output", output: "camera" }, payload: { ...camera } });
}

function sendTheControl(control, value) {
  if (port === null || target.projectFolder === null || target.streamAsset === null) return;
  const operations = [];
  for (const [nodeName, input, shape] of control.sets) {
    const nodeId = target.nodeIds.get(nodeName);
    if (nodeId === undefined) continue;
    operations.push({ operation: "set_node_input_value", stream: target.streamAsset, node: nodeId, input, value: shape ? shape(value) : value });
  }
  if (operations.length === 0) return;
  port.postMessage({
    kind: "ask",
    ask_id: nextAskId++,
    ask: { kind: "transact_in_the_project", project_folder: target.projectFolder, operations, mode: "live", idempotency_key: null },
  });
}

function throttled(control) {
  let lastAt = 0;
  let pending = null;
  return (value) => {
    pending = value;
    const wait = CONTROL_SEND_EVERY_MS - (performance.now() - lastAt);
    if (wait > 0) {
      if (control.timer === undefined) {
        control.timer = setTimeout(() => { control.timer = undefined; lastAt = performance.now(); sendTheControl(control, pending); }, wait);
      }
      return;
    }
    lastAt = performance.now();
    sendTheControl(control, pending);
  };
}

// ---------------------------------------------------------------- the mouse and keys

let drag = null;
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
  drag.x = event.clientX;
  drag.y = event.clientY;
  drag.moved += Math.abs(dx) + Math.abs(dy);
  camera.yaw_degrees -= dx * DEGREES_PER_PIXEL;
  camera.pitch_degrees = Math.max(3, Math.min(89.5, camera.pitch_degrees + dy * DEGREES_PER_PIXEL));
  sendTheCamera();
});
function endTheDrag(event) {
  if (drag === null) return;
  const wasAClick = drag.moved < CLICK_SLOP_PX;
  drag = null;
  canvas.classList.remove("is-dragging");
  if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
  if (wasAClick && event.type === "pointerup") dropAnEddy(event);
}
function dropAnEddy(event) {
  const rect = pictureRect();
  const u = (event.clientX - rect.left) / rect.width;
  const v = (event.clientY - rect.top) / rect.height;
  if (u < 0 || u > 1 || v < 0 || v > 1) return;
  const point = seaPointAt(stageCamera(), u, v, rect.aspect);
  if (point === null) return;
  camera.burst_x = point[0];
  camera.burst_y = point[1];
  camera.bursts += 1;
  ripples.push({ u, v, born: performance.now() });
  lastSentAt = 0;
  sendTheCamera();
}
canvas.addEventListener("pointerup", endTheDrag);
canvas.addEventListener("pointercancel", endTheDrag);
canvas.addEventListener("wheel", (event) => {
  event.preventDefault();
  camera.distance = Math.max(0.03, Math.min(4.5, camera.distance * Math.exp(event.deltaY * 0.0015)));
  sendTheCamera();
}, { passive: false });
window.addEventListener("keydown", (event) => {
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
  keysDown.add(event.code);
});
window.addEventListener("keyup", (event) => keysDown.delete(event.code));
window.addEventListener("blur", () => keysDown.clear());

function flyWithTheKeys(seconds) {
  const ahead = (keysDown.has("KeyW") || keysDown.has("ArrowUp") ? 1 : 0) - (keysDown.has("KeyS") || keysDown.has("ArrowDown") ? 1 : 0);
  const aside = (keysDown.has("KeyD") || keysDown.has("ArrowRight") ? 1 : 0) - (keysDown.has("KeyA") || keysDown.has("ArrowLeft") ? 1 : 0);
  if (ahead === 0 && aside === 0) return;
  const yaw = (camera.yaw_degrees * Math.PI) / 180;
  const speed = camera.distance * 0.9 * seconds;
  camera.target_x += (Math.sin(yaw) * ahead + Math.cos(yaw) * aside) * speed;
  camera.target_y += (Math.cos(yaw) * ahead - Math.sin(yaw) * aside) * speed;
  sendTheCamera();
}

document.querySelector(".stage-home").addEventListener("click", () => {
  Object.assign(camera, STAGE_HOME);
  sendTheCamera();
});

// ---------------------------------------------------------------- the panel

function formatted(control, value) {
  if (control.step >= 1) return value.toFixed(0);
  if (control.step >= 0.01) return value.toFixed(2);
  return value.toFixed(3);
}

function buildThePanel() {
  CONTROLS.forEach((control, index) => {
    if (control.group !== undefined) {
      const heading = document.createElement("h3");
      heading.className = "stage-panel-group";
      heading.textContent = control.group;
      panelControls.append(heading);
      return;
    }
    const row = document.createElement("div");
    row.className = control.hero ? "stage-control is-hero" : "stage-control";
    row.dataset.control = control.label;
    const label = document.createElement("label");
    label.htmlFor = `stage-control-${index}`;
    label.textContent = control.label;
    if (control.options !== undefined) {
      row.classList.add("is-select");
      const picker = document.createElement("select");
      picker.id = label.htmlFor;
      control.options.forEach((name, optionIndex) => picker.append(new Option(name, String(optionIndex), optionIndex === control.value, optionIndex === control.value)));
      picker.addEventListener("change", () => sendTheControl(control, Number(picker.value)));
      row.append(label, picker);
      panelControls.append(row);
      return;
    }
    const input = document.createElement("input");
    input.id = label.htmlFor;
    input.type = "range";
    input.min = String(control.min);
    input.max = String(control.max);
    input.step = String(control.step);
    input.value = String(control.value);
    const shown = document.createElement("output");
    shown.htmlFor = input.id;
    shown.textContent = formatted(control, control.value);
    const send = throttled(control);
    input.addEventListener("input", () => {
      const value = Number(input.value);
      shown.textContent = formatted(control, value);
      if (control.local) local[control.local] = value;
      send(value);
    });
    input.addEventListener("dblclick", () => {
      input.value = String(control.value);
      shown.textContent = formatted(control, control.value);
      if (control.local) local[control.local] = control.value;
      sendTheControl(control, control.value);
    });
    row.append(label, shown, input);
    panelControls.append(row);
  });
}

function sendEveryControl() {
  CONTROLS.forEach((control) => {
    if (control.sets !== undefined) sendTheControl(control, control.value);
  });
}

// ---------------------------------------------------------------- the picture, the labels and the data

const frameTimes = [];
function countAFrame() {
  const now = performance.now();
  frameTimes.push(now);
  while (frameTimes.length > 0 && frameTimes[0] < now - 1000) frameTimes.shift();
}

function showThePicture(source, width, height) {
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  context2d.drawImage(source, 0, 0, width, height);
  notice.textContent = "";
  countAFrame();
}

function showThePixels(event) {
  const { width, height, bytes_per_row: bytesPerRow, pixels } = event;
  const isBgra = event.format_wire_name.startsWith("bgra");
  const rgba = new Uint8ClampedArray(width * height * 4);
  for (let row = 0; row < height; row += 1) {
    rgba.set(pixels.subarray(row * bytesPerRow, row * bytesPerRow + width * 4), row * width * 4);
  }
  if (isBgra) {
    for (let offset = 0; offset < rgba.length; offset += 4) {
      const blue = rgba[offset];
      rgba[offset] = rgba[offset + 2];
      rgba[offset + 2] = blue;
    }
  }
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  context2d.putImageData(new ImageData(rgba, width, height), 0, 0);
  notice.textContent = "";
  countAFrame();
}

const ripples = [];
function drawTheLabels() {
  const dpr = window.devicePixelRatio || 1;
  const box = labels.getBoundingClientRect();
  if (labels.width !== Math.round(box.width * dpr) || labels.height !== Math.round(box.height * dpr)) {
    labels.width = Math.round(box.width * dpr);
    labels.height = Math.round(box.height * dpr);
  }
  labels2d.setTransform(dpr, 0, 0, dpr, 0, 0);
  labels2d.clearRect(0, 0, box.width, box.height);
  const rect = pictureRect();
  const ox = rect.left - box.left;
  const oy = rect.top - box.top;
  const cam = stageCamera();
  const opacity = Math.max(0, 1 - local.morph * 1.6);
  if (opacity > 0.01 && canvas.width > 0) {
    labels2d.globalAlpha = opacity;
    labels2d.textBaseline = "middle";
    for (const [name, longitude, latitude, kind] of PLACES) {
      const [x, y] = worldOf(longitude, latitude);
      const at = project(cam, [x, y, 0], rect.aspect);
      if (at === null || at[0] < 0.02 || at[0] > 0.98 || at[1] < 0.02 || at[1] > 0.98) continue;
      const px = ox + at[0] * rect.width;
      const py = oy + at[1] * rect.height;
      const size = Math.max(11, Math.min(20, rect.height / 44));
      // The Tato look: Tektur sign lettering for places, JetBrains Mono for the station's code.
      labels2d.font = kind === "station" ? `700 ${size * 0.9}px "JetBrains Mono", monospace` : `${kind === "water" || kind === "stream" ? 900 : 800} ${size}px Tektur, sans-serif`;
      if (kind === "city" || kind === "place" || kind === "station") {
        labels2d.fillStyle = kind === "station" ? INK.mint : INK.cream;
        labels2d.beginPath();
        labels2d.arc(px, py, kind === "station" ? 4 : 3, 0, Math.PI * 2);
        labels2d.fill();
      }
      const labelX = px + (kind === "water" || kind === "stream" ? -labels2d.measureText(name).width / 2 : 9);
      // An offset ink shadow, like the site's sign lettering.
      labels2d.fillStyle = INK.ink;
      labels2d.fillText(name, labelX + size * 0.07, py + size * 0.08);
      labels2d.fillStyle = kind === "stream" ? INK.yellow : kind === "water" ? INK.cream : kind === "station" ? INK.mint : INK.cream;
      labels2d.fillText(name, labelX, py);
    }
    labels2d.globalAlpha = 1;
  }
  const now = performance.now();
  for (let i = ripples.length - 1; i >= 0; i -= 1) {
    const age = (now - ripples[i].born) / 900;
    if (age > 1) { ripples.splice(i, 1); continue; }
    labels2d.strokeStyle = `rgba(47, 214, 160, ${1 - age})`;
    labels2d.lineWidth = 2;
    labels2d.beginPath();
    labels2d.arc(ox + ripples[i].u * rect.width, oy + ripples[i].v * rect.height, 6 + age * 40, 0, Math.PI * 2);
    labels2d.stroke();
  }
}

let lastTick = performance.now();
function tick(now) {
  flyWithTheKeys(Math.min(0.1, (now - lastTick) / 1000));
  lastTick = now;
  drawTheLabels();
  fpsText.textContent = `${frameTimes.length} FPS · ${canvas.width}×${canvas.height} · LIVE ON THE GPU`;
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);

const COMPASS = ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];
function showTheStation(record) {
  const at = new Date(Number(record.observed_at_unix) * 1000);
  const stamp = `${at.toISOString().slice(0, 10)} ${at.toISOString().slice(11, 16)} UTC`;
  document.querySelector(".d-time").textContent = stamp;
  document.querySelector(".d-level").textContent = `${Number(record.water_level_m).toFixed(3)} m MLLW`;
  const from = COMPASS[Math.round(Number(record.wind_from_degrees) / 22.5) % 16];
  document.querySelector(".d-wind").textContent = `${Number(record.wind_speed_mps).toFixed(1)} m/s ${from} · gust ${Number(record.wind_gust_mps).toFixed(1)}`;
  document.querySelector(".d-water").textContent = `${Number(record.water_temperature_c).toFixed(1)} °C`;
  document.querySelector(".d-air").textContent = `${Number(record.air_temperature_c).toFixed(1)} °C · ${Number(record.air_pressure_hpa).toFixed(1)} hPa`;
  document.querySelector(".stage-data-mode").textContent = Number(record.live) > 0.5 ? "■ Live · NOAA CO-OPS API" : "Logged snapshot · replaying the last 24 h";
}

function onVideoReading(reading) {
  switch (reading.kind) {
    case "image_frame":
      showThePicture(reading.video_frame, reading.video_frame.displayWidth, reading.video_frame.displayHeight);
      reading.video_frame.close();
      return;
    case "image_pixels":
      showThePixels(reading);
      return;
    case "unwired":
    case "reopening":
    case "ended":
      notice.textContent = reading.because;
      return;
  }
}

function onStationReading(reading) {
  if (reading.kind === "message" && reading.reading?.family === "records") showTheStation(reading.reading.record);
}

function onOpenProject(reading) {
  target.projectFolder = reading.kind === "read" ? reading.open_project.project.folder : null;
  if (target.projectFolder === null) {
    panelStatus.textContent = "No project is open.";
    return;
  }
  panelStatus.textContent = "Live: every slider changes the running stream as you drag.";
  sendEveryControl();
}

function talkOver(handedPort) {
  port = handedPort;
  port.addEventListener("message", ({ data: message }) => {
    if (message?.kind === "hello") {
      port.postMessage({ kind: "watch", watch_id: VIDEO_WATCH_ID, subject: { kind: "input", input: "video" } });
      port.postMessage({ kind: "watch", watch_id: STATION_WATCH_ID, subject: { kind: "input", input: "station" } });
      port.postMessage({ kind: "watch", watch_id: OPEN_PROJECT_WATCH_ID, subject: { kind: "open_project" } });
      sendTheCamera();
      return;
    }
    if (message?.kind === "watched" && message.watch_id === VIDEO_WATCH_ID && message.watched.subject === "input") {
      onVideoReading(message.watched.reading);
    } else if (message?.kind === "watched" && message.watch_id === STATION_WATCH_ID && message.watched.subject === "input") {
      onStationReading(message.watched.reading);
    } else if (message?.kind === "watched" && message.watched.subject === "open_project") {
      onOpenProject(message.watched.reading);
    } else if (message?.kind === "watch_refused" && message.watch_id === VIDEO_WATCH_ID) {
      notice.textContent = message.because;
    } else if (message?.kind === "answered" && message.answer?.kind === "refused") {
      notice.textContent = message.answer.because;
    } else if (message?.kind === "answered" && message.answer?.kind === "project_action_refused") {
      panelStatus.textContent = message.answer.refusal?.runtime_words ?? "The runtime refused that change.";
    }
  });
  port.start();
}

buildThePanel();
// The canvas labels draw in the bundled fonts once they have loaded.
for (const face of ["800 16px Tektur", "900 16px Tektur", "700 16px \"JetBrains Mono\""]) void document.fonts.load(face);
window.addEventListener("message", function onHandover(event) {
  const handedPort = event.ports[0];
  if (event.source !== window.parent || event.data?.kind !== VIEW_PORT_HANDOVER || handedPort === undefined) return;
  window.removeEventListener("message", onHandover);
  talkOver(handedPort);
});

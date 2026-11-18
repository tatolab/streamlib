// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// View contract 1.0: Studio hands this frame a MessagePort in a `tatolab_view_port` message,
// says `hello` over it, and answers each `ask` under its `ask_id`. The view watches its
// `video` input to show the picture, and sends its `camera` Records output with the
// `send_output` ask, which Studio sets live on this node so the stream picks it up.
const VIEW_PORT_HANDOVER = "tatolab_view_port";
const VIDEO_WATCH_ID = 1;
const OPEN_PROJECT_WATCH_ID = 2;
const LISTED_STREAMS_WATCH_ID = 3;
const STREAM_GRAPH_WATCH_ID = 4;
const CONTROL_SEND_EVERY_MS = 60;

// Each control sets one or more node inputs live, by the node's name in the graph. Today these
// are node settings set through a live graph transaction; once typed pins land they are pins.
const CONTROLS = [
  { group: "Music" },
  { label: "Volume", min: 0, max: 1, step: 0.01, value: 0.8, sets: [["synth_song", "volume"]] },
  { label: "Sensitivity", min: 0.3, max: 3, step: 0.05, value: 1.25, sets: [["spectrum_listener", "sensitivity"]] },
  { label: "Explosion", min: 0, max: 2, step: 0.05, value: 1.2, sets: [["spectrum_listener", "spread"]] },
  { group: "Ball" },
  { label: "Spin", min: -0.6, max: 0.6, step: 0.01, value: 0.12, sets: [["spin", "rate"]] },
  { label: "Flow", toggle: true, value: true, sets: [["paint_ball", "flow"]] },
  { group: "Paint" },
  { label: "Brush size", min: 4, max: 30, step: 0.5, value: 11, sets: [["stroke_from_bass", "to_low"], ["stroke_from_bass", "to_high", (v) => v * 2]] },
  { label: "Paint", min: 0, max: 1, step: 0.01, value: 1, sets: [["painterly", "amount"]] },
  { label: "Canvas", min: 0, max: 0.1, step: 0.005, value: 0.03, sets: [["painterly", "canvas"]] },
  { group: "Colour" },
  { label: "Saturation", min: 0.5, max: 2.5, step: 0.05, value: 1.5, sets: [["saturation_from_energy", "to_high"]] },
  { label: "Hue", min: -0.5, max: 0.5, step: 0.01, value: 0, sets: [["colour", "hue_shift"]] },
  { label: "Kick split", min: 0, max: 0.03, step: 0.001, value: 0.008, sets: [["split_from_beat", "to_high"]] },
  { label: "Vignette", min: 0, max: 1, step: 0.01, value: 0.55, sets: [["vignette", "amount"]] },
];
const SEND_EVERY_MS = 33;
const RADIANS_PER_PIXEL = 0.008;
const CLICK_SLOP_PX = 4;

const canvas = document.querySelector(".stage-picture");
const notice = document.querySelector(".stage-notice");
const context2d = canvas.getContext("2d");
const panelStatus = document.querySelector(".stage-panel-status");
const panelControls = document.querySelector(".stage-panel-controls");

// Where a live edit goes: the project's folder, the stream's asset, and each node's id by name.
const target = { projectFolder: null, streamId: null, streamAsset: null, nodeIds: new Map() };

const camera = { yaw: 0, pitch: 0, zoom: 1, burst: 0 };
let port = null;
let nextAskId = 1;
let sendQueued = false;
let lastSentAt = 0;

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

function burst() {
  camera.burst += 1;
  lastSentAt = 0;
  sendTheCamera();
}

function resetTheView() {
  camera.yaw = 0;
  camera.pitch = 0;
  camera.zoom = 1;
  sendTheCamera();
}

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
  camera.yaw -= dx * RADIANS_PER_PIXEL;
  camera.pitch = Math.max(-1.3, Math.min(1.3, camera.pitch + dy * RADIANS_PER_PIXEL));
  sendTheCamera();
});
function endTheDrag(event) {
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
canvas.addEventListener("wheel", (event) => {
  event.preventDefault();
  camera.zoom = Math.max(0.4, Math.min(2.5, camera.zoom * Math.exp(event.deltaY * 0.0015)));
  sendTheCamera();
}, { passive: false });
window.addEventListener("keydown", (event) => {
  if (event.code === "Space" && !(event.target instanceof HTMLInputElement)) { event.preventDefault(); burst(); }
});
buildThePanel();
document.querySelector(".stage-burst").addEventListener("click", burst);
document.querySelector(".stage-reset").addEventListener("click", resetTheView);

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
    row.className = control.toggle ? "stage-control is-toggle" : "stage-control";
    const input = document.createElement("input");
    input.id = `stage-control-${index}`;
    const label = document.createElement("label");
    label.htmlFor = input.id;
    label.textContent = control.label;
    if (control.toggle) {
      input.type = "checkbox";
      input.checked = control.value;
      input.addEventListener("change", () => sendTheControl(control, input.checked));
      row.append(input, label);
    } else {
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
        send(value);
      });
      input.addEventListener("dblclick", () => {
        input.value = String(control.value);
        shown.textContent = formatted(control, control.value);
        sendTheControl(control, control.value);
      });
      row.append(label, shown, input);
    }
    panelControls.append(row);
  });
}

function onOpenProject(reading) {
  target.projectFolder = reading.kind === "read" ? reading.open_project.project.folder : null;
  if (target.projectFolder === null) panelStatus.textContent = "No project is open.";
}

function onListedStreams(reading) {
  if (reading.kind !== "listed") {
    panelStatus.textContent = reading.because;
    return;
  }
  const ours = reading.streams.find((stream) => stream.location !== null && stream.location.project_directory === target.projectFolder)
    ?? reading.streams.find((stream) => stream.stream.toLowerCase().startsWith("paint-pulse"));
  if (ours === undefined) {
    panelStatus.textContent = "Paint Pulse's stream isn't listed.";
    return;
  }
  if (target.projectFolder === null && ours.location !== null) target.projectFolder = ours.location.project_directory;
  if (ours.stream !== target.streamId) {
    target.streamId = ours.stream;
    port.postMessage({ kind: "watch", watch_id: STREAM_GRAPH_WATCH_ID, subject: { kind: "stream_graph", stream: ours.stream } });
  }
}

function onStreamGraph(reading) {
  if (reading.kind !== "read") {
    panelStatus.textContent = reading.because;
    return;
  }
  target.streamAsset = reading.graph.stream_asset;
  target.nodeIds = new Map(reading.graph.nodes.map((graphNode) => [graphNode.name, graphNode.id]));
  panelStatus.textContent = target.streamAsset === null ? "The stream has no graph to edit." : "Live: changes apply as you drag. Double-click a slider to reset it.";
}

function showThePicture(source, width, height) {
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  context2d.drawImage(source, 0, 0, width, height);
  notice.textContent = "";
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

function talkOver(handedPort) {
  port = handedPort;
  port.addEventListener("message", ({ data: message }) => {
    if (message?.kind === "hello") {
      port.postMessage({ kind: "watch", watch_id: VIDEO_WATCH_ID, subject: { kind: "input", input: "video" } });
      port.postMessage({ kind: "watch", watch_id: OPEN_PROJECT_WATCH_ID, subject: { kind: "open_project" } });
      port.postMessage({ kind: "watch", watch_id: LISTED_STREAMS_WATCH_ID, subject: { kind: "listed_streams" } });
      sendTheCamera();
      return;
    }
    if (message?.kind === "watched" && message.watch_id === VIDEO_WATCH_ID && message.watched.subject === "input") {
      onVideoReading(message.watched.reading);
    } else if (message?.kind === "watched" && message.watched.subject === "open_project") {
      onOpenProject(message.watched.reading);
    } else if (message?.kind === "watched" && message.watched.subject === "listed_streams") {
      onListedStreams(message.watched.reading);
    } else if (message?.kind === "watched" && message.watched.subject === "stream_graph") {
      onStreamGraph(message.watched.reading);
    } else if (message?.kind === "watch_refused") {
      notice.textContent = message.because;
    } else if (message?.kind === "answered" && message.answer?.kind === "refused") {
      notice.textContent = message.answer.because;
    } else if (message?.kind === "answered" && message.answer?.kind === "project_action_refused") {
      panelStatus.textContent = message.answer.refusal?.runtime_words ?? "The runtime refused that change.";
    }
  });
  port.start();
}

window.addEventListener("message", function onHandover(event) {
  const handedPort = event.ports[0];
  if (event.source !== window.parent || event.data?.kind !== VIEW_PORT_HANDOVER || handedPort === undefined) return;
  window.removeEventListener("message", onHandover);
  talkOver(handedPort);
});

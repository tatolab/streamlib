// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// View contract 1.0: Studio hands this frame a MessagePort in a `tatolab_view_port` message,
// says `hello` over it, and answers each `ask` under its `ask_id`. The view watches its
// `video` input to show the picture, and sends its `camera` Records output with the
// `send_output` ask, which Studio sets live on this node so the stream picks it up.
const VIEW_PORT_HANDOVER = "tatolab_view_port";
const VIDEO_WATCH_ID = 1;
const SEND_EVERY_MS = 33;
const RADIANS_PER_PIXEL = 0.008;
const CLICK_SLOP_PX = 4;

const canvas = document.querySelector(".stage-picture");
const notice = document.querySelector(".stage-notice");
const context2d = canvas.getContext("2d");

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
  if (event.code === "Space") { event.preventDefault(); burst(); }
});
document.querySelector(".stage-burst").addEventListener("click", burst);
document.querySelector(".stage-reset").addEventListener("click", resetTheView);

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
      sendTheCamera();
      return;
    }
    if (message?.kind === "watched" && message.watch_id === VIDEO_WATCH_ID && message.watched.subject === "input") {
      onVideoReading(message.watched.reading);
    } else if (message?.kind === "watch_refused") {
      notice.textContent = message.because;
    } else if (message?.kind === "answered" && message.answer?.kind === "refused") {
      notice.textContent = message.answer.because;
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

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

import { ProgramAudioPlayback } from "./program_audio_playback.js";

// Wire contract with Tato Studio, view contract 1.0: Studio posts one
// `tatolab_view_port` message into this frame carrying a MessagePort; over it
// Studio says `hello`, the view sends `watch` with a subject, and each reading
// arrives as `watched` under the watch's id. An `audio` Channels reading
// carries `channel_samples`, one Float32Array per channel, when Studio hands
// a view the samples.
const VIEW_PORT_HANDOVER = "tatolab_view_port";

const VIDEO_INPUT = "video";
const AUDIO_INPUT = "audio";

// The quietest level a meter shows, in decibels below full scale.
const METER_FLOOR_DECIBELS = -60;
const LOUD_FROM_DECIBELS = -12;
const CLIPPING_FROM_DECIBELS = -1;

const pictureCanvas = document.querySelector(".program-picture-canvas");
const pictureNotice = document.querySelector(".program-picture-notice");
const audioMeters = document.querySelector(".program-audio-meters");
const audioNotice = document.querySelector(".program-audio-notice");
const pictureContext = pictureCanvas.getContext("2d");
const audioPlayback = new ProgramAudioPlayback();

function showThePicture(source, width, height) {
  if (pictureCanvas.width !== width || pictureCanvas.height !== height) {
    pictureCanvas.width = width;
    pictureCanvas.height = height;
  }
  pictureContext.clearRect(0, 0, width, height);
  pictureContext.drawImage(source, 0, 0, width, height);
  pictureNotice.textContent = "";
}

// `image_pixels` carries rows `bytes_per_row` wide in the runtime's `rgba32` or `bgra32`.
function showThePixels(event) {
  const { width, height, bytes_per_row: bytesPerRow, pixels } = event;
  const isBgra = event.format_wire_name.startsWith("bgra");
  if (!isBgra && !event.format_wire_name.startsWith("rgba")) {
    pictureNotice.textContent = `Program cannot draw ${event.format_wire_name} pixels.`;
    return;
  }
  const rgba = new Uint8ClampedArray(width * height * 4);
  for (let row = 0; row < height; row += 1) {
    const rowStart = row * bytesPerRow;
    rgba.set(pixels.subarray(rowStart, rowStart + width * 4), row * width * 4);
  }
  if (isBgra) {
    for (let offset = 0; offset < rgba.length; offset += 4) {
      const blue = rgba[offset];
      rgba[offset] = rgba[offset + 2];
      rgba[offset + 2] = blue;
    }
  }
  if (pictureCanvas.width !== width || pictureCanvas.height !== height) {
    pictureCanvas.width = width;
    pictureCanvas.height = height;
  }
  pictureContext.putImageData(new ImageData(rgba, width, height), 0, 0);
  pictureNotice.textContent = "";
}

function onVideoEvent(event) {
  switch (event.kind) {
    case "image_frame": {
      const videoFrame = event.video_frame;
      showThePicture(videoFrame, videoFrame.displayWidth, videoFrame.displayHeight);
      videoFrame.close();
      return;
    }
    case "image_pixels":
      showThePixels(event);
      return;
    case "unwired":
    case "reopening":
    case "ended":
      pictureNotice.textContent = event.because;
      return;
  }
}

function meterHeightPercent(linearLevel) {
  if (linearLevel <= 0) return 0;
  const decibels = Math.max(20 * Math.log10(linearLevel), METER_FLOOR_DECIBELS);
  return ((decibels - METER_FLOOR_DECIBELS) / -METER_FLOOR_DECIBELS) * 100;
}

function meterForChannel(channelIndex) {
  while (audioMeters.children.length <= channelIndex) {
    const meter = document.createElement("div");
    meter.className = "program-audio-meter";
    const level = document.createElement("div");
    level.className = "program-audio-meter-level";
    const peak = document.createElement("div");
    peak.className = "program-audio-meter-peak";
    meter.append(level, peak);
    audioMeters.append(meter);
  }
  return audioMeters.children[channelIndex];
}

function showTheLevels(channelWaveforms) {
  while (audioMeters.children.length > channelWaveforms.length) audioMeters.lastElementChild.remove();
  channelWaveforms.forEach((waveform, channelIndex) => {
    const [level, peak] = meterForChannel(channelIndex).children;
    const peakDecibels = waveform.peak > 0 ? 20 * Math.log10(waveform.peak) : METER_FLOOR_DECIBELS;
    level.style.height = `${meterHeightPercent(waveform.rms)}%`;
    level.classList.toggle("is-loud", peakDecibels >= LOUD_FROM_DECIBELS && peakDecibels < CLIPPING_FROM_DECIBELS);
    level.classList.toggle("is-clipping", peakDecibels >= CLIPPING_FROM_DECIBELS);
    peak.style.bottom = `${meterHeightPercent(waveform.peak)}%`;
  });
  audioNotice.textContent = "";
}

function onAudioEvent(event) {
  switch (event.kind) {
    case "message":
      if (event.reading.family === "channels") {
        audioPlayback.play(event.reading);
        showTheLevels(event.reading.channel_waveforms);
      } else audioNotice.textContent = `audio carries ${event.reading.family}, not Channels.`;
      return;
    case "unwired":
    case "reopening":
    case "ended":
      audioPlayback.stop();
      audioMeters.replaceChildren();
      audioNotice.textContent = event.because;
      return;
  }
}

function watchTheInputsOver(port) {
  const listenerByWatchId = new Map([
    [1, { input: VIDEO_INPUT, onEvent: onVideoEvent, notice: pictureNotice }],
    [2, { input: AUDIO_INPUT, onEvent: onAudioEvent, notice: audioNotice }],
  ]);
  port.addEventListener("message", ({ data: message }) => {
    if (message?.kind === "hello") {
      for (const [watchId, { input }] of listenerByWatchId) {
        port.postMessage({ kind: "watch", watch_id: watchId, subject: { kind: "input", input } });
      }
      return;
    }
    const listener = listenerByWatchId.get(message?.watch_id);
    if (listener === undefined) return;
    if (message.kind === "watched" && message.watched.subject === "input") listener.onEvent(message.watched.reading);
    if (message.kind === "watch_refused") listener.notice.textContent = message.because;
  });
  port.start();
}

window.addEventListener("message", function onHandover(event) {
  const port = event.ports[0];
  if (event.source !== window.parent || event.data?.kind !== VIEW_PORT_HANDOVER || port === undefined) return;
  window.removeEventListener("message", onHandover);
  watchTheInputsOver(port);
});

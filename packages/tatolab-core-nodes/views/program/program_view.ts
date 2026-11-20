// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The Program view: the picture wired into `video`, full frame, and the sound wired into `audio`,
// played on the default output device and drawn as one level meter per channel. Each input is read
// through the view SDK's `watchInput`; a picture arrives as a transferred `VideoFrame`.

import { connectToTatoApp, type PortViewerChannelWaveform, type ViewInputEvent } from "@tatolab/tato-app-view-sdk";
import { ProgramAudioPlayback } from "./program_audio_playback.ts";

const VIDEO_INPUT = "video";
const AUDIO_INPUT = "audio";

// The quietest level a meter shows, in decibels below full scale.
const METER_FLOOR_DECIBELS = -60;
const LOUD_FROM_DECIBELS = -12;
const CLIPPING_FROM_DECIBELS = -1;

/** The element of the page `selector` names, which the page always holds. */
function elementOfThePage<Element extends HTMLElement>(selector: string, ofType: new () => Element): Element {
  const element = document.querySelector(selector);
  if (!(element instanceof ofType)) throw new Error(`the Program view's page holds no ${selector}`);
  return element;
}

const pictureCanvas = elementOfThePage(".program-picture-canvas", HTMLCanvasElement);
const pictureNotice = elementOfThePage(".program-picture-notice", HTMLParagraphElement);
const audioMeters = elementOfThePage(".program-audio-meters", HTMLDivElement);
const audioNotice = elementOfThePage(".program-audio-notice", HTMLParagraphElement);
const pictureContext = pictureCanvas.getContext("2d");
const audioPlayback = new ProgramAudioPlayback();

/** Size the canvas to `width` by `height`, which clears it. */
function sizeTheCanvas(width: number, height: number): void {
  if (pictureCanvas.width !== width || pictureCanvas.height !== height) {
    pictureCanvas.width = width;
    pictureCanvas.height = height;
  }
}

function showThePicture(videoFrame: VideoFrame): void {
  const { displayWidth: width, displayHeight: height } = videoFrame;
  sizeTheCanvas(width, height);
  pictureContext?.clearRect(0, 0, width, height);
  pictureContext?.drawImage(videoFrame, 0, 0, width, height);
  pictureNotice.textContent = "";
}

/** `pixels` drawn, rows `bytesPerRow` wide in the runtime's `rgba32` or `bgra32`. */
function showThePixels(inputEvent: Extract<ViewInputEvent, { kind: "image_pixels" }>): void {
  const { width, height, bytes_per_row: bytesPerRow, pixels, format_wire_name: formatWireName } = inputEvent;
  const isBgra = formatWireName.startsWith("bgra");
  if (!isBgra && !formatWireName.startsWith("rgba")) {
    pictureNotice.textContent = `Program cannot draw ${formatWireName} pixels.`;
    return;
  }
  const rgba = new Uint8ClampedArray(width * height * 4);
  for (let row = 0; row < height; row += 1) {
    const rowStart = row * bytesPerRow;
    rgba.set(pixels.subarray(rowStart, rowStart + width * 4), row * width * 4);
  }
  if (isBgra) {
    for (let offset = 0; offset < rgba.length; offset += 4) {
      const blue = rgba[offset] ?? 0;
      rgba[offset] = rgba[offset + 2] ?? 0;
      rgba[offset + 2] = blue;
    }
  }
  sizeTheCanvas(width, height);
  pictureContext?.putImageData(new ImageData(rgba, width, height), 0, 0);
  pictureNotice.textContent = "";
}

function onVideoEvent(inputEvent: ViewInputEvent): void {
  switch (inputEvent.kind) {
    case "image_frame":
      showThePicture(inputEvent.video_frame);
      inputEvent.video_frame.close();
      return;
    case "image_pixels":
      showThePixels(inputEvent);
      return;
    case "message":
      pictureNotice.textContent = `video carries ${inputEvent.reading.family}, not a picture.`;
      return;
    case "unwired":
    case "reopening":
    case "ended":
      pictureNotice.textContent = inputEvent.because;
      return;
  }
}

function meterHeightPercent(linearLevel: number): number {
  if (linearLevel <= 0) return 0;
  const decibels = Math.max(20 * Math.log10(linearLevel), METER_FLOOR_DECIBELS);
  return ((decibels - METER_FLOOR_DECIBELS) / -METER_FLOOR_DECIBELS) * 100;
}

/** The level bar and the peak mark of a channel's meter. */
type ChannelMeter = { level: HTMLDivElement; peak: HTMLDivElement };

const channelMeters: ChannelMeter[] = [];

function meterForChannel(channelIndex: number): ChannelMeter {
  while (channelMeters.length <= channelIndex) {
    const meter = document.createElement("div");
    meter.className = "program-audio-meter";
    const level = document.createElement("div");
    level.className = "program-audio-meter-level";
    const peak = document.createElement("div");
    peak.className = "program-audio-meter-peak";
    meter.append(level, peak);
    audioMeters.append(meter);
    channelMeters.push({ level, peak });
  }
  const channelMeter = channelMeters[channelIndex];
  if (channelMeter === undefined) throw new Error(`no meter was made for channel ${channelIndex}`);
  return channelMeter;
}

function forgetTheMeters(): void {
  channelMeters.length = 0;
  audioMeters.replaceChildren();
}

function showTheLevels(channelWaveforms: readonly PortViewerChannelWaveform[]): void {
  while (channelMeters.length > channelWaveforms.length) {
    channelMeters.pop();
    audioMeters.lastElementChild?.remove();
  }
  channelWaveforms.forEach((waveform, channelIndex) => {
    const { level, peak } = meterForChannel(channelIndex);
    const peakDecibels = waveform.peak > 0 ? 20 * Math.log10(waveform.peak) : METER_FLOOR_DECIBELS;
    level.style.height = `${meterHeightPercent(waveform.rms)}%`;
    level.classList.toggle("is-loud", peakDecibels >= LOUD_FROM_DECIBELS && peakDecibels < CLIPPING_FROM_DECIBELS);
    level.classList.toggle("is-clipping", peakDecibels >= CLIPPING_FROM_DECIBELS);
    peak.style.bottom = `${meterHeightPercent(waveform.peak)}%`;
  });
  audioNotice.textContent = "";
}

function onAudioEvent(inputEvent: ViewInputEvent): void {
  switch (inputEvent.kind) {
    case "message":
      if (inputEvent.reading.family === "channels") {
        audioPlayback.play(inputEvent.reading);
        showTheLevels(inputEvent.reading.channel_waveforms);
      } else audioNotice.textContent = `audio carries ${inputEvent.reading.family}, not Channels.`;
      return;
    case "image_frame":
      inputEvent.video_frame.close();
      audioNotice.textContent = "audio carries a picture, not Channels.";
      return;
    case "image_pixels":
      audioNotice.textContent = "audio carries a picture, not Channels.";
      return;
    case "unwired":
    case "reopening":
    case "ended":
      audioPlayback.stop();
      forgetTheMeters();
      audioNotice.textContent = inputEvent.because;
      return;
  }
}

const connection = await connectToTatoApp();
connection.watchInput(VIDEO_INPUT, onVideoEvent, (because) => {
  pictureNotice.textContent = because;
});
connection.watchInput(AUDIO_INPUT, onAudioEvent, (because) => {
  audioNotice.textContent = because;
});

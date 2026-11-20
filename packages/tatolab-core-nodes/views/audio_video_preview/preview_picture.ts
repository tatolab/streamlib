// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The picture wired into `video`, letterboxed on black, or at its own size, with the safe areas over it when asked for,
// and what the frames say of it: their size, their rate and the timecode of the last one.

import type { TatoAppViewConnection, ViewInputEvent } from "@tatolab/tato-app-view-sdk";
import { createElement, useEffect, useRef, useState } from "react";

/** How the picture sits in the area. */
export type PictureFit = "fit" | "actual_size";

/** What the frames wired into `video` say of the picture, or why there is none. */
export type PictureReading = { kind: "waiting" } | { kind: "shown"; width: number; height: number; frames_per_second: number | null; timecode: string } | { kind: "not_shown"; because: string };

/** How often the readout under the picture follows the frames, which arrive far faster. */
const READOUT_EVERY_MS = 250;
/** How much each new frame interval counts toward the frame rate shown, the rest kept from the frames before. */
const FRAME_RATE_SMOOTHING = 0.1;

/** `timestampMicroseconds` as hours, minutes, seconds and frames at `framesPerSecond`, as 00:12:41:17. */
export function timecodeOf(timestampMicroseconds: number, framesPerSecond: number | null): string {
  const totalSeconds = Math.max(timestampMicroseconds, 0) / 1_000_000;
  const wholeSeconds = Math.floor(totalSeconds);
  const frames = framesPerSecond === null ? 0 : Math.floor((totalSeconds - wholeSeconds) * Math.round(framesPerSecond));
  const twoDigits = (value: number) => String(value).padStart(2, "0");
  return [Math.floor(wholeSeconds / 3600), Math.floor(wholeSeconds / 60) % 60, wholeSeconds % 60, frames].map(twoDigits).join(":");
}

/** The frame rate after a frame `intervalMicroseconds` after the one before, smoothed over the frames before it; `null` until it is known. */
export function frameRateAfter(framesPerSecond: number | null, intervalMicroseconds: number): number | null {
  if (intervalMicroseconds <= 0) return framesPerSecond;
  const rateOfThisInterval = 1_000_000 / intervalMicroseconds;
  return framesPerSecond === null ? rateOfThisInterval : framesPerSecond + (rateOfThisInterval - framesPerSecond) * FRAME_RATE_SMOOTHING;
}

/** `pixels`, rows `bytesPerRow` wide in the runtime's `rgba32` or `bgra32`, as an RGBA image; `null` for another format. */
function imageOfThePixels(inputEvent: Extract<ViewInputEvent, { kind: "image_pixels" }>): ImageData | null {
  const { width, height, bytes_per_row: bytesPerRow, pixels, format_wire_name: formatWireName } = inputEvent;
  const isBgra = formatWireName.startsWith("bgra");
  if (!isBgra && !formatWireName.startsWith("rgba")) return null;
  const rgba = new Uint8ClampedArray(width * height * 4);
  for (let row = 0; row < height; row += 1) rgba.set(pixels.subarray(row * bytesPerRow, row * bytesPerRow + width * 4), row * width * 4);
  if (isBgra) {
    for (let offset = 0; offset < rgba.length; offset += 4) {
      const blue = rgba[offset] ?? 0;
      rgba[offset] = rgba[offset + 2] ?? 0;
      rgba[offset + 2] = blue;
    }
  }
  return new ImageData(rgba, width, height);
}

/** The room the picture keeps clear of the area's edges, in pixels. */
const PICTURE_MARGIN_PX = 10;

/** The frame's size: the picture's own at 100%, or else as large as the area holds at the picture's shape, letterboxed; hidden until a picture arrives. */
function frameStyleOf(fit: PictureFit, size: { width: number; height: number } | null): Record<string, string | number> {
  if (size === null) return { display: "none" };
  if (fit === "actual_size") return { width: size.width, height: size.height };
  const shape = size.width / size.height;
  return { width: `min(100cqw - ${2 * PICTURE_MARGIN_PX}px, (100cqh - ${2 * PICTURE_MARGIN_PX}px) * ${shape})`, aspectRatio: `${size.width} / ${size.height}` };
}

/** The picture wired into `video`, drawn as it arrives, and its reading told to `onReading` a few times a second. */
export function PreviewPicture({ connection, fit, showsSafeAreas, onReading }: { connection: TatoAppViewConnection; fit: PictureFit; showsSafeAreas: boolean; onReading: (reading: PictureReading) => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState<{ width: number; height: number } | null>(null);
  useEffect(() => {
    let framesPerSecond: number | null = null;
    let lastTimestamp: number | null = null;
    let lastReadoutAt = 0;
    const canvasSized = (width: number, height: number): CanvasRenderingContext2D | null => {
      const canvas = canvasRef.current;
      if (canvas === null) return null;
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      setSize((shown) => (shown !== null && shown.width === width && shown.height === height ? shown : { width, height }));
      return canvas.getContext("2d");
    };
    const tellTheReading = (width: number, height: number, timestamp: number) => {
      const now = performance.now();
      if (now - lastReadoutAt < READOUT_EVERY_MS) return;
      lastReadoutAt = now;
      onReading({ kind: "shown", width, height, frames_per_second: framesPerSecond, timecode: timecodeOf(timestamp, framesPerSecond) });
    };
    const notShown = (because: string) => {
      framesPerSecond = null;
      lastTimestamp = null;
      setSize(null);
      onReading({ kind: "not_shown", because });
    };
    return connection.watchInput(
      "video",
      (inputEvent) => {
        switch (inputEvent.kind) {
          case "image_frame": {
            const { video_frame: videoFrame } = inputEvent;
            const { displayWidth: width, displayHeight: height, timestamp } = videoFrame;
            if (lastTimestamp !== null) framesPerSecond = frameRateAfter(framesPerSecond, timestamp - lastTimestamp);
            lastTimestamp = timestamp;
            canvasSized(width, height)?.drawImage(videoFrame, 0, 0, width, height);
            videoFrame.close();
            tellTheReading(width, height, timestamp);
            return;
          }
          case "image_pixels": {
            const image = imageOfThePixels(inputEvent);
            if (image === null) return notShown(`The preview can't draw ${inputEvent.format_wire_name} pixels.`);
            canvasSized(image.width, image.height)?.putImageData(image, 0, 0);
            tellTheReading(image.width, image.height, 0);
            return;
          }
          case "message":
            return notShown(`video carries ${inputEvent.reading.family}, not a picture.`);
          case "unwired":
          case "reopening":
          case "ended":
            return notShown(inputEvent.because);
        }
      },
      notShown,
    );
  }, [connection, onReading]);
  return createElement(
    "div",
    { className: "preview-picture", "data-preview-fit": fit },
    createElement(
      "div",
      { className: "preview-picture-frame", style: frameStyleOf(fit, size) },
      createElement("canvas", { ref: canvasRef, className: "preview-picture-canvas", "aria-label": "The picture wired into video" }),
      showsSafeAreas ? createElement("div", { className: "preview-safe-area preview-safe-area-action", "aria-hidden": true }) : null,
      showsSafeAreas ? createElement("div", { className: "preview-safe-area preview-safe-area-title", "aria-hidden": true }) : null,
    ),
  );
}

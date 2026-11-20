// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The sound wired into `audio`: played through the view's monitor level, and drawn as one meter per channel on a
// decibel scale, each with the peak it held over the last moment.

import type { PortViewerChannelWaveform, TatoAppViewConnection } from "@tatolab/tato-app-view-sdk";
import { createElement, useEffect, useState } from "react";
import { PreviewAudioPlayback } from "./preview_audio_playback.ts";

/** What the blocks wired into `audio` say of the sound, or why there is none. */
export type SoundReading = { kind: "waiting" } | { kind: "heard"; sample_rate: number; channel_count: number } | { kind: "not_heard"; because: string };

/** The quietest level a meter shows, in decibels below full scale. */
const METER_FLOOR_DECIBELS = -60;
/** The marks along the meters' scale, in decibels. */
const SCALE_MARKS_DECIBELS = [0, -6, -12, -24, -40, -60] as const;
/** How long a channel's peak mark holds before it falls back to the level. */
const PEAK_HOLD_MS = 1500;

/** The held peak's mark, as tall as `.preview-meter-peak` draws it, so a full-scale peak stays inside the bar. */
const PEAK_MARK_HEIGHT_PX = 2;

/** One channel's meter as drawn: its level and held peak, each from 0 (the floor) to 1 (full scale). */
interface ChannelMeter {
  level: number;
  peak: number;
  peakHeldUntil: number;
}

/** `linearLevel`, from 0 to 1 of full scale, as a share of the meter from its floor. */
export function meterShareOf(linearLevel: number): number {
  if (linearLevel <= 0) return 0;
  const decibels = Math.max(20 * Math.log10(linearLevel), METER_FLOOR_DECIBELS);
  return (decibels - METER_FLOOR_DECIBELS) / -METER_FLOOR_DECIBELS;
}

/** The meters after a block whose channels read `waveforms` at `now`, each peak held for a moment before it falls to the level. */
export function metersAfter(meters: readonly ChannelMeter[], waveforms: readonly PortViewerChannelWaveform[], now: number): ChannelMeter[] {
  return waveforms.map((waveform, channel) => {
    const level = meterShareOf(waveform.rms);
    const blockPeak = meterShareOf(waveform.peak);
    const held = meters[channel];
    if (held !== undefined && held.peak >= blockPeak && now < held.peakHeldUntil) return { level, peak: held.peak, peakHeldUntil: held.peakHeldUntil };
    return { level, peak: blockPeak, peakHeldUntil: now + PEAK_HOLD_MS };
  });
}

/** A channel's name under its meter: L and R for stereo, else its number. */
function channelNameOf(channel: number, channelCount: number): string {
  if (channelCount === 2) return channel === 0 ? "L" : "R";
  return String(channel + 1);
}

/** The sound wired into `audio`, played at `monitorLevel` and drawn as meters, its reading told to `onReading` as it changes. */
export function PreviewMeters({ connection, monitorLevel, onReading }: { connection: TatoAppViewConnection; monitorLevel: number; onReading: (reading: SoundReading) => void }) {
  const [playback] = useState(() => new PreviewAudioPlayback());
  const [meters, setMeters] = useState<readonly ChannelMeter[]>([]);
  useEffect(() => playback.setMonitorLevel(monitorLevel), [playback, monitorLevel]);
  useEffect(() => {
    let readingTold = "";
    const tell = (reading: SoundReading) => {
      const readingAsJson = JSON.stringify(reading);
      if (readingAsJson === readingTold) return;
      readingTold = readingAsJson;
      onReading(reading);
    };
    const notHeard = (because: string) => {
      playback.stop();
      setMeters([]);
      tell({ kind: "not_heard", because });
    };
    const stopWatching = connection.watchInput(
      "audio",
      (inputEvent) => {
        switch (inputEvent.kind) {
          case "message": {
            if (inputEvent.reading.family !== "channels") return notHeard(`audio carries ${inputEvent.reading.family}, not sound.`);
            playback.play(inputEvent.reading);
            const { channel_waveforms: waveforms, sample_rate: sampleRate, channel_count: channelCount } = inputEvent.reading;
            setMeters((metersBefore) => metersAfter(metersBefore, waveforms, performance.now()));
            tell({ kind: "heard", sample_rate: sampleRate, channel_count: channelCount });
            return;
          }
          case "image_frame":
            inputEvent.video_frame.close();
            return notHeard("audio carries a picture, not sound.");
          case "image_pixels":
            return notHeard("audio carries a picture, not sound.");
          case "unwired":
          case "reopening":
          case "ended":
            return notHeard(inputEvent.because);
        }
      },
      notHeard,
    );
    return () => {
      stopWatching();
      playback.stop();
    };
  }, [connection, playback, onReading]);
  return createElement(
    "div",
    { className: "preview-meters", role: "img", "aria-label": meters.length === 0 ? "No sound" : `Sound level of ${meters.length} channels` },
    createElement(
      "div",
      { className: "preview-meter-scale", "aria-hidden": true },
      SCALE_MARKS_DECIBELS.map((decibels) => createElement("span", { key: decibels, style: { bottom: `${meterShareOf(10 ** (decibels / 20)) * 100}%` } }, decibels === 0 ? "0" : String(decibels))),
    ),
    (meters.length === 0 ? [null, null] : meters).map((meter, channel) =>
      createElement(
        "div",
        { key: channel, className: "preview-meter" },
        createElement(
          "div",
          { className: "preview-meter-bar" },
          meter === null ? null : createElement("div", { className: "preview-meter-level", style: { clipPath: `inset(${(1 - meter.level) * 100}% 0 0 0)` } }),
          meter === null ? null : createElement("div", { className: "preview-meter-peak", style: { bottom: `calc(${meter.peak * 100}% - ${meter.peak * PEAK_MARK_HEIGHT_PX}px)` } }),
        ),
        createElement("span", { className: "preview-meter-channel" }, meter === null ? "" : channelNameOf(channel, meters.length)),
      ),
    ),
  );
}

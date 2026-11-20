// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The Audio Video Preview: Core Nodes' output view, and an example of one written on the view SDK alone. Its node takes a
// typed `video` input, drawn letterboxed in the area, and a typed `audio` input, played on the default output device and
// drawn as meters. Its header row holds how the picture fits and the safe areas, which its node keeps as settings, and a
// monitor level that only changes what this view plays; the bar under the picture says what the frames and blocks say.
// Streams always run, so the view has no transport of its own.

import { connectToTatoApp, type TatoAppViewConnection, type ViewHeaderRow, type ViewSettings } from "@tatolab/tato-app-view-sdk";
import { RefusalLine } from "@tatolab/tato-app-view-sdk/design_system/refusal_line";
import { SegmentedChoiceControl } from "@tatolab/tato-app-view-sdk/design_system/segmented_choice_control";
import { StudioAreaHeaderRowInAFrame } from "@tatolab/tato-app-view-sdk/design_system/studio_area_frame";
import designSystemStyles from "@tatolab/tato-app-view-sdk/design_system/styles/design_system.css" with { type: "css" };
import { ToggleSwitch } from "@tatolab/tato-app-view-sdk/design_system/toggle_switch";
import { useViewReading } from "@tatolab/tato-app-view-sdk/design_system/view_readings";
import { WaitingState } from "@tatolab/tato-app-view-sdk/design_system/waiting_state";
import { createElement, useCallback, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import viewStyles from "./audio_video_preview_view.css" with { type: "css" };
import { PreviewMeters, type SoundReading } from "./preview_meters.ts";
import { PreviewPicture, type PictureFit, type PictureReading } from "./preview_picture.ts";

/** The node's settings as the view reads them, each its default where the node holds none. */
interface PreviewSettings {
  fit: PictureFit;
  safe_areas: boolean;
}

function previewSettingsOf(settings: ViewSettings): PreviewSettings {
  return { fit: settings?.["fit"] === "actual_size" ? "actual_size" : "fit", safe_areas: settings?.["safe_areas"] === true };
}

/** The bar under the picture: what the frames and blocks say, or why each is missing. */
function PreviewReadout({ picture, sound }: { picture: PictureReading; sound: SoundReading }) {
  const chip = (words: string, key: string) => createElement("span", { key, className: "preview-readout-chip" }, words);
  const pictureWords =
    picture.kind === "shown"
      ? [
          createElement("span", { key: "timecode", className: "preview-readout-timecode", "data-preview-timecode": "" }, picture.timecode),
          chip(`${picture.width}×${picture.height}`, "size"),
          ...(picture.frames_per_second === null ? [] : [chip(`${picture.frames_per_second.toFixed(2)} fps`, "rate")]),
        ]
      : [createElement("span", { key: "picture", className: "preview-readout-missing" }, picture.kind === "waiting" ? "Waiting for video" : `video: ${picture.because}`)];
  const soundWords =
    sound.kind === "heard"
      ? [chip(`${sound.sample_rate / 1000} kHz · ${sound.channel_count === 1 ? "mono" : sound.channel_count === 2 ? "stereo" : `${sound.channel_count} channels`}`, "sound")]
      : [createElement("span", { key: "sound", className: "preview-readout-missing" }, sound.kind === "waiting" ? "Waiting for audio" : `audio: ${sound.because}`)];
  return createElement("div", { className: "preview-readout", role: "status" }, ...pictureWords, createElement("span", { key: "gap", className: "preview-readout-gap" }), ...soundWords);
}

function AudioVideoPreview({ connection }: { connection: TatoAppViewConnection }) {
  const headerRow = useViewReading(useCallback((listener: (headerRow: ViewHeaderRow) => void) => connection.watchHeaderRow(listener), [connection]));
  const settings = previewSettingsOf(useViewReading(useCallback((listener: (settings: ViewSettings) => void) => connection.watchSettings(listener), [connection])) ?? null);
  const [settingRefusal, setSettingRefusal] = useState<string | null>(null);
  const [monitorLevel, setMonitorLevel] = useState(1);
  const [picture, setPicture] = useState<PictureReading>({ kind: "waiting" });
  const [sound, setSound] = useState<SoundReading>({ kind: "waiting" });
  const askedSettings = useRef<Partial<PreviewSettings>>({});
  const keepSettings = (changed: Partial<PreviewSettings>) => {
    askedSettings.current = { ...askedSettings.current, ...changed };
    void connection.rememberSettings({ ...settings, ...askedSettings.current }).then((answer) => setSettingRefusal(answer.kind === "refused" ? answer.because : null));
  };
  return createElement(
    "div",
    { className: "audio-video-preview" },
    createElement(
      StudioAreaHeaderRowInAFrame,
      { headerRow: headerRow ?? null },
      createElement("span", { className: "preview-header-gap" }),
      createElement(SegmentedChoiceControl<PictureFit>, {
        label: "How the picture fits",
        choices: [
          { choice: "fit", label: "Fit" },
          { choice: "actual_size", label: "100%" },
        ],
        chosen: settings.fit,
        onChosen: (fit: PictureFit) => keepSettings({ fit }),
        dataAttributeOfEachSegment: "data-preview-fit-choice",
      }),
      createElement(
        "span",
        { className: "preview-header-field" },
        createElement("span", { id: "preview-safe-areas-label" }, "Safe areas"),
        createElement(ToggleSwitch, { isOn: settings.safe_areas, onSwitched: (isOn: boolean) => keepSettings({ safe_areas: isOn }), labelledBy: "preview-safe-areas-label", "data-preview-safe-areas": "" }),
      ),
      createElement(
        "label",
        { className: "preview-header-field" },
        "Monitor",
        createElement("input", {
          type: "range",
          className: "preview-monitor-level",
          min: 0,
          max: 1,
          step: 0.01,
          value: monitorLevel,
          "aria-label": "Monitor level, for this view only",
          onChange: (changeEvent: { currentTarget: HTMLInputElement }) => setMonitorLevel(changeEvent.currentTarget.valueAsNumber),
        }),
      ),
    ),
    settingRefusal === null ? null : createElement(RefusalLine, { lead: "The preview's setting wasn't kept:", words: settingRefusal, layout: "bar" }),
    createElement(
      "div",
      { className: "preview-stage" },
      createElement(PreviewPicture, { connection, fit: settings.fit, showsSafeAreas: settings.safe_areas, onReading: setPicture }),
      createElement(PreviewMeters, { connection, monitorLevel, onReading: setSound }),
    ),
    createElement(PreviewReadout, { picture, sound }),
  );
}

document.adoptedStyleSheets = [...document.adoptedStyleSheets, designSystemStyles, viewStyles];
const viewRoot = createRoot(document.getElementById("view") ?? document.body);
viewRoot.render(createElement(WaitingState, { title: "Connecting to Tato Studio" }));
void connectToTatoApp().then((connection) => viewRoot.render(createElement(AudioVideoPreview, { connection })));

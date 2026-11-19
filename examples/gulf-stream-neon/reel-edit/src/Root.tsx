// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import React from "react";
import { Composition, Still } from "remotion";
import { Reel, Cover, FPS, DURATION_S, ReelProps } from "./Reel";

// The music is a prop: `--props='{"audio":"music_x.wav","credit":"MUSIC …"}'` swaps it.
const MUSIC: Omit<ReelProps, "vertical"> = { audio: "music_eleven_take3.wav", credit: "MUSIC ORIGINAL, MADE WITH ELEVENLABS MUSIC" };

export const Root: React.FC = () => (
  <>
    <Composition id="Vertical" component={Reel} durationInFrames={DURATION_S * FPS} fps={FPS} width={1080} height={1920} defaultProps={{ vertical: true, ...MUSIC }} />
    <Composition id="Wide" component={Reel} durationInFrames={DURATION_S * FPS} fps={FPS} width={1920} height={1080} defaultProps={{ vertical: false, ...MUSIC }} />
    <Still id="Cover" component={Cover} width={1080} height={1920} />
  </>
);

// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

import React from "react";
import { Composition, Still } from "remotion";
import { Reel, Cover } from "./Reel";

export const Root: React.FC = () => (
  <>
    <Composition id="Vertical" component={Reel} durationInFrames={898} fps={30} width={1080} height={1920} defaultProps={{ vertical: true }} />
    <Composition id="Wide" component={Reel} durationInFrames={898} fps={30} width={1920} height={1080} defaultProps={{ vertical: false }} />
    <Still id="Cover" component={Cover} width={1080} height={1920} />
  </>
);

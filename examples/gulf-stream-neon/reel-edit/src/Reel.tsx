// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The Gulf Stream Neon reel: real-time Tato Studio captures cut to the soundtrack, with the
// stepped renders as hero B-roll. Every Studio shot is a screen recording of the live app.
import React from "react";
import { AbsoluteFill, Audio, Img, interpolate, OffthreadVideo, Sequence, staticFile, useCurrentFrame, Easing } from "remotion";
import { loadFont as loadInter } from "@remotion/google-fonts/Inter";
import { loadFont as loadPoiret } from "@remotion/google-fonts/PoiretOne";

const { fontFamily: inter } = loadInter("normal", { weights: ["400", "600", "800"] });
const { fontFamily: poiret } = loadPoiret();

const FPS = 30;
const PINK = "#FF2E88";
const CYAN = "#00E5D4";
const PEACH = "#FFB36B";
const NIGHT = "#120A2E";

type Rect = { x: number; y: number; w: number; h: number };
type Scene = {
  at: number;
  until: number;
  kind: "studio" | "broll" | "end";
  src: string;
  from: number;
  label?: string;
  sub?: string;
  // Vertical: the stage picture and a zoomed detail of the hand at work.
  picture?: Rect;
  detail?: Rect;
  // Wide: a push-in toward this point of the Studio window.
  focus?: [number, number];
};

const STAGE_PICTURE: Rect = { x: 175, y: 230, w: 775, h: 627 };
const SCENES: Scene[] = [
  { at: 0, until: 4.5, kind: "studio", src: "s1_real_map_orbit.mp4", from: 4.0, label: "The ocean off Miami. Real data.", sub: "Built live in Tato Studio · NOAA bathymetry · the real Gulf Stream · NOAA readings", picture: STAGE_PICTURE, detail: { x: 836, y: 84, w: 272, h: 146 }, focus: [970, 160] },
  { at: 4.5, until: 10, kind: "studio", src: "s2_morph_slider.mp4", from: 3.9, label: "drag: real map → neon", sub: "one slider, live in Tato Studio", picture: STAGE_PICTURE, detail: { x: 1126, y: 150, w: 270, h: 70 }, focus: [1260, 200] },
  { at: 10, until: 12, kind: "broll", src: "take", from: 10.0, label: "running at 60 fps on an RTX 3090", sub: "2,097,152 GPU particles on the real current" },
  { at: 12, until: 14, kind: "studio", src: "s3_time_of_day.mp4", from: 2.2, label: "drag: time of day", picture: STAGE_PICTURE, detail: { x: 1126, y: 240, w: 270, h: 52 }, focus: [1260, 270] },
  { at: 14, until: 16, kind: "broll", src: "take", from: 14.0, label: "HDR bloom · god rays · ACES", sub: "a raymarched Deco skyline, mirrored in the water" },
  { at: 16, until: 18, kind: "studio", src: "s5_eddies.mp4", from: 2.6, label: "click: drop an eddy", sub: "the real current carries it north", picture: STAGE_PICTURE, focus: [560, 760] },
  { at: 18, until: 20, kind: "studio", src: "s4_orbit_neon.mp4", from: 1.4, label: "orbit · zoom · fly, live", picture: STAGE_PICTURE, focus: [700, 560] },
  { at: 20, until: 22, kind: "broll", src: "take", from: 2.4, label: "rendered live, frame by frame", sub: "no video, no AI: a running stream" },
  { at: 22, until: 24, kind: "studio", src: "s6_skyline_neon.mp4", from: 1.4, label: "drag: skyline height", picture: STAGE_PICTURE, detail: { x: 1126, y: 636, w: 270, h: 52 }, focus: [1260, 650] },
  { at: 24, until: 26.2, kind: "studio", src: "s7_graph_knobs.mp4", from: 5.6, label: "the graph: 27 nodes", sub: "GPU compute · Slang shader nodes · function nodes", picture: { x: 1405, y: 112, w: 510, h: 290 }, detail: { x: 1410, y: 700, w: 500, h: 150 }, focus: [1500, 600] },
  { at: 26.2, until: 27.4, kind: "studio", src: "s8_studio_code.mp4", from: 7.2, label: "Slang, on the GPU", picture: { x: 5, y: 135, w: 700, h: 394 }, focus: [400, 400] },
  { at: 27.4, until: 30, kind: "end", src: "take", from: 20.5 },
];

const seconds = (s: number) => Math.round(s * FPS);

const Crop: React.FC<{ src: string; from: number; crop: Rect; box: Rect; frames: number }> = ({ src, from, crop, box, frames }) => {
  const scale = box.w / crop.w;
  return (
    <div style={{ position: "absolute", left: box.x, top: box.y, width: box.w, height: crop.h * scale, overflow: "hidden", borderRadius: 18, boxShadow: `0 0 0 2px rgba(0,229,212,0.35), 0 20px 60px rgba(0,0,0,0.6)` }}>
      <OffthreadVideo
        src={staticFile(src)}
        startFrom={seconds(from)}
        muted
        style={{ position: "absolute", width: 1920 * scale, height: 1080 * scale, left: -crop.x * scale, top: -crop.y * scale, maxWidth: "none" }}
      />
    </div>
  );
};

const Label: React.FC<{ text: string; sub?: string; vertical: boolean; frames: number }> = ({ text, sub, vertical, frames }) => {
  const frame = useCurrentFrame();
  const enter = interpolate(frame, [0, 8], [0, 1], { extrapolateRight: "clamp", easing: Easing.out(Easing.cubic) });
  const leave = interpolate(frame, [frames - 6, frames], [1, 0], { extrapolateLeft: "clamp" });
  const o = Math.min(enter, leave);
  return (
    <div style={{ position: "absolute", left: vertical ? 70 : 90, top: vertical ? 170 : 70, right: vertical ? 160 : 600, opacity: o, transform: `translateY(${(1 - enter) * 18}px)` }}>
      <div style={{ display: "inline-block", fontFamily: inter, fontWeight: 800, fontSize: vertical ? 58 : 52, color: "#fff", letterSpacing: -0.5, textShadow: "0 3px 18px rgba(0,0,0,0.7)", borderLeft: `8px solid ${PINK}`, paddingLeft: 22, lineHeight: 1.1 }}>{text}</div>
      {sub && <div style={{ fontFamily: inter, fontWeight: 600, fontSize: vertical ? 32 : 28, color: CYAN, marginTop: 14, paddingLeft: 30, textShadow: "0 2px 12px rgba(0,0,0,0.8)" }}>{sub}</div>}
    </div>
  );
};

const LiveBadge: React.FC<{ vertical: boolean }> = ({ vertical }) => {
  const frame = useCurrentFrame();
  const on = Math.floor(frame / 15) % 2 === 0;
  return (
    <div style={{ position: "absolute", left: vertical ? 70 : 90, top: vertical ? 90 : 24, display: "flex", alignItems: "center", gap: 12, fontFamily: inter, fontWeight: 800, fontSize: vertical ? 26 : 22, color: "#fff", letterSpacing: 2 }}>
      <span style={{ width: 16, height: 16, borderRadius: 8, background: "#ff3b5c", opacity: on ? 1 : 0.35, boxShadow: "0 0 14px #ff3b5c" }} />
      SCREEN RECORDING · TATO STUDIO · REAL TIME
    </div>
  );
};

const StudioScene: React.FC<{ scene: Scene; vertical: boolean; frames: number }> = ({ scene, vertical, frames }) => {
  const frame = useCurrentFrame();
  if (vertical) {
    const pictureBox: Rect = { x: 40, y: 380, w: 1000, h: 0 };
    const pictureHeight = scene.picture ? (scene.picture.h * 1000) / scene.picture.w : 0;
    return (
      <AbsoluteFill style={{ background: `radial-gradient(ellipse at 50% 30%, #2a0f4a 0%, ${NIGHT} 60%, #07040f 100%)` }}>
        {scene.picture && <Crop src={scene.src} from={scene.from} crop={scene.picture} box={pictureBox} frames={frames} />}
        {scene.detail && (
          <Crop src={scene.src} from={scene.from} crop={scene.detail} box={{ x: 220, y: 380 + pictureHeight + 40, w: 640, h: 0 }} frames={frames} />
        )}
        <LiveBadge vertical />
      </AbsoluteFill>
    );
  }
  const push = interpolate(frame, [0, frames], [1.0, 1.12], { easing: Easing.inOut(Easing.quad) });
  const [fx, fy] = scene.focus ?? [960, 540];
  return (
    <AbsoluteFill style={{ background: "#000" }}>
      <OffthreadVideo src={staticFile(scene.src)} startFrom={seconds(scene.from)} muted style={{ width: 1920, height: 1080, transformOrigin: `${fx}px ${fy}px`, transform: `scale(${push})` }} />
      <AbsoluteFill style={{ background: "linear-gradient(180deg, rgba(0,0,0,0.75) 0%, rgba(0,0,0,0) 26%)" }} />
      <LiveBadge vertical={false} />
    </AbsoluteFill>
  );
};

const Broll: React.FC<{ scene: Scene; vertical: boolean; frames: number }> = ({ scene, vertical, frames }) => {
  const frame = useCurrentFrame();
  const flash = interpolate(frame, [0, 5], [0.55, 0], { extrapolateRight: "clamp" });
  return (
    <AbsoluteFill style={{ background: "#000" }}>
      <OffthreadVideo src={staticFile(vertical ? "take_vertical.mp4" : "take_wide.mp4")} startFrom={seconds(scene.from)} muted style={{ width: "100%", height: "100%" }} />
      <AbsoluteFill style={{ background: `linear-gradient(180deg, rgba(18,10,46,0.65) 0%, rgba(18,10,46,0) 24%)` }} />
      <AbsoluteFill style={{ background: "#fff", opacity: flash }} />
    </AbsoluteFill>
  );
};

const EndCard: React.FC<{ scene: Scene; vertical: boolean; frames: number }> = ({ scene, vertical, frames }) => {
  const frame = useCurrentFrame();
  const fade = interpolate(frame, [0, 14], [0, 1], { extrapolateRight: "clamp" });
  const rise = interpolate(frame, [0, 22], [30, 0], { extrapolateRight: "clamp", easing: Easing.out(Easing.cubic) });
  return (
    <AbsoluteFill style={{ background: "#000" }}>
      <OffthreadVideo src={staticFile(vertical ? "take_vertical.mp4" : "take_wide.mp4")} startFrom={seconds(scene.from)} muted style={{ width: "100%", height: "100%", filter: `blur(${fade * 14}px) brightness(${1 - 0.55 * fade})` }} />
      <AbsoluteFill style={{ alignItems: "center", justifyContent: vertical ? "flex-start" : "center", paddingTop: vertical ? 560 : 0, opacity: fade, transform: `translateY(${rise}px)` }}>
        <div style={{ fontFamily: poiret, fontSize: vertical ? 140 : 150, color: "#fff", letterSpacing: 6, textShadow: `0 0 30px ${PINK}, 0 0 80px ${PINK}` }}>Tato Studio</div>
        <div style={{ fontFamily: inter, fontWeight: 600, fontSize: vertical ? 46 : 44, color: CYAN, marginTop: 18, letterSpacing: 4 }}>tatolab.com</div>
        <div style={{ fontFamily: inter, fontWeight: 600, fontSize: vertical ? 30 : 28, color: PEACH, marginTop: 34 }}>Built live in Tato Studio · real data · real time</div>
        <div style={{ fontFamily: inter, fontWeight: 400, fontSize: vertical ? 21 : 20, color: "rgba(255,255,255,0.72)", marginTop: vertical ? 260 : 120, textAlign: "center", lineHeight: 1.5, maxWidth: vertical ? 860 : 1300 }}>
          Bathymetry: NOAA NCEI ETOPO 2022 · Currents: HYCOM.org / U.S. Navy ESPC-D (data snapshot 16 Nov 2026)<br />
          Station: NOAA CO-OPS 8723214 Virginia Key (logged 19 Nov 2026) · Music: original, synthesised in code
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};

export const Reel: React.FC<{ vertical: boolean }> = ({ vertical }) => (
  <AbsoluteFill style={{ background: NIGHT }}>
    {SCENES.map((scene, index) => {
      const frames = seconds(scene.until) - seconds(scene.at);
      return (
        <Sequence key={index} from={seconds(scene.at)} durationInFrames={frames}>
          {scene.kind === "studio" && <StudioScene scene={scene} vertical={vertical} frames={frames} />}
          {scene.kind === "broll" && <Broll scene={scene} vertical={vertical} frames={frames} />}
          {scene.kind === "end" && <EndCard scene={scene} vertical={vertical} frames={frames} />}
          {scene.label && <Label text={scene.label} sub={scene.sub} vertical={vertical} frames={frames} />}
        </Sequence>
      );
    })}
    <Audio src={staticFile("reel_audio.wav")} />
  </AbsoluteFill>
);

export const Cover: React.FC = () => (
  <AbsoluteFill style={{ background: NIGHT }}>
    <Img src={staticFile("cover_frame.png")} style={{ width: "100%", height: "100%" }} />
    <AbsoluteFill style={{ background: "linear-gradient(180deg, rgba(18,10,46,0.92) 0%, rgba(18,10,46,0.7) 22%, rgba(18,10,46,0) 42%, rgba(18,10,46,0) 70%, rgba(18,10,46,0.7) 100%)" }} />
    <div style={{ position: "absolute", top: 210, left: 0, right: 0, textAlign: "center" }}>
      <div style={{ fontFamily: poiret, fontSize: 120, color: "#fff", letterSpacing: 4, textShadow: `0 0 30px ${PINK}, 0 0 70px ${PINK}` }}>Gulf Stream Neon</div>
      <div style={{ fontFamily: inter, fontWeight: 800, fontSize: 46, color: CYAN, marginTop: 20 }}>Built live in Tato Studio</div>
      <div style={{ fontFamily: inter, fontWeight: 600, fontSize: 32, color: PEACH, marginTop: 12, textShadow: "0 2px 12px rgba(0,0,0,0.9)" }}>real NOAA data · real time · RTX 3090</div>
    </div>
  </AbsoluteFill>
);

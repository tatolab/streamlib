// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The Gulf Stream Neon reel, v2: real-time Tato Studio captures cut to the music at 60 fps, with
// the stepped renders as hero B-roll, in the Tato look from tatolab.com: Tektur sign lettering,
// Golos Text, JetBrains Mono, the Tato inks, skewed slabs with ink offsets, cut joins and grain.
// The music is an input (`audio`), so a track can be swapped without touching the cut.
import React from "react";
import { AbsoluteFill, Audio, Img, interpolate, OffthreadVideo, Sequence, staticFile, useCurrentFrame, Easing } from "remotion";
import { loadFont } from "@remotion/fonts";

const FONTS: [string, string, string][] = [
  ["Tektur", "fonts/tektur-latin-800-normal.woff2", "800"],
  ["Tektur", "fonts/tektur-latin-900-normal.woff2", "900"],
  ["Golos Text", "fonts/golos-text-latin-400-normal.woff2", "400"],
  ["Golos Text", "fonts/golos-text-latin-600-normal.woff2", "600"],
  ["JetBrains Mono", "fonts/jetbrains-mono-latin-400-normal.woff2", "400"],
  ["JetBrains Mono", "fonts/jetbrains-mono-latin-700-normal.woff2", "700"],
];
for (const [family, file, weight] of FONTS) loadFont({ family, url: staticFile(file), weight });

export const FPS = 60;
const INK = { blue: "#2A5BFF", pink: "#FF3D8B", yellow: "#FFD60A", cream: "#FFF4E0", ink: "#14101F", mint: "#2FD6A0", violet: "#5B2EE6", band: "#0D1A4A" };
const DSP = "Tektur, 'Arial Black', Impact, sans-serif";
const TXT = "'Golos Text', system-ui, sans-serif";
const MONO = "'JetBrains Mono', monospace";

type Rect = { x: number; y: number; w: number; h: number };
type Scene = {
  at: number;
  until: number;
  kind: "studio" | "broll" | "end";
  src: string;
  from: number;
  slab?: string;
  sub?: string;
  picture?: Rect;
  detail?: Rect;
  focus?: [number, number];
  // The recording's size, and the part of it the 16:9 cut shows (all of it by default).
  size?: [number, number];
  wide?: Rect;
  // The 9:16 cut stacks these two parts of the recording, one above the other, instead.
  stack?: [Rect, Rect];
};

const STAGE_PICTURE: Rect = { x: 175, y: 230, w: 775, h: 627 };
// Cut to the music, ElevenLabs take 3: 120 BPM, a bar every 2 s, the drop on the downbeat at
// 16 s, the body to 24 s, then the fade the end card rides out on. 28 s in all.
export const DURATION_S = 28;
// The 60 fps screen recording of Tato Studio on the owner's desktop, 3440x1440.
const LIVE = "native_take2.mp4";
const LIVE_SIZE: [number, number] = [3440, 1440];
const LIVE_PICTURE: Rect = { x: 366, y: 110, w: 1500, h: 1226 };
const LIVE_WIDE: Rect = { x: 0, y: 0, w: 2560, h: 1440 };
const live = (scene: Partial<Scene>): Scene => ({ kind: "studio", src: LIVE, size: LIVE_SIZE, wide: LIVE_WIDE, picture: LIVE_PICTURE, at: 0, until: 0, from: 0, ...scene });
// The second, smoothed take: the stage on the left, the graph and the node visualizer on the right.
const T3 = "native_take3.mp4";
const T3_STAGE: Rect = { x: 4, y: 236, w: 1728, h: 974 };
const T3_RIGHT: Rect = { x: 2024, y: 112, w: 1408, h: 1268 };
const T3_GRAPH: Rect = { x: 2024, y: 112, w: 1408, h: 478 };
const T3_PREVIEW: Rect = { x: 2088, y: 640, w: 1280, h: 720 };
export const SCENES: Scene[] = [
  live({ at: 0, until: 8, from: 13.0, slab: "The ocean off Miami", sub: "Real NOAA data, live in Tato Studio", detail: { x: 1924, y: 118, w: 286, h: 172 }, focus: [1300, 400] }),
  live({ at: 8, until: 16, src: T3, from: 72.0, picture: T3_STAGE, slab: "Build realtime interactive experiences", sub: "drag · real map → neon", focus: [1100, 600] }),
  { at: 16, until: 18, kind: "broll", src: "take", from: 10.0, slab: "60 FPS on an RTX 3090", sub: "two million particles on the real Gulf Stream" },
  live({ at: 18, until: 19, from: 84.0, slab: "Drag · skyline height", detail: { x: 2228, y: 722, w: 284, h: 52 }, focus: [1100, 700] }),
  live({ at: 19, until: 20, src: T3, from: 36.0, picture: T3_STAGE, slab: "Orbit · zoom · fly", focus: [1100, 700] }),
  live({ at: 20, until: 21, src: T3, from: 41.6, picture: T3_STAGE, slab: "Click · drop an eddy", focus: [1100, 700] }),
  live({ at: 21, until: 22.5, src: T3, from: 44.0, picture: T3_RIGHT, stack: [T3_GRAPH, T3_PREVIEW], wide: { x: 880, y: 0, w: 2560, h: 1440 }, slab: "Live graph", sub: "every node's output, as it plays", focus: [1800, 700] }),
  live({ at: 22.5, until: 24, src: T3, from: 82.5, picture: T3_GRAPH, stack: [T3_GRAPH, T3_STAGE], wide: { x: 880, y: 0, w: 2560, h: 1440 }, slab: "No render button", sub: "35 nodes, running while you work", focus: [1800, 300] }),
  { at: 24, until: 28, kind: "end", src: "take", from: 21.0 },
];

const frames = (s: number) => Math.round(s * FPS);

const Grain: React.FC = () => (
  <AbsoluteFill style={{ backgroundImage: `url(${staticFile("grain.png")})`, backgroundSize: "220px 220px", mixBlendMode: "overlay", opacity: 0.5, pointerEvents: "none" }} />
);

// A skewed slab with an offset shadow: the site's .btn3 and .lab.
const Slab: React.FC<{ children: React.ReactNode; fill: string; shadow: string; color: string; size: number; pad?: string }> = ({ children, fill, shadow, color, size, pad }) => (
  <div style={{ position: "relative", display: "inline-block", isolation: "isolate", padding: pad ?? `${size * 0.32}px ${size * 0.62}px ${size * 0.28}px`, fontFamily: DSP, fontWeight: 900, fontSize: size, lineHeight: 1, textTransform: "uppercase", letterSpacing: "0.01em", color }}>
    <div style={{ position: "absolute", inset: 0, background: shadow, transform: "skewX(-14deg)", translate: `${size * 0.14}px ${size * 0.14}px`, zIndex: -2 }} />
    <div style={{ position: "absolute", inset: 0, background: fill, transform: "skewX(-14deg)", zIndex: -1 }} />
    {children}
  </div>
);

const Title: React.FC<{ slab: string; sub?: string; vertical: boolean; length: number }> = ({ slab, sub, vertical, length }) => {
  const frame = useCurrentFrame();
  const enter = interpolate(frame, [0, 14], [0, 1], { extrapolateRight: "clamp", easing: Easing.out(Easing.cubic) });
  const leave = interpolate(frame, [length - 10, length], [1, 0], { extrapolateLeft: "clamp" });
  const size = vertical ? 64 : 58;
  return (
    <div style={{ position: "absolute", left: vertical ? 72 : 96, top: vertical ? 190 : 92, right: vertical ? 150 : 520, opacity: Math.min(enter, leave), transform: `translateX(${(1 - enter) * -40}px)` }}>
      <Slab fill={INK.yellow} shadow={INK.ink} color={INK.ink} size={size}>{slab}</Slab>
      {sub && (
        <div style={{ marginTop: size * 0.5, marginLeft: size * 0.25 }}>
          <Slab fill={INK.ink} shadow={INK.pink} color={INK.cream} size={size * 0.44} pad={`${size * 0.16}px ${size * 0.36}px ${size * 0.13}px`}>
            <span style={{ fontFamily: TXT, fontWeight: 600, textTransform: "none", letterSpacing: 0 }}>{sub}</span>
          </Slab>
        </div>
      )}
    </div>
  );
};

// The running tape across the top of every Studio shot, like the site's .tape.
const Tape: React.FC<{ vertical: boolean }> = ({ vertical }) => {
  const frame = useCurrentFrame();
  const words = "SCREEN RECORDING ✶ TATO STUDIO ✶ REAL TIME ✶ LIVE ON THE GPU ✶ ";
  const shift = (frame * 2.2) % 900;
  return (
    <div style={{ position: "absolute", left: "-6%", width: "112%", top: vertical ? 80 : 18, rotate: "-2.6deg", background: INK.ink, boxShadow: `0 ${vertical ? 10 : 8}px 0 ${INK.pink}`, overflow: "hidden", padding: vertical ? "14px 0" : "10px 0" }}>
      <div style={{ whiteSpace: "nowrap", fontFamily: DSP, fontWeight: 800, fontSize: vertical ? 30 : 24, color: INK.cream, letterSpacing: "0.02em", transform: `translateX(${-shift}px)` }}>
        {words.repeat(8).split("✶").map((part, index) => (
          <span key={index}>{part}{index < 63 && <span style={{ color: INK.pink, fontWeight: 900 }}>✶</span>}</span>
        ))}
      </div>
    </div>
  );
};

// A cut join: a skewed band of ink that wipes across at the head of each scene.
const CutJoin: React.FC<{ color: string }> = ({ color }) => {
  const frame = useCurrentFrame();
  const t = interpolate(frame, [0, 12], [0, 1], { extrapolateRight: "clamp", easing: Easing.inOut(Easing.cubic) });
  if (t >= 1) return null;
  return <AbsoluteFill style={{ background: color, clipPath: `polygon(${t * 140 - 20}% 0, 120% 0, 120% 100%, ${t * 140 - 40}% 100%)` }} />;
};

const Crop: React.FC<{ src: string; from: number; crop: Rect; box: Rect; size?: [number, number] }> = ({ src, from, crop, box, size = [1920, 1080] }) => {
  const scale = box.w / crop.w;
  const h = crop.h * scale;
  return (
    <div style={{ position: "absolute", left: box.x, top: box.y, width: box.w, height: h }}>
      <div style={{ position: "absolute", inset: 0, background: INK.pink, translate: "12px 12px", clipPath: "polygon(0 0, calc(100% - 34px) 0, 100% 34px, 100% 100%, 18px 100%, 0 calc(100% - 18px))" }} />
      <div style={{ position: "absolute", inset: 0, overflow: "hidden", clipPath: "polygon(0 0, calc(100% - 34px) 0, 100% 34px, 100% 100%, 18px 100%, 0 calc(100% - 18px))" }}>
        <OffthreadVideo src={staticFile(src)} startFrom={frames(from)} muted style={{ position: "absolute", width: size[0] * scale, height: size[1] * scale, left: -crop.x * scale, top: -crop.y * scale, maxWidth: "none" }} />
      </div>
    </div>
  );
};

// A part of the recording scaled to fill a box, cropped from its centre: the 9:16 cut fills the frame.
const Fill: React.FC<{ src: string; from: number; crop: Rect; box: Rect; size?: [number, number] }> = ({ src, from, crop, box, size = [1920, 1080] }) => {
  const scale = Math.max(box.w / crop.w, box.h / crop.h);
  const cx = crop.x + crop.w / 2;
  const cy = crop.y + crop.h / 2;
  const cut = "polygon(0 0, calc(100% - 40px) 0, 100% 40px, 100% 100%, 22px 100%, 0 calc(100% - 22px))";
  return (
    <div style={{ position: "absolute", left: box.x, top: box.y, width: box.w, height: box.h }}>
      <div style={{ position: "absolute", inset: 0, background: INK.pink, translate: "12px 12px", clipPath: cut }} />
      <div style={{ position: "absolute", inset: 0, overflow: "hidden", clipPath: cut }}>
        <OffthreadVideo src={staticFile(src)} startFrom={frames(from)} muted style={{ position: "absolute", width: size[0] * scale, height: size[1] * scale, left: box.w / 2 - cx * scale, top: box.h / 2 - cy * scale, maxWidth: "none" }} />
      </div>
    </div>
  );
};

// The 9:16 frame's room for footage: under the titles, clear of Instagram's caption (bottom 20%) and buttons (right 120 px).
const V_AREA: Rect = { x: 36, y: 400, w: 912, h: 1110 };

const StudioScene: React.FC<{ scene: Scene; vertical: boolean; length: number }> = ({ scene, vertical, length }) => {
  const frame = useCurrentFrame();
  if (vertical) {
    const area = V_AREA;
    return (
      <AbsoluteFill style={{ background: INK.band }}>
        {scene.stack ? (
          <>
            <Fill src={scene.src} from={scene.from} crop={scene.stack[0]} size={scene.size} box={{ x: area.x, y: area.y, w: area.w, h: area.h * 0.47 }} />
            <Fill src={scene.src} from={scene.from} crop={scene.stack[1]} size={scene.size} box={{ x: area.x, y: area.y + area.h * 0.53, w: area.w, h: area.h * 0.47 }} />
          </>
        ) : (
          scene.picture && <Fill src={scene.src} from={scene.from} crop={scene.picture} size={scene.size} box={area} />
        )}
        {scene.detail && !scene.stack && (
          <Crop src={scene.src} from={scene.from} crop={scene.detail} size={scene.size} box={{ x: area.x + 40, y: area.y + area.h - 40 - Math.round((scene.detail.h * 560) / scene.detail.w), w: 560, h: 0 }} />
        )}
        <Tape vertical />
      </AbsoluteFill>
    );
  }
  const push = interpolate(frame, [0, length], [1.0, 1.1], { easing: Easing.inOut(Easing.quad) });
  const [fx, fy] = scene.focus ?? [960, 540];
  return (
    <AbsoluteFill style={{ background: INK.ink }}>
      <div style={{ position: "absolute", inset: 0, transformOrigin: `${fx * (1920 / (scene.wide?.w ?? 1920))}px ${fy * (1080 / (scene.wide?.h ?? 1080))}px`, transform: `scale(${push})` }}>
        {(() => {
          const size = scene.size ?? [1920, 1080];
          const view = scene.wide ?? { x: 0, y: 0, w: size[0], h: size[1] };
          const k = 1920 / view.w;
          return <OffthreadVideo src={staticFile(scene.src)} startFrom={frames(scene.from)} muted style={{ position: "absolute", width: size[0] * k, height: size[1] * k, left: -view.x * k, top: -view.y * k, maxWidth: "none" }} />;
        })()}
      </div>
      <AbsoluteFill style={{ background: "linear-gradient(180deg, rgba(20,16,31,0.8) 0%, rgba(20,16,31,0) 30%)" }} />
      <Tape vertical={false} />
    </AbsoluteFill>
  );
};

const Broll: React.FC<{ scene: Scene; vertical: boolean }> = ({ scene, vertical }) => (
  <AbsoluteFill style={{ background: INK.ink }}>
    <OffthreadVideo src={staticFile(vertical ? "take_vertical.mp4" : "take_wide.mp4")} startFrom={frames(scene.from)} muted style={{ width: "100%", height: "100%" }} />
    <AbsoluteFill style={{ background: "linear-gradient(180deg, rgba(13,26,74,0.7) 0%, rgba(13,26,74,0) 28%)" }} />
  </AbsoluteFill>
);

const EndCard: React.FC<{ scene: Scene; vertical: boolean; credit: string }> = ({ scene, vertical, credit }) => {
  const frame = useCurrentFrame();
  const slide = interpolate(frame, [0, 16], [0, 1], { extrapolateRight: "clamp", easing: Easing.out(Easing.cubic) });
  const rise = interpolate(frame, [8, 30], [40, 0], { extrapolateLeft: "clamp", extrapolateRight: "clamp", easing: Easing.out(Easing.cubic) });
  const shown = interpolate(frame, [8, 24], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const big = vertical ? 112 : 104;
  return (
    <AbsoluteFill style={{ background: INK.ink }}>
      <OffthreadVideo src={staticFile(vertical ? "take_vertical.mp4" : "take_wide.mp4")} startFrom={frames(scene.from)} muted style={{ width: "100%", height: "100%" }} />
      {/* The site's blue, cut in on a slant. */}
      <AbsoluteFill style={{ background: INK.blue, clipPath: `polygon(0 0, ${slide * 130}% 0, ${slide * 130 - 30}% 100%, 0 100%)` }} />
      <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", opacity: shown, transform: `translateY(${rise}px)`, padding: vertical ? "0 70px" : "0 160px" }}>
        <Img src={staticFile("tatolab-logo-on-blue.svg")} style={{ width: vertical ? 560 : 520 }} />
        <div style={{ marginTop: vertical ? 70 : 46, fontFamily: DSP, fontWeight: 900, fontSize: big, lineHeight: 0.98, textTransform: "uppercase", color: INK.cream, textAlign: "center", textShadow: `0.034em 0.04em 0 ${INK.pink}` }}>Build realtime interactive experiences</div>
        <div style={{ marginTop: 34, fontFamily: TXT, fontWeight: 600, fontSize: vertical ? 34 : 30, lineHeight: 1.4, color: INK.cream, textAlign: "center", maxWidth: vertical ? 880 : 1180 }}>
          If you've used TouchDesigner, Blender or Unreal Engine, you'll feel right at home. Tato is free (like free free) and open source. What will you build?
        </div>
        <div style={{ marginTop: 40 }}>
          <Slab fill={INK.yellow} shadow={INK.ink} color={INK.ink} size={vertical ? 46 : 40}>tatolab.com</Slab>
        </div>
        <div style={{ marginTop: vertical ? 120 : 56, fontFamily: MONO, fontSize: vertical ? 19 : 17, color: "rgba(255,244,224,0.82)", textAlign: "center", lineHeight: 1.6, maxWidth: vertical ? 900 : 1400 }}>
          BATHYMETRY NOAA NCEI ETOPO 2022 · CURRENTS HYCOM.ORG / U.S. NAVY ESPC-D, SNAPSHOT 16 NOV 2026 · STATION NOAA CO-OPS 8723214 VIRGINIA KEY<br />{credit}
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};

export type ReelProps = { vertical: boolean; audio: string; credit: string };

export const Reel: React.FC<ReelProps> = ({ vertical, audio, credit }) => (
  <AbsoluteFill style={{ background: INK.ink }}>
    {SCENES.map((scene, index) => {
      const length = frames(scene.until) - frames(scene.at);
      return (
        <Sequence key={index} from={frames(scene.at)} durationInFrames={length}>
          {scene.kind === "studio" && <StudioScene scene={scene} vertical={vertical} length={length} />}
          {scene.kind === "broll" && <Broll scene={scene} vertical={vertical} />}
          {scene.kind === "end" && <EndCard scene={scene} vertical={vertical} credit={credit} />}
          {scene.slab && <Title slab={scene.slab} sub={scene.sub} vertical={vertical} length={length} />}
          {index > 0 && <CutJoin color={index % 2 ? INK.pink : INK.blue} />}
        </Sequence>
      );
    })}
    <Grain />
    <Audio src={staticFile(audio)} />
  </AbsoluteFill>
);

export const Cover: React.FC = () => (
  <AbsoluteFill style={{ background: INK.blue }}>
    <Img src={staticFile("cover_frame.png")} style={{ position: "absolute", width: "100%", height: "100%", objectFit: "cover", clipPath: "polygon(0 34%, 100% 26%, 100% 100%, 0 100%)" }} />
    <div style={{ position: "absolute", top: 150, left: 72, right: 72 }}>
      <Img src={staticFile("tatolab-logo-on-blue.svg")} style={{ width: 360 }} />
      <div style={{ marginTop: 44, fontFamily: DSP, fontWeight: 900, fontSize: 118, lineHeight: 0.96, textTransform: "uppercase", color: INK.cream, textShadow: `0.034em 0.04em 0 ${INK.pink}` }}>The ocean off Miami</div>
    </div>
    <div style={{ position: "absolute", bottom: 420, left: 72 }}>
      <Slab fill={INK.yellow} shadow={INK.ink} color={INK.ink} size={54}>Live in Tato Studio</Slab>
    </div>
    <Grain />
  </AbsoluteFill>
);

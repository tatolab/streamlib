// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// The stage's camera as the renderer flies it, and the real map's square as the renderer lays it on
// world units from -2.5 to 2.5, so the view draws its labels where the picture shows each place
// and drops an eddy where the water was clicked.

import { STAGE_FOV_DEGREES } from "./stage_graph.ts";

/** Where the stage has been orbited, zoomed and flown to, and where it was last clicked to drop an eddy: the record the view sends on `camera`. */
export type StageCamera = {
  target_x: number;
  target_y: number;
  distance: number;
  yaw_degrees: number;
  pitch_degrees: number;
  bursts: number;
  burst_x: number;
  burst_y: number;
};

/** A point in world units: across, along and up. */
export type WorldPoint = readonly [number, number, number];

/** The camera's place and its three axes, and its focal length, as the renderer projects with them. */
type CameraFrame = {
  position: WorldPoint;
  forward: WorldPoint;
  right: WorldPoint;
  up: WorldPoint;
  focal: number;
};

/** The real map's square, as the renderer maps it onto world units from -`half` to `half`. */
const MAP = { south: 24.3, north: 27.3, west: -81.0, east: -77.67, half: 2.5 };

/** The world point, on the sea, of `longitude` and `latitude`. */
export function worldOf(longitude: number, latitude: number): readonly [number, number] {
  const across = (longitude - MAP.west) / (MAP.east - MAP.west);
  const along = (latitude - MAP.south) / (MAP.north - MAP.south);
  return [(across * 2 - 1) * MAP.half, (along * 2 - 1) * MAP.half];
}

const dot = (a: WorldPoint, b: WorldPoint): number => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

/** The camera's frame as the renderer flies it: pitch and distance held to what it draws. */
export function stageCameraFrame(camera: StageCamera): CameraFrame {
  const pitchDegrees = Math.min(Math.max(camera.pitch_degrees, 2), 89.9);
  const distance = Math.min(Math.max(camera.distance, 0.02), 6);
  const yaw = (camera.yaw_degrees * Math.PI) / 180;
  const pitch = (pitchDegrees * Math.PI) / 180;
  const forward: WorldPoint = [Math.sin(yaw) * Math.cos(pitch), Math.cos(yaw) * Math.cos(pitch), -Math.sin(pitch)];
  const right: WorldPoint = [Math.cos(yaw), -Math.sin(yaw), 0];
  const up: WorldPoint = [
    right[1] * forward[2] - right[2] * forward[1],
    right[2] * forward[0] - right[0] * forward[2],
    right[0] * forward[1] - right[1] * forward[0],
  ];
  const position: WorldPoint = [camera.target_x - forward[0] * distance, camera.target_y - forward[1] * distance, Math.max(0.006, -forward[2] * distance)];
  return { position, forward, right, up, focal: 1 / Math.tan((STAGE_FOV_DEGREES * Math.PI) / 360) };
}

/** Where `point` falls in a picture of `aspect`, from 0 to 1 across and down; `null` behind the camera. */
export function projectedOnThePicture(frame: CameraFrame, point: WorldPoint, aspect: number): readonly [number, number] | null {
  const relative: WorldPoint = [point[0] - frame.position[0], point[1] - frame.position[1], point[2] - frame.position[2]];
  const depth = dot(relative, frame.forward);
  if (depth <= 1e-6) return null;
  const x = (dot(relative, frame.right) / depth) * frame.focal;
  const y = (dot(relative, frame.up) / depth) * frame.focal;
  return [0.5 + (0.5 * x) / aspect, 0.5 - 0.5 * y];
}

/** The point on the sea seen at `across` and `down` in a picture of `aspect`; `null` where the picture shows sky. */
export function seaPointAt(frame: CameraFrame, across: number, down: number, aspect: number): readonly [number, number] | null {
  const x = (across - 0.5) * 2 * aspect;
  const y = (0.5 - down) * 2;
  const ray = [0, 1, 2].map((axis) => (frame.forward[axis] ?? 0) * frame.focal + (frame.right[axis] ?? 0) * x + (frame.up[axis] ?? 0) * y);
  const [rayAcross = 0, rayAlong = 0, rayUp = 0] = ray;
  if (rayUp >= -1e-6) return null;
  const travelled = frame.position[2] / -rayUp;
  return [frame.position[0] + rayAcross * travelled, frame.position[1] + rayAlong * travelled];
}

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Writes the project's graph: `streams/main.graph.json`.

    python write_graph.py live
    python write_graph.py stage
    python write_graph.py take WIDTH HEIGHT FOV OUTPUT [STEPS] [NODE.PORT]

Every graph runs at FPS frames a second (60): a take steps 1/FPS s at a time, so
1440 steps is the 24 s take, and the current advances and fades per step to match.

`live` is the take's graph on a real-time clock, the picture into Program.
`stage` is the one to play with in Tato Studio: the picture into the
Gulf Stream Neon stage view, whose camera and sliders steer it live. It also
writes `views/stage/stage_graph.js`, the node ids the view's sliders set. `take` records one frame-exact take: a stepped clock that stops
after STEPS steps (1440 is 24 s at 60 fps), the picture into FrameRecorder too,
or any NODE.PORT of the graph instead, to look at one stage.
"""

from __future__ import annotations

import json
import sys
import uuid
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
FPS = 60
STEP_NS = round(1e9 / FPS)
# The trails keep the same length in seconds at any frame rate: 0.86 a frame at 30.
TRAIL_DECAY = round(0.86 ** (30 / FPS), 4)
GRAPH_ASSET = "9e1f3a5c-7b9d-4e2f-8a4c-6e8f0a2c4e61"
CORE = "imports/core-nodes/core_nodes"
NAMESPACE = uuid.UUID("5b0f6c2e-8d7a-4f1e-9c3b-2a6e4d8f1c70")
# The stage values the view's sliders set, each eased by a Glide settling in this time.
SMOOTHED_STAGE_VALUES = {"morph": 0.0, "time_of_day": 0.3, "skyline_height": 0.055, "neon": 1.0, "current_speed": 1.0}
SMOOTHING_SECONDS = 0.25
COLUMN = 430
ROW = 230


def node_id(name: str) -> str:
    return str(uuid.uuid5(NAMESPACE, name))


def graph(
    width: int, height: int, fov: float, output: str | None, steps: int | None, tap: str | None = None, stage: bool = False
) -> dict:
    aspect = width / height
    nodes: list[dict] = []
    links: list[dict] = []

    def add(name: str, type_path: str, column: float, row: float, **values) -> None:
        entry = {
            "id": node_id(name),
            "name": name,
            "type": type_path,
            "studio": {"contract": 1, "position": [round(column * COLUMN), round(row * ROW)]},
        }
        if values:
            entry["input_values"] = values
        nodes.append(entry)

    def wire(source: str, source_port: str, target: str, target_port: str) -> None:
        links.append(
            {
                "source": {"node": node_id(source), "port": source_port},
                "target": {"node": node_id(target), "port": target_port},
            }
        )

    # Time, music and the real world.
    add("take_clock", f"{CORE}.animation:Timer", 0, -3)
    add("soundtrack_pulse", "nodes.soundtrack_pulse:SoundtrackPulse", 0, 1, soundtrack_path="media/soundtrack.wav")
    add("noaa_virginia_key", "nodes.noaa_station:NoaaStationFeed", 0, 4, mode="replay", take_seconds=90.0 if stage else 24.0)

    # The number nodes: the take and the kick, turned into knobs.
    add("soundtrack_beat", "nodes.take_functions:SoundtrackBeat", 1, -5)
    if stage:
        add("stage_sun", "nodes.take_functions:StageSunOnScreen", 1, -1, aspect=aspect, vertical_fov_degrees=fov, time_of_day=0.3)
    else:
        add("take_beats", "nodes.take_functions:TakeBeats", 1, -3)
        add("sun_on_screen", "nodes.take_functions:SunOnScreen", 1, -1, aspect=aspect, vertical_fov_degrees=fov)
    sun = "stage_sun" if stage else "sun_on_screen"
    add("kick_lag", f"{CORE}.animation:Lag", 2, -6, seconds=0.08)
    add("bloom_from_kick", f"{CORE}.math_nodes:Remap", 3, -7, from_low=0.0, from_high=1.0, to_low=0.12, to_high=0.4)
    add("streak_from_kick", f"{CORE}.math_nodes:Remap", 3, -6, from_low=0.0, from_high=1.0, to_low=0.12, to_high=0.45)
    add("exposure_through_drop", f"{CORE}.math_nodes:Lerp", 3, -5, a=0.85, b=1.15, **({"t": 0.0} if stage else {}))
    add("drop_energy", f"{CORE}.math_nodes:Multiply", 3, -4, **({"a": 0.35} if stage else {}))
    add("aberration_from_energy", f"{CORE}.math_nodes:Remap", 4, -4, from_low=0.0, from_high=1.0, to_low=0.015, to_high=0.09)
    add("haze_shimmer", f"{CORE}.animation:Lfo", 5, -6, rate_hz=0.35)
    add("haze_amount", f"{CORE}.math_nodes:Remap", 6, -6, from_low=-1.0, from_high=1.0, to_low=0.0015, to_high=0.005)
    add("ghosts_through_sunset", f"{CORE}.math_nodes:Remap", 4, -3, from_low=0.0, from_high=1.0, to_low=0.05, to_high=0.02, **({"x": 0.0} if stage else {}))

    # The compute nodes: the real seafloor, the real current, the picture.
    add("etopo_seafloor", "nodes.bathymetry_relief:BathymetryRelief", 1, 2)
    if stage:
        add("hycom_gulf_stream", "nodes.gulf_stream_current:GulfStreamCurrent", 1, 5, particle_count=4_194_304, active_particles=2_097_152, step_seconds=1.0 / FPS, trail_decay=TRAIL_DECAY)
    else:
        add("hycom_gulf_stream", "nodes.gulf_stream_current:GulfStreamCurrent", 1, 5, particle_count=2_097_152, step_seconds=1.0 / FPS, trail_decay=TRAIL_DECAY)
    add(
        "neon_horizon",
        "nodes.neon_horizon_renderer:NeonHorizonRenderer",
        2.5,
        2,
        width=width,
        height=height,
        vertical_field_of_view_degrees=fov,
        **({"mode": "stage", "morph": 0.0, "time_of_day": 0.3} if stage else {}),
    )
    add("hdr_bloom", "nodes.hdr_bloom:HdrBloomPyramid", 3.6, 3.5)
    add("god_rays", "nodes.lens_shaders:GodRays", 3.6, 0.5)

    # The lens, a chain of shader nodes the runtime fuses on the GPU.
    add("lens_composite", "nodes.lens_shaders:LensComposite", 5, 1.5, rays_strength=0.45)
    add(
        "depth_of_field",
        "nodes.lens_shaders:DepthOfField",
        6,
        1.5,
        aperture=0.0012 if stage else (0.003 if aspect < 1 else 0.002),
        aspect=1.0 / aspect,
        **({"focus_distance": 0.32} if stage else {}),
    )
    add("heat_haze", "nodes.lens_shaders:HeatHaze", 7, 1.5, band=0.07)
    add("chromatic_aberration", "nodes.lens_shaders:ChromaticAberration", 8, 1.5, aspect=aspect)
    add("neon_grade", "nodes.lens_shaders:NeonGrade", 9, 1.5, contrast=1.18, saturation=1.15)
    add("film_finish", "nodes.lens_shaders:FilmFinish", 10, 1.5, lines_across=float(min(height, 1080)))
    add("vignette", f"{CORE}.filters:Vignette", 11, 1.5, amount=0.42, size=0.62, softness=0.55)
    if stage:
        add("stage", "Gulf Stream Neon:stage", 12, 0.5)
        # The stage's hand, smoothed in the graph: every slider eases through a Glide spring, the
        # camera through SmoothCamera, so a few edits a second arrive as a 60 fps glide.
        for row, (name, value) in enumerate(SMOOTHED_STAGE_VALUES.items()):
            add(f"smooth_{name}", "nodes.take_functions:Glide", 0.9, 7 + row, x=value, seconds=SMOOTHING_SECONDS)
        add("stage_look", "nodes.take_functions:StageLook", 2.0, 8.5)
        add("smooth_camera", "nodes.take_functions:SmoothCamera", 11, 3.2, seconds=0.18)
    else:
        add("audio_video_preview", "Core Nodes:audio_video_preview", 12, 0.5)

    wire("take_clock", "result", "soundtrack_beat", "seconds")
    if not stage:
        wire("take_clock", "result", "take_beats", "seconds")
        wire("take_clock", "result", "sun_on_screen", "seconds")
    wire("take_clock", "result", "heat_haze", "seconds")
    wire("take_clock", "result", "film_finish", "seconds")
    wire("soundtrack_beat", "beat", "kick_lag", "x")
    wire("kick_lag", "result", "bloom_from_kick", "x")
    wire("kick_lag", "result", "streak_from_kick", "x")
    if not stage:
        wire("take_beats", "drop", "exposure_through_drop", "t")
        wire("take_beats", "drop", "drop_energy", "a")
        wire("take_beats", "sunset", "ghosts_through_sunset", "x")
        wire("take_beats", "focus_distance", "depth_of_field", "focus_distance")
    wire("soundtrack_beat", "energy", "drop_energy", "b")
    wire("drop_energy", "result", "aberration_from_energy", "x")
    wire("haze_shimmer", "result", "haze_amount", "x")

    wire("soundtrack_pulse", "pulse", "etopo_seafloor", "soundtrack")
    wire("soundtrack_pulse", "pulse", "hycom_gulf_stream", "soundtrack")
    wire("soundtrack_pulse", "pulse", "neon_horizon", "soundtrack")
    wire("noaa_virginia_key", "station", "hycom_gulf_stream", "station")
    wire("noaa_virginia_key", "station", "neon_horizon", "station")
    wire("etopo_seafloor", "contours", "neon_horizon", "contours")
    wire("etopo_seafloor", "relief", "neon_horizon", "relief")
    wire("hycom_gulf_stream", "trails", "neon_horizon", "trails")

    wire("neon_horizon", "scene", "hdr_bloom", "scene")
    wire("neon_horizon", "scene", "god_rays", "scene")
    wire(sun, "sun_x", "god_rays", "sun_x")
    wire(sun, "sun_y", "god_rays", "sun_y")
    wire(sun, "sun_visible", "god_rays", "visible")
    wire("neon_horizon", "scene", "lens_composite", "scene")
    wire("hdr_bloom", "bloom", "lens_composite", "bloom")
    wire("hdr_bloom", "streak", "lens_composite", "streak")
    wire("god_rays", "result", "lens_composite", "rays")
    wire("exposure_through_drop", "result", "lens_composite", "exposure")
    wire("bloom_from_kick", "result", "lens_composite", "bloom_strength")
    wire("streak_from_kick", "result", "lens_composite", "streak_strength")
    wire("ghosts_through_sunset", "result", "lens_composite", "ghost_strength")
    wire(sun, "sun_x", "lens_composite", "sun_x")
    wire(sun, "sun_y", "lens_composite", "sun_y")
    wire("lens_composite", "result", "depth_of_field", "image")
    wire("neon_horizon", "scene", "depth_of_field", "depth")
    wire("depth_of_field", "result", "heat_haze", "image")
    wire("haze_amount", "result", "heat_haze", "amount")
    wire(sun, "horizon_y", "heat_haze", "horizon_y")
    wire("heat_haze", "result", "chromatic_aberration", "image")
    wire("aberration_from_energy", "result", "chromatic_aberration", "amount")
    wire("chromatic_aberration", "result", "neon_grade", "image")
    wire("neon_grade", "result", "film_finish", "image")
    wire("film_finish", "result", "vignette", "image")
    if stage:
        wire("vignette", "result", "stage", "video")
        wire("noaa_virginia_key", "station", "stage", "station")
        wire("stage", "camera", "smooth_camera", "stage")
        wire("smooth_camera", "result", "neon_horizon", "stage")
        wire("smooth_camera", "result", "hycom_gulf_stream", "stage")
        wire("smooth_camera", "result", "stage_sun", "stage")
        for name in SMOOTHED_STAGE_VALUES:
            wire(f"smooth_{name}", "result", "stage_look", name)
        wire("smooth_time_of_day", "result", "stage_sun", "time_of_day")
        wire("stage_look", "result", "neon_horizon", "look")
        wire("stage_look", "result", "hycom_gulf_stream", "look")
    else:
        wire("vignette", "result", "audio_video_preview", "video")

    document: dict = {"asset": GRAPH_ASSET}
    if output is not None:
        add("frame_recorder", "nodes.frame_recorder:FrameRecorder", 12, 2.5, output_path=output, frames_per_second=FPS)
        tap_node, tap_port = (tap.split(".") if tap else ("vignette", "result"))
        wire(tap_node, tap_port, "frame_recorder", "program")
        document["clock"] = {"mode": "stepped", "step_ns": STEP_NS, "pace": None, "stop_after_steps": steps}
    document["links"] = links
    document["nodes"] = nodes
    return document


def write_the_stage_view_ids(document: dict) -> None:
    """The node ids and the stream asset the stage view's sliders set, and its camera's home."""
    sys.path.insert(0, str(PROJECT))
    from nodes.neon_world import STAGE_HOME

    ids = {entry["name"]: entry["id"] for entry in document["nodes"]}
    lines = [
        "// Copyright (c) 2025 Tatolab LLC",
        "// SPDX-License-Identifier: GPL-3.0-only",
        "",
        "// Written by soundtrack/write_graph.py stage: the stage graph's node ids by name, its asset,",
        "// the renderer's lens and the camera's home, so the view needs no graph read to steer it.",
        f"export const STREAM_ASSET = {json.dumps(document['asset'])};",
        "export const STAGE_FOV_DEGREES = 50;",
        f"export const STAGE_HOME = {json.dumps({k: round(v, 5) for k, v in STAGE_HOME.items()})};",
        f"export const NODE_IDS = {json.dumps(ids, indent=2)};",
        "",
    ]
    (PROJECT / "views" / "stage" / "stage_graph.js").write_text("\n".join(lines))


def main(arguments: list[str]) -> None:
    if arguments[0] == "stage":
        document = graph(1280, 720, 50.0, None, None, stage=True)
        write_the_stage_view_ids(document)
    elif arguments[0] == "live":
        width, height, fov = (int(arguments[1]), int(arguments[2]), float(arguments[3])) if len(arguments) > 3 else (1080, 1920, 62.0)
        document = graph(width, height, fov, None, None)
    else:
        width, height, fov, output = int(arguments[1]), int(arguments[2]), float(arguments[3]), arguments[4]
        steps = int(arguments[5]) if len(arguments) > 5 else 24 * FPS
        tap = arguments[6] if len(arguments) > 6 else None
        document = graph(width, height, fov, output, steps, tap)
    path = PROJECT / "streams" / "main.graph.json"
    path.write_text(json.dumps(document, indent=2) + "\n")
    print(f"wrote {path}: {len(document['nodes'])} nodes, {len(document['links'])} links")


if __name__ == "__main__":
    main(sys.argv[1:])

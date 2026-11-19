# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
"""Writes streams/main.graph.json for one take: extent, steps, output and lens from argv."""
import json, sys
w, h, steps, out, fov = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), sys.argv[4], float(sys.argv[5])
ids = {
 "pulse": "1d3c5e7a-0b2f-4c6d-8e9a-1b2c3d4e5f60",
 "seafloor": "2e4d6f8b-1c3a-4d7e-9fab-2c3d4e5f6071",
 "current": "3f5e7a9c-2d4b-4e8f-a0bc-3d4e5f607182",
 "horizon": "4a6f8bad-3e5c-4f9a-b1cd-4e5f60718293",
 "recorder": "5b7a9cbe-4f6d-4a0b-c2de-5f60718293a4",
}
def link(a, ap, b, bp): return {"source": {"node": ids[a], "port": ap}, "target": {"node": ids[b], "port": bp}}
doc = {
 "asset": "9e1f3a5c-7b9d-4e2f-8a4c-6e8f0a2c4e61",
 "clock": {"mode": "stepped", "step_ns": 33_333_333, "pace": None, "stop_after_steps": steps},
 "links": [
  link("pulse", "pulse", "seafloor", "soundtrack"),
  link("pulse", "pulse", "current", "soundtrack"),
  link("pulse", "pulse", "horizon", "soundtrack"),
  link("seafloor", "contours_to_downstream", "horizon", "contours_from_upstream"),
  link("current", "current_to_downstream", "horizon", "current_from_upstream"),
  link("horizon", "program_to_downstream", "recorder", "program_from_upstream"),
 ],
 "nodes": [
  {"id": ids["pulse"], "name": "soundtrackpulse", "type": "nodes.soundtrack_pulse:SoundtrackPulse", "config": {"soundtrack_path": "media/soundtrack.wav"}},
  {"id": ids["seafloor"], "name": "seafloorrelief", "type": "nodes.seafloor_relief:SeafloorRelief"},
  {"id": ids["current"], "name": "gulfstreamcurrent", "type": "nodes.gulf_stream_current:GulfStreamCurrent", "config": {"particle_count": 160000, "trail_decay": 0.9, "turbulence": 1.0}},
  {"id": ids["horizon"], "name": "neonhorizoncompositor", "type": "nodes.neon_horizon_compositor:NeonHorizonCompositor", "config": {"width": w, "height": h, "vertical_field_of_view_degrees": fov, "exposure": 1.15}},
  {"id": ids["recorder"], "name": "framerecorder", "type": "nodes.frame_recorder:FrameRecorder", "config": {"output_path": out}},
 ],
}
json.dump(doc, open("__import__("pathlib").Path(__file__).resolve().parent.parent / "streams" / "main.graph.json"", "w"), indent=2)

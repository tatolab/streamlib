// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// Written by soundtrack/write_graph.py stage: the stage graph's node ids by name, its asset,
// the renderer's lens and the camera's home, so the view needs no graph read to steer it.
export const STREAM_ASSET = "9e1f3a5c-7b9d-4e2f-8a4c-6e8f0a2c4e61";
export const STAGE_FOV_DEGREES = 50;
export const STAGE_HOME = {"target_x": -1.02853, "target_y": 0.1, "distance": 0.32, "yaw_degrees": -22.0, "pitch_degrees": 16.0};
export const NODE_IDS: Readonly<Record<string, string>> = {
  "take_clock": "48a546bf-eb3d-55df-98f1-b4ad1287fbd8",
  "soundtrack_pulse": "adbc5ed2-0ee0-5e74-af6c-dd8179639ee4",
  "noaa_virginia_key": "10ddb26d-1ff7-5eee-9f92-332feff0e2fa",
  "soundtrack_beat": "61c24bbb-6452-5b31-bc93-669e1dcda6d7",
  "stage_sun": "8e7f3d8f-a4c7-5b15-b1a7-7d644a6ceea8",
  "kick_lag": "6a17694b-3418-5c66-a52b-fde48dffc763",
  "bloom_from_kick": "27c0b05a-0be1-587c-8bfb-747eeb33f3b2",
  "streak_from_kick": "e5319ce5-3c08-5cca-8a23-b2b4e4eb6a68",
  "exposure_through_drop": "5474b15f-70e5-5d54-a979-f25de96ea40a",
  "drop_energy": "f882471b-ba10-5c1e-93b4-3bbca2b5dced",
  "aberration_from_energy": "c6d86a66-48d0-5d51-8263-3065bb3d77f9",
  "haze_shimmer": "1ebbb135-0aae-5feb-aedf-b3486702c007",
  "haze_amount": "8ee805e7-4840-5f5c-bae8-b26f17288ca4",
  "ghosts_through_sunset": "13468854-5f12-529b-af16-2a7858b56f47",
  "etopo_seafloor": "a48b501f-1a70-5446-9d2b-1655bb9fb6bb",
  "hycom_gulf_stream": "b59ffd8c-ebcc-5699-99d3-bb8166b2c4fa",
  "neon_horizon": "c51b146e-0f83-539c-ac69-cdf3e971bc7f",
  "hdr_bloom": "438b4e73-7c1c-5655-ab89-4db96c2022e8",
  "god_rays": "d70ee5d1-44b9-57f9-8bbf-255a9c91c8ed",
  "lens_composite": "85981a3b-a504-5342-9b99-3c35c4dac41c",
  "depth_of_field": "6324e71e-0d87-5e0e-a57f-ea1467e0d65a",
  "heat_haze": "be3774e1-7d9b-531f-a5a3-648bf4e38568",
  "chromatic_aberration": "a9446055-83d7-5f7e-afd0-496d6f83e8e5",
  "neon_grade": "dd28435b-95a7-5421-8406-cbd757d67850",
  "film_finish": "9780eeab-6413-5185-a1e1-459bc38f0122",
  "vignette": "2b6b780b-b287-5045-9e6e-4c248f68bce9",
  "stage": "271e5c66-0178-5594-89c7-2c6dc0710f7a",
  "smooth_morph": "591adecd-91d8-5ff4-a3b2-eac2c159ee9a",
  "smooth_time_of_day": "e60fd662-6339-517a-8507-4e47067c777a",
  "smooth_skyline_height": "1c3f57f2-e2c7-5b54-ad9b-0ea0a05350e7",
  "smooth_neon": "5d1c19e5-8c7c-5431-96c1-dea886be22b5",
  "smooth_current_speed": "7bb05b8f-bad8-5f12-94ec-1be071a894d1",
  "stage_look": "4e28ead1-5693-5665-acbc-c82e5947bee9",
  "smooth_camera": "047ac7a1-8e99-58ab-b9bf-21327b27dbbd"
};

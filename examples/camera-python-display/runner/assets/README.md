# `camera-python-display` assets

> **Linux (#484) does not need any of these files.** The Linux pipeline runs a
> camera-bg + neon-skeleton overlay (see
> `python/pose_overlay_renderer.py`) entirely from YOLOv8 keypoints — no
> 3D mesh, no Mixamo skeleton, no asset gating.

This directory holds optional assets the **macOS** path uses for its
GLB-skinned 3D character (`python/avatar_character.py` macOS branch +
`python/character_renderer_3d.py`). When either file is missing the macOS
renderer falls back to a background-only or solid-clear frame, so the
pipeline still validates without them — but the avatar character won't
appear.

## `character/character.glb`

A Mixamo-rigged GLB character. The macOS `PoseSolver` looks up bone
rotations by Mixamo bone names (`mixamorig:LeftArm`, `mixamorig:RightArm`,
`mixamorig:LeftForeArm`, `mixamorig:RightForeArm`, etc.) so any GLB
exported with a different rig won't drive the skinning shader.

To add one:

1. Sign in at <https://www.mixamo.com> (free, requires an Adobe account).
2. Pick a humanoid character — `Ch41_nonPBR` and `X Bot` are popular
   defaults, but anything humanoid works.
3. Pick **T-Pose** as the animation (or any short loop — the runtime
   ignores Mixamo's animation track and overrides bone rotations from
   detected pose).
4. Download as **GLB** with **Skin** included. Save the file to
   `assets/character/character.glb`.

The file is intentionally not committed to the repo — Mixamo content
is gated by Adobe's terms of service and isn't redistributable from
this repository.

## `alley.jpg`

The 2D backdrop the macOS scene renders behind the character. Any image
works — JPEG / PNG / etc. — the renderer downsamples it to the
output resolution at load time. Cyberpunk alley shots match the
existing material palette in `character_renderer_3d.py`.

If the file is absent, the macOS renderer falls back to a cyberpunk
dark-violet clear color.

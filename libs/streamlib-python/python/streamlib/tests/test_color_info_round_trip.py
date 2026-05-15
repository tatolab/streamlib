# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Wire round-trip for VideoFrame's color metadata fields (#811).

Locks the JSON shape Python sees for ColorInfo / MasteringDisplay /
ContentLight after jtd-codegen regenerates from `packages/core/schemas/`.
Mirrors the Rust round-trip in
`libs/streamlib-engine/src/core/context/gpu_context.rs::videoframe_color_metadata_round_trip`
and the Deno equivalent — schema changes have to update all three.

Skip-when-None semantics: the Python jtd-codegen emits dataclasses where
optional fields default to a sentinel and round-trip as JSON only when
populated. We assert on the *parsed-back* value, not on the wire JSON,
because that's what application code observes.
"""

from __future__ import annotations

import pytest

# pyright: reportMissingImports=false
_color_info_mod = pytest.importorskip("streamlib._generated_.tatolab__core.color_info")
_content_light_mod = pytest.importorskip("streamlib._generated_.tatolab__core.content_light")
_mastering_display_mod = pytest.importorskip("streamlib._generated_.tatolab__core.mastering_display")
_video_frame_mod = pytest.importorskip("streamlib._generated_.tatolab__core.video_frame")

ColorInfo = _color_info_mod.ColorInfo
ColorInfoMatrix = _color_info_mod.ColorInfoMatrix
ColorInfoPrimaries = _color_info_mod.ColorInfoPrimaries
ColorInfoRange = _color_info_mod.ColorInfoRange
ColorInfoTransfer = _color_info_mod.ColorInfoTransfer
ContentLight = _content_light_mod.ContentLight
MasteringDisplay = _mastering_display_mod.MasteringDisplay
VideoFrame = _video_frame_mod.VideoFrame


def _bt2020_pq_color_info() -> ColorInfo:
    """Reference HDR10 stream tag — BT.2020 primaries + PQ transfer +
    BT.2020 NCL matrix + limited range. Matches the Rust + Deno
    fixtures so a wire-format drift on any axis surfaces in all three
    suites."""
    return ColorInfo(
        primaries=ColorInfoPrimaries.BT2020,
        transfer=ColorInfoTransfer.SMPTE2084,
        matrix=ColorInfoMatrix.BT2020_NCL,
        range=ColorInfoRange.LIMITED,
    )


def _bt2020_mastering_display() -> MasteringDisplay:
    """Reference ST.2086 mastering display in 1/50000 chromaticity +
    0.0001 cd/m^2 luminance increments — the wire format the H.265
    SEI / MP4 mdcv box carries verbatim."""
    return MasteringDisplay(
        display_primaries_r_x=35400,
        display_primaries_r_y=14600,
        display_primaries_g_x=8500,
        display_primaries_g_y=39850,
        display_primaries_b_x=6550,
        display_primaries_b_y=2300,
        white_point_x=15635,
        white_point_y=16450,
        min_luminance=1,         # 0.0001 cd/m^2
        max_luminance=10_000_000,  # 1000 cd/m^2
    )


def test_color_info_round_trip():
    info = _bt2020_pq_color_info()
    payload = info.to_json_data()
    assert payload == {
        "primaries": "bt2020",
        "transfer": "smpte2084",
        "matrix": "bt2020_ncl",
        "range": "limited",
    }
    parsed = ColorInfo.from_json_data(payload)
    assert parsed == info


def test_mastering_display_round_trip():
    mdcv = _bt2020_mastering_display()
    payload = mdcv.to_json_data()
    assert payload["display_primaries_r_x"] == 35400
    assert payload["max_luminance"] == 10_000_000
    parsed = MasteringDisplay.from_json_data(payload)
    assert parsed == mdcv


def test_content_light_round_trip():
    cll = ContentLight(max_cll=1000, max_fall=400)
    payload = cll.to_json_data()
    assert payload == {"max_cll": 1000, "max_fall": 400}
    parsed = ContentLight.from_json_data(payload)
    assert parsed == cll


def test_video_frame_with_full_color_metadata_round_trips():
    frame = VideoFrame(
        surface_id="s",
        width=1920,
        height=1080,
        timestamp_ns="0",
        frame_index="0",
        fps=None,
        texture_layout=None,
        color_info=_bt2020_pq_color_info(),
        mastering_display=_bt2020_mastering_display(),
        content_light=ContentLight(max_cll=1000, max_fall=400),
    )
    payload = frame.to_json_data()
    # Optional fields present on the wire when populated.
    assert payload["color_info"]["transfer"] == "smpte2084"
    assert payload["mastering_display"]["max_luminance"] == 10_000_000
    assert payload["content_light"]["max_cll"] == 1000
    parsed = VideoFrame.from_json_data(payload)
    assert parsed.color_info == frame.color_info
    assert parsed.mastering_display == frame.mastering_display
    assert parsed.content_light == frame.content_light


def test_video_frame_without_color_metadata_omits_fields_on_wire():
    """Absent color metadata must not serialize as `null` — older
    Python / Deno consumers (pre-#811) reject unknown fields, and a
    `null` value is structurally distinct from absence."""
    frame = VideoFrame(
        surface_id="s",
        width=1920,
        height=1080,
        timestamp_ns="0",
        frame_index="0",
        fps=None,
        texture_layout=None,
        color_info=None,
        mastering_display=None,
        content_light=None,
    )
    payload = frame.to_json_data()
    assert "color_info" not in payload
    assert "mastering_display" not in payload
    assert "content_light" not in payload
    parsed = VideoFrame.from_json_data(payload)
    assert parsed.color_info is None
    assert parsed.mastering_display is None
    assert parsed.content_light is None

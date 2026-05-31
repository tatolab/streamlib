// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Wire round-trip for VideoFrame's color metadata fields (#811).
 *
 * Locks the JSON shape Deno sees for ColorInfo / MasteringDisplay /
 * ContentLight after jtd-codegen regenerates from
 * `packages/core/schemas/`. Mirrors the Rust round-trip in
 * `libs/streamlib-engine/src/core/context/gpu_context.rs::videoframe_color_metadata_round_trip`
 * and the Python equivalent — schema changes have to update all three.
 *
 * TypeScript codegen emits `interface` types — there's no runtime
 * `from_json_data` step like Python's; round-trip is plain
 * JSON.stringify / JSON.parse with the type cast preserved.
 */

import { assertEquals } from "@std/assert";

import {
  type ColorInfo,
  ColorInfoMatrix,
  ColorInfoPrimaries,
  ColorInfoRange,
  ColorInfoTransfer,
} from "./_generated_/tatolab__core/color_info.ts";
import { type ContentLight } from "./_generated_/tatolab__core/content_light.ts";
import { type MasteringDisplay } from "./_generated_/tatolab__core/mastering_display.ts";
import { type VideoFrame } from "./_generated_/tatolab__core/video_frame.ts";

function bt2020PqColorInfo(): ColorInfo {
  return {
    primaries: ColorInfoPrimaries.Bt2020,
    transfer: ColorInfoTransfer.Smpte2084,
    matrix: ColorInfoMatrix.Bt2020Ncl,
    range: ColorInfoRange.Limited,
  };
}

function bt2020MasteringDisplay(): MasteringDisplay {
  return {
    display_primaries_r_x: 35400,
    display_primaries_r_y: 14600,
    display_primaries_g_x: 8500,
    display_primaries_g_y: 39850,
    display_primaries_b_x: 6550,
    display_primaries_b_y: 2300,
    white_point_x: 15635,
    white_point_y: 16450,
    min_luminance: 1, // 0.0001 cd/m^2
    max_luminance: 10_000_000, // 1000 cd/m^2
  };
}

Deno.test("ColorInfo serializes as snake_case discriminants (H.273 alignment)", () => {
  const info = bt2020PqColorInfo();
  const json = JSON.parse(JSON.stringify(info));
  assertEquals(json, {
    primaries: "bt2020",
    transfer: "smpte2084",
    matrix: "bt2020_ncl",
    range: "limited",
  });
  // Round-trip back to typed.
  const parsed = JSON.parse(JSON.stringify(info)) as ColorInfo;
  assertEquals(parsed.primaries, ColorInfoPrimaries.Bt2020);
  assertEquals(parsed.transfer, ColorInfoTransfer.Smpte2084);
  assertEquals(parsed.matrix, ColorInfoMatrix.Bt2020Ncl);
  assertEquals(parsed.range, ColorInfoRange.Limited);
});

Deno.test("MasteringDisplay round-trips with ST.2086 native units", () => {
  const mdcv = bt2020MasteringDisplay();
  const parsed = JSON.parse(JSON.stringify(mdcv)) as MasteringDisplay;
  assertEquals(parsed.display_primaries_r_x, 35400);
  assertEquals(parsed.max_luminance, 10_000_000);
  assertEquals(parsed.min_luminance, 1);
});

Deno.test("ContentLight round-trips with cd/m^2 values", () => {
  const cll: ContentLight = { max_cll: 1000, max_fall: 400 };
  const parsed = JSON.parse(JSON.stringify(cll)) as ContentLight;
  assertEquals(parsed, cll);
});

Deno.test("ColorInfo with no axes set serializes as empty object", () => {
  // Locks the foot-gun fix: every axis is optional in the schema, so
  // a ColorInfo with no axes set serializes to `{}` (no axis fields
  // on the wire). Mirrors Rust `ColorInfo::default()` and the Python
  // empty-dict invariant. Mentally revert `optionalProperties:` in
  // the schema and the JSON gains four `null` axis fields.
  const info: ColorInfo = {};
  const json = JSON.parse(JSON.stringify(info));
  assertEquals(json, {}, "ColorInfo with no axes must round-trip as empty object");
});

Deno.test("VideoFrame with all color metadata round-trips", () => {
  const frame: VideoFrame = {
    surface_id: "s",
    width: 1920,
    height: 1080,
    timestamp_ns: "0",
    color_info: bt2020PqColorInfo(),
    mastering_display: bt2020MasteringDisplay(),
    content_light: { max_cll: 1000, max_fall: 400 },
  };
  const json = JSON.parse(JSON.stringify(frame));
  assertEquals(json.color_info.transfer, "smpte2084");
  assertEquals(json.mastering_display.max_luminance, 10_000_000);
  assertEquals(json.content_light.max_cll, 1000);

  const parsed = JSON.parse(JSON.stringify(frame)) as VideoFrame;
  assertEquals(parsed.color_info, frame.color_info);
  assertEquals(parsed.mastering_display, frame.mastering_display);
  assertEquals(parsed.content_light, frame.content_light);
});

Deno.test("VideoFrame without color metadata omits fields on wire", () => {
  // Optional fields not set — the JSON wire form does not carry them
  // (mirrors Rust's `skip_serializing_if = "Option::is_none"` and
  // Python's absent-on-None contract). Older consumers rely on this:
  // a `null` value is structurally distinct from absence.
  const frame: VideoFrame = {
    surface_id: "s",
    width: 1920,
    height: 1080,
    timestamp_ns: "0",
  };
  const json = JSON.parse(JSON.stringify(frame));
  assertEquals(json.color_info, undefined);
  assertEquals(json.mastering_display, undefined);
  assertEquals(json.content_light, undefined);
});

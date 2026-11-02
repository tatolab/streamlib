# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`V4l2CameraSource`: a camera node whose capture is this wheel's Rust and whose conversion is its GLSL.

Per frame, on today's public surface and nothing else:

1. the native capture thread has dequeued the newest V4L2 buffer and copied
   its rows out, so the driver gets the buffer straight back;
2. `process()` takes those rows and writes them, byte for byte, into a
   host-visible `rgba` pixel buffer — four captured bytes to a texel;
3. the engine copies that buffer into a sampled `rgba8_unorm` texture, because
   a kernel binds textures and never a pixel buffer;
4. this wheel's kernel reads the raw YUYV or NV12 bytes back out of the
   texels and writes RGBA into the node's output pool, which is published as
   a `VideoFrame`.
"""

from __future__ import annotations

import dataclasses
import struct
import typing
from typing import Annotated, Any

import numpy

from tatolab.stream import (
    ColorInfo,
    GpuSurfaceHandle,
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    log,
    node,
)
from tatolab.stream.clock import monotonic_now_ns
from tatolab.stream.video_frame import Matrix, Primaries, Range, Transfer

from . import _native
from ._duration_samples import DurationSamplesInOneReportingWindow

VIDEO_OUTPUT_PORT = "video"

#: Bounds how long one `process()` parks in the native wait, and so how long a
#: lifecycle command waits behind it.
NEWEST_FRAME_WAIT_TIMEOUT_MS = 100

FRAMES_BETWEEN_REPORTS = 600

#: Four captured bytes ride one `rgba8` texel, so a row of `bytes_per_line`
#: bytes is `bytes_per_line / 4` texels wide.
CAPTURED_BYTES_PER_TEXEL = 4

UPLOAD_PIXEL_BUFFER_FORMAT = "rgba"
UPLOAD_TEXTURE_FORMAT = "rgba8_unorm"
CONVERTED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
CONVERTED_FRAME_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

CAPTURED_FRAME_BYTES_BINDING = "captured_frame_bytes"
CONVERTED_FRAME_BINDING = "converted_frame"

WORKGROUP_TILE_SIZE = 8

#: `int is_nv12, int frame_height, int range_is_limited, float kr, float kb`,
#: little-endian, at the offsets the GLSL block below declares.
CAPTURE_LAYOUT_PUSH_CONSTANTS_FORMAT = "<iiiff"
CAPTURE_LAYOUT_PUSH_CONSTANTS_SIZE = struct.calcsize(CAPTURE_LAYOUT_PUSH_CONSTANTS_FORMAT)

YCBCR_TO_RGBA_COMPUTE_GLSL = (
    f"#version 450\n#define WORKGROUP_TILE_SIZE {WORKGROUP_TILE_SIZE}\n"
    """
layout(local_size_x = WORKGROUP_TILE_SIZE, local_size_y = WORKGROUP_TILE_SIZE) in;

layout(set = 0, binding = 0) uniform sampler2D captured_frame_bytes;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D converted_frame;

layout(push_constant) uniform CaptureLayout {
    int is_nv12;
    int frame_height;
    int range_is_limited;
    float kr;
    float kb;
} capture_layout;

float captured_byte(int byte_column, int row) {
    vec4 texel = texelFetch(captured_frame_bytes, ivec2(byte_column >> 2, row), 0);
    int component = byte_column & 3;
    if (component == 0) return texel.r;
    if (component == 1) return texel.g;
    if (component == 2) return texel.b;
    return texel.a;
}

void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(converted_frame);
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    float luma;
    float cb;
    float cr;
    if (capture_layout.is_nv12 != 0) {
        int chroma_row = capture_layout.frame_height + (at.y >> 1);
        int chroma_column = at.x & ~1;
        luma = captured_byte(at.x, at.y);
        cb = captured_byte(chroma_column, chroma_row);
        cr = captured_byte(chroma_column + 1, chroma_row);
    } else {
        int pair_column = (at.x >> 1) << 2;
        luma = captured_byte(pair_column + ((at.x & 1) << 1), at.y);
        cb = captured_byte(pair_column + 1, at.y);
        cr = captured_byte(pair_column + 3, at.y);
    }

    if (capture_layout.range_is_limited != 0) {
        luma = (luma * 255.0 - 16.0) / 219.0;
        cb = (cb * 255.0 - 128.0) / 224.0;
        cr = (cr * 255.0 - 128.0) / 224.0;
    } else {
        cb -= 128.0 / 255.0;
        cr -= 128.0 / 255.0;
    }

    float kr = capture_layout.kr;
    float kb = capture_layout.kb;
    float red = luma + 2.0 * (1.0 - kr) * cr;
    float blue = luma + 2.0 * (1.0 - kb) * cb;
    float green = (luma - kr * red - kb * blue) / (1.0 - kr - kb);
    imageStore(converted_frame, at, vec4(clamp(vec3(red, green, blue), 0.0, 1.0), 1.0));
}
"""
)


def _workgroups_covering(pixels: int) -> int:
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


def _h273_axis_name(name: "str | None", axis_names: Any) -> Any:
    """`name`, refused unless it is one of the `Literal` `axis_names` spells."""
    if name is not None and name not in typing.get_args(axis_names):
        raise ValueError(f"V4l2CameraSource: {name!r} is not an H.273 name ColorInfo knows")
    return name


@dataclasses.dataclass
class V4l2CameraSourceConfig:
    """What a `V4l2CameraSource` is configured with."""

    device_path: Annotated[
        "str | None", "A V4L2 node such as `/dev/video0`; the first capture device when absent."
    ] = None
    max_width: Annotated[int, "The widest frame size negotiated."] = 1920
    max_height: Annotated[int, "The tallest frame size negotiated."] = 1080


@node(
    execution="continuous",
    description="Captures a V4L2 camera in this wheel's Rust and converts it with this wheel's GLSL",
)
class V4l2CameraSource:
    """A V4L2 camera, published as RGBA `VideoFrame`s."""

    def __init__(self, config: V4l2CameraSourceConfig) -> None:
        self._config = config
        self._capture: "_native.V4l2CameraCapture | None" = None
        self._upload_pixel_buffer: "GpuSurfaceHandle | None" = None
        self._upload_texture: "GpuSurfaceHandle | None" = None
        self._converted_frame_ring = NodeOutputTextureRing(
            CONVERTED_FRAME_TEXTURE_FORMAT, CONVERTED_FRAME_TEXTURE_USAGE
        )
        self._frames_published = 0
        self._frames_dropped_at_a_full_output_pool = 0
        self._stage_durations = {
            stage: DurationSamplesInOneReportingWindow()
            for stage in (
                "capture_to_process",
                "cpu_upload",
                "pixel_buffer_to_texture_copy",
                "output_pool_slot",
                "conversion_dispatch",
                "output_write",
                "capture_to_publish",
            )
        }

    @node.output(description="RGBA frames from the camera, as VideoFrame records")
    def video(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        capture = _native.V4l2CameraCapture(
            self._config.device_path, self._config.max_width, self._config.max_height
        )
        self._capture = capture
        if capture.bytes_per_line % CAPTURED_BYTES_PER_TEXEL != 0:
            raise ValueError(
                f"V4l2CameraSource: {capture.device_path} delivers {capture.bytes_per_line}-byte "
                f"rows, which do not pack into {CAPTURED_BYTES_PER_TEXEL}-byte texels"
            )
        upload_texels_per_row = capture.bytes_per_line // CAPTURED_BYTES_PER_TEXEL
        gpu = ctx.gpu_full_access
        self._upload_pixel_buffer = gpu.acquire_pixel_buffer(
            upload_texels_per_row, capture.rows_in_one_frame, UPLOAD_PIXEL_BUFFER_FORMAT
        )
        self._upload_texture = gpu.acquire_texture(
            upload_texels_per_row,
            capture.rows_in_one_frame,
            UPLOAD_TEXTURE_FORMAT,
            ["texture_binding"],
        )
        self._conversion_kernel = gpu.create_compute_kernel(
            source=YCBCR_TO_RGBA_COMPUTE_GLSL,
            push_constant_size=CAPTURE_LAYOUT_PUSH_CONSTANTS_SIZE,
            bindings={
                CAPTURED_FRAME_BYTES_BINDING: "sampled_texture",
                CONVERTED_FRAME_BINDING: "storage_image",
            },
        )
        kr, kb = capture.luma_weights()
        self._capture_layout_push_constants = struct.pack(
            CAPTURE_LAYOUT_PUSH_CONSTANTS_FORMAT,
            1 if capture.fourcc == "NV12" else 0,
            capture.height,
            0 if capture.colour_range == "full" else 1,
            kr,
            kb,
        )
        self._color_info = ColorInfo(
            primaries=_h273_axis_name(capture.colour_primaries, Primaries),
            transfer=_h273_axis_name(capture.colour_transfer, Transfer),
            matrix=_h273_axis_name(capture.colour_matrix, Matrix),
            range=_h273_axis_name(capture.colour_range, Range),
        )
        log.info(
            f"V4l2CameraSource: {capture.device_path} ({capture.card_name}) streams "
            f"{capture.fourcc} {capture.width}x{capture.height} at "
            f"{capture.nominal_frames_per_second} fps, {capture.bytes_per_line} bytes a row, "
            f"colour {self._color_info}"
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        capture = self._capture
        upload_pixel_buffer = self._upload_pixel_buffer
        upload_texture = self._upload_texture
        if capture is None or upload_pixel_buffer is None or upload_texture is None:
            raise RuntimeError("V4l2CameraSource: process() ran before setup() opened the camera")

        captured = capture.wait_for_newest_frame(NEWEST_FRAME_WAIT_TIMEOUT_MS)
        if captured is None:
            return
        process_started_ns = monotonic_now_ns()
        self._stage_durations["capture_to_process"].add(
            process_started_ns - captured.timestamp_ns
        )

        self._write_captured_rows_into(upload_pixel_buffer, captured.frame_bytes, capture)
        uploaded_ns = monotonic_now_ns()
        self._stage_durations["cpu_upload"].add(uploaded_ns - process_started_ns)

        ctx.gpu_limited_access.copy_surface_to_surface(
            upload_pixel_buffer.surface_id, upload_texture
        )
        copied_ns = monotonic_now_ns()
        self._stage_durations["pixel_buffer_to_texture_copy"].add(copied_ns - uploaded_ns)

        try:
            converted_frame_texture = self._converted_frame_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, capture.width, capture.height
            )
        except RuntimeError as full_output_pool:
            self._frames_dropped_at_a_full_output_pool += 1
            if self._frames_dropped_at_a_full_output_pool == 1:
                log.warn(f"V4l2CameraSource: dropping a frame: {full_output_pool}")
            return
        slot_ns = monotonic_now_ns()
        self._stage_durations["output_pool_slot"].add(slot_ns - copied_ns)

        self._conversion_kernel.dispatch(
            bindings={
                CAPTURED_FRAME_BYTES_BINDING: upload_texture,
                CONVERTED_FRAME_BINDING: converted_frame_texture,
            },
            group_count=(
                _workgroups_covering(capture.width),
                _workgroups_covering(capture.height),
                1,
            ),
            push_constants=self._capture_layout_push_constants,
        )
        dispatched_ns = monotonic_now_ns()
        self._stage_durations["conversion_dispatch"].add(dispatched_ns - slot_ns)

        converted_frame = VideoFrame(
            surface_id=converted_frame_texture.surface_id,
            width=capture.width,
            height=capture.height,
            timestamp_ns=captured.timestamp_ns,
            fps=capture.nominal_frames_per_second,
            color_info=self._color_info,
        )
        ctx.outputs.write(
            VIDEO_OUTPUT_PORT, converted_frame.to_bytes(), timestamp_ns=captured.timestamp_ns
        )
        published_ns = monotonic_now_ns()
        self._stage_durations["output_write"].add(published_ns - dispatched_ns)
        self._stage_durations["capture_to_publish"].add(published_ns - captured.timestamp_ns)

        self._frames_published += 1
        if self._frames_published == 1:
            log.info(
                f"V4l2CameraSource: first frame published (timestamp from "
                f"{captured.timestamp_source})"
            )
        if self._frames_published % FRAMES_BETWEEN_REPORTS == 0:
            self._report(captured)

    def _write_captured_rows_into(
        self,
        upload_pixel_buffer: GpuSurfaceHandle,
        frame_bytes: bytes,
        capture: "_native.V4l2CameraCapture",
    ) -> None:
        captured_rows = numpy.frombuffer(frame_bytes, dtype=numpy.uint8).reshape(
            capture.rows_in_one_frame, capture.bytes_per_line
        )
        upload_pixel_buffer.lock(read_only=False)
        try:
            upload_pixels = upload_pixel_buffer.as_numpy()
            upload_rows = upload_pixels.reshape(upload_pixels.shape[0], -1)
            if not numpy.shares_memory(upload_rows, upload_pixels):
                raise RuntimeError(
                    "V4l2CameraSource: the upload buffer's rows are not one view over its "
                    f"pixels (shape {upload_pixels.shape}, strides {upload_pixels.strides})"
                )
            upload_rows[:, : capture.bytes_per_line] = captured_rows
        finally:
            upload_pixel_buffer.unlock()

    def _report(self, captured: "_native.CapturedCameraFrame") -> None:
        stage_summaries = " | ".join(
            f"{stage} {samples.summary_in_milliseconds()}"
            for stage, samples in self._stage_durations.items()
        )
        log.info(
            f"V4l2CameraSource: frames_published={self._frames_published} "
            f"frames_captured={captured.frames_captured} "
            f"frames_replaced_before_they_were_read={captured.frames_replaced_before_they_were_read} "
            f"frames_dropped_at_a_full_output_pool={self._frames_dropped_at_a_full_output_pool} "
            f"(ms) {stage_summaries}"
        )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._capture is not None:
            self._capture.stop()
            self._capture = None
        for handle in (self._upload_texture, self._upload_pixel_buffer):
            if handle is not None:
                handle.close()
        self._upload_texture = None
        self._upload_pixel_buffer = None

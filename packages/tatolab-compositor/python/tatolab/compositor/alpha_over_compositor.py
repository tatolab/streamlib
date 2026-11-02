# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`AlphaOverCompositor`: video layers stacked back to front, in premultiplied alpha, on the GPU.

Each arriving frame is copied by the engine into a texture this node owns, so
a held layer never pins its producer's pool. Each `layer_0` frame then runs one
compute dispatch over the canvas, which reads every layer that has sent a frame
and writes the composite into a texture from the node's output pool.
"""

from __future__ import annotations

import dataclasses
import struct
from collections.abc import Mapping
from typing import Annotated, Any

from tatolab.stream import (
    ComputeKernel,
    GpuContextLimitedAccess,
    GpuSurfaceHandle,
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    msgpack,
    node,
)

ALPHA_OVER_LAYER_COUNT = 8
"""How many `layer_N` inputs an `AlphaOverCompositor` has, `layer_0` the backmost."""

LAYER_INPUT_PORT_NAMES = tuple(f"layer_{index}" for index in range(ALPHA_OVER_LAYER_COUNT))

COMPOSITE_OUTPUT_PORT_NAME = "video"

_COMPOSITE_TEXTURE_FORMAT = "rgba8_unorm"
_COMPOSITE_TEXTURE_USAGE = ["storage_binding", "texture_binding"]
_LAYER_LANDING_TEXTURE_FORMAT = "rgba8_unorm"
_LAYER_LANDING_TEXTURE_USAGE = ["texture_binding"]

_WORKGROUP_TILE_SIZE_IN_PIXELS = 8

_COMPOSITE_OUTPUT_BINDING_NAME = "composite_output"

# One std430 `LayerPlacement`: `ivec2 offset`, `float opacity`, `int present`.
_LAYER_PLACEMENT_STRUCT = struct.Struct("<iifi")

# Eight placements fill Vulkan's guaranteed 128-byte `maxPushConstantsSize` exactly,
# which is what caps the layer count.
_PUSH_CONSTANT_BLOCK_BYTE_SIZE = _LAYER_PLACEMENT_STRUCT.size * ALPHA_OVER_LAYER_COUNT


def _layer_texture_binding_name(layer_index: int) -> str:
    return f"layer_{layer_index}_texture"


def alpha_over_compute_kernel_glsl() -> str:
    """The compositor's compute kernel: every layer over the one below it, onto transparent black."""
    layer_texture_declarations = "".join(
        f"layout(set = 0, binding = {index}) uniform sampler2D "
        f"{_layer_texture_binding_name(index)};\n"
        for index in range(ALPHA_OVER_LAYER_COUNT)
    )
    layer_applications = "".join(
        f"    composite = layer_over(composite, {_layer_texture_binding_name(index)}, "
        f"placements.layers[{index}], at);\n"
        for index in range(ALPHA_OVER_LAYER_COUNT)
    )
    return (
        "#version 450\n"
        f"layout(local_size_x = {_WORKGROUP_TILE_SIZE_IN_PIXELS}, "
        f"local_size_y = {_WORKGROUP_TILE_SIZE_IN_PIXELS}) in;\n"
        f"{layer_texture_declarations}"
        f"layout(set = 0, binding = {ALPHA_OVER_LAYER_COUNT}, rgba8) uniform writeonly image2D "
        f"{_COMPOSITE_OUTPUT_BINDING_NAME};\n"
        "struct LayerPlacement {\n"
        "    ivec2 offset;\n"
        "    float opacity;\n"
        "    int present;\n"
        "};\n"
        "layout(push_constant) uniform AlphaOverLayerPlacements {\n"
        f"    LayerPlacement layers[{ALPHA_OVER_LAYER_COUNT}];\n"
        "} placements;\n"
        "vec4 layer_over(vec4 below, sampler2D layer_texture, LayerPlacement placement, ivec2 at) {\n"
        "    if (placement.present == 0) {\n"
        "        return below;\n"
        "    }\n"
        "    ivec2 within_layer = at - placement.offset;\n"
        "    ivec2 layer_extent = textureSize(layer_texture, 0);\n"
        "    if (any(lessThan(within_layer, ivec2(0)))\n"
        "            || any(greaterThanEqual(within_layer, layer_extent))) {\n"
        "        return below;\n"
        "    }\n"
        "    vec4 above = texelFetch(layer_texture, within_layer, 0) * placement.opacity;\n"
        "    return above + below * (1.0 - above.a);\n"
        "}\n"
        "void main() {\n"
        "    ivec2 at = ivec2(gl_GlobalInvocationID.xy);\n"
        f"    ivec2 canvas_extent = imageSize({_COMPOSITE_OUTPUT_BINDING_NAME});\n"
        "    if (at.x >= canvas_extent.x || at.y >= canvas_extent.y) {\n"
        "        return;\n"
        "    }\n"
        "    vec4 composite = vec4(0.0);\n"
        f"{layer_applications}"
        f"    imageStore({_COMPOSITE_OUTPUT_BINDING_NAME}, at, composite);\n"
        "}\n"
    )


def _is_plain_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_plain_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


@dataclasses.dataclass
class AlphaOverLayerPlacement:
    """Where one layer sits on the canvas and how much of it shows."""

    x: Annotated[int, "Canvas column of the layer's left edge; negative clips it."] = 0
    y: Annotated[int, "Canvas row of the layer's top edge; negative clips it."] = 0
    opacity: Annotated[float, "0.0 hides the layer, 1.0 shows it as it arrives."] = 1.0

    def __post_init__(self) -> None:
        if not _is_plain_int(self.x) or not _is_plain_int(self.y):
            raise ValueError(
                f"AlphaOverLayerPlacement: x and y are whole canvas pixels, got "
                f"x={self.x!r} y={self.y!r}"
            )
        if not _is_plain_number(self.opacity) or not 0.0 <= self.opacity <= 1.0:
            raise ValueError(
                f"AlphaOverLayerPlacement: opacity is a number from 0.0 to 1.0, got "
                f"{self.opacity!r}"
            )
        self.opacity = float(self.opacity)


@dataclasses.dataclass
class AlphaOverCompositorConfig:
    """What an `AlphaOverCompositor` is configured with."""

    width: Annotated[int, "Canvas width in pixels; every composite is this wide."] = 1920
    height: Annotated[int, "Canvas height in pixels; every composite is this tall."] = 1080
    layers: Annotated[
        "list[AlphaOverLayerPlacement]",
        "Entry N places `layer_N`; a layer with no entry sits at 0,0 at full opacity.",
    ] = dataclasses.field(default_factory=list)

    def __post_init__(self) -> None:
        for dimension_name, dimension in (("width", self.width), ("height", self.height)):
            if not _is_plain_int(dimension) or dimension < 1:
                raise ValueError(
                    f"AlphaOverCompositorConfig: {dimension_name} is a canvas size in "
                    f"pixels, at least 1, got {dimension!r}"
                )
        if len(self.layers) > ALPHA_OVER_LAYER_COUNT:
            raise ValueError(
                f"AlphaOverCompositorConfig: {len(self.layers)} layer placements, but "
                f"the compositor has {ALPHA_OVER_LAYER_COUNT} inputs, `layer_0` to "
                f"`layer_{ALPHA_OVER_LAYER_COUNT - 1}`"
            )
        self.layers = [
            AlphaOverLayerPlacement(**placement) if isinstance(placement, Mapping) else placement
            for placement in self.layers
        ]
        for index, placement in enumerate(self.layers):
            if not isinstance(placement, AlphaOverLayerPlacement):
                raise ValueError(
                    f"AlphaOverCompositorConfig: layers[{index}] is a map of x, y and "
                    f"opacity, got {placement!r}"
                )

    def placement_of_layer(self, layer_index: int) -> AlphaOverLayerPlacement:
        """The placement `layer_index` composites at."""
        if layer_index < len(self.layers):
            return self.layers[layer_index]
        return AlphaOverLayerPlacement()


def pack_layer_placements(
    config: AlphaOverCompositorConfig, layer_index_has_a_frame: "list[bool]"
) -> bytes:
    """The kernel's push-constant block: each layer's placement, and whether it has a frame."""
    return b"".join(
        _LAYER_PLACEMENT_STRUCT.pack(
            placement.x, placement.y, placement.opacity, int(has_a_frame)
        )
        for placement, has_a_frame in (
            (config.placement_of_layer(index), layer_index_has_a_frame[index])
            for index in range(ALPHA_OVER_LAYER_COUNT)
        )
    )


def _workgroups_covering(pixels: int) -> int:
    return (pixels + _WORKGROUP_TILE_SIZE_IN_PIXELS - 1) // _WORKGROUP_TILE_SIZE_IN_PIXELS


class _LayerLandingTexture:
    """The texture one layer's newest frame is copied into, which the kernel samples."""

    def __init__(self) -> None:
        self._landing_ring = NodeOutputTextureRing(
            _LAYER_LANDING_TEXTURE_FORMAT, _LAYER_LANDING_TEXTURE_USAGE, depth=1
        )
        self.newest_frame_texture: "GpuSurfaceHandle | None" = None

    def land(self, gpu_limited_access: GpuContextLimitedAccess, frame: VideoFrame, port_name: str) -> None:
        landing_texture = self._landing_ring.next_texture_for_this_frame(
            gpu_limited_access, frame.width, frame.height
        )
        try:
            gpu_limited_access.copy_surface_to_surface(frame.surface_id, landing_texture)
        except RuntimeError as copy_refusal:
            raise ValueError(
                f"AlphaOverCompositor: the frame {frame.surface_id!r} ({frame.width}x"
                f"{frame.height}) on `{port_name}` could not be copied into an "
                f"{_LAYER_LANDING_TEXTURE_FORMAT} layer texture: {copy_refusal}"
            ) from copy_refusal
        self.newest_frame_texture = landing_texture


@node(description="Layers up to eight video inputs back to front, in premultiplied alpha, on the GPU")
class AlphaOverCompositor:
    """`layer_0` to `layer_7` over one another onto transparent black, on `layer_0`'s cadence.

    Each `layer_0` frame yields one composite carrying its timestamp. Every other
    layer contributes its newest frame, held until a newer one arrives; a layer
    that has never sent a frame is left out. Frames are read as premultiplied RGBA.
    """

    def __init__(self, config: AlphaOverCompositorConfig) -> None:
        self._config = config
        self._layer_landing_textures = [
            _LayerLandingTexture() for _ in range(ALPHA_OVER_LAYER_COUNT)
        ]
        self._composite_ring = NodeOutputTextureRing(
            _COMPOSITE_TEXTURE_FORMAT, _COMPOSITE_TEXTURE_USAGE
        )
        self._compute_kernel: "ComputeKernel | None" = None

    @node.input(delivery_profile="newest", description="The backmost layer; each frame yields one composite")
    def layer_0(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest", description="Layered over layer_0; its newest frame is held")
    def layer_1(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest", description="Layered over layer_1; its newest frame is held")
    def layer_2(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest", description="Layered over layer_2; its newest frame is held")
    def layer_3(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest", description="Layered over layer_3; its newest frame is held")
    def layer_4(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest", description="Layered over layer_4; its newest frame is held")
    def layer_5(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest", description="Layered over layer_5; its newest frame is held")
    def layer_6(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest", description="The frontmost layer; its newest frame is held")
    def layer_7(self) -> VideoFrame: ...

    @node.output(description="The composite, premultiplied RGBA at the configured canvas size")
    def video(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        bindings = {
            _layer_texture_binding_name(index): "sampled_texture"
            for index in range(ALPHA_OVER_LAYER_COUNT)
        }
        bindings[_COMPOSITE_OUTPUT_BINDING_NAME] = "storage_image"
        self._compute_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=alpha_over_compute_kernel_glsl(),
            push_constant_size=_PUSH_CONSTANT_BLOCK_BYTE_SIZE,
            bindings=bindings,
        )

    def configure(self, config: AlphaOverCompositorConfig) -> None:
        """Take a new canvas size or new placements while running, from the next composite on."""
        self._config = config

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        gpu_limited_access = ctx.gpu_limited_access
        for layer_index in range(1, ALPHA_OVER_LAYER_COUNT):
            self._land_newest_frame_of_layer(ctx, gpu_limited_access, layer_index)
        backmost_frame = ctx.inputs.read(LAYER_INPUT_PORT_NAMES[0], into=VideoFrame)
        if backmost_frame is None:
            return
        self._layer_landing_textures[0].land(
            gpu_limited_access, backmost_frame, LAYER_INPUT_PORT_NAMES[0]
        )
        composite_record = self._composite(gpu_limited_access, backmost_frame)
        ctx.outputs.write(
            COMPOSITE_OUTPUT_PORT_NAME,
            msgpack.pack(composite_record),
            timestamp_ns=backmost_frame.timestamp_ns,
        )

    def _land_newest_frame_of_layer(
        self,
        ctx: RuntimeContextLimitedAccess,
        gpu_limited_access: GpuContextLimitedAccess,
        layer_index: int,
    ) -> None:
        port_name = LAYER_INPUT_PORT_NAMES[layer_index]
        frame = ctx.inputs.read(port_name, into=VideoFrame)
        if frame is not None:
            self._layer_landing_textures[layer_index].land(gpu_limited_access, frame, port_name)

    def _composite(
        self, gpu_limited_access: GpuContextLimitedAccess, backmost_frame: VideoFrame
    ) -> "dict[str, Any]":
        assert self._compute_kernel is not None, "setup() builds the kernel before any process()"
        newest_frame_textures = [
            landing.newest_frame_texture for landing in self._layer_landing_textures
        ]
        backmost_texture = newest_frame_textures[0]
        assert backmost_texture is not None
        canvas_width, canvas_height = self._config.width, self._config.height
        composite_texture = self._composite_ring.next_texture_for_this_frame(
            gpu_limited_access, canvas_width, canvas_height
        )
        # A layer with no frame yet still needs a texture bound; the kernel never reads it.
        bindings: "dict[str, GpuSurfaceHandle | str]" = {
            _layer_texture_binding_name(index): texture or backmost_texture
            for index, texture in enumerate(newest_frame_textures)
        }
        bindings[_COMPOSITE_OUTPUT_BINDING_NAME] = composite_texture
        self._compute_kernel.dispatch(
            bindings=bindings,
            group_count=(
                _workgroups_covering(canvas_width),
                _workgroups_covering(canvas_height),
                1,
            ),
            push_constants=pack_layer_placements(
                self._config, [texture is not None for texture in newest_frame_textures]
            ),
        )
        composite_record: "dict[str, Any]" = {
            "surface_id": composite_texture.surface_id,
            "width": canvas_width,
            "height": canvas_height,
            "timestamp_ns": backmost_frame.timestamp_ns,
        }
        if backmost_frame.fps is not None:
            composite_record["fps"] = backmost_frame.fps
        if backmost_frame.color_info is not None:
            composite_record["color_info"] = {
                axis: value
                for axis, value in dataclasses.asdict(backmost_frame.color_info).items()
                if value is not None
            }
        return composite_record

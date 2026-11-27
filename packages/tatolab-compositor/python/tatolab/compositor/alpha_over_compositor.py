# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`AlphaOverCompositor`: video layers stacked back to front, in premultiplied alpha, on the GPU.

Its layers are a pin group that grows as it is wired and shrinks as its last layer is
unwired, while the stream plays, each layer a picture with its place on the canvas and its
opacity. Its kernels are a computed value of how many layers it holds, so a layer added or
taken out builds them again off the frame thread, and a place or an opacity set builds
nothing. Each arriving frame is copied by the engine into a texture this node owns, so a held
layer never pins its producer's pool. Each frame of the backmost layer then runs the
composite: one compute dispatch over the canvas for up to eight layers, and for more, each
further dispatch laying up to seven more over the composite so far, so any number of layers
fits Vulkan's guaranteed push-constant block.
"""

from __future__ import annotations

import struct
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from typing import Annotated

from tatolab.stream import (
    ComputeKernel,
    Description,
    GpuContextLimitedAccess,
    GpuSurfaceHandle,
    Input,
    InputPin,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    PinGroup,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    computed,
    node,
)

_COMPOSITE_TEXTURE_FORMAT = "rgba8_unorm"
_COMPOSITE_TEXTURE_USAGE = ["storage_binding", "texture_binding"]
_LAYER_LANDING_TEXTURE_FORMAT = "rgba8_unorm"
_LAYER_LANDING_TEXTURE_USAGE = ["texture_binding"]

_WORKGROUP_TILE_SIZE_IN_PIXELS = 8

_COMPOSITE_OUTPUT_BINDING_NAME = "composite_output"

# One std430 `LayerPlacement`: `int2 offset`, `float opacity`, `int present`.
_LAYER_PLACEMENT_STRUCT = struct.Struct("<iifi")

MOST_TEXTURES_ONE_DISPATCH_LAYERS = 8
"""How many textures one dispatch lays over one another: eight placements fill Vulkan's
guaranteed 128-byte `maxPushConstantsSize` exactly."""

# A dispatch after the first lays the composite so far first, at the canvas's origin.
_COMPOSITE_SO_FAR_PLACEMENT = (0, 0, 1.0)


def _texture_binding_name(texture_index: int) -> str:
    return f"layer_{texture_index}_texture"


def alpha_over_compute_kernel_slang(texture_count: int) -> str:
    """The compute kernel laying `texture_count` textures over one another onto transparent black."""
    if not 1 <= texture_count <= MOST_TEXTURES_ONE_DISPATCH_LAYERS:
        raise ValueError(
            f"AlphaOverCompositor: one dispatch lays 1 to {MOST_TEXTURES_ONE_DISPATCH_LAYERS} "
            f"textures, not {texture_count}"
        )
    texture_declarations = "".join(
        f"[[vk::binding({index}, 0)]] Sampler2D {_texture_binding_name(index)};\n"
        for index in range(texture_count)
    )
    texture_applications = "".join(
        f"    composite = layer_over(composite, {_texture_binding_name(index)}, "
        f"placements.layers[{index}], at);\n"
        for index in range(texture_count)
    )
    return (
        f"{texture_declarations}"
        f"[[vk::binding({texture_count}, 0)]] [format(\"rgba8\")] RWTexture2D<float4> "
        f"{_COMPOSITE_OUTPUT_BINDING_NAME};\n"
        "struct LayerPlacement {\n"
        "    int2 offset;\n"
        "    float opacity;\n"
        "    int present;\n"
        "};\n"
        "struct AlphaOverLayerPlacements {\n"
        f"    LayerPlacement layers[{texture_count}];\n"
        "};\n"
        "[[vk::push_constant]] ConstantBuffer<AlphaOverLayerPlacements> placements;\n"
        "float4 layer_over(float4 below, Sampler2D layer_texture, LayerPlacement placement, int2 at) {\n"
        "    if (placement.present == 0) {\n"
        "        return below;\n"
        "    }\n"
        "    int2 within_layer = at - placement.offset;\n"
        "    uint layer_width;\n"
        "    uint layer_height;\n"
        "    layer_texture.GetDimensions(layer_width, layer_height);\n"
        "    if (any(within_layer < int2(0))\n"
        "            || any(within_layer >= int2(int(layer_width), int(layer_height)))) {\n"
        "        return below;\n"
        "    }\n"
        "    float4 above = layer_texture.Load(int3(within_layer, 0)) * placement.opacity;\n"
        "    return above + below * (1.0 - above.a);\n"
        "}\n"
        f"[numthreads({_WORKGROUP_TILE_SIZE_IN_PIXELS}, {_WORKGROUP_TILE_SIZE_IN_PIXELS}, 1)]\n"
        "void main(uint3 invocation : SV_DispatchThreadID) {\n"
        "    int2 at = int2(invocation.xy);\n"
        "    uint canvas_width;\n"
        "    uint canvas_height;\n"
        f"    {_COMPOSITE_OUTPUT_BINDING_NAME}.GetDimensions(canvas_width, canvas_height);\n"
        "    if (at.x >= int(canvas_width) || at.y >= int(canvas_height)) {\n"
        "        return;\n"
        "    }\n"
        "    float4 composite = float4(0.0);\n"
        f"{texture_applications}"
        f"    {_COMPOSITE_OUTPUT_BINDING_NAME}[at] = composite;\n"
        "}\n"
    )


def layers_of_each_dispatch(layer_count: int) -> "list[range]":
    """The layers each dispatch of a composite of `layer_count` layers lays, back to front: up to
    eight in the first, then up to seven over the composite so far in each one after it."""
    dispatches: "list[range]" = []
    first_layer = 0
    while first_layer < layer_count:
        room = MOST_TEXTURES_ONE_DISPATCH_LAYERS - (1 if dispatches else 0)
        dispatches.append(range(first_layer, min(first_layer + room, layer_count)))
        first_layer = dispatches[-1].stop
    return dispatches


def texture_count_of_a_dispatch(dispatch_index: int, layers: range) -> int:
    """How many textures a dispatch binds: its layers, and the composite so far after the first."""
    return len(layers) + (1 if dispatch_index else 0)


@dataclass(frozen=True)
class AlphaOverLayer:
    """One layer: its picture, where it sits on the canvas and how much of it shows."""

    picture: InputPin[
        Annotated[
            Texture,
            StreamDelivery("newest"),
            Description(
                "The layer's picture, read as premultiplied RGBA; the backmost layer's frames "
                "set the composite's cadence, and every other layer's newest frame is held"
            ),
        ]
    ]
    x: Annotated[int, Description("Canvas column of the layer's left edge; negative clips it.")] = 0
    y: Annotated[int, Description("Canvas row of the layer's top edge; negative clips it.")] = 0
    opacity: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        Description("0.0 hides the layer, 1.0 shows it as it arrives."),
    ] = 1.0


def pack_layer_placements(
    placements: "list[tuple[int, int, float, bool]]",
) -> bytes:
    """One dispatch's push-constant block: each texture's `x`, `y`, `opacity` and whether it has a frame."""
    return b"".join(
        _LAYER_PLACEMENT_STRUCT.pack(x, y, opacity, int(has_a_frame))
        for x, y, opacity, has_a_frame in placements
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

    def land(self, gpu_limited_access: GpuContextLimitedAccess, frame: Texture, layer: int) -> None:
        landing_texture = self._landing_ring.next_texture_for_this_frame(
            gpu_limited_access, frame.width, frame.height
        )
        try:
            gpu_limited_access.copy_surface_to_surface(frame.surface_id, landing_texture)
        except RuntimeError as copy_refusal:
            raise ValueError(
                f"AlphaOverCompositor: the frame {frame.surface_id!r} ({frame.width}x"
                f"{frame.height}) on layer {layer} could not be copied into an "
                f"{_LAYER_LANDING_TEXTURE_FORMAT} layer texture: {copy_refusal}"
            ) from copy_refusal
        self.newest_frame_texture = landing_texture


class AlphaOverKernels:
    """The kernels a composite of `layer_count` layers dispatches, one for each number of
    textures a dispatch binds, closed once a composite of another count replaces them."""

    def __init__(self, gpu_limited_access: GpuContextLimitedAccess, layer_count: int) -> None:
        self.layer_count = layer_count
        """How many layers the composite these kernels dispatch lays."""
        self.dispatches = layers_of_each_dispatch(layer_count)
        """The layers each dispatch lays, back to front."""
        self.kernel_by_texture_count: "dict[int, ComputeKernel]" = {}
        for dispatch_index, layers in enumerate(self.dispatches):
            texture_count = texture_count_of_a_dispatch(dispatch_index, layers)
            if texture_count in self.kernel_by_texture_count:
                continue
            bindings = {
                _texture_binding_name(index): "sampled_texture" for index in range(texture_count)
            }
            bindings[_COMPOSITE_OUTPUT_BINDING_NAME] = "storage_image"
            self.kernel_by_texture_count[texture_count] = gpu_limited_access.create_compute_kernel(
                source=alpha_over_compute_kernel_slang(texture_count),
                push_constant_size=_LAYER_PLACEMENT_STRUCT.size * texture_count,
                bindings=bindings,
            )

    def close(self) -> None:
        for kernel in self.kernel_by_texture_count.values():
            kernel.close()


class AlphaOverCompositing:
    """What a compositor keeps from frame to frame: each layer's newest frame, and the rings its
    composites are written into."""

    def __init__(self) -> None:
        self._layer_landing_textures: "list[_LayerLandingTexture]" = []
        self._composite_ring = NodeOutputTextureRing(
            _COMPOSITE_TEXTURE_FORMAT, _COMPOSITE_TEXTURE_USAGE
        )
        # Two composites so far, one read while the other is written, for a dispatch after the first.
        self._composite_so_far_rings = (
            NodeOutputTextureRing(_COMPOSITE_TEXTURE_FORMAT, _COMPOSITE_TEXTURE_USAGE, depth=1),
            NodeOutputTextureRing(_COMPOSITE_TEXTURE_FORMAT, _COMPOSITE_TEXTURE_USAGE, depth=1),
        )

    def composite_one_frame(
        self,
        gpu_limited_access: GpuContextLimitedAccess,
        kernels: AlphaOverKernels,
        layers: "Sequence[AlphaOverLayer]",
        canvas: "tuple[int, int]",
        write: "Callable[[TextureToWrite, int], None]",
    ) -> None:
        """Land each layer's newest frame and, on a frame of the backmost, lay the layers
        `kernels` were built for over one another at `canvas`, written with `write`.

        A layer past those the kernels were built for waits for the kernels of its count, and a
        layer they were built for that is no longer held is left out.
        """
        del self._layer_landing_textures[len(layers) :]
        while len(self._layer_landing_textures) < len(layers):
            self._layer_landing_textures.append(_LayerLandingTexture())
        if not layers:
            return
        for layer_index in range(1, len(layers)):
            frame = layers[layer_index].picture.read()
            if frame is not None:
                self._layer_landing_textures[layer_index].land(
                    gpu_limited_access, frame, layer_index
                )
        backmost_frame = layers[0].picture.read()
        if backmost_frame is None:
            return
        self._layer_landing_textures[0].land(gpu_limited_access, backmost_frame, 0)
        if not kernels.dispatches:
            # Kernels built for no layer: the first layer wired waits for the kernels of its count.
            return
        composite = self._composite(gpu_limited_access, kernels, layers, canvas)
        write(
            TextureToWrite.of_texture(composite, backmost_frame.colour),
            backmost_frame.timestamp_ns,
        )

    def _composite(
        self,
        gpu_limited_access: GpuContextLimitedAccess,
        kernels: AlphaOverKernels,
        layers: "Sequence[AlphaOverLayer]",
        canvas: "tuple[int, int]",
    ) -> GpuSurfaceHandle:
        newest_frame_textures = [
            landing.newest_frame_texture for landing in self._layer_landing_textures
        ]
        backmost_texture = newest_frame_textures[0]
        assert backmost_texture is not None
        canvas_width, canvas_height = canvas
        composite_so_far: "GpuSurfaceHandle | None" = None
        for dispatch_index, dispatch_layers in enumerate(kernels.dispatches):
            is_the_last = dispatch_index == len(kernels.dispatches) - 1
            ring = (
                self._composite_ring
                if is_the_last
                else self._composite_so_far_rings[dispatch_index % 2]
            )
            written = ring.next_texture_for_this_frame(
                gpu_limited_access, canvas_width, canvas_height
            )
            # A layer with no frame yet still needs a texture bound; the kernel never reads it.
            textures: "list[GpuSurfaceHandle]" = []
            placements: "list[tuple[int, int, float, bool]]" = []
            if composite_so_far is not None:
                textures.append(composite_so_far)
                placements.append((*_COMPOSITE_SO_FAR_PLACEMENT, True))
            for layer_index in dispatch_layers:
                if layer_index < len(layers):
                    layer = layers[layer_index]
                    texture = newest_frame_textures[layer_index]
                    placements.append((layer.x, layer.y, layer.opacity, texture is not None))
                else:
                    texture = None
                    placements.append((0, 0, 0.0, False))
                textures.append(texture or backmost_texture)
            bindings: "dict[str, GpuSurfaceHandle | str]" = {
                _texture_binding_name(index): texture for index, texture in enumerate(textures)
            }
            bindings[_COMPOSITE_OUTPUT_BINDING_NAME] = written
            kernels.kernel_by_texture_count[len(textures)].dispatch(
                bindings=bindings,
                group_count=(
                    _workgroups_covering(canvas_width),
                    _workgroups_covering(canvas_height),
                    1,
                ),
                push_constants=pack_layer_placements(placements),
            )
            composite_so_far = written
        assert composite_so_far is not None
        return composite_so_far


@node.meta(
    description="Layers any number of video inputs back to front, in premultiplied alpha, on the GPU",
)
@node(execution="reactive")
def AlphaOverCompositor(
    *,
    ctx: RuntimeContextFullAccess,
    width: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("Canvas width in pixels; every composite is this wide."),
        ]
    ] = Input(1920),
    height: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("Canvas height in pixels; every composite is this tall."),
        ]
    ] = Input(1080),
    layers: PinGroup[AlphaOverLayer],
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(_COMPOSITE_TEXTURE_FORMAT),
            Description("The composite, premultiplied RGBA at the canvas size"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Its layers over one another onto transparent black, on the backmost layer's cadence.

    Each frame of layer 0 yields one composite carrying its timestamp. Every other layer
    contributes its newest frame, held until a newer one arrives; a layer that has never sent a
    frame is left out. Frames are read as premultiplied RGBA. The canvas size and each layer's
    place and opacity are read at each composite, so a change shows from the next one on. A
    layer wired or unwired while the stream plays builds the kernels for the new count off the
    frame thread, and composites keep the layers the last kernels were built for until then.
    """
    gpu = ctx.gpu_limited_access
    layer_count = computed(lambda: len(layers.get()))
    # Built from the count alone, so a layer's place or opacity set builds no kernel.
    kernels = computed(lambda: AlphaOverKernels(gpu, layer_count.get()))
    compositing = AlphaOverCompositing()

    def composite_one_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        compositing.composite_one_frame(
            frame_ctx.gpu_limited_access,
            kernels.get(),
            layers.get(),
            (width.get(), height.get()),
            lambda frame, timestamp_ns: video.write(frame, timestamp_ns=timestamp_ns),
        )

    return composite_one_frame

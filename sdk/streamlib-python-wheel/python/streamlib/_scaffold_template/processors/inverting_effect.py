"""The effect the app wires between its source and its window.

Importable as `processors.inverting_effect:InvertingEffect`, which is the
name the engine spawns this processor's child interpreter with.
"""

from streamlib import (
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,  # noqa: A004 — streamlib's port decorator
    output,
    processor,
)


@processor
class InvertingEffect:
    """Reads each frame, inverts its colors in place, and passes it on."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)
        # The frame arrives as a surface id, not pixels: resolve it and open
        # CPU access to the engine's own memory.
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            pixels = surface.as_numpy()
            # One bulk read out, edit on the host, one bulk write back. On
            # Linux the mapping is write-combined: CPU reads of it run around
            # 175 MB/s, so editing in place through a strided view re-reads
            # that memory per channel and costs ~225ms a frame against ~30ms
            # this way. On a Mac the mapping is cached and both ways are fast.
            edited = pixels.copy()
            # Color channels only — inverting alpha would erase the picture.
            edited[:, :, :3] = 255 - edited[:, :, :3]
            pixels[...] = edited
            surface.unlock()
        ctx.outputs.write("video_to_downstream", bag)

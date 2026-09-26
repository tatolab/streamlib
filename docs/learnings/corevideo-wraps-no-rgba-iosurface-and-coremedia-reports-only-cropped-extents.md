# CoreVideo wraps no `'RGBA'` IOSurface, and CoreMedia reports only cropped extents

## Symptom

- `CVPixelBufferCreateWithIOSurface` returns `-6661`
  (`kCVReturnInvalidArgument`) for an IOSurface whose pixel format is
  `'RGBA'` (`kCVPixelFormatType_32RGBA`), at every size tried (320×180,
  256×256, 1920×1080), with or without `kIOSurfaceBytesPerRow` set. The same
  surface tagged `'BGRA'` wraps.
- A decode cap meant to bound a stream's *coded* extent (1920×1088 for a
  1080-line H.264 or H.265 stream) never fires on macOS: every extent
  CoreMedia or VideoToolbox reports is 1920×1080.

## Constraint

Measured on macOS 15, Apple Silicon (M1 Max):

- **`'RGBA'` never becomes a `CVPixelBuffer`.** A frame in that layout —
  which is what the engine's pooled `Rgba32` pixel buffers are — cannot be
  handed to VideoToolbox as it is. Changing the tag to `'BGRA'` would swap the
  channels, not fix it.
- **Cropped everywhere.** For a format description built from the stream's
  own parameter sets, `CMVideoFormatDescriptionGetDimensions`,
  `CMVideoFormatDescriptionGetPresentationDimensions` (clean aperture on or
  off), and a decoded `CVPixelBuffer`'s width and height all answer the
  conformance-window extent, and no `kCMFormatDescriptionExtension_CleanAperture`
  is attached. The coded extent is written only in the SPS.
- **Presentation stamps.** With `kVTCompressionPropertyKey_AllowFrameReordering`
  off, a compression session accepts presentation stamps that repeat or step
  backwards and still emits one access unit per frame.

## Fix pattern

- Encode: convert the frame on the GPU into an NV12 surface from the
  compression session's own pixel-buffer pool (`'420f'` or `'420v'` by the
  range the parameter sets signal) and encode that — never wrap the source.
- Decode: read the picture extent from `CVImageBufferGetCleanRect`, which is
  the conformance window, and hold anything meant for the coded extent
  against the picture unless an SPS parser is at hand.

## Orientation

- `runtime/streamlib-engine/src/apple/videotoolbox_video_codec_backend/`
- `runtime/streamlib-engine/src/vulkan/rhi/shaders/color_convert_rgba_image_to_nv12_buffer.comp`

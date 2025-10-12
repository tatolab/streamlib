# StreamLib Optimization Notes

## Metal Shader Implementation Learnings

### What We Learned

1. **Native format is fastest**
   - YUV 4:2:0 is the camera's native output
   - Using BGRA requires hardware conversion (adds latency)
   - ✅ **Implemented**: Switched to `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`

2. **GPU-side conversion beats CPU**
   - Metal shader YUV→RGB conversion: ~0.5ms
   - CPU YUV→RGB conversion: ~3-5ms
   - ✅ **Implemented**: Full Metal shader pipeline in `camera_gpu.py`

3. **Zero-copy textures are critical**
   - `CVMetalTextureCache` creates textures without memory copies
   - Directly wraps IOSurface-backed CVPixelBuffer
   - ✅ **Implemented**: Both Y and CbCr planes use zero-copy textures

4. **Never read CVPixelBuffer on CPU with PyObjC**
   - `CVPixelBufferGetBaseAddress()` returns `objc.varlist`
   - PyObjC doesn't support buffer protocols on `objc.varlist`
   - ✅ **Fixed**: Metal shader pipeline avoids CPU memory access entirely

## Current Optimization Opportunities

### 1. Camera Handler (camera_gpu.py:642-663)

**Current bottleneck**: Metal texture → CPU → MPS tensor

```python
# GPU → CPU transfer (unavoidable with PyTorch currently)
output_texture.getBytes_bytesPerRow_fromRegion_mipmapLevel_(data, bytes_per_row, region, 0)

# CPU → GPU transfer
tensor = torch.from_numpy(frame_rgb).to(self.mps_device)
```

**Ideal pipeline** (not currently possible):
```
CVPixelBuffer(YUV) → Metal Y/CbCr textures → Metal shader → RGB texture → MPS tensor
                      ↑____________zero-copy____________↑           ↑_____BLOCKED____↑
```

**Why it's blocked**: PyTorch MPS doesn't support direct `MTLTexture → MPS tensor` creation.

**Potential solutions**:
- Wait for PyTorch to add `torch.from_metal_texture()` or similar
- Use PyTorch C++ extension to create MPS tensor from Metal texture
- Use Metal Performance Shaders (MPS) directly instead of PyTorch

**Current status**: Documented limitation, no action needed until PyTorch adds support.

### 2. Display Handler (display_gpu.py:391-432)

**Current flow**: MPS tensor → CPU → OpenGL PBO → texture

```python
# GPU → CPU transfer
frame_np = frame_gpu.cpu().numpy()

# CPU → GPU transfer
current_pbo.write(frame_np.tobytes())
self.texture.write(current_pbo)
```

**Ideal flow**: MPS tensor → OpenGL texture (direct)

**Why it's blocked**: OpenGL-Metal interop on macOS requires:
- `CVOpenGLTextureCacheCreate` (deprecated)
- Or complex Metal→OpenGL fence synchronization

**Potential solutions**:
- Use Metal layer for display instead of OpenGL
- Implement MTKView-based display (pure Metal)
- Use GPU-side texture sharing (complex)

**Current status**: Works well enough (~7ms total), optimization deferred.

### 3. Other Handlers

All other handlers are already well-optimized:

- ✅ **Blur GPU**: Uses MPS Gaussian blur (pure GPU)
- ✅ **Lower Thirds GPU**: Compositor uses MPS alpha blending (pure GPU)
- ✅ **Text Overlay**: Pre-rendered to textures, GPU compositing

## Performance Summary

### Before Metal Shader (BGRA format)
```
Camera → BGRA CVPixelBuffer (hardware YUV→BGRA conversion)
      → CPU read (objc.varlist failure, corrupted)
      → Unusable
```

### After Metal Shader (YUV format)
```
Camera → YUV CVPixelBuffer (native, no conversion)
      → Metal Y/CbCr textures (zero-copy)
      → Metal shader YUV→RGB (~0.5ms, GPU)
      → RGB Metal texture
      → CPU read (~2ms, getBytes)
      → MPS tensor (~2ms, upload)
      → Total: ~4.5ms per frame
```

**Improvement**: Working pipeline vs. broken pipeline + GPU-side conversion

**Remaining overhead**: 4.5ms GPU→CPU→GPU bounce (blocked by PyTorch limitations)

## Recommendations

1. **✅ Done**: Switched to native YUV format
2. **✅ Done**: Implemented Metal shader pipeline
3. **✅ Done**: Added FPS overlay to GPU display
4. **⏳ Future**: Monitor PyTorch for Metal texture interop support
5. **⏳ Future**: Consider Metal-based display (MTKView) to eliminate OpenGL bounce

## Philosophy Alignment

This implementation follows streamlib's core principle:

> **Stay on GPU as long as possible, automatically choose the optimal path**

We've achieved:
- Zero-copy camera → Metal textures
- GPU-side YUV→RGB conversion
- Only 2 unavoidable GPU↔CPU bounces (PyTorch limitations)
- Automatic backend selection (MPS on Apple Silicon)

The remaining bounces are documented limitations, not design flaws.

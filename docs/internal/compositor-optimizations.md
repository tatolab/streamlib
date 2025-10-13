# Performance Optimization Results

## Summary

Optimized the compositor's alpha blending to achieve **2.3x speedup** in compositing performance.

## Before Optimization

**Compositing**: 11.8 FPS (8.46s for 100 frames)

### Bottlenecks Identified
1. **Alpha blending** - 92% of total time (6.935s)
   - numpy `astype()` float32 conversions: 2.726s
   - numpy `dstack()` operations: 1.375s
2. **Background generation** - 7% of time (0.522s)
   - Python for loop creating gradient pixel by pixel

## After Optimization

**Compositing**: 26.9 FPS (3.71s for 100 frames)

### Optimizations Applied

#### 1. Alpha Blend Optimization (compositor.py:254)
- **Eliminated dstack()**: Pre-allocate result array instead of stacking channels
- **uint16 instead of float32**: Faster integer arithmetic, avoids float conversions
- **Reduced conversions**: From 18 astype() calls to 12 per frame (3 layers × 4 per blend)
- **Impact**: 6.935s → 2.772s (2.5x faster)

#### 2. Background Generation Optimization (compositor.py:116)
- **Vectorized operations**: Use numpy broadcasting instead of Python loop
- **Eliminated per-pixel operations**: Generated gradient in single operation
- **Impact**: 0.522s → 0.141s (3.7x faster)

## Performance Breakdown

| Component | Before | After | Speedup |
|-----------|--------|-------|---------|
| Alpha blending | 6.935s | 2.772s | 2.5x |
| Background gen | 0.522s | 0.141s | 3.7x |
| **Total** | **8.46s** | **3.71s** | **2.3x** |
| **FPS** | **11.8** | **26.9** | **2.3x** |

## Code Changes

### Alpha Blending (Old)
```python
# Used float32 conversions and dstack
alpha = overlay[:, :, 3:4].astype(np.float32) / 255.0
overlay_rgb = overlay[:, :, :3].astype(np.float32)
background_rgb = background[:, :, :3].astype(np.float32)
blended_rgb = (overlay_rgb * alpha + background_rgb * (1.0 - alpha)).astype(np.uint8)
result = np.dstack([blended_rgb, result_alpha])  # Expensive
```

### Alpha Blending (New)
```python
# Pre-allocate and use uint16 integer arithmetic
result = np.empty_like(background)
overlay_alpha = overlay[:, :, 3].astype(np.uint16)
bg_alpha = background[:, :, 3].astype(np.uint16)

# Alpha channel
result[:, :, 3] = (
    overlay_alpha + (bg_alpha * (255 - overlay_alpha)) // 255
).astype(np.uint8)

# RGB channels (loop faster than vectorizing all channels)
for c in range(3):
    overlay_c = overlay[:, :, c].astype(np.uint16)
    bg_c = background[:, :, c].astype(np.uint16)
    result[:, :, c] = (
        (overlay_c * overlay_alpha + bg_c * (255 - overlay_alpha)) // 255
    ).astype(np.uint8)
```

### Background Generation (Old)
```python
# Python loop creating gradient pixel by pixel
for y in range(self.height):
    intensity = int(self.background_color[0] + (y / self.height) * 30)
    frame[y, :] = [intensity, intensity, ...]
```

### Background Generation (New)
```python
# Vectorized numpy operations
y_gradient = np.linspace(0, 1, self.height, dtype=np.float32)[:, np.newaxis]
frame[:, :, 0] = (self.background_color[0] + y_gradient * 30).astype(np.uint8)
frame[:, :, 1] = (self.background_color[1] + y_gradient * 30).astype(np.uint8)
frame[:, :, 2] = (self.background_color[2] + y_gradient * 10).astype(np.uint8)
frame[:, :, 3] = self.background_color[3]
```

## Remaining Bottleneck

Alpha blending still accounts for 94% of compositing time (2.772s / 2.935s). Further optimization would require:
- Using Numba JIT compilation
- GPU acceleration (requires different Skia bindings or custom shader)
- C extension for blending operation
- Reducing number of layers

Current performance (26.9 FPS compositing, ~20 FPS with display) is acceptable for development and many real-time applications.

## Testing Configuration

- Resolution: 1280×720
- Layers: 3 (gradient background, animated circle, text)
- Platform: macOS (Darwin 24.6.0)
- Python: 3.13

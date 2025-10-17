# GPU Audio Processing

Audio data stays on GPU as `wgpu.GPUBuffer` for realtime performance.

## What Needs Implementation

### AudioBuffer Data Type
- Change `AudioBuffer.data` from `np.ndarray` to `wgpu.GPUBuffer`
- Store PCM samples as f32 in GPU memory
- Include sample_rate, channels, sample_count metadata

### GPU Buffer Operations
- `gpu.create_buffer(size)` - Allocate GPU buffer for audio
- `gpu.upload_to_buffer(buffer, data)` - CPU → GPU (initial upload only)
- `gpu.download_from_buffer(buffer)` - GPU → CPU (debug/export only)

### Audio Compute Shaders (WGSL)
- Process audio samples in compute shaders
- Each workgroup processes chunk of samples
- Same zero-copy principle as video textures

## Example: Reverb Effect

```python
from streamlib import audio_effect, AudioBuffer
from streamlib.gpu import GPUContext

REVERB_SHADER = """
@group(0) @binding(0) var<storage, read> input: array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<f32>;
@group(0) @binding(2) var<uniform> params: ReverbParams;

struct ReverbParams {
    decay: f32,
    sample_count: u32,
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.sample_count) { return; }

    // Simple feedback delay reverb
    var sample = input[idx];
    if (idx > 4800) {  // 100ms delay at 48kHz
        sample += input[idx - 4800] * params.decay;
    }
    output[idx] = sample;
}
"""

@audio_effect
def reverb(audio: AudioBuffer, gpu: GPUContext, decay: float = 0.5) -> AudioBuffer:
    # audio.data is wgpu.GPUBuffer (GPU memory)
    if not hasattr(reverb, 'pipeline'):
        reverb.pipeline = gpu.create_compute_pipeline(REVERB_SHADER)

    output = gpu.create_buffer(audio.sample_count * 4)  # f32 = 4 bytes
    gpu.run_compute(
        reverb.pipeline,
        input=audio.data,
        output=output,
        params={'decay': decay, 'sample_count': audio.sample_count}
    )

    return AudioBuffer(
        data=output,
        timestamp=audio.timestamp,
        sample_rate=audio.sample_rate,
        channels=audio.channels
    )
```

## Camera Audio Capture

Platform-specific capture outputs GPU buffer:

```python
class CameraHandler(StreamHandler):
    async def process(self, tick):
        # Capture video and audio to GPU
        video_texture = self.capture.get_video_texture()  # wgpu.GPUTexture
        audio_buffer = self.capture.get_audio_buffer()    # wgpu.GPUBuffer

        self.outputs['video'].write(VideoFrame(data=video_texture, timestamp=tick.timestamp))
        self.outputs['audio'].write(AudioBuffer(data=audio_buffer, timestamp=tick.timestamp))
```

## Implementation Priority

1. Add `gpu.create_buffer()` to GPUContext
2. Update AudioBuffer to use wgpu.GPUBuffer
3. Implement basic audio shaders (gain, mix)
4. Test with audio test tone generation
5. Add platform audio capture

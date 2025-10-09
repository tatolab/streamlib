#!/usr/bin/env python3
"""
GPU pipeline demo: Automatic transfer handler insertion

Demonstrates:
- BlurFilterGPU running ONLY on GPU with torch
- Runtime auto-inserts CPU→GPU and GPU→CPU transfer handlers
- Capability negotiation (CPU-only TestPattern + Display, GPU-only blur)

Pipeline topology:
    TestPattern (CPU) → [CPUtoGPU] → BlurFilterGPU (GPU) → [GPUtoCP] → Display (CPU)
                         ^^^^^^^^^^                         ^^^^^^^^^^
                         Auto-inserted by runtime

Requirements:
- PyTorch with CUDA support
- CUDA-capable GPU
"""

import asyncio
from streamlib import (
    StreamRuntime,
    Stream,
    TestPatternHandler,
    DisplayHandler,
)

# Try to import GPU blur
try:
    from streamlib import BlurFilterGPU
    HAS_GPU_BLUR = True
except (ImportError, AttributeError):
    HAS_GPU_BLUR = False


async def main():
    """Run GPU blur pipeline with automatic transfer handlers."""

    # Check if GPU blur is available
    if not HAS_GPU_BLUR:
        print("❌ BlurFilterGPU not available.")
        print("\nThis demo requires:")
        print("  1. PyTorch with CUDA support")
        print("  2. A CUDA-capable GPU")
        print("\nInstall with: pip install torch torchvision")
        return

    # Check CUDA availability
    import torch
    if not torch.cuda.is_available():
        print("❌ CUDA not available on this system.")
        print("\nThis demo requires a CUDA-capable GPU.")
        print(f"PyTorch version: {torch.__version__}")
        print(f"CUDA available: {torch.cuda.is_available()}")
        return

    print(f"✅ CUDA available: {torch.cuda.get_device_name(0)}")
    print(f"   PyTorch version: {torch.__version__}\n")

    # Create handlers
    pattern = TestPatternHandler(
        width=640,
        height=480,
        pattern='gradient'  # Gradient pattern shows blur better than SMPTE
    )
    # Pattern outputs CPU: capabilities=['cpu']

    # GPU-only blur - forces transfer handler insertion
    blur = BlurFilterGPU(
        kernel_size=21,  # Heavy blur
        sigma=5.0,
        device='cuda:0'
    )
    # Blur has GPU-only capability: capabilities=['gpu']
    # Runtime MUST insert CPUtoGPU and GPUtoCPU transfers

    display = DisplayHandler(
        window_name="GPU Pipeline Demo"
    )
    # Display expects CPU: capabilities=['cpu']

    # Create runtime
    runtime = StreamRuntime(fps=30)

    # Add streams
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(blur, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='asyncio'))

    # Connect pipeline - runtime will auto-insert transfers if needed
    print("Connecting TestPattern → BlurFilter...")
    runtime.connect(pattern.outputs['video'], blur.inputs['video'])

    print("Connecting BlurFilter → Display...")
    runtime.connect(blur.outputs['video'], display.inputs['video'])

    # Start runtime
    runtime.start()

    # Run for 10 seconds
    print("\nRunning GPU pipeline for 10 seconds...")
    print("Watch for auto-inserted ⚠️ transfer handlers in the output above.")
    print("Press Ctrl+C to stop early\n")

    try:
        await asyncio.sleep(10)
    except KeyboardInterrupt:
        print("\nStopping...")

    # Stop runtime
    await runtime.stop()

    print("\nDemo complete!")
    print("\n" + "="*60)
    print("What happened:")
    print("="*60)
    print("1. TestPattern generated frames on CPU (numpy arrays)")
    print("2. Runtime detected capability mismatch:")
    print("   - TestPattern outputs: ['cpu']")
    print("   - BlurFilterGPU inputs: ['gpu']")
    print("3. ⚠️  Runtime auto-inserted CPUtoGPU transfer handler")
    print("4. Blur processed frames on GPU with torch conv2d")
    print("5. Runtime detected another mismatch:")
    print("   - BlurFilterGPU outputs: ['gpu']")
    print("   - Display inputs: ['cpu']")
    print("6. ⚠️  Runtime auto-inserted GPUtoCPU transfer handler")
    print("7. Display received CPU frames (OpenCV expects numpy)")
    print("\nThis demonstrates the core innovation of streamlib:")
    print("  • Handlers declare capabilities (['cpu'], ['gpu'], or both)")
    print("  • Runtime negotiates connections automatically")
    print("  • Transfer handlers inserted transparently when needed")
    print("  • Zero manual memory management required!")
    print("="*60)


if __name__ == '__main__':
    asyncio.run(main())

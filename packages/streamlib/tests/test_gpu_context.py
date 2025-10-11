#!/usr/bin/env python3
"""
Test runtime GPU context initialization.
"""

import asyncio
import sys
sys.path.insert(0, 'packages/streamlib/src')

from streamlib import StreamRuntime


async def main():
    print("Testing Runtime GPU Context")
    print("="*60)

    # Create runtime with GPU support
    runtime = StreamRuntime(fps=60, enable_gpu=True)

    if runtime.gpu_context:
        print(f"✅ GPU context initialized!")
        print(f"   Backend: {runtime.gpu_context['backend']}")
        print(f"   Device: {runtime.gpu_context['device']}")
        print(f"   Memory Pool: {runtime.gpu_context['memory_pool']}")
        print(f"   Transfer Optimizer: {runtime.gpu_context['transfer_optimizer']}")

        # Test memory pool allocation
        mem_pool = runtime.gpu_context['memory_pool']
        tensor = mem_pool.allocate((480, 640, 3), 'uint8')
        print(f"\n✅ Allocated tensor: shape={tensor.shape}")

        mem_pool.release(tensor)
        print(f"✅ Released tensor back to pool")

        # Test reuse
        tensor2 = mem_pool.allocate((480, 640, 3), 'uint8')
        print(f"✅ Reused tensor from pool: shape={tensor2.shape}")

    else:
        print("❌ GPU context not initialized")
        print("   (This is OK if no GPU is available)")

    await runtime.stop()
    print("\n✅ Test complete!")


if __name__ == '__main__':
    asyncio.run(main())

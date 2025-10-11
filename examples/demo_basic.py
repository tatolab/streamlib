#!/usr/bin/env python3
"""
Basic pipeline demo: TestPattern → Display

Demonstrates:
- TestPatternHandler generating SMPTE bars
- DisplayHandler showing video in OpenCV window
- StreamRuntime managing lifecycle
- Capability-based port connection
"""

import asyncio
from streamlib import (
    StreamRuntime,
    Stream,
    TestPatternHandler,
    DisplayHandler,
)


async def main():
    """Run basic test pattern → display pipeline."""
    
    # Create handlers
    pattern = TestPatternHandler(
        width=640,
        height=480,
        pattern='smpte_bars'
    )
    
    display = DisplayHandler(
        window_name="Basic Pipeline Demo"
    )
    
    # Create runtime
    runtime = StreamRuntime(fps=30)
    
    # Add streams with appropriate dispatchers
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))  # Lightweight pattern generation
    runtime.add_stream(Stream(display, dispatcher='threadpool'))  # Blocking OpenCV calls
    
    # Connect: TestPattern → Display
    runtime.connect(pattern.outputs['video'], display.inputs['video'])
    
    # Start runtime (not async)
    runtime.start()

    # Run for 5 seconds
    print("\nRunning pipeline for 5 seconds...")
    print("Press Ctrl+C to stop early\n")
    
    try:
        await asyncio.sleep(5)
    except KeyboardInterrupt:
        print("\nStopping...")
    
    # Stop runtime
    await runtime.stop()
    
    print("\nDemo complete!")


if __name__ == '__main__':
    asyncio.run(main())

"""
Tests for Phase 3 Actor Model core infrastructure.

Tests:
- Ring buffers (CPU)
- Clocks (Software)
- Dispatchers (Asyncio)
- Actor base class
- StreamInput/StreamOutput connections
- Basic actors (TestPatternActor, DisplayActor stub)
"""

import pytest
import asyncio
import numpy as np
from streamlib import (
    RingBuffer,
    SoftwareClock,
    AsyncioDispatcher,
    Actor,
    StreamInput,
    StreamOutput,
    VideoFrame,
    TestPatternActor,
)


class TestRingBuffer:
    """Test ring buffer functionality."""

    def test_create_ring_buffer(self):
        """Test creating a ring buffer."""
        buffer = RingBuffer(slots=3)
        assert buffer.slots == 3
        assert buffer.is_empty()

    def test_write_read(self):
        """Test basic write/read."""
        buffer = RingBuffer(slots=3)

        # Write data
        buffer.write('test1')
        assert not buffer.is_empty()

        # Read data
        data = buffer.read_latest()
        assert data == 'test1'

    def test_overwrite_oldest(self):
        """Test overwriting oldest slot."""
        buffer = RingBuffer(slots=3)

        # Fill buffer
        buffer.write('data1')
        buffer.write('data2')
        buffer.write('data3')

        # Overwrite (should replace data1)
        buffer.write('data4')

        # Read latest (should be data4)
        assert buffer.read_latest() == 'data4'

    def test_latest_read_semantics(self):
        """Test that we always read the latest."""
        buffer = RingBuffer(slots=3)

        buffer.write('old')
        buffer.write('new')

        # Read twice - should get 'new' both times
        assert buffer.read_latest() == 'new'
        assert buffer.read_latest() == 'new'


class TestSoftwareClock:
    """Test software clock."""

    @pytest.mark.asyncio
    async def test_create_clock(self):
        """Test creating a software clock."""
        clock = SoftwareClock(fps=60.0)
        assert clock.get_fps() == 60.0
        assert clock.get_clock_id() == 'software'

    @pytest.mark.asyncio
    async def test_generate_ticks(self):
        """Test tick generation."""
        clock = SoftwareClock(fps=100.0)  # Fast for testing

        # Get a few ticks
        ticks = []
        for _ in range(3):
            tick = await clock.next_tick()
            ticks.append(tick)

        # Check frame numbers increment
        assert ticks[0].frame_number == 0
        assert ticks[1].frame_number == 1
        assert ticks[2].frame_number == 2

        # Check timestamps increase
        assert ticks[1].timestamp > ticks[0].timestamp
        assert ticks[2].timestamp > ticks[1].timestamp


class TestAsyncioDispatcher:
    """Test asyncio dispatcher."""

    @pytest.mark.asyncio
    async def test_create_dispatcher(self):
        """Test creating a dispatcher."""
        dispatcher = AsyncioDispatcher()
        assert dispatcher is not None

    @pytest.mark.asyncio
    async def test_dispatch_coroutine(self):
        """Test dispatching a coroutine."""
        dispatcher = AsyncioDispatcher()

        # Track execution
        executed = []

        async def test_coro():
            await asyncio.sleep(0.001)
            executed.append(True)

        # Dispatch
        await dispatcher.dispatch(test_coro())

        # Wait a bit
        await asyncio.sleep(0.01)

        # Check executed
        assert len(executed) == 1


class TestStreamConnection:
    """Test StreamInput/StreamOutput connections."""

    def test_create_ports(self):
        """Test creating input/output ports."""
        input_port = StreamInput('test_input')
        output_port = StreamOutput('test_output')

        assert input_port.name == 'test_input'
        assert output_port.name == 'test_output'
        assert not input_port.is_connected()

    def test_connect_ports(self):
        """Test connecting ports."""
        input_port = StreamInput('input')
        output_port = StreamOutput('output')

        # Connect using >> operator
        output_port >> input_port

        assert input_port.is_connected()

    def test_data_flow(self):
        """Test data flows through connection."""
        input_port = StreamInput('input')
        output_port = StreamOutput('output')

        # Connect
        output_port >> input_port

        # Write data
        output_port.write('test_data')

        # Read data
        data = input_port.read_latest()
        assert data == 'test_data'


class TestActor:
    """Test Actor base class."""

    @pytest.mark.asyncio
    async def test_simple_actor(self):
        """Test a simple actor implementation."""

        # Create simple actor that counts ticks
        class CounterActor(Actor):
            def __init__(self):
                super().__init__('counter', clock=SoftwareClock(fps=100))
                self.count = 0
                self.outputs['count'] = StreamOutput('count')
                self.start()

            async def process(self, tick):
                self.count += 1
                self.outputs['count'].write(self.count)

        # Create actor
        actor = CounterActor()

        # Wait for a few ticks
        await asyncio.sleep(0.05)

        # Check some ticks processed
        assert actor.count > 0
        assert actor.is_running()

        # Stop actor
        await actor.stop()
        assert not actor.is_running()


class TestTestPatternActor:
    """Test TestPatternActor."""

    @pytest.mark.asyncio
    async def test_create_test_pattern(self):
        """Test creating a test pattern actor."""
        actor = TestPatternActor(
            actor_id='test',
            width=640,
            height=480,
            pattern='smpte_bars',
            fps=30.0
        )

        assert actor.actor_id == 'test'
        assert actor.width == 640
        assert actor.height == 480
        assert actor.is_running()

        # Stop actor
        await actor.stop()

    @pytest.mark.asyncio
    async def test_generate_frames(self):
        """Test that frames are generated."""
        actor = TestPatternActor(
            actor_id='test',
            width=320,
            height=240,
            pattern='smpte_bars',
            fps=60.0
        )

        # Wait for a few frames
        await asyncio.sleep(0.1)

        # Read a frame
        frame = actor.outputs['video'].buffer.read_latest()
        assert frame is not None
        assert isinstance(frame, VideoFrame)
        assert frame.width == 320
        assert frame.height == 240
        assert frame.data.shape == (240, 320, 3)
        assert frame.data.dtype == np.uint8

        # Stop actor
        await actor.stop()

    @pytest.mark.asyncio
    async def test_smpte_bars(self):
        """Test SMPTE bars pattern."""
        actor = TestPatternActor(
            actor_id='test',
            width=700,  # Divisible by 7
            height=100,
            pattern='smpte_bars',
            fps=30.0
        )

        # Wait for frame
        await asyncio.sleep(0.05)

        # Read frame
        frame = actor.outputs['video'].buffer.read_latest()
        assert frame is not None

        # Check it's not all black (should have colors)
        assert frame.data.max() > 0

        await actor.stop()


class TestActorConnection:
    """Test connecting actors together."""

    @pytest.mark.asyncio
    async def test_connect_actors(self):
        """Test connecting two actors."""

        # Producer actor
        class ProducerActor(Actor):
            def __init__(self):
                super().__init__('producer', clock=SoftwareClock(fps=60))
                self.outputs['data'] = StreamOutput('data')
                self.counter = 0
                self.start()

            async def process(self, tick):
                self.counter += 1
                self.outputs['data'].write(self.counter)

        # Consumer actor
        class ConsumerActor(Actor):
            def __init__(self):
                super().__init__('consumer')
                self.inputs['data'] = StreamInput('data')
                self.received = []
                self.start()

            async def process(self, tick):
                data = self.inputs['data'].read_latest()
                if data is not None:
                    self.received.append(data)

        # Create actors
        producer = ProducerActor()
        consumer = ConsumerActor()

        # Connect
        producer.outputs['data'] >> consumer.inputs['data']

        # Set consumer clock to match producer
        consumer.clock = producer.clock

        # Wait
        await asyncio.sleep(0.1)

        # Check data flowed
        assert len(consumer.received) > 0
        assert consumer.received[-1] == producer.counter

        # Stop
        await producer.stop()
        await consumer.stop()


class TestCompositorActor:
    """Test CompositorActor."""

    @pytest.mark.asyncio
    async def test_create_compositor(self):
        """Test creating a compositor actor."""
        from streamlib import CompositorActor

        compositor = CompositorActor(
            actor_id='compositor-test',
            width=640,
            height=480,
            fps=30,
            num_inputs=2
        )

        assert compositor.actor_id == 'compositor-test'
        assert compositor.width == 640
        assert compositor.height == 480
        assert compositor.is_running()

        # Should have 2 inputs
        assert 'input0' in compositor.inputs
        assert 'input1' in compositor.inputs

        # Should have video output
        assert 'video' in compositor.outputs

        await compositor.stop()

    @pytest.mark.asyncio
    async def test_compositor_with_sources(self):
        """Test compositor with multiple video sources."""
        from streamlib import CompositorActor, TestPatternActor

        # Create two test pattern generators
        gen1 = TestPatternActor(
            actor_id='gen1',
            width=320,
            height=240,
            pattern='smpte_bars',
            fps=60
        )

        gen2 = TestPatternActor(
            actor_id='gen2',
            width=320,
            height=240,
            pattern='gradient',
            fps=60
        )

        # Create compositor
        compositor = CompositorActor(
            actor_id='compositor',
            width=320,
            height=240,
            fps=60,
            num_inputs=2
        )

        # Connect generators to compositor
        gen1.outputs['video'] >> compositor.inputs['input0']
        gen2.outputs['video'] >> compositor.inputs['input1']

        # Wait for frames to be composited
        await asyncio.sleep(0.1)

        # Read composited frame
        frame = compositor.outputs['video'].buffer.read_latest()
        assert frame is not None
        assert frame.width == 320
        assert frame.height == 240
        assert frame.data.shape == (240, 320, 3)
        assert frame.data.dtype == np.uint8

        # Clean up
        await gen1.stop()
        await gen2.stop()
        await compositor.stop()


if __name__ == '__main__':
    pytest.main([__file__, '-v'])

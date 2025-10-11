"""
Tests for Phase 3 Actor Registry infrastructure.

Tests:
- ActorURI parsing and validation
- ActorRegistry (register, lookup, unregister)
- PortAllocator (port allocation, pairs, freeing)
- ActorStub (connect to local actors)
- Auto-registration in Actor base class
"""

import pytest
import asyncio
from streamlib import (
    ActorURI,
    ActorRegistry,
    PortAllocator,
    ActorStub,
    LocalActorStub,
    RemoteActorStub,
    connect_actor,
    Actor,
    SoftwareClock,
    StreamOutput,
)


class TestActorURI:
    """Test ActorURI parsing and validation."""

    def test_parse_valid_uri(self):
        """Test parsing a valid URI."""
        uri = ActorURI.parse('actor://local/TestPatternActor/test1')

        assert uri.scheme == 'actor'
        assert uri.host == 'local'
        assert uri.actor_class == 'TestPatternActor'
        assert uri.instance_id == 'test1'
        assert uri.is_local is True

    def test_parse_remote_uri(self):
        """Test parsing a remote URI."""
        uri = ActorURI.parse('actor://192.168.1.100/DisplayActor/monitor1')

        assert uri.scheme == 'actor'
        assert uri.host == '192.168.1.100'
        assert uri.actor_class == 'DisplayActor'
        assert uri.instance_id == 'monitor1'
        assert uri.is_local is False

    def test_parse_localhost(self):
        """Test that localhost is treated as local."""
        uri = ActorURI.parse('actor://localhost/TestActor/test')
        assert uri.is_local is True

        uri2 = ActorURI.parse('actor://127.0.0.1/TestActor/test')
        assert uri2.is_local is True

    def test_invalid_scheme(self):
        """Test that invalid scheme raises ValueError."""
        with pytest.raises(ValueError, match="Invalid URI scheme"):
            ActorURI.parse('http://local/TestActor/test')

    def test_missing_host(self):
        """Test that missing host raises ValueError."""
        with pytest.raises(ValueError, match="Missing host"):
            ActorURI.parse('actor:///TestActor/test')

    def test_invalid_path(self):
        """Test that invalid path raises ValueError."""
        with pytest.raises(ValueError, match="Invalid URI path"):
            ActorURI.parse('actor://local/TestActor')  # Missing instance ID

        with pytest.raises(ValueError, match="Invalid URI path"):
            ActorURI.parse('actor://local/TestActor/test/extra')  # Too many parts

    def test_invalid_actor_class(self):
        """Test that invalid actor class name raises ValueError."""
        with pytest.raises(ValueError, match="Invalid actor class name"):
            ActorURI.parse('actor://local/Test-Actor/test')  # Dash in class name

        with pytest.raises(ValueError, match="Invalid actor class name"):
            ActorURI.parse('actor://local/123Actor/test')  # Starts with number

    def test_invalid_instance_id(self):
        """Test that invalid instance ID raises ValueError."""
        with pytest.raises(ValueError, match="Invalid instance ID"):
            ActorURI.parse('actor://local/TestActor/test@123')  # Invalid char

    def test_to_string(self):
        """Test converting URI back to string."""
        original = 'actor://local/TestPatternActor/test1'
        uri = ActorURI.parse(original)
        assert uri.to_string() == original
        assert str(uri) == original


class TestActorRegistry:
    """Test ActorRegistry functionality."""

    def setup_method(self):
        """Reset registry before each test."""
        ActorRegistry.reset()

    def test_singleton(self):
        """Test that registry is a singleton."""
        reg1 = ActorRegistry.get()
        reg2 = ActorRegistry.get()
        assert reg1 is reg2

    @pytest.mark.asyncio
    async def test_register_actor(self):
        """Test registering an actor."""
        # Create simple test actor
        class TestActor(Actor):
            def __init__(self):
                super().__init__('test1', auto_register=False)  # Manual registration
                self.outputs['data'] = StreamOutput('data')
                self.start()

            async def process(self, tick):
                pass

        actor = TestActor()
        registry = ActorRegistry.get()

        # Register
        uri = registry.register(actor)
        assert uri == 'actor://local/TestActor/test1'

        # Lookup
        found = registry.lookup(uri)
        assert found is actor

        # Clean up
        await actor.stop()

    @pytest.mark.asyncio
    async def test_auto_register(self):
        """Test automatic registration on actor creation."""
        # Create actor with auto-registration (default)
        class TestActor(Actor):
            def __init__(self):
                super().__init__('test2')  # auto_register=True by default
                self.outputs['data'] = StreamOutput('data')
                self.start()

            async def process(self, tick):
                pass

        actor = TestActor()
        registry = ActorRegistry.get()

        # Should be registered automatically
        uri = 'actor://local/TestActor/test2'
        found = registry.lookup(uri)
        assert found is actor

        # Clean up
        await actor.stop()

    @pytest.mark.asyncio
    async def test_unregister_on_stop(self):
        """Test that actor unregisters when stopped."""
        class TestActor(Actor):
            def __init__(self):
                super().__init__('test3')
                self.outputs['data'] = StreamOutput('data')
                self.start()

            async def process(self, tick):
                pass

        actor = TestActor()
        registry = ActorRegistry.get()

        # Verify registered
        uri = 'actor://local/TestActor/test3'
        assert registry.lookup(uri) is actor

        # Stop actor
        await actor.stop()

        # Should be unregistered
        assert registry.lookup(uri) is None

    def test_register_duplicate(self):
        """Test that duplicate registration raises ValueError."""
        class TestActor(Actor):
            def __init__(self, actor_id):
                super().__init__(actor_id, auto_register=False)
                self.outputs['data'] = StreamOutput('data')

            async def process(self, tick):
                pass

        actor1 = TestActor('test4')
        registry = ActorRegistry.get()

        # Register first actor
        uri = registry.register(actor1)

        # Try to register at same URI
        actor2 = TestActor('test4')  # Same ID
        with pytest.raises(ValueError, match="already registered"):
            registry.register(actor2)

    def test_unregister_missing(self):
        """Test that unregistering missing actor raises KeyError."""
        registry = ActorRegistry.get()

        with pytest.raises(KeyError, match="not found"):
            registry.unregister('actor://local/TestActor/missing')

    def test_list_actors(self):
        """Test listing all registered actors."""
        class TestActor(Actor):
            def __init__(self, actor_id):
                super().__init__(actor_id, auto_register=False)
                self.outputs['data'] = StreamOutput('data')

            async def process(self, tick):
                pass

        actor1 = TestActor('actor1')
        actor2 = TestActor('actor2')

        registry = ActorRegistry.get()
        registry.register(actor1)
        registry.register(actor2)

        actors = registry.list_actors()
        assert len(actors) == 2
        assert 'actor://local/TestActor/actor1' in actors
        assert 'actor://local/TestActor/actor2' in actors

    def test_find_by_class(self):
        """Test finding actors by class name."""
        class ActorA(Actor):
            def __init__(self, actor_id):
                super().__init__(actor_id, auto_register=False)
            async def process(self, tick):
                pass

        class ActorB(Actor):
            def __init__(self, actor_id):
                super().__init__(actor_id, auto_register=False)
            async def process(self, tick):
                pass

        a1 = ActorA('a1')
        a2 = ActorA('a2')
        b1 = ActorB('b1')

        registry = ActorRegistry.get()
        registry.register(a1)
        registry.register(a2)
        registry.register(b1)

        # Find all ActorA instances
        actors_a = registry.find_by_class('ActorA')
        assert len(actors_a) == 2
        assert all('ActorA' in uri for uri in actors_a.keys())

        # Find all ActorB instances
        actors_b = registry.find_by_class('ActorB')
        assert len(actors_b) == 1
        assert 'ActorB' in list(actors_b.keys())[0]

    def test_find_by_instance_id(self):
        """Test finding actors by instance ID."""
        class TestActor(Actor):
            def __init__(self, actor_id):
                super().__init__(actor_id, auto_register=False)
            async def process(self, tick):
                pass

        actor1 = TestActor('test')
        registry = ActorRegistry.get()
        registry.register(actor1)

        # Find by instance ID
        actors = registry.find_by_instance_id('test')
        assert len(actors) == 1
        assert 'test' in list(actors.keys())[0]


class TestPortAllocator:
    """Test PortAllocator functionality."""

    def test_create_allocator(self):
        """Test creating a port allocator."""
        allocator = PortAllocator(start_port=20000, end_port=20100)
        assert allocator.start_port == 20000
        assert allocator.end_port == 20100

    def test_invalid_start_port(self):
        """Test that odd start port raises ValueError."""
        with pytest.raises(ValueError, match="start_port must be even"):
            PortAllocator(start_port=20001, end_port=20100)

    def test_invalid_end_port(self):
        """Test that odd end port raises ValueError."""
        with pytest.raises(ValueError, match="end_port must be even"):
            PortAllocator(start_port=20000, end_port=20101)

    def test_invalid_range(self):
        """Test that invalid range raises ValueError."""
        with pytest.raises(ValueError, match="start_port must be < end_port"):
            PortAllocator(start_port=20100, end_port=20000)

    def test_allocate_port(self):
        """Test allocating a single port."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        port = allocator.allocate('test-rtp')
        assert port == 20000
        assert port % 2 == 0  # Must be even
        assert allocator.is_allocated(port)

    def test_allocate_multiple_ports(self):
        """Test allocating multiple ports."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        port1 = allocator.allocate('video')
        port2 = allocator.allocate('audio')
        port3 = allocator.allocate('data')

        assert port1 == 20000
        assert port2 == 20002
        assert port3 == 20004

    def test_allocate_pair(self):
        """Test allocating RTP/RTCP port pair."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        rtp, rtcp = allocator.allocate_pair('video')
        assert rtcp == rtp + 1
        assert allocator.is_allocated(rtp)
        assert allocator.is_allocated(rtcp)

    def test_free_port(self):
        """Test freeing a port."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        port = allocator.allocate('test')
        assert allocator.is_allocated(port)

        allocator.free(port)
        assert not allocator.is_allocated(port)

    def test_free_pair(self):
        """Test freeing a port pair."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        rtp, rtcp = allocator.allocate_pair('video')
        allocator.free_pair(rtp)

        assert not allocator.is_allocated(rtp)
        assert not allocator.is_allocated(rtcp)

    def test_free_unallocated(self):
        """Test freeing unallocated port raises KeyError."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        with pytest.raises(KeyError, match="not allocated"):
            allocator.free(20050)

    def test_allocate_wraparound(self):
        """Test port allocation wraps around."""
        allocator = PortAllocator(start_port=20000, end_port=20010)  # Small range

        # Allocate all ports
        ports = []
        for i in range(5):  # (20010 - 20000) / 2 = 5 ports
            ports.append(allocator.allocate(f'port{i}'))

        # Should be out of ports
        with pytest.raises(RuntimeError, match="No available ports"):
            allocator.allocate('extra')

        # Free first port
        allocator.free(ports[0])

        # Should be able to allocate again (wrapped around)
        port = allocator.allocate('reused')
        assert port == ports[0]

    def test_get_allocated_ports(self):
        """Test getting all allocated ports."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        allocator.allocate('video')
        allocator.allocate('audio')

        allocated = allocator.get_allocated_ports()
        assert len(allocated) == 2
        assert 20000 in allocated
        assert 20002 in allocated

    def test_reset(self):
        """Test resetting allocator."""
        allocator = PortAllocator(start_port=20000, end_port=20100)

        allocator.allocate('port1')
        allocator.allocate('port2')

        allocator.reset()

        assert len(allocator.get_allocated_ports()) == 0

        # Should start from beginning again
        port = allocator.allocate('after-reset')
        assert port == 20000


class TestActorStub:
    """Test ActorStub functionality."""

    def setup_method(self):
        """Reset registry before each test."""
        ActorRegistry.reset()

    @pytest.mark.asyncio
    async def test_connect_local_actor(self):
        """Test connecting to a local actor via stub."""
        # Create actor (auto-registers)
        class TestActor(Actor):
            def __init__(self):
                super().__init__('test-stub')
                self.outputs['data'] = StreamOutput('data')
                self.start()

            async def process(self, tick):
                pass

        actor = TestActor()

        # Connect via stub
        stub = connect_actor('actor://local/TestActor/test-stub')

        assert isinstance(stub, LocalActorStub)
        assert stub.is_running()
        assert stub.get_actor() is actor

        # Stop via stub
        await stub.stop()
        assert not stub.is_running()

    def test_connect_missing_actor(self):
        """Test connecting to missing actor raises LookupError."""
        with pytest.raises(LookupError, match="not found"):
            connect_actor('actor://local/TestActor/missing')

    def test_connect_remote_actor(self):
        """Test connecting to remote actor returns RemoteActorStub."""
        stub = connect_actor('actor://192.168.1.100/TestActor/remote')

        assert isinstance(stub, RemoteActorStub)
        assert stub.uri.host == '192.168.1.100'
        assert not stub.is_running()  # Stub returns False

    @pytest.mark.asyncio
    async def test_local_stub_status(self):
        """Test getting status via local stub."""
        class TestActor(Actor):
            def __init__(self):
                super().__init__('status-test')
                self.outputs['data'] = StreamOutput('data')
                self.start()

            async def process(self, tick):
                pass

        actor = TestActor()
        stub = connect_actor('actor://local/TestActor/status-test')

        status = stub.get_status()
        assert status['actor_id'] == 'status-test'
        assert status['running'] is True

        await stub.stop()


if __name__ == '__main__':
    pytest.main([__file__, '-v'])

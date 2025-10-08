"""
Actor stubs for network-transparent operations.

Provides:
- ActorStub: Abstract base for actor proxies
- LocalActorStub: Direct reference to local actor
- RemoteActorStub: Network proxy for remote actor (stub for Phase 4)

These stubs enable network-transparent operations where local and remote
actors have the same interface. Code doesn't need to know if an actor is
local or remote.

Example:
    # Connect to actor (could be local or remote)
    actor = ActorStub.connect('actor://host/TestPatternActor/test1')

    # Use actor (same API regardless of location)
    status = actor.get_status()
    actor.outputs['video'] >> other_actor.inputs['video']
"""

import asyncio
from abc import ABC, abstractmethod
from typing import Optional, Dict, Any, TYPE_CHECKING

if TYPE_CHECKING:
    from streamlib.actor import Actor, StreamInput, StreamOutput
    from streamlib.registry import ActorURI


class ActorStub(ABC):
    """
    Abstract base class for actor stubs (proxies).

    Stubs provide network-transparent access to actors. Whether an actor
    is local or remote, the stub provides the same interface.
    """

    def __init__(self, uri: 'ActorURI'):
        """
        Initialize stub.

        Args:
            uri: Parsed ActorURI
        """
        self.uri = uri

    @abstractmethod
    def get_status(self) -> Dict[str, Any]:
        """
        Get actor status.

        Returns:
            Status dictionary
        """
        pass

    @abstractmethod
    async def stop(self) -> None:
        """Stop actor."""
        pass

    @abstractmethod
    def is_running(self) -> bool:
        """Check if actor is running."""
        pass

    @property
    @abstractmethod
    def inputs(self) -> Dict[str, 'StreamInput']:
        """Get input ports."""
        pass

    @property
    @abstractmethod
    def outputs(self) -> Dict[str, 'StreamOutput']:
        """Get output ports."""
        pass

    @classmethod
    def connect(cls, uri: str) -> 'ActorStub':
        """
        Connect to actor by URI.

        Automatically determines if actor is local or remote and returns
        appropriate stub.

        Args:
            uri: Actor URI string

        Returns:
            LocalActorStub or RemoteActorStub

        Raises:
            ValueError: If URI is invalid
            LookupError: If actor not found
        """
        from streamlib.registry import ActorURI, ActorRegistry

        # Parse URI
        parsed = ActorURI.parse(uri)

        # Check if local
        if parsed.is_local:
            # Look up in registry
            registry = ActorRegistry.get()
            actor = registry.lookup(uri)
            if actor is None:
                raise LookupError(f"Local actor not found: {uri}")
            return LocalActorStub(parsed, actor)
        else:
            # Remote actor (stub for Phase 4)
            return RemoteActorStub(parsed)


class LocalActorStub(ActorStub):
    """
    Stub for local actor (direct reference).

    This is a simple wrapper around a local actor instance. It provides
    the same interface as RemoteActorStub but with direct access.
    """

    def __init__(self, uri: 'ActorURI', actor: 'Actor'):
        """
        Initialize local stub.

        Args:
            uri: Parsed ActorURI
            actor: Local actor instance
        """
        super().__init__(uri)
        self.actor = actor

    def get_status(self) -> Dict[str, Any]:
        """Get actor status (direct call)."""
        return self.actor.get_status()

    async def stop(self) -> None:
        """Stop actor (direct call)."""
        await self.actor.stop()

    def is_running(self) -> bool:
        """Check if actor is running (direct call)."""
        return self.actor.is_running()

    @property
    def inputs(self) -> Dict[str, 'StreamInput']:
        """Get input ports (direct access)."""
        return self.actor.inputs

    @property
    def outputs(self) -> Dict[str, 'StreamOutput']:
        """Get output ports (direct access)."""
        return self.actor.outputs

    def get_actor(self) -> 'Actor':
        """
        Get underlying actor instance.

        Returns:
            Actor instance

        Note: Only available for local stubs.
        """
        return self.actor


class RemoteActorStub(ActorStub):
    """
    Stub for remote actor (network proxy) - Phase 4 implementation.

    This stub communicates with a remote actor over the network using
    the control plane protocol. It makes remote actors look like local ones.

    Phase 4 will implement:
    - WebSocket connection to remote actor
    - Command serialization (JSON-RPC or similar)
    - Status polling
    - Remote port connections (sets up SMPTE ST 2110 streams)

    For now, this is a stub that prepares the interface.
    """

    def __init__(self, uri: 'ActorURI'):
        """
        Initialize remote stub.

        Args:
            uri: Parsed ActorURI

        Note: Phase 4 will establish network connection here.
        """
        super().__init__(uri)
        self._status_cache: Optional[Dict[str, Any]] = None
        self._inputs: Dict[str, 'StreamInput'] = {}
        self._outputs: Dict[str, 'StreamOutput'] = {}

        print(f"[RemoteActorStub] Warning: Remote actor stubs not implemented (Phase 4)")
        print(f"[RemoteActorStub] URI: {uri}")

    def get_status(self) -> Dict[str, Any]:
        """
        Get actor status (network call).

        TODO Phase 4: Send status request over network.

        Returns:
            Status dictionary (stub returns cached or dummy status)
        """
        if self._status_cache is not None:
            return self._status_cache

        # Stub: Return dummy status
        return {
            'actor_id': self.uri.instance_id,
            'running': False,
            'remote': True,
            'uri': str(self.uri),
            'warning': 'Remote stubs not implemented (Phase 4)'
        }

    async def stop(self) -> None:
        """
        Stop remote actor (network call).

        TODO Phase 4: Send stop command over network.
        """
        print(f"[RemoteActorStub] Stop command not implemented (Phase 4)")
        print(f"[RemoteActorStub] Would send stop to: {self.uri}")

    def is_running(self) -> bool:
        """
        Check if remote actor is running (network call).

        TODO Phase 4: Query remote actor status.

        Returns:
            False (stub implementation)
        """
        return False

    @property
    def inputs(self) -> Dict[str, 'StreamInput']:
        """
        Get input ports (network discovery).

        TODO Phase 4: Discover remote input ports and create network receivers.

        Returns:
            Empty dict (stub implementation)
        """
        return self._inputs

    @property
    def outputs(self) -> Dict[str, 'StreamOutput']:
        """
        Get output ports (network discovery).

        TODO Phase 4: Discover remote output ports and create network senders.

        Returns:
            Empty dict (stub implementation)
        """
        return self._outputs

    async def connect(self) -> None:
        """
        Establish connection to remote actor.

        TODO Phase 4: Open WebSocket connection, authenticate, discover ports.
        """
        print(f"[RemoteActorStub] Connect not implemented (Phase 4)")
        print(f"[RemoteActorStub] Would connect to: {self.uri}")

    async def disconnect(self) -> None:
        """
        Close connection to remote actor.

        TODO Phase 4: Close WebSocket, clean up network streams.
        """
        print(f"[RemoteActorStub] Disconnect not implemented (Phase 4)")


# Helper function for connecting to actors
def connect_actor(uri: str) -> ActorStub:
    """
    Connect to an actor by URI.

    Convenience function that wraps ActorStub.connect().

    Args:
        uri: Actor URI string (e.g., 'actor://local/TestPatternActor/test1')

    Returns:
        ActorStub (LocalActorStub or RemoteActorStub)

    Raises:
        ValueError: If URI is invalid
        LookupError: If actor not found

    Example:
        actor = connect_actor('actor://local/TestPatternActor/test1')
        status = actor.get_status()
        actor.outputs['video'] >> display.inputs['video']
    """
    return ActorStub.connect(uri)

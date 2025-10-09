"""
Actor registry and URI system for network-transparent operations.

Provides:
- ActorURI: Parse and validate actor URIs (actor://host/ActorClass/instance-id)
- ActorRegistry: Global registry mapping URIs to actor instances
- PortAllocator: UDP port allocation for SMPTE ST 2110 streams

URI Format:
    actor://host/ActorClass/instance-id

    Examples:
        actor://local/TestPatternActor/test1
        actor://192.168.1.100/DisplayActor/monitor1
        actor://edge-server.local/CompositorActor/main
"""

import re
from dataclasses import dataclass
from typing import Optional, Dict, Any, TYPE_CHECKING
from urllib.parse import urlparse

if TYPE_CHECKING:
    from streamlib.actor import Actor


@dataclass
class ActorURI:
    """
    Parsed actor URI.

    Format: actor://host/ActorClass/instance-id

    Attributes:
        scheme: Must be 'actor'
        host: Hostname, IP address, or 'local'
        actor_class: Actor class name (e.g., 'TestPatternActor')
        instance_id: Unique instance identifier
        is_local: True if host is 'local' or matches local hostname
    """
    scheme: str
    host: str
    actor_class: str
    instance_id: str
    is_local: bool = False

    @classmethod
    def parse(cls, uri: str) -> 'ActorURI':
        """
        Parse actor URI string.

        Args:
            uri: URI string (e.g., 'actor://local/TestPatternActor/test1')

        Returns:
            Parsed ActorURI

        Raises:
            ValueError: If URI format is invalid
        """
        # Parse using urlparse
        parsed = urlparse(uri)

        # Validate scheme
        if parsed.scheme != 'actor':
            raise ValueError(f"Invalid URI scheme '{parsed.scheme}', expected 'actor'")

        # Validate host
        if not parsed.netloc:
            raise ValueError(f"Missing host in URI: {uri}")
        host = parsed.netloc

        # Parse path: /ActorClass/instance-id
        path = parsed.path.strip('/')
        parts = path.split('/')

        if len(parts) != 2:
            raise ValueError(
                f"Invalid URI path '{parsed.path}', expected /ActorClass/instance-id"
            )

        actor_class, instance_id = parts

        # Validate actor class name (must be valid Python identifier)
        if not cls._is_valid_identifier(actor_class):
            raise ValueError(f"Invalid actor class name '{actor_class}'")

        # Validate instance ID (alphanumeric, dash, underscore)
        if not cls._is_valid_instance_id(instance_id):
            raise ValueError(f"Invalid instance ID '{instance_id}'")

        # Check if local
        is_local = (host == 'local' or host == 'localhost' or host == '127.0.0.1')

        return cls(
            scheme='actor',
            host=host,
            actor_class=actor_class,
            instance_id=instance_id,
            is_local=is_local
        )

    @staticmethod
    def _is_valid_identifier(name: str) -> bool:
        """Check if string is a valid Python identifier."""
        return name.isidentifier()

    @staticmethod
    def _is_valid_instance_id(instance_id: str) -> bool:
        """Check if instance ID is valid (alphanumeric, dash, underscore)."""
        return bool(re.match(r'^[a-zA-Z0-9_-]+$', instance_id))

    def to_string(self) -> str:
        """
        Convert back to URI string.

        Returns:
            URI string
        """
        return f"{self.scheme}://{self.host}/{self.actor_class}/{self.instance_id}"

    def __str__(self) -> str:
        return self.to_string()


class ActorRegistry:
    """
    Global registry for actor instances.

    Maps URIs to actor instances, enabling network-transparent operations.
    Actors can be looked up by URI and connected across machines.

    This is a singleton - use ActorRegistry.get() to access.
    """

    _instance: Optional['ActorRegistry'] = None

    def __init__(self):
        """Initialize registry."""
        self._actors: Dict[str, 'Actor'] = {}  # URI string -> Actor instance
        self._local_hostname = 'local'  # TODO: Get actual hostname

    @classmethod
    def get(cls) -> 'ActorRegistry':
        """
        Get global registry instance (singleton).

        Returns:
            Global ActorRegistry instance
        """
        if cls._instance is None:
            cls._instance = cls()
        return cls._instance

    @classmethod
    def reset(cls) -> None:
        """
        Reset global registry (for testing).

        Warning: This clears all registered actors.
        """
        cls._instance = None

    def register(self, actor: 'Actor', uri: Optional[str] = None) -> str:
        """
        Register an actor in the registry.

        Args:
            actor: Actor instance to register
            uri: Optional URI string. If None, auto-generate from actor_id

        Returns:
            URI string for the registered actor

        Raises:
            ValueError: If URI is invalid or already registered
        """
        # Auto-generate URI if not provided
        if uri is None:
            actor_class = actor.__class__.__name__
            instance_id = actor.actor_id
            uri = f"actor://{self._local_hostname}/{actor_class}/{instance_id}"

        # Parse and validate URI
        parsed = ActorURI.parse(uri)

        # Check if already registered
        uri_str = parsed.to_string()
        if uri_str in self._actors:
            raise ValueError(f"Actor already registered at URI: {uri_str}")

        # Register
        self._actors[uri_str] = actor

        return uri_str

    def unregister(self, uri: str) -> None:
        """
        Unregister an actor from the registry.

        Args:
            uri: URI string of actor to unregister

        Raises:
            KeyError: If URI not found in registry
        """
        # Parse to normalize URI
        parsed = ActorURI.parse(uri)
        uri_str = parsed.to_string()

        # Unregister
        if uri_str not in self._actors:
            raise KeyError(f"Actor not found in registry: {uri_str}")

        del self._actors[uri_str]

    def lookup(self, uri: str) -> Optional['Actor']:
        """
        Look up actor by URI.

        Args:
            uri: URI string (e.g., 'actor://local/TestPatternActor/test1')

        Returns:
            Actor instance if found, None otherwise
        """
        # Parse to normalize URI
        parsed = ActorURI.parse(uri)
        uri_str = parsed.to_string()

        return self._actors.get(uri_str)

    def list_actors(self) -> Dict[str, 'Actor']:
        """
        List all registered actors.

        Returns:
            Dictionary mapping URI strings to Actor instances
        """
        return self._actors.copy()

    def find_by_class(self, actor_class: str) -> Dict[str, 'Actor']:
        """
        Find all actors of a given class.

        Args:
            actor_class: Actor class name (e.g., 'TestPatternActor')

        Returns:
            Dictionary mapping URI strings to Actor instances
        """
        result = {}
        for uri_str, actor in self._actors.items():
            parsed = ActorURI.parse(uri_str)
            if parsed.actor_class == actor_class:
                result[uri_str] = actor
        return result

    def find_by_instance_id(self, instance_id: str) -> Dict[str, 'Actor']:
        """
        Find all actors with a given instance ID.

        Args:
            instance_id: Instance ID (e.g., 'test1')

        Returns:
            Dictionary mapping URI strings to Actor instances
        """
        result = {}
        for uri_str, actor in self._actors.items():
            parsed = ActorURI.parse(uri_str)
            if parsed.instance_id == instance_id:
                result[uri_str] = actor
        return result


class PortAllocator:
    """
    UDP port allocator for SMPTE ST 2110 streams.

    SMPTE ST 2110 uses RTP/UDP with specific port allocation rules:
    - Video, audio, and ancillary data each get separate ports
    - Ports must be even numbers (odd ports for RTCP)
    - Typical range: 20000-30000

    This allocator ensures ports are allocated safely without conflicts.
    """

    def __init__(self, start_port: int = 20000, end_port: int = 30000):
        """
        Initialize port allocator.

        Args:
            start_port: First port in allocation range (must be even)
            end_port: Last port in allocation range (must be even)

        Raises:
            ValueError: If port range is invalid
        """
        if start_port % 2 != 0:
            raise ValueError(f"start_port must be even, got {start_port}")
        if end_port % 2 != 0:
            raise ValueError(f"end_port must be even, got {end_port}")
        if start_port >= end_port:
            raise ValueError(f"start_port must be < end_port ({start_port} >= {end_port})")

        self.start_port = start_port
        self.end_port = end_port
        self._allocated: Dict[int, str] = {}  # port -> description
        self._next_port = start_port

    def allocate(self, description: str = '') -> int:
        """
        Allocate next available port.

        Args:
            description: Optional description for tracking (e.g., 'video-rtp')

        Returns:
            Allocated port number (even)

        Raises:
            RuntimeError: If no ports available
        """
        # Find next available port
        for _ in range((self.end_port - self.start_port) // 2):
            port = self._next_port

            # Advance to next port (skip odd port for RTCP)
            self._next_port += 2
            if self._next_port >= self.end_port:
                self._next_port = self.start_port

            # Check if port is free
            if port not in self._allocated:
                self._allocated[port] = description
                return port

        raise RuntimeError(f"No available ports in range {self.start_port}-{self.end_port}")

    def allocate_pair(self, description: str = '') -> tuple[int, int]:
        """
        Allocate port pair (RTP + RTCP).

        Args:
            description: Optional description for tracking

        Returns:
            Tuple of (rtp_port, rtcp_port) where rtcp_port = rtp_port + 1

        Raises:
            RuntimeError: If no ports available
        """
        rtp_port = self.allocate(f"{description}-rtp")
        rtcp_port = rtp_port + 1
        self._allocated[rtcp_port] = f"{description}-rtcp"
        return (rtp_port, rtcp_port)

    def free(self, port: int) -> None:
        """
        Free an allocated port.

        Args:
            port: Port number to free

        Raises:
            KeyError: If port was not allocated
        """
        if port not in self._allocated:
            raise KeyError(f"Port {port} was not allocated")
        del self._allocated[port]

    def free_pair(self, rtp_port: int) -> None:
        """
        Free a port pair (RTP + RTCP).

        Args:
            rtp_port: RTP port number (RTCP is rtp_port + 1)

        Raises:
            KeyError: If ports were not allocated
        """
        self.free(rtp_port)
        self.free(rtp_port + 1)

    def is_allocated(self, port: int) -> bool:
        """
        Check if port is allocated.

        Args:
            port: Port number to check

        Returns:
            True if port is allocated, False otherwise
        """
        return port in self._allocated

    def get_allocated_ports(self) -> Dict[int, str]:
        """
        Get all allocated ports.

        Returns:
            Dictionary mapping port numbers to descriptions
        """
        return self._allocated.copy()

    def reset(self) -> None:
        """Reset allocator (free all ports)."""
        self._allocated.clear()
        self._next_port = self.start_port

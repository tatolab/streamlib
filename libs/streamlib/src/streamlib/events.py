"""
Event bus for clean runtime communication.

Inspired by Pipeless's event-driven architecture, provides:
- Broadcast tick distribution (fixes sequential tick bug)
- Error propagation
- Metadata change notifications
- Clean separation of concerns

Example:
    bus = EventBus()

    # Subscribe to events
    async for event in bus.subscribe(ClockTickEvent):
        tick = event.tick
        await handler.process(tick)
"""

import asyncio
from typing import Dict, List, Type, Any, Optional
from dataclasses import dataclass
from .clocks import TimedTick


@dataclass
class Event:
    """Base class for all events."""
    pass


@dataclass
class ClockTickEvent(Event):
    """Clock tick broadcast to all handlers."""
    tick: TimedTick


@dataclass
class ErrorEvent(Event):
    """Handler error event."""
    handler_id: str
    exception: Exception
    tick: Optional[TimedTick] = None


@dataclass
class HandlerStartedEvent(Event):
    """Handler started successfully."""
    handler_id: str


@dataclass
class HandlerStoppedEvent(Event):
    """Handler stopped."""
    handler_id: str
    frame_count: int


class EventBus:
    """
    Async event bus for runtime communication.

    Uses asyncio queues for efficient broadcast without copying.
    Each subscriber gets their own queue.

    Example:
        bus = EventBus()

        # Publisher
        await bus.publish(ClockTickEvent(tick))

        # Subscriber
        async for event in bus.subscribe(ClockTickEvent):
            # Process event
            pass
    """

    def __init__(self, buffer_size: int = 100):
        """
        Initialize event bus.

        Args:
            buffer_size: Max queued events per subscriber (backpressure)
        """
        self.buffer_size = buffer_size

        # Subscribers by event type
        self._subscribers: Dict[Type[Event], List[asyncio.Queue]] = {}

        # All subscribers (for broadcast)
        self._all_queues: List[asyncio.Queue] = []

    def subscribe(self, event_type: Type[Event]) -> 'EventSubscription':
        """
        Subscribe to specific event type.

        Args:
            event_type: Event class to subscribe to

        Returns:
            EventSubscription async iterator

        Example:
            async for event in bus.subscribe(ClockTickEvent):
                print(event.tick)
        """
        # Create queue for this subscriber
        queue = asyncio.Queue(maxsize=self.buffer_size)

        # Register subscriber
        if event_type not in self._subscribers:
            self._subscribers[event_type] = []
        self._subscribers[event_type].append(queue)
        self._all_queues.append(queue)

        return EventSubscription(queue, self, event_type)

    def publish(self, event: Event) -> None:
        """
        Publish event to all subscribers of that event type.

        Args:
            event: Event to publish

        Note: Uses put_nowait for non-blocking publish.
        If subscriber queue is full, event is dropped (graceful degradation).
        """
        event_type = type(event)

        if event_type not in self._subscribers:
            return  # No subscribers

        # Send to all subscribers of this event type
        for queue in self._subscribers[event_type]:
            try:
                queue.put_nowait(event)
            except asyncio.QueueFull:
                # Graceful degradation: drop event if subscriber is behind
                pass

    def unsubscribe(self, queue: asyncio.Queue, event_type: Type[Event]) -> None:
        """
        Unsubscribe from event type.

        Args:
            queue: Subscriber's queue
            event_type: Event type to unsubscribe from
        """
        if event_type in self._subscribers:
            if queue in self._subscribers[event_type]:
                self._subscribers[event_type].remove(queue)

        if queue in self._all_queues:
            self._all_queues.remove(queue)

    async def clear(self) -> None:
        """Clear all subscribers and queues."""
        for queues in self._subscribers.values():
            for queue in queues:
                # Drain queue
                while not queue.empty():
                    try:
                        queue.get_nowait()
                    except asyncio.QueueEmpty:
                        break

        self._subscribers.clear()
        self._all_queues.clear()


class EventSubscription:
    """
    Async iterator for event subscription.

    Allows using `async for` to receive events.
    """

    def __init__(self, queue: asyncio.Queue, bus: EventBus, event_type: Type[Event]):
        self.queue = queue
        self.bus = bus
        self.event_type = event_type
        self._active = True

    def __aiter__(self):
        return self

    async def __anext__(self):
        """Get next event from queue."""
        if not self._active:
            raise StopAsyncIteration

        try:
            event = await self.queue.get()
            return event
        except asyncio.CancelledError:
            self._active = False
            raise StopAsyncIteration

    def unsubscribe(self):
        """Unsubscribe from events."""
        self._active = False
        self.bus.unsubscribe(self.queue, self.event_type)

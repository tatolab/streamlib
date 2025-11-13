#!/usr/bin/env python3
"""
Python Event Bus Demo

Demonstrates how to use the event bus from Python to:
1. Subscribe to events with callback functions
2. Subscribe with class-based listeners
3. Publish custom events
4. React to processor lifecycle events
"""

import time
from streamlib import EventBus, Event, topics

# Example 1: Subscribe with function callback
def on_runtime_event(event):
    """Simple function callback"""
    print(f"\n[Function Callback] Received event on topic: {event.topic}")
    print(f"  - is_runtime_global: {event.is_runtime_global}")
    print(f"  - is_processor_event: {event.is_processor_event}")
    print(f"  - is_custom: {event.is_custom}")
    if event.is_custom:
        print(f"  - custom_data: {event.custom_data}")


# Example 2: Subscribe with class-based listener
class ProcessorMonitor:
    """Class-based event listener"""
    def __init__(self):
        self.processor_count = 0

    def on_event(self, event):
        """Called when events are received"""
        print(f"\n[ProcessorMonitor] Received event on topic: {event.topic}")
        print(f"  - Event type: custom={event.is_custom}, processor={event.is_processor_event}")


# Example 3: AI Diagnostic Listener (simulates correlation)
class DiagnosticAI:
    """Simulates AI-powered diagnostic insights"""
    def __init__(self):
        self.events = []

    def on_event(self, event):
        self.events.append(event)
        print(f"\n[DiagnosticAI] Analyzing event: {event.topic}")
        print(f"  - Total events collected: {len(self.events)}")

    def verify_events(self, expected_count, expected_topics):
        """Verify we received the expected events"""
        assert len(self.events) == expected_count, \
            f"Expected {expected_count} events, got {len(self.events)}"

        received_topics = [e.topic for e in self.events]
        for topic in expected_topics:
            assert topic in received_topics, \
                f"Expected to receive event with topic '{topic}', but got: {received_topics}"

        print(f"\n✓ Verified: Received all {expected_count} expected events")
        return True


def main():
    print("=" * 60)
    print("Python Event Bus Demo")
    print("=" * 60)
    print()

    # Create event bus instance
    bus = EventBus()

    print("1. Setting up subscribers...")
    print("-" * 60)

    # Subscribe function callback to runtime global events
    print(f"Subscribing function to '{topics.RUNTIME_GLOBAL}'...")
    bus.subscribe(topics.RUNTIME_GLOBAL, on_runtime_event)
    print(f"✓ Subscribed function to '{topics.RUNTIME_GLOBAL}'")

    # Subscribe class-based listener to keyboard events
    print(f"Subscribing ProcessorMonitor to '{topics.KEYBOARD}'...")
    monitor = ProcessorMonitor()
    bus.subscribe(topics.KEYBOARD, monitor)
    print(f"✓ Subscribed ProcessorMonitor to '{topics.KEYBOARD}'")

    # Subscribe AI diagnostic to all topics
    print("Subscribing DiagnosticAI to multiple topics...")
    ai = DiagnosticAI()
    bus.subscribe(topics.RUNTIME_GLOBAL, ai)
    bus.subscribe(topics.KEYBOARD, ai)
    bus.subscribe(topics.MOUSE, ai)
    bus.subscribe("diagnostics", ai)
    print("✓ Subscribed DiagnosticAI to multiple topics")
    print()

    print("2. Publishing events (with delays to see callbacks)...")
    print("-" * 60)

    # Give subscribers time to register
    time.sleep(0.5)

    # Publish custom events
    print("\nPublishing custom event to 'diagnostics' topic...")
    custom_event = Event.custom("diagnostics", {"temperature": 78.3, "status": "warning"})
    bus.publish("diagnostics", custom_event)
    time.sleep(0.1)  # Give time for callbacks to execute

    # Publish keyboard event
    print("\nPublishing keyboard event (Ctrl+Space)...")
    kb_event = Event.keyboard("space", pressed=True, ctrl=True)
    bus.publish(topics.KEYBOARD, kb_event)
    time.sleep(0.1)

    # Publish mouse event
    print("\nPublishing mouse event (Left click at 100, 200)...")
    mouse_event = Event.mouse("left", x=100.0, y=200.0, pressed=True)
    bus.publish(topics.MOUSE, mouse_event)
    time.sleep(0.1)

    # Publish to runtime global (should trigger multiple listeners)
    print("\nPublishing to runtime:global topic (multiple subscribers)...")
    runtime_event = Event.custom("test_message", {"msg": "Hello from event bus!"})
    bus.publish(topics.RUNTIME_GLOBAL, runtime_event)
    time.sleep(0.1)

    # Test with custom topic
    print("\nPublishing to custom 'test:demo' topic...")
    test_event = Event.custom("test:demo", {"iteration": 1, "data": "test data"})
    bus.publish("test:demo", test_event)
    time.sleep(0.1)

    print("\n" + "=" * 60)
    print("Waiting 2 seconds for any remaining callbacks...")
    print("=" * 60)
    time.sleep(2)
    print()

    print("3. Verifying event delivery...")
    print("-" * 60)

    # Verify DiagnosticAI received all expected events
    ai.verify_events(
        expected_count=4,
        expected_topics=["diagnostics", "input:keyboard", "input:mouse", "test_message"]
    )

    # Verify custom event payloads
    print("\n4. Verifying event payloads...")
    print("-" * 60)

    # Find the diagnostics event
    diagnostics_event = next((e for e in ai.events if e.topic == "diagnostics"), None)
    assert diagnostics_event is not None, "Diagnostics event not found"
    assert diagnostics_event.is_custom, "Diagnostics event should be custom"

    # Parse and verify the JSON payload
    import json
    diagnostics_data = json.loads(diagnostics_event.custom_data)
    assert diagnostics_data["temperature"] == 78.3, f"Expected temperature 78.3, got {diagnostics_data['temperature']}"
    assert diagnostics_data["status"] == "warning", f"Expected status 'warning', got {diagnostics_data['status']}"
    print("✓ Verified diagnostics event payload: temperature=78.3, status=warning")

    # Find the test_message event
    test_msg_event = next((e for e in ai.events if e.topic == "test_message"), None)
    assert test_msg_event is not None, "test_message event not found"
    test_msg_data = json.loads(test_msg_event.custom_data)
    assert test_msg_data["msg"] == "Hello from event bus!", f"Expected message, got {test_msg_data['msg']}"
    print("✓ Verified test_message event payload: msg='Hello from event bus!'")

    print()
    print("5. Event Bus Features Demonstrated:")
    print("-" * 60)
    print("✓ Function-based callbacks")
    print("✓ Class-based listeners (with on_event method)")
    print("✓ Multiple subscribers per topic")
    print("✓ Topic-based routing")
    print("✓ Custom JSON events")
    print("✓ Built-in event types (keyboard, mouse, processor)")
    print("✓ Event delivery verification")
    print("✓ Payload data validation")
    print()

    print("=" * 60)
    print("Demo Complete!")
    print("=" * 60)
    print()
    print("Integration Examples:")
    print()
    print("Voice Command Processor:")
    print("  def on_voice_command(cmd):")
    print("    if cmd == 'show stats':")
    print("      bus.publish(topics.RUNTIME_GLOBAL,")
    print("                  Event.custom('enable_profiling', {}))")
    print()
    print("HUD Processor:")
    print("  class HUDProcessor:")
    print("    def on_event(self, event):")
    print("      if event.topic == 'thermal_warning':")
    print("        self.show_overlay('⚠️ High temperature')")
    print()
    print("Thermal Monitor:")
    print("  def on_temperature(temp):")
    print("    if temp > 75:")
    print("      bus.publish('thermal_warning',")
    print("                  Event.custom('thermal', {'celsius': temp}))")
    print()


if __name__ == "__main__":
    main()

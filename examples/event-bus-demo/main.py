#!/usr/bin/env python3
"""
Python Event Bus Demo

Demonstrates how to use the event bus from Python to:
1. Subscribe to events with callback functions
2. Subscribe with class-based listeners
3. Publish custom events
4. React to processor lifecycle events
"""

from streamlib import EventBus, Event, topics

# Example 1: Subscribe with function callback
def on_runtime_event(event):
    """Simple function callback"""
    print(f"[Function Callback] Received event on topic: {event.topic}")
    if event.is_runtime_global:
        print(f"  - Runtime global event")
    if event.is_processor_event:
        print(f"  - Processor event for: {event.processor_id}")


# Example 2: Subscribe with class-based listener
class ProcessorMonitor:
    """Class-based event listener"""
    def __init__(self):
        self.processor_count = 0

    def on_event(self, event):
        """Called when events are received"""
        if event.is_processor_event:
            processor_id = event.processor_id
            print(f"[ProcessorMonitor] Event for processor '{processor_id}' on topic: {event.topic}")
        else:
            print("[ProcessorMonitor] Received event on topic:", event.topic)


# Example 3: AI Diagnostic Listener (simulates correlation)
class DiagnosticAI:
    """Simulates AI-powered diagnostic insights"""
    def __init__(self):
        self.events = []

    def on_event(self, event):
        self.events.append(event)
        print(f"[DiagnosticAI] Analyzing event: {event.topic}")

        # Simulate correlation analysis
        if len(self.events) > 5:
            print(f"[DiagnosticAI] 💡 Insight: Detected {len(self.events)} events so far")


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
    bus.subscribe(topics.RUNTIME_GLOBAL, on_runtime_event)
    print(f"✓ Subscribed function to '{topics.RUNTIME_GLOBAL}'")

    # Subscribe class-based listener to keyboard events
    monitor = ProcessorMonitor()
    bus.subscribe(topics.KEYBOARD, monitor)
    print(f"✓ Subscribed ProcessorMonitor to '{topics.KEYBOARD}'")

    # Subscribe AI diagnostic to all topics
    ai = DiagnosticAI()
    bus.subscribe(topics.RUNTIME_GLOBAL, ai)
    bus.subscribe(topics.KEYBOARD, ai)
    bus.subscribe(topics.MOUSE, ai)
    print("✓ Subscribed DiagnosticAI to multiple topics")
    print()

    print("2. Publishing events...")
    print("-" * 60)

    # Publish custom events
    custom_event = Event.custom("diagnostics", {"temperature": 78.3, "status": "warning"})
    bus.publish("diagnostics", custom_event)
    print("✓ Published custom diagnostic event")

    # Publish keyboard event
    kb_event = Event.keyboard("space", pressed=True, ctrl=True)
    bus.publish(topics.KEYBOARD, kb_event)
    print("✓ Published keyboard event (Ctrl+Space)")

    # Publish mouse event
    mouse_event = Event.mouse("left", x=100.0, y=200.0, pressed=True)
    bus.publish(topics.MOUSE, mouse_event)
    print("✓ Published mouse event (Left click at 100, 200)")

    # Publish processor event
    proc_event = Event.processor("processor_0", "started")
    proc_topic = "processor:processor_0"  # topics.processor_topic() not exported yet
    bus.publish(proc_topic, proc_event)
    print(f"✓ Published processor started event to '{proc_topic}'")
    print()

    print("3. Event Bus Features Demonstrated:")
    print("-" * 60)
    print("✓ Function-based callbacks")
    print("✓ Class-based listeners (with on_event method)")
    print("✓ Multiple subscribers per topic")
    print("✓ Topic-based routing")
    print("✓ Custom JSON events")
    print("✓ Built-in event types (keyboard, mouse, processor)")
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

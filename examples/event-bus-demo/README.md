# Event Bus Demo

Demonstrates the Python event bus API for the Streamlib framework.

## Features

- ✅ Subscribe to events with function callbacks
- ✅ Subscribe with class-based listeners (with `on_event` method)
- ✅ Publish custom JSON events
- ✅ Built-in event types (keyboard, mouse, processor)
- ✅ Topic-based routing
- ✅ Multiple subscribers per topic

## Running

```bash
# From repository root
nx run event-bus-demo:sync  # Install dependencies
nx run event-bus-demo:run   # Run the demo
```

## What This Demonstrates

### 1. Function Callbacks
```python
def on_runtime_event(event):
    print(f"Received: {event.topic}")

bus = EventBus()
bus.subscribe(topics.RUNTIME_GLOBAL, on_runtime_event)
```

### 2. Class-Based Listeners
```python
class ProcessorMonitor:
    def on_event(self, event):
        print(f"Event: {event.topic}")

monitor = ProcessorMonitor()
bus.subscribe(topics.KEYBOARD, monitor)
```

### 3. Publishing Events
```python
# Custom JSON events
event = Event.custom("diagnostics", {"temp": 78.3})
bus.publish("diagnostics", event)

# Built-in event types
kb_event = Event.keyboard("space", pressed=True, ctrl=True)
bus.publish(topics.KEYBOARD, kb_event)
```

## Use Cases

This event bus enables:

- 🎤 **Voice-activated diagnostics** - "show stats" triggers profiling
- 🤖 **AI correlation** - Analyze thermal + FPS events for insights
- 📊 **HUD updates** - React to any system event
- 🔥 **Thermal monitoring** - Publish temperature warnings
- ⚙️ **Dynamic processor loading** - React to ProcessorAdded/Removed events

## Integration Example

```python
from streamlib import EventBus, Event, topics, processor

@processor(description="Voice command processor")
class VoiceCommandProcessor:
    def setup(self):
        self.bus = EventBus()

    def process(self):
        command = self.recognize_speech()
        if command == "show stats":
            # Trigger profiling across the system
            event = Event.custom("enable_profiling", {})
            self.bus.publish(topics.RUNTIME_GLOBAL, event)

@processor(description="HUD overlay processor")
class HUDProcessor:
    def setup(self):
        bus = EventBus()
        bus.subscribe("thermal_warning", self.on_thermal_warning)

    def on_thermal_warning(self, event):
        data = event.custom_data
        self.show_overlay(f"⚠️ High temperature: {data['celsius']}°C")

    def process(self):
        # Render HUD
        pass
```

## Topics

Available topic constants:
- `topics.RUNTIME_GLOBAL` - Runtime lifecycle events
- `topics.KEYBOARD` - Keyboard input events
- `topics.MOUSE` - Mouse input events
- `topics.WINDOW` - Window events
- `processor_topic(id)` - Processor-specific events

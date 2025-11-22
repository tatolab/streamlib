//! Event types for pub/sub messaging

use crate::core::error::Result;
use serde::{Deserialize, Serialize};

/// Common topic constants for system events
pub mod topics {
    /// Runtime global events (lifecycle, errors)
    pub const RUNTIME_GLOBAL: &str = "runtime:global";

    /// Keyboard input events
    pub const KEYBOARD: &str = "input:keyboard";

    /// Mouse input events
    pub const MOUSE: &str = "input:mouse";

    /// Window events (resize, focus, close)
    pub const WINDOW: &str = "input:window";

    /// Get topic for specific processor
    pub fn processor(processor_id: &str) -> String {
        format!("processor:{}", processor_id)
    }
}

/// Trait for objects that can receive events
pub trait EventListener: Send {
    fn on_event(&mut self, event: &Event) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    RuntimeGlobal(RuntimeEvent),
    ProcessorEvent {
        processor_id: String,
        event: ProcessorEvent,
    },
    Custom {
        topic: String,
        data: serde_json::Value,
    },
}

impl Event {
    /// Get the appropriate topic string for this event
    ///
    /// This is a helper to determine which topic to publish to.
    /// You can still use custom topic strings directly.
    pub fn topic(&self) -> String {
        match self {
            Event::RuntimeGlobal(runtime_event) => {
                // Route input events to specific topics
                match runtime_event {
                    RuntimeEvent::KeyboardInput { .. } => topics::KEYBOARD.to_string(),
                    RuntimeEvent::MouseInput { .. } => topics::MOUSE.to_string(),
                    RuntimeEvent::WindowEvent { .. } => topics::WINDOW.to_string(),
                    _ => topics::RUNTIME_GLOBAL.to_string(),
                }
            }
            Event::ProcessorEvent { processor_id, .. } => topics::processor(processor_id),
            Event::Custom { topic, .. } => topic.clone(),
        }
    }

    /// Create a keyboard input event
    pub fn keyboard(key: KeyCode, modifiers: Modifiers, state: KeyState) -> Self {
        Event::RuntimeGlobal(RuntimeEvent::KeyboardInput {
            key,
            modifiers,
            state,
        })
    }

    /// Create a mouse input event
    pub fn mouse(button: MouseButton, position: (f64, f64), state: MouseState) -> Self {
        Event::RuntimeGlobal(RuntimeEvent::MouseInput {
            button,
            position,
            state,
        })
    }

    /// Create a window event
    pub fn window(event: WindowEventType) -> Self {
        Event::RuntimeGlobal(RuntimeEvent::WindowEvent { event })
    }

    /// Create a processor event
    pub fn processor(processor_id: impl Into<String>, event: ProcessorEvent) -> Self {
        Event::ProcessorEvent {
            processor_id: processor_id.into(),
            event,
        }
    }

    /// Create a custom event
    pub fn custom(topic: impl Into<String>, data: serde_json::Value) -> Self {
        Event::Custom {
            topic: topic.into(),
            data,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuntimeEvent {
    // ===== Runtime Lifecycle =====
    RuntimeStart,
    RuntimeStop,
    RuntimeShutdown,

    // ===== Input Events =====
    KeyboardInput {
        key: KeyCode,
        modifiers: Modifiers,
        state: KeyState,
    },
    MouseInput {
        button: MouseButton,
        position: (f64, f64),
        state: MouseState,
    },
    WindowEvent {
        event: WindowEventType,
    },

    // ===== Runtime Errors =====
    RuntimeError {
        error: String,
    },

    // ===== Processor Registry Events =====
    ProcessorAdded {
        processor_id: String,
        processor_type: String,
    },
    ProcessorRemoved {
        processor_id: String,
    },

    // ===== Connection Lifecycle Events =====
    ConnectionCreated {
        connection_id: String,
        from_port: String, // "processor_id.port_name"
        to_port: String,
    },
    ConnectionRemoved {
        connection_id: String,
        from_port: String,
        to_port: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortType {
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProcessorEvent {
    // ===== State Control Commands =====
    Start,
    Stop,
    Pause,
    Resume,

    // ===== Status Events =====
    Started,
    Stopped,
    Paused,
    Resumed,
    Error(String),
    StateChanged {
        old_state: ProcessorState,
        new_state: ProcessorState,
    },

    // ===== Connection Lifecycle Events =====
    WillConnect {
        connection_id: String,
        port_name: String,
        port_type: PortType,
    },
    Connected {
        connection_id: String,
        port_name: String,
        port_type: PortType,
    },
    WillDisconnect {
        connection_id: String,
        port_name: String,
        port_type: PortType,
    },
    Disconnected {
        connection_id: String,
        port_name: String,
        port_type: PortType,
    },

    // ===== Generic Commands =====
    SetParameter {
        name: String,
        value: serde_json::Value,
    },

    // ===== Custom Processor Commands =====
    Custom {
        command: String,
        args: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessorState {
    Idle,    // Setup complete, not processing
    Running, // Actively processing
    Paused,  // Paused (resources still allocated)
    Error,   // Error state
}

// ===== Input Types =====

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyCode {
    // Letters
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,

    // Numbers
    Key0,
    Key1,
    Key2,
    Key3,
    Key4,
    Key5,
    Key6,
    Key7,
    Key8,
    Key9,

    // Modifier keys (these are actual key presses)
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    MetaLeft,  // Command on macOS, Windows key on Windows
    MetaRight, // Right Command/Windows key

    // Common keys
    Space,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,

    // Arrow keys
    Left,
    Right,
    Up,
    Down,

    // Navigation
    Home,
    End,
    PageUp,
    PageDown,

    // Function keys
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,

    Unknown,
}

/// Modifier key state
///
/// Tracks which modifier keys are currently held down during a keyboard event.
/// This is **separate** from the actual key press - pressing Shift triggers TWO events:
///
/// 1. KeyboardInput { key: KeyCode::ShiftLeft, modifiers: { shift: false }, state: Pressed }
/// 2. KeyboardInput { key: KeyCode::A, modifiers: { shift: true }, state: Pressed }
///
/// This matches web behavior where modifiers are both keys AND state.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool, // Command on macOS, Windows key on Windows
}

impl Modifiers {
    /// Check if any modifier is active
    pub fn any(&self) -> bool {
        self.shift || self.ctrl || self.alt || self.meta
    }

    /// Check if only shift is active
    pub fn only_shift(&self) -> bool {
        self.shift && !self.ctrl && !self.alt && !self.meta
    }

    /// Check if only ctrl is active
    pub fn only_ctrl(&self) -> bool {
        !self.shift && self.ctrl && !self.alt && !self.meta
    }

    /// Check if only alt is active
    pub fn only_alt(&self) -> bool {
        !self.shift && !self.ctrl && self.alt && !self.meta
    }

    /// Check if only meta is active
    pub fn only_meta(&self) -> bool {
        !self.shift && !self.ctrl && !self.alt && self.meta
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyState {
    Pressed,
    Released,
    Held,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowEventType {
    Resized { width: u32, height: u32 },
    Closed,
    Focused,
    Unfocused,
    Moved { x: i32, y: i32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::pubsub::event_bus::EventBus;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Test listener that counts events and tracks received events
    struct TestListener {
        count: Arc<AtomicUsize>,
        last_event: Option<Event>,
    }

    impl TestListener {
        fn new() -> Self {
            Self {
                count: Arc::new(AtomicUsize::new(0)),
                last_event: None,
            }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    impl EventListener for TestListener {
        fn on_event(&mut self, event: &Event) -> crate::core::error::Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.last_event = Some(event.clone());
            Ok(())
        }
    }

    #[test]
    fn test_keyboard_event_routing() {
        let bus = EventBus::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to keyboard topic
        bus.subscribe(topics::KEYBOARD, listener);

        // Create keyboard event
        let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);

        // Verify topic routing
        assert_eq!(event.topic(), topics::KEYBOARD);

        // Publish using the event's topic
        bus.publish(&event.topic(), &event);

        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_mouse_event_routing() {
        let bus = EventBus::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to mouse topic
        bus.subscribe(topics::MOUSE, listener);

        // Create mouse event
        let event = Event::mouse(MouseButton::Left, (100.0, 200.0), MouseState::Pressed);

        // Verify topic routing
        assert_eq!(event.topic(), topics::MOUSE);

        // Publish using the event's topic
        bus.publish(&event.topic(), &event);

        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_window_event_routing() {
        let bus = EventBus::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to window topic
        bus.subscribe(topics::WINDOW, listener);

        // Create window event
        let event = Event::window(WindowEventType::Resized {
            width: 1920,
            height: 1080,
        });

        // Verify topic routing
        assert_eq!(event.topic(), topics::WINDOW);

        // Publish using the event's topic
        bus.publish(&event.topic(), &event);

        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_processor_event_routing() {
        let bus = EventBus::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        let processor_id = "audio-mixer";
        let topic = topics::processor(processor_id);

        // Subscribe to processor-specific topic
        bus.subscribe(&topic, listener);

        // Create processor event
        let event = Event::processor(processor_id, ProcessorEvent::Started);

        // Verify topic routing
        assert_eq!(event.topic(), topic);

        // Publish using the event's topic
        bus.publish(&event.topic(), &event);

        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_runtime_global_event_routing() {
        let bus = EventBus::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to runtime global topic
        bus.subscribe(topics::RUNTIME_GLOBAL, listener);

        // Create runtime event (non-input)
        let event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStart);

        // Verify topic routing
        assert_eq!(event.topic(), topics::RUNTIME_GLOBAL);

        // Publish using the event's topic
        bus.publish(&event.topic(), &event);

        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_custom_event_routing() {
        let bus = EventBus::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        let custom_topic = "game:player-scored";

        // Subscribe to custom topic
        bus.subscribe(custom_topic, listener);

        // Create custom event
        let event = Event::custom(
            custom_topic,
            serde_json::json!({"player": "Alice", "points": 100}),
        );

        // Verify topic routing
        assert_eq!(event.topic(), custom_topic);

        // Publish using the event's topic
        bus.publish(&event.topic(), &event);

        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_input_events_routed_to_specific_topics() {
        let bus = EventBus::new();

        let keyboard_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let mouse_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let window_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let runtime_listener_concrete = Arc::new(Mutex::new(TestListener::new()));

        let keyboard_listener: Arc<Mutex<dyn EventListener>> = keyboard_listener_concrete.clone();
        let mouse_listener: Arc<Mutex<dyn EventListener>> = mouse_listener_concrete.clone();
        let window_listener: Arc<Mutex<dyn EventListener>> = window_listener_concrete.clone();
        let runtime_listener: Arc<Mutex<dyn EventListener>> = runtime_listener_concrete.clone();

        // Subscribe to all topics
        bus.subscribe(topics::KEYBOARD, keyboard_listener);
        bus.subscribe(topics::MOUSE, mouse_listener);
        bus.subscribe(topics::WINDOW, window_listener);
        bus.subscribe(topics::RUNTIME_GLOBAL, runtime_listener);

        // Create keyboard input event (RuntimeEvent variant but routed to KEYBOARD)
        let kb_event = Event::RuntimeGlobal(RuntimeEvent::KeyboardInput {
            key: KeyCode::Space,
            modifiers: Modifiers::default(),
            state: KeyState::Pressed,
        });
        assert_eq!(kb_event.topic(), topics::KEYBOARD);
        bus.publish(&kb_event.topic(), &kb_event);

        // Create mouse input event (RuntimeEvent variant but routed to MOUSE)
        let mouse_event = Event::RuntimeGlobal(RuntimeEvent::MouseInput {
            button: MouseButton::Right,
            position: (50.0, 75.0),
            state: MouseState::Released,
        });
        assert_eq!(mouse_event.topic(), topics::MOUSE);
        bus.publish(&mouse_event.topic(), &mouse_event);

        // Create window event (RuntimeEvent variant but routed to WINDOW)
        let window_event = Event::RuntimeGlobal(RuntimeEvent::WindowEvent {
            event: WindowEventType::Focused,
        });
        assert_eq!(window_event.topic(), topics::WINDOW);
        bus.publish(&window_event.topic(), &window_event);

        // Create non-input runtime event (stays on RUNTIME_GLOBAL)
        let runtime_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStop);
        assert_eq!(runtime_event.topic(), topics::RUNTIME_GLOBAL);
        bus.publish(&runtime_event.topic(), &runtime_event);

        // Verify routing - each listener only received its specific events
        assert_eq!(keyboard_listener_concrete.lock().count(), 1);
        assert_eq!(mouse_listener_concrete.lock().count(), 1);
        assert_eq!(window_listener_concrete.lock().count(), 1);
        assert_eq!(runtime_listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_modifier_keys_as_regular_keys() {
        let bus = EventBus::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        bus.subscribe(topics::KEYBOARD, listener);

        // Test pressing shift key itself
        let shift_event = Event::keyboard(
            KeyCode::ShiftLeft,
            Modifiers {
                shift: false,
                ctrl: false,
                alt: false,
                meta: false,
            },
            KeyState::Pressed,
        );
        bus.publish(&shift_event.topic(), &shift_event);

        // Test pressing a key WITH shift held
        let a_with_shift = Event::keyboard(
            KeyCode::A,
            Modifiers {
                shift: true,
                ctrl: false,
                alt: false,
                meta: false,
            },
            KeyState::Pressed,
        );
        bus.publish(&a_with_shift.topic(), &a_with_shift);

        // Both events should be received
        assert_eq!(listener_concrete.lock().count(), 2);
    }

    #[test]
    fn test_modifiers_helper_methods() {
        // Test any()
        let no_mods = Modifiers::default();
        assert!(!no_mods.any());

        let with_shift = Modifiers {
            shift: true,
            ctrl: false,
            alt: false,
            meta: false,
        };
        assert!(with_shift.any());

        // Test only_shift()
        assert!(with_shift.only_shift());
        assert!(!no_mods.only_shift());

        let shift_and_ctrl = Modifiers {
            shift: true,
            ctrl: true,
            alt: false,
            meta: false,
        };
        assert!(!shift_and_ctrl.only_shift());

        // Test only_ctrl()
        let with_ctrl = Modifiers {
            shift: false,
            ctrl: true,
            alt: false,
            meta: false,
        };
        assert!(with_ctrl.only_ctrl());

        // Test only_alt()
        let with_alt = Modifiers {
            shift: false,
            ctrl: false,
            alt: true,
            meta: false,
        };
        assert!(with_alt.only_alt());

        // Test only_meta()
        let with_meta = Modifiers {
            shift: false,
            ctrl: false,
            alt: false,
            meta: true,
        };
        assert!(with_meta.only_meta());
    }

    #[test]
    fn test_multiple_processors_isolated_topics() {
        let bus = EventBus::new();

        let audio_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let video_listener_concrete = Arc::new(Mutex::new(TestListener::new()));

        let audio_listener: Arc<Mutex<dyn EventListener>> = audio_listener_concrete.clone();
        let video_listener: Arc<Mutex<dyn EventListener>> = video_listener_concrete.clone();

        // Subscribe to different processor topics
        bus.subscribe(&topics::processor("audio-mixer"), audio_listener);
        bus.subscribe(&topics::processor("video-filter"), video_listener);

        // Publish to audio processor
        let audio_event = Event::processor("audio-mixer", ProcessorEvent::Paused);
        bus.publish(&audio_event.topic(), &audio_event);

        // Publish to video processor
        let video_event = Event::processor("video-filter", ProcessorEvent::Resumed);
        bus.publish(&video_event.topic(), &video_event);

        // Each listener only received its own processor's events
        assert_eq!(audio_listener_concrete.lock().count(), 1);
        assert_eq!(video_listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_event_helper_constructors() {
        // Test keyboard() helper
        let kb = Event::keyboard(KeyCode::Enter, Modifiers::default(), KeyState::Pressed);
        assert!(matches!(
            kb,
            Event::RuntimeGlobal(RuntimeEvent::KeyboardInput { .. })
        ));
        assert_eq!(kb.topic(), topics::KEYBOARD);

        // Test mouse() helper
        let mouse = Event::mouse(MouseButton::Middle, (0.0, 0.0), MouseState::Pressed);
        assert!(matches!(
            mouse,
            Event::RuntimeGlobal(RuntimeEvent::MouseInput { .. })
        ));
        assert_eq!(mouse.topic(), topics::MOUSE);

        // Test window() helper
        let window = Event::window(WindowEventType::Closed);
        assert!(matches!(
            window,
            Event::RuntimeGlobal(RuntimeEvent::WindowEvent { .. })
        ));
        assert_eq!(window.topic(), topics::WINDOW);

        // Test processor() helper
        let proc = Event::processor("test", ProcessorEvent::Started);
        assert!(matches!(proc, Event::ProcessorEvent { .. }));
        assert_eq!(proc.topic(), topics::processor("test"));

        // Test custom() helper
        let custom = Event::custom("my-topic", serde_json::json!({"key": "value"}));
        assert!(matches!(custom, Event::Custom { .. }));
        assert_eq!(custom.topic(), "my-topic");
    }
}

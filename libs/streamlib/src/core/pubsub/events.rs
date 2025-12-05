// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
    /// Emitted when runtime is about to start
    RuntimeStarting,
    /// Emitted when runtime has started successfully
    RuntimeStarted,
    /// Emitted when runtime failed to start
    RuntimeStartFailed {
        error: String,
    },
    /// Emitted when runtime is about to stop
    RuntimeStopping,
    /// Emitted when runtime has stopped successfully
    RuntimeStopped,
    /// Emitted when runtime failed to stop cleanly
    RuntimeStopFailed {
        error: String,
    },
    /// Emitted when runtime is about to pause
    RuntimePausing,
    /// Emitted when runtime has paused successfully
    RuntimePaused,
    /// Emitted when runtime failed to pause
    RuntimePauseFailed {
        error: String,
    },
    /// Emitted when runtime is about to resume
    RuntimeResuming,
    /// Emitted when runtime has resumed successfully
    RuntimeResumed,
    /// Emitted when runtime failed to resume
    RuntimeResumeFailed {
        error: String,
    },
    /// Emitted when shutdown is requested (e.g., Ctrl+C, Cmd+Q)
    RuntimeShutdown,

    // Legacy variants (kept for compatibility)
    #[doc(hidden)]
    RuntimeStart,
    #[doc(hidden)]
    RuntimeStop,

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

    // ===== Runtime Processor Events =====
    // Emitted by Runtime when user adds/removes processors
    /// Emitted when runtime will add a processor to the graph
    RuntimeWillAddProcessor {
        processor_id: String,
        processor_type: String,
    },
    /// Emitted when runtime did add a processor to the graph
    RuntimeDidAddProcessor {
        processor_id: String,
        processor_type: String,
    },
    /// Emitted when runtime will remove a processor from the graph
    RuntimeWillRemoveProcessor {
        processor_id: String,
    },
    /// Emitted when runtime did remove a processor from the graph
    RuntimeDidRemoveProcessor {
        processor_id: String,
    },

    // ===== Runtime Link Events =====
    // Emitted by Runtime when user connects/disconnects ports
    /// Emitted when runtime will connect two ports
    RuntimeWillConnect {
        from_processor: String,
        from_port: String,
        to_processor: String,
        to_port: String,
    },
    /// Emitted when runtime did connect two ports
    RuntimeDidConnect {
        link_id: String,
        from_port: String,
        to_port: String,
    },
    /// Emitted when runtime will disconnect a link
    RuntimeWillDisconnect {
        link_id: String,
        from_port: String,
        to_port: String,
    },
    /// Emitted when runtime did disconnect a link
    RuntimeDidDisconnect {
        link_id: String,
        from_port: String,
        to_port: String,
    },

    // ===== Processor State Events =====
    /// Emitted when a processor's configuration is updated
    ProcessorConfigDidChange {
        processor_id: String,
    },
    /// Emitted when a processor's state changes (started, stopped, paused, etc.)
    ProcessorStateDidChange {
        processor_id: String,
        old_state: ProcessorState,
        new_state: ProcessorState,
    },

    // ===== Compiler Events =====
    // Emitted by Compiler during graph compilation
    /// Emitted when compiler will compile the graph
    CompilerWillCompile,
    /// Emitted when compiler did compile the graph successfully
    CompilerDidCompile,
    /// Emitted when compiler failed to compile the graph
    CompilerDidFail {
        error: String,
    },
    /// Emitted when compiler will create a processor instance
    CompilerWillCreateProcessor {
        processor_id: String,
        processor_type: String,
    },
    /// Emitted when compiler did create a processor instance
    CompilerDidCreateProcessor {
        processor_id: String,
        processor_type: String,
    },
    /// Emitted when compiler will destroy a processor instance
    CompilerWillDestroyProcessor {
        processor_id: String,
    },
    /// Emitted when compiler did destroy a processor instance
    CompilerDidDestroyProcessor {
        processor_id: String,
    },
    /// Emitted when compiler will wire a link (create ring buffer)
    CompilerWillWireLink {
        link_id: String,
        from_port: String,
        to_port: String,
    },
    /// Emitted when compiler did wire a link
    CompilerDidWireLink {
        link_id: String,
        from_port: String,
        to_port: String,
    },
    /// Emitted when compiler will unwire a link
    CompilerWillUnwireLink {
        link_id: String,
        from_port: String,
        to_port: String,
    },
    /// Emitted when compiler did unwire a link
    CompilerDidUnwireLink {
        link_id: String,
        from_port: String,
        to_port: String,
    },

    // ===== Graph Events =====
    // Emitted by Graph when topology changes
    /// Emitted when graph topology changed (nodes or edges added/removed)
    GraphTopologyDidChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkPortDirection {
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

    // ===== Link Lifecycle Events =====
    WillLink {
        link_id: String,
        port_name: String,
        port_direction: LinkPortDirection,
    },
    Linked {
        link_id: String,
        port_name: String,
        port_direction: LinkPortDirection,
    },
    WillUnlink {
        link_id: String,
        port_name: String,
        port_direction: LinkPortDirection,
    },
    Unlinked {
        link_id: String,
        port_name: String,
        port_direction: LinkPortDirection,
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

// Re-export ProcessorState from the canonical location
pub use crate::core::processors::ProcessorState;

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
    use crate::core::pubsub::PubSub;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Wait for rayon thread pool to complete pending tasks
    fn wait_for_rayon() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

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
        let pubsub = PubSub::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to keyboard topic
        pubsub.subscribe(topics::KEYBOARD, listener);

        // Create keyboard event
        let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);

        // Verify topic routing
        assert_eq!(event.topic(), topics::KEYBOARD);

        // Publish using the event's topic
        pubsub.publish(&event.topic(), &event);

        wait_for_rayon();
        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_mouse_event_routing() {
        let pubsub = PubSub::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to mouse topic
        pubsub.subscribe(topics::MOUSE, listener);

        // Create mouse event
        let event = Event::mouse(MouseButton::Left, (100.0, 200.0), MouseState::Pressed);

        // Verify topic routing
        assert_eq!(event.topic(), topics::MOUSE);

        // Publish using the event's topic
        pubsub.publish(&event.topic(), &event);

        wait_for_rayon();
        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_window_event_routing() {
        let pubsub = PubSub::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to window topic
        pubsub.subscribe(topics::WINDOW, listener);

        // Create window event
        let event = Event::window(WindowEventType::Resized {
            width: 1920,
            height: 1080,
        });

        // Verify topic routing
        assert_eq!(event.topic(), topics::WINDOW);

        // Publish using the event's topic
        pubsub.publish(&event.topic(), &event);

        wait_for_rayon();
        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_processor_event_routing() {
        let pubsub = PubSub::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        let processor_id = "audio-mixer";
        let topic = topics::processor(processor_id);

        // Subscribe to processor-specific topic
        pubsub.subscribe(&topic, listener);

        // Create processor event
        let event = Event::processor(processor_id, ProcessorEvent::Started);

        // Verify topic routing
        assert_eq!(event.topic(), topic);

        // Publish using the event's topic
        pubsub.publish(&event.topic(), &event);

        wait_for_rayon();
        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_runtime_global_event_routing() {
        let pubsub = PubSub::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        // Subscribe to runtime global topic
        pubsub.subscribe(topics::RUNTIME_GLOBAL, listener);

        // Create runtime event (non-input)
        let event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStart);

        // Verify topic routing
        assert_eq!(event.topic(), topics::RUNTIME_GLOBAL);

        // Publish using the event's topic
        pubsub.publish(&event.topic(), &event);

        wait_for_rayon();
        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_custom_event_routing() {
        let pubsub = PubSub::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        let custom_topic = "game:player-scored";

        // Subscribe to custom topic
        pubsub.subscribe(custom_topic, listener);

        // Create custom event
        let event = Event::custom(
            custom_topic,
            serde_json::json!({"player": "Alice", "points": 100}),
        );

        // Verify topic routing
        assert_eq!(event.topic(), custom_topic);

        // Publish using the event's topic
        pubsub.publish(&event.topic(), &event);

        wait_for_rayon();
        // Verify listener received it
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_input_events_routed_to_specific_topics() {
        let pubsub = PubSub::new();

        let keyboard_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let mouse_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let window_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let runtime_listener_concrete = Arc::new(Mutex::new(TestListener::new()));

        let keyboard_listener: Arc<Mutex<dyn EventListener>> = keyboard_listener_concrete.clone();
        let mouse_listener: Arc<Mutex<dyn EventListener>> = mouse_listener_concrete.clone();
        let window_listener: Arc<Mutex<dyn EventListener>> = window_listener_concrete.clone();
        let runtime_listener: Arc<Mutex<dyn EventListener>> = runtime_listener_concrete.clone();

        // Subscribe to all topics
        pubsub.subscribe(topics::KEYBOARD, keyboard_listener);
        pubsub.subscribe(topics::MOUSE, mouse_listener);
        pubsub.subscribe(topics::WINDOW, window_listener);
        pubsub.subscribe(topics::RUNTIME_GLOBAL, runtime_listener);

        // Create keyboard input event (RuntimeEvent variant but routed to KEYBOARD)
        let kb_event = Event::RuntimeGlobal(RuntimeEvent::KeyboardInput {
            key: KeyCode::Space,
            modifiers: Modifiers::default(),
            state: KeyState::Pressed,
        });
        assert_eq!(kb_event.topic(), topics::KEYBOARD);
        pubsub.publish(&kb_event.topic(), &kb_event);

        // Create mouse input event (RuntimeEvent variant but routed to MOUSE)
        let mouse_event = Event::RuntimeGlobal(RuntimeEvent::MouseInput {
            button: MouseButton::Right,
            position: (50.0, 75.0),
            state: MouseState::Released,
        });
        assert_eq!(mouse_event.topic(), topics::MOUSE);
        pubsub.publish(&mouse_event.topic(), &mouse_event);

        // Create window event (RuntimeEvent variant but routed to WINDOW)
        let window_event = Event::RuntimeGlobal(RuntimeEvent::WindowEvent {
            event: WindowEventType::Focused,
        });
        assert_eq!(window_event.topic(), topics::WINDOW);
        pubsub.publish(&window_event.topic(), &window_event);

        // Create non-input runtime event (stays on RUNTIME_GLOBAL)
        let runtime_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStop);
        assert_eq!(runtime_event.topic(), topics::RUNTIME_GLOBAL);
        pubsub.publish(&runtime_event.topic(), &runtime_event);

        wait_for_rayon();
        // Verify routing - each listener only received its specific events
        assert_eq!(keyboard_listener_concrete.lock().count(), 1);
        assert_eq!(mouse_listener_concrete.lock().count(), 1);
        assert_eq!(window_listener_concrete.lock().count(), 1);
        assert_eq!(runtime_listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_modifier_keys_as_regular_keys() {
        let pubsub = PubSub::new();
        let listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();

        pubsub.subscribe(topics::KEYBOARD, listener);

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
        pubsub.publish(&shift_event.topic(), &shift_event);

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
        pubsub.publish(&a_with_shift.topic(), &a_with_shift);

        wait_for_rayon();
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
        let pubsub = PubSub::new();

        let audio_listener_concrete = Arc::new(Mutex::new(TestListener::new()));
        let video_listener_concrete = Arc::new(Mutex::new(TestListener::new()));

        let audio_listener: Arc<Mutex<dyn EventListener>> = audio_listener_concrete.clone();
        let video_listener: Arc<Mutex<dyn EventListener>> = video_listener_concrete.clone();

        // Subscribe to different processor topics
        pubsub.subscribe(&topics::processor("audio-mixer"), audio_listener);
        pubsub.subscribe(&topics::processor("video-filter"), video_listener);

        // Publish to audio processor
        let audio_event = Event::processor("audio-mixer", ProcessorEvent::Paused);
        pubsub.publish(&audio_event.topic(), &audio_event);

        // Publish to video processor
        let video_event = Event::processor("video-filter", ProcessorEvent::Resumed);
        pubsub.publish(&video_event.topic(), &video_event);

        wait_for_rayon();
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

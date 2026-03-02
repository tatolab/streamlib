// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::Result;
use crate::core::graph::ProcessorUniqueId;
use serde::{Deserialize, Serialize};

/// Common topic constants for system events
pub mod topics {
    /// Wildcard topic - receives ALL events from any topic
    pub const ALL: &str = "*";

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
        processor_id: ProcessorUniqueId,
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
            Event::ProcessorEvent { processor_id, .. } => topics::processor(processor_id.as_str()),
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
    pub fn processor(processor_id: impl Into<ProcessorUniqueId>, event: ProcessorEvent) -> Self {
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

    /// Returns a readable log name like "RuntimeGlobal.GraphDidChange"
    pub fn log_name(&self) -> String {
        match self {
            Event::RuntimeGlobal(inner) => {
                let variant = format!("{:?}", inner);
                // Extract just the variant name (before any { or ()
                let variant_name = variant.split(['{', '(']).next().unwrap_or(&variant).trim();
                format!("RuntimeGlobal.{}", variant_name)
            }
            Event::ProcessorEvent {
                processor_id,
                event,
            } => {
                let variant = format!("{:?}", event);
                let variant_name = variant.split(['{', '(']).next().unwrap_or(&variant).trim();
                format!("ProcessorEvent.{} ({})", variant_name, processor_id)
            }
            Event::Custom { topic, .. } => {
                format!("Custom.{}", topic)
            }
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
        processor_id: ProcessorUniqueId,
    },
    /// Emitted when runtime did add a processor to the graph
    RuntimeDidAddProcessor {
        processor_id: ProcessorUniqueId,
    },
    /// Emitted when runtime will remove a processor from the graph
    RuntimeWillRemoveProcessor {
        processor_id: ProcessorUniqueId,
    },
    /// Emitted when runtime did remove a processor from the graph
    RuntimeDidRemoveProcessor {
        processor_id: ProcessorUniqueId,
    },

    // ===== Runtime Link Events =====
    // Emitted by Runtime when user connects/disconnects ports
    /// Emitted when runtime will connect two ports
    RuntimeWillConnect {
        from_processor: ProcessorUniqueId,
        from_port: String,
        to_processor: ProcessorUniqueId,
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
        processor_id: ProcessorUniqueId,
    },
    /// Emitted when a processor's state changes (started, stopped, paused, etc.)
    ProcessorStateDidChange {
        processor_id: ProcessorUniqueId,
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
        processor_id: ProcessorUniqueId,
        processor_type: String,
    },
    /// Emitted when compiler did create a processor instance
    CompilerDidCreateProcessor {
        processor_id: ProcessorUniqueId,
        processor_type: String,
    },
    /// Emitted when compiler will destroy a processor instance
    CompilerWillDestroyProcessor {
        processor_id: ProcessorUniqueId,
    },
    /// Emitted when compiler did destroy a processor instance
    CompilerDidDestroyProcessor {
        processor_id: ProcessorUniqueId,
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
    // Emitted when graph structure changes (nodes or edges added/removed)
    /// Emitted before graph changes
    GraphWillChange,
    /// Emitted after graph changed
    GraphDidChange,

    // ===== Factory/Registration Events =====
    /// Emitted when a new processor type is registered with the factory
    RuntimeDidRegisterProcessorType {
        processor_type: String,
    },
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

    #[test]
    fn test_event_topic_routing() {
        // RuntimeEvent non-input → RUNTIME_GLOBAL
        let event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted);
        assert_eq!(event.topic(), topics::RUNTIME_GLOBAL);

        // Keyboard input → KEYBOARD
        let kb = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
        assert_eq!(kb.topic(), topics::KEYBOARD);

        // Mouse input → MOUSE
        let mouse = Event::mouse(MouseButton::Left, (100.0, 200.0), MouseState::Pressed);
        assert_eq!(mouse.topic(), topics::MOUSE);

        // Window → WINDOW
        let window = Event::window(WindowEventType::Closed);
        assert_eq!(window.topic(), topics::WINDOW);

        // Processor → processor:{id}
        let proc = Event::processor("audio-mixer", ProcessorEvent::Started);
        assert_eq!(proc.topic(), topics::processor("audio-mixer"));

        // Custom → custom topic string
        let custom = Event::custom("my-topic", serde_json::json!({"key": "value"}));
        assert_eq!(custom.topic(), "my-topic");
    }

    #[test]
    fn test_event_helper_constructors() {
        let kb = Event::keyboard(KeyCode::Enter, Modifiers::default(), KeyState::Pressed);
        assert!(matches!(
            kb,
            Event::RuntimeGlobal(RuntimeEvent::KeyboardInput { .. })
        ));

        let mouse = Event::mouse(MouseButton::Middle, (0.0, 0.0), MouseState::Pressed);
        assert!(matches!(
            mouse,
            Event::RuntimeGlobal(RuntimeEvent::MouseInput { .. })
        ));

        let window = Event::window(WindowEventType::Closed);
        assert!(matches!(
            window,
            Event::RuntimeGlobal(RuntimeEvent::WindowEvent { .. })
        ));

        let proc = Event::processor("test", ProcessorEvent::Started);
        assert!(matches!(proc, Event::ProcessorEvent { .. }));

        let custom = Event::custom("my-topic", serde_json::json!({"key": "value"}));
        assert!(matches!(custom, Event::Custom { .. }));
    }

    #[test]
    fn test_modifiers_helper_methods() {
        let no_mods = Modifiers::default();
        assert!(!no_mods.any());

        let with_shift = Modifiers {
            shift: true,
            ctrl: false,
            alt: false,
            meta: false,
        };
        assert!(with_shift.any());
        assert!(with_shift.only_shift());
        assert!(!no_mods.only_shift());

        let shift_and_ctrl = Modifiers {
            shift: true,
            ctrl: true,
            alt: false,
            meta: false,
        };
        assert!(!shift_and_ctrl.only_shift());

        let with_ctrl = Modifiers {
            shift: false,
            ctrl: true,
            alt: false,
            meta: false,
        };
        assert!(with_ctrl.only_ctrl());

        let with_alt = Modifiers {
            shift: false,
            ctrl: false,
            alt: true,
            meta: false,
        };
        assert!(with_alt.only_alt());

        let with_meta = Modifiers {
            shift: false,
            ctrl: false,
            alt: false,
            meta: true,
        };
        assert!(with_meta.only_meta());
    }

    #[test]
    fn test_event_serialization_roundtrip() {
        // Verify events can be serialized/deserialized via MessagePack
        // (critical for iceoryx2 transport)
        let events = vec![
            Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted),
            Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
            Event::processor("test-proc", ProcessorEvent::Started),
            Event::custom("my-topic", serde_json::json!({"key": "value"})),
            Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed),
        ];

        for event in events {
            let bytes = rmp_serde::to_vec_named(&event).unwrap();
            let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
            // Verify round-trip preserves the event type
            assert_eq!(event.topic(), deserialized.topic());
            assert_eq!(event.log_name(), deserialized.log_name());
        }
    }
}

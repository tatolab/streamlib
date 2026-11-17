// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::Result;
use crate::core::graph::{InputLinkPortRef, NodeRunId, OutputLinkPortRef};
use crate::core::runtime::LoadedStreamTag;
use serde::{Deserialize, Serialize};

use super::PUBSUB;

/// Common topic constants for system events
pub mod topics {
    use super::LoadedStreamIdentity;

    /// Wildcard topic - receives ALL events from any topic
    pub const ALL: &str = "*";

    /// The topic one loaded stream's events publish on:
    /// `stream:<runtime id>/<stream name>#<stream tag>`.
    pub fn loaded_stream(stream: &LoadedStreamIdentity) -> String {
        format!("stream:{stream}")
    }

    /// Keyboard input events
    pub const KEYBOARD: &str = "input:keyboard";

    /// Mouse input events
    pub const MOUSE: &str = "input:mouse";

    /// Window events (resize, focus, close)
    pub const WINDOW: &str = "input:window";

    pub fn node(node_run_id: &str) -> String {
        format!("node:{}", node_run_id)
    }
}

/// A receiver of the events published on the topic it is subscribed to.
pub trait EventListener: Send {
    /// Hear one event.
    fn on_event(&mut self, event: &Event) -> Result<()>;

    /// Hear that the bus dropped `lost_event_count` events for this listener
    /// at its full queue, each published after the event it last heard.
    fn on_events_lost(&mut self, lost_event_count: u64) -> Result<()>;
}

/// The loaded stream an event belongs to: the runtime that loaded it, its
/// stream id, and its process-unique tag, which tells apart two streams loaded
/// under one id — one abandoned by its watchdog and the one loaded after it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LoadedStreamIdentity {
    /// The id of the runtime the stream is loaded in.
    pub runtime_id: String,
    /// The stream's id.
    pub stream_id: String,
    /// The stream's process-unique tag, never reused in one process.
    pub stream_tag: LoadedStreamTag,
}

impl LoadedStreamIdentity {
    /// Publish `event` on this stream's topic.
    pub fn publish_on_this_streams_topic(&self, event: RuntimeEvent) {
        let event = Event::OfALoadedStream {
            stream: self.clone(),
            event,
        };
        PUBSUB.publish(&event.topic(), &event);
    }
}

/// `<runtime id>/<stream id>#<stream tag>`, unique in the process.
impl std::fmt::Display for LoadedStreamIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}/{}#{}",
            self.runtime_id, self.stream_id, self.stream_tag
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// An event of one loaded stream, published on its topic.
    OfALoadedStream {
        stream: LoadedStreamIdentity,
        event: RuntimeEvent,
    },
    /// Keyboard, mouse and window input, published on the input topics.
    KeyboardMouseOrWindowInput(KeyboardMouseOrWindowInputEvent),
    NodeEvent {
        node_run_id: NodeRunId,
        event: NodeEvent,
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
            Event::OfALoadedStream { stream, .. } => topics::loaded_stream(stream),
            Event::KeyboardMouseOrWindowInput(input_event) => match input_event {
                KeyboardMouseOrWindowInputEvent::KeyboardInput { .. } => {
                    topics::KEYBOARD.to_string()
                }
                KeyboardMouseOrWindowInputEvent::MouseInput { .. } => topics::MOUSE.to_string(),
                KeyboardMouseOrWindowInputEvent::WindowEvent { .. } => topics::WINDOW.to_string(),
            },
            Event::NodeEvent { node_run_id, .. } => topics::node(node_run_id.as_str()),
            Event::Custom { topic, .. } => topic.clone(),
        }
    }

    /// Create a keyboard input event
    pub fn keyboard(key: KeyCode, modifiers: Modifiers, state: KeyState) -> Self {
        Event::KeyboardMouseOrWindowInput(KeyboardMouseOrWindowInputEvent::KeyboardInput {
            key,
            modifiers,
            state,
        })
    }

    /// Create a mouse input event
    pub fn mouse(button: MouseButton, position: (f64, f64), state: MouseState) -> Self {
        Event::KeyboardMouseOrWindowInput(KeyboardMouseOrWindowInputEvent::MouseInput {
            button,
            position,
            state,
        })
    }

    /// Create a window event
    pub fn window(event: WindowEventType) -> Self {
        Event::KeyboardMouseOrWindowInput(KeyboardMouseOrWindowInputEvent::WindowEvent { event })
    }

    pub fn node(node_run_id: impl Into<NodeRunId>, event: NodeEvent) -> Self {
        Event::NodeEvent {
            node_run_id: node_run_id.into(),
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

    /// Returns a readable log name like
    /// `OfALoadedStream.GraphDidChange (<runtime id>/main#1)`.
    pub fn log_name(&self) -> String {
        match self {
            Event::OfALoadedStream { stream, event } => {
                format!("OfALoadedStream.{} ({})", debug_variant_name(event), stream)
            }
            Event::KeyboardMouseOrWindowInput(inner) => {
                format!("KeyboardMouseOrWindowInput.{}", debug_variant_name(inner))
            }
            Event::NodeEvent { node_run_id, event } => {
                format!("NodeEvent.{} ({})", debug_variant_name(event), node_run_id)
            }
            Event::Custom { topic, .. } => {
                format!("Custom.{}", topic)
            }
        }
    }
}

/// The variant name of an enum value's `Debug` rendering, without its fields.
fn debug_variant_name(value: &impl std::fmt::Debug) -> String {
    let rendered = format!("{value:?}");
    rendered
        .split(['{', '('])
        .next()
        .unwrap_or(&rendered)
        .trim()
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Emitted when this stream's shutdown is requested.
    RuntimeShutdown,

    // ===== Runtime Errors =====
    RuntimeError {
        error: String,
    },

    RuntimeWillAddNode {
        node_run_id: NodeRunId,
    },
    RuntimeDidAddNode {
        node_run_id: NodeRunId,
    },
    RuntimeWillRemoveNode {
        node_run_id: NodeRunId,
    },
    RuntimeDidRemoveNode {
        node_run_id: NodeRunId,
    },

    // ===== Runtime Link Events =====
    // Emitted by Runtime when user connects/disconnects ports
    /// Emitted when runtime will connect two ports.
    RuntimeWillConnect {
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    },
    /// Emitted when runtime did connect two ports
    RuntimeDidConnect {
        link_id: String,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
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

    NodeConfigDidChange {
        node_run_id: NodeRunId,
    },
    NodeStateDidChange {
        node_run_id: NodeRunId,
        old_state: NodeState,
        new_state: NodeState,
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
    CompilerWillCreateNode {
        node_run_id: NodeRunId,
        node_type: NodeClassImportPath,
    },
    CompilerDidCreateNode {
        node_run_id: NodeRunId,
        node_type: NodeClassImportPath,
    },
    CompilerWillDestroyNode {
        node_run_id: NodeRunId,
    },
    CompilerDidDestroyNode {
        node_run_id: NodeRunId,
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
    /// The value one input of a node reads while unlinked changed, set live or saved, and no
    /// node, link or port changed with it: `value_read` is what `graph` renders for the input,
    /// `None` for an input that takes no stored value.
    InputValueDidChange {
        node: uuid::Uuid,
        input: String,
        member: Option<String>,
        value_read: Option<crate::core::graph::input_values::InputValueRead>,
    },

    RuntimeDidRegisterNodeType {
        node_type: NodeClassImportPath,
    },
}

/// Keyboard, mouse and window input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum KeyboardMouseOrWindowInputEvent {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkPortDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeEvent {
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
        old_state: NodeState,
        new_state: NodeState,
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

    Custom {
        command: String,
        args: serde_json::Value,
    },
}

pub(super) use crate::core::nodes::NodeState;

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    fn a_loaded_stream_identity() -> LoadedStreamIdentity {
        LoadedStreamIdentity {
            runtime_id: "Rtest".to_string(),
            stream_id: "main".to_string(),
            stream_tag: LoadedStreamTag::numbered_for_a_test(7),
        }
    }

    #[test]
    fn test_event_topic_routing() {
        let stream = a_loaded_stream_identity();
        let event = Event::OfALoadedStream {
            stream: stream.clone(),
            event: RuntimeEvent::RuntimeStarted,
        };
        assert_eq!(event.topic(), topics::loaded_stream(&stream));
        assert_eq!(event.topic(), "stream:Rtest/main#7");

        // Keyboard input → KEYBOARD
        let kb = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
        assert_eq!(kb.topic(), topics::KEYBOARD);

        // Mouse input → MOUSE
        let mouse = Event::mouse(MouseButton::Left, (100.0, 200.0), MouseState::Pressed);
        assert_eq!(mouse.topic(), topics::MOUSE);

        // Window → WINDOW
        let window = Event::window(WindowEventType::Closed);
        assert_eq!(window.topic(), topics::WINDOW);

        let proc = Event::node("audio-mixer", NodeEvent::Started);
        assert_eq!(proc.topic(), topics::node("audio-mixer"));

        // Custom → custom topic string
        let custom = Event::custom("my-topic", serde_json::json!({"key": "value"}));
        assert_eq!(custom.topic(), "my-topic");
    }

    #[test]
    fn test_event_helper_constructors() {
        let kb = Event::keyboard(KeyCode::Enter, Modifiers::default(), KeyState::Pressed);
        assert!(matches!(
            kb,
            Event::KeyboardMouseOrWindowInput(
                KeyboardMouseOrWindowInputEvent::KeyboardInput { .. }
            )
        ));

        let mouse = Event::mouse(MouseButton::Middle, (0.0, 0.0), MouseState::Pressed);
        assert!(matches!(
            mouse,
            Event::KeyboardMouseOrWindowInput(KeyboardMouseOrWindowInputEvent::MouseInput { .. })
        ));

        let window = Event::window(WindowEventType::Closed);
        assert!(matches!(
            window,
            Event::KeyboardMouseOrWindowInput(KeyboardMouseOrWindowInputEvent::WindowEvent { .. })
        ));

        let proc = Event::node("test", NodeEvent::Started);
        assert!(matches!(proc, Event::NodeEvent { .. }));

        let custom = Event::custom("my-topic", serde_json::json!({"key": "value"}));
        assert!(matches!(custom, Event::Custom { .. }));
    }

    #[test]
    fn each_log_name_names_its_variant_without_its_fields() {
        let of_a_loaded_stream = Event::OfALoadedStream {
            stream: a_loaded_stream_identity(),
            event: RuntimeEvent::RuntimeStartFailed {
                error: "no device".to_string(),
            },
        };
        assert_eq!(
            of_a_loaded_stream.log_name(),
            "OfALoadedStream.RuntimeStartFailed (Rtest/main#7)"
        );
        assert_eq!(
            Event::window(WindowEventType::Closed).log_name(),
            "KeyboardMouseOrWindowInput.WindowEvent"
        );
        assert_eq!(
            Event::node("audio-mixer", NodeEvent::Started).log_name(),
            "NodeEvent.Started (audio-mixer)"
        );
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

    /// The JSON the WebSocket event stream and MCP `logs` hand clients reads
    /// back as the same event, discriminator and all.
    #[test]
    fn test_event_serialization_roundtrip() {
        let events = vec![
            Event::OfALoadedStream {
                stream: a_loaded_stream_identity(),
                event: RuntimeEvent::RuntimeStarted,
            },
            Event::OfALoadedStream {
                stream: a_loaded_stream_identity(),
                event: RuntimeEvent::GraphDidChange,
            },
            Event::node("test-proc", NodeEvent::Started),
            Event::custom("my-topic", serde_json::json!({"key": "value"})),
            Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed),
            Event::mouse(MouseButton::Left, (100.5, 200.25), MouseState::Pressed),
            Event::window(WindowEventType::Resized {
                width: 1920,
                height: 1080,
            }),
            Event::OfALoadedStream {
                stream: a_loaded_stream_identity(),
                event: RuntimeEvent::RuntimeWillConnect {
                    from: OutputLinkPortRef::new("Pcam", "video_out"),
                    to: InputLinkPortRef::new("Pdisplay", "video_in"),
                },
            },
        ];

        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let deserialized: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, deserialized, "round-trip mismatch");
        }
    }
}

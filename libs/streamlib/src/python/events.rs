//! Python bindings for the event bus system
//!
//! Exposes the global EVENT_BUS to Python, allowing Python processors
//! to publish events and subscribe to events from the event-driven architecture.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use parking_lot::Mutex as ParkingLotMutex;

use crate::core::pubsub::{
    Event, ProcessorEvent,
    EventListener, EVENT_BUS,
    KeyCode, KeyState, Modifiers, MouseButton, MouseState,
};
use crate::core::pubsub::topics as event_topics;
use crate::core::error::{Result, StreamError};

/// Python wrapper for Event
#[pyclass(name = "Event", module = "streamlib")]
#[derive(Clone)]
pub struct PyEvent {
    inner: Event,
}

impl PyEvent {
    pub fn from_rust(event: Event) -> Self {
        Self { inner: event }
    }

    pub fn into_rust(self) -> Event {
        self.inner
    }
}

#[pymethods]
impl PyEvent {
    /// Get the topic for this event
    #[getter]
    fn topic(&self) -> String {
        self.inner.topic()
    }

    /// Check if this is a RuntimeGlobal event
    #[getter]
    fn is_runtime_global(&self) -> bool {
        matches!(self.inner, Event::RuntimeGlobal(_))
    }

    /// Check if this is a ProcessorEvent
    #[getter]
    fn is_processor_event(&self) -> bool {
        matches!(self.inner, Event::ProcessorEvent { .. })
    }

    /// Check if this is a Custom event
    #[getter]
    fn is_custom(&self) -> bool {
        matches!(self.inner, Event::Custom { .. })
    }

    /// Get processor_id if this is a ProcessorEvent
    #[getter]
    fn processor_id(&self) -> Option<String> {
        match &self.inner {
            Event::ProcessorEvent { processor_id, .. } => Some(processor_id.clone()),
            _ => None,
        }
    }

    /// Get custom data as JSON string if this is a Custom event
    #[getter]
    fn custom_data(&self) -> Option<String> {
        match &self.inner {
            Event::Custom { data, .. } => Some(data.to_string()),
            _ => None,
        }
    }

    /// Create a custom event
    #[staticmethod]
    fn custom(py: Python<'_>, topic: String, data: &Bound<'_, PyAny>) -> PyResult<Self> {
        // Convert Python object to JSON using json module
        let json_module = py.import_bound("json")?;
        let json_dumps = json_module.getattr("dumps")?;
        let json_str: String = json_dumps.call1((data,))?.extract()?;

        let json_value: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("Failed to serialize data: {}", e)
            ))?;

        Ok(Self {
            inner: Event::custom(topic, json_value),
        })
    }

    /// Create a keyboard event
    #[staticmethod]
    #[pyo3(signature = (key, pressed, shift=false, ctrl=false, alt=false, meta=false))]
    fn keyboard(
        key: String,
        pressed: bool,
        shift: bool,
        ctrl: bool,
        alt: bool,
        meta: bool,
    ) -> PyResult<Self> {
        let key_code = parse_key_code(&key)?;
        let modifiers = Modifiers { shift, ctrl, alt, meta };
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };

        Ok(Self {
            inner: Event::keyboard(key_code, modifiers, state),
        })
    }

    /// Create a mouse event
    #[staticmethod]
    fn mouse(button: String, x: f64, y: f64, pressed: bool) -> PyResult<Self> {
        let mouse_button = match button.as_str() {
            "left" => MouseButton::Left,
            "right" => MouseButton::Right,
            "middle" => MouseButton::Middle,
            _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("Invalid mouse button: {}", button)
            )),
        };

        let state = if pressed { MouseState::Pressed } else { MouseState::Released };

        Ok(Self {
            inner: Event::mouse(mouse_button, (x, y), state),
        })
    }

    /// Create a processor event
    #[staticmethod]
    fn processor(processor_id: String, event_type: String) -> PyResult<Self> {
        let proc_event = match event_type.as_str() {
            "start" => ProcessorEvent::Start,
            "stop" => ProcessorEvent::Stop,
            "pause" => ProcessorEvent::Pause,
            "resume" => ProcessorEvent::Resume,
            "started" => ProcessorEvent::Started,
            "stopped" => ProcessorEvent::Stopped,
            "paused" => ProcessorEvent::Paused,
            "resumed" => ProcessorEvent::Resumed,
            _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("Invalid processor event type: {}", event_type)
            )),
        };

        Ok(Self {
            inner: Event::processor(processor_id, proc_event),
        })
    }

    /// String representation
    fn __repr__(&self) -> String {
        format!("Event(topic='{}')", self.topic())
    }
}

/// Helper function to parse key code from string
fn parse_key_code(key: &str) -> PyResult<KeyCode> {
    let code = match key.to_lowercase().as_str() {
        "a" => KeyCode::A,
        "b" => KeyCode::B,
        "c" => KeyCode::C,
        "d" => KeyCode::D,
        "e" => KeyCode::E,
        "f" => KeyCode::F,
        "g" => KeyCode::G,
        "h" => KeyCode::H,
        "i" => KeyCode::I,
        "j" => KeyCode::J,
        "k" => KeyCode::K,
        "l" => KeyCode::L,
        "m" => KeyCode::M,
        "n" => KeyCode::N,
        "o" => KeyCode::O,
        "p" => KeyCode::P,
        "q" => KeyCode::Q,
        "r" => KeyCode::R,
        "s" => KeyCode::S,
        "t" => KeyCode::T,
        "u" => KeyCode::U,
        "v" => KeyCode::V,
        "w" => KeyCode::W,
        "x" => KeyCode::X,
        "y" => KeyCode::Y,
        "z" => KeyCode::Z,
        "space" => KeyCode::Space,
        "enter" => KeyCode::Enter,
        "escape" | "esc" => KeyCode::Escape,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("Unknown key: {}", key)
        )),
    };
    Ok(code)
}

/// Python event listener that wraps a Python callable
struct PyEventListener {
    callback: Py<PyAny>,
}

impl EventListener for PyEventListener {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        eprintln!("[RUST] PyEventListener::on_event called for topic: {}", event.topic());
        tracing::info!("PyEventListener::on_event called for topic: {}", event.topic());
        Python::with_gil(|py| {
            eprintln!("[RUST]   - GIL acquired, calling Python callback");
            tracing::info!("GIL acquired, calling Python callback");
            let py_event = PyEvent::from_rust(event.clone());

            self.callback.call1(py, (py_event,))
                .map_err(|e| {
                    eprintln!("[RUST]   - ERROR: Python callback error: {}", e);
                    tracing::error!("Python callback error: {}", e);
                    StreamError::Runtime(format!("Python event listener error: {}", e))
                })?;

            eprintln!("[RUST]   - Python callback completed successfully");
            tracing::info!("Python callback completed");
            Ok(())
        })
    }
}

/// Python wrapper for the global EventBus
#[pyclass(name = "EventBus", module = "streamlib")]
pub struct PyEventBus {
    /// Keep strong references to listeners so they don't get dropped
    /// The Vec holds the Arc references to keep listeners alive
    _listeners: Arc<ParkingLotMutex<Vec<Arc<ParkingLotMutex<dyn EventListener>>>>>,
}

#[pymethods]
impl PyEventBus {
    #[new]
    fn new() -> Self {
        Self {
            _listeners: Arc::new(ParkingLotMutex::new(Vec::new())),
        }
    }

    /// Subscribe to a topic with a callback
    ///
    /// The callback can be:
    /// - A function: `def callback(event): ...`
    /// - An object with `on_event` method: `class Listener: def on_event(self, event): ...`
    ///
    /// Args:
    ///     topic: Topic string (e.g., "runtime:global", "processor:processor_0")
    ///     callback: Python callable or object with on_event method
    ///
    /// Example:
    ///     ```python
    ///     def on_keyboard(event):
    ///         print(f"Key: {event.topic()}")
    ///
    ///     bus = EventBus()
    ///     bus.subscribe("input:keyboard", on_keyboard)
    ///     ```
    fn subscribe(&self, topic: String, callback: Py<PyAny>) -> PyResult<()> {
        eprintln!("[RUST] PyEventBus::subscribe called for topic: {}", topic);
        tracing::info!("PyEventBus::subscribe called for topic: {}", topic);
        Python::with_gil(|py| {
            let callback_bound = callback.bind(py);

            // Check if it's a callable or has on_event method
            let actual_callback = if callback_bound.is_callable() {
                eprintln!("[RUST]   - Callback is a function");
                tracing::info!("  - Callback is a function");
                callback.clone_ref(py)
            } else if callback_bound.hasattr("on_event")? {
                eprintln!("[RUST]   - Callback is an object with on_event method");
                tracing::info!("  - Callback is an object with on_event method");
                callback_bound.getattr("on_event")?.into()
            } else {
                return Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                    "Listener must be callable or have on_event method"
                ));
            };

            let listener = Arc::new(ParkingLotMutex::new(PyEventListener {
                callback: actual_callback,
            }));

            // Subscribe to event bus
            EVENT_BUS.subscribe(&topic, listener.clone());

            // Keep strong reference to prevent listener from being dropped
            self._listeners.lock().push(listener);

            eprintln!("[RUST]   - Listener registered successfully");
            tracing::info!("  - Listener registered successfully");
            Ok(())
        })
    }

    /// Publish an event to a topic
    ///
    /// Args:
    ///     topic: Topic string
    ///     event: PyEvent to publish
    ///
    /// Example:
    ///     ```python
    ///     bus = EventBus()
    ///     event = Event.custom("my-topic", {"data": 123})
    ///     bus.publish("my-topic", event)
    ///     ```
    fn publish(&self, py: Python<'_>, topic: String, event: PyEvent) -> PyResult<()> {
        eprintln!("[RUST] PyEventBus::publish called for topic: {}", topic);
        tracing::info!("PyEventBus::publish called for topic: {}", topic);
        let rust_event = event.into_rust();

        // Release GIL before publishing to avoid deadlock with rayon threads
        // that need to acquire GIL to call Python callbacks
        py.allow_threads(|| {
            EVENT_BUS.publish(&topic, &rust_event);
        });

        eprintln!("[RUST]   - Event published");
        tracing::info!("  - Event published");
        Ok(())
    }

}

/// Helper function to create processor topic
#[pyfunction]
fn processor_topic(processor_id: String) -> String {
    event_topics::processor(&processor_id)
}

/// Register event-related classes and functions
pub fn register_events(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyEvent>()?;
    m.add_class::<PyEventBus>()?;

    // Add topics submodule
    let topics_module = PyModule::new_bound(m.py(), "topics")?;
    topics_module.add("RUNTIME_GLOBAL", event_topics::RUNTIME_GLOBAL)?;
    topics_module.add("KEYBOARD", event_topics::KEYBOARD)?;
    topics_module.add("MOUSE", event_topics::MOUSE)?;
    topics_module.add("WINDOW", event_topics::WINDOW)?;
    topics_module.add_function(wrap_pyfunction!(processor_topic, &topics_module)?)?;

    m.add_submodule(&topics_module)?;

    Ok(())
}

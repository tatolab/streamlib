// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The process-wide window event pump: one winit event loop, N registered
//! windows.
//!
//! winit permits exactly one `EventLoop` per process — a second
//! `EventLoop::build()` returns `RecreationAttempt` for the lifetime of the
//! process, and dropping the first loop does not free the slot. A processor
//! that builds its own loop therefore works only if it is the only one, so
//! the loop is owned here and window-owning processors register with it.
//!
//! The pump owns the scarce resource and nothing else. Window policy — title,
//! size, what a resize means, when to redraw, what closing does — stays with
//! the registering processor: it supplies the attributes and consumes the
//! events. The raw-window-handle seam between the window and the present
//! target is untouched; the pump never mints a surface and never draws.
//!
//! Linux-gated but in `core/` rather than `linux/`: this is the seam an Apple
//! main-thread implementation fills, and Apple's rule — the loop must live on
//! the process's first thread — changes where the loop is driven, not what a
//! window owner asks for or what it is handed back.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::core::error::{Error, Result};

/// How long a caller waits for the pump thread to reach the point where it can
/// mint windows, and for an individual window request to come back. Generous
/// enough for a cold X11 / Wayland connection, bounded so a wedged compositor
/// surfaces as a degraded display rather than a hung graph.
const WINDOW_EVENT_PUMP_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// What a window-owning processor asks the pump to create for it.
#[derive(Debug, Clone)]
pub struct WindowRegistrationRequestFromOwningProcessor {
    /// Window title, owned by the requesting processor.
    pub window_title: String,
    /// Requested initial width in physical pixels.
    pub initial_width_in_physical_pixels: u32,
    /// Requested initial height in physical pixels.
    pub initial_height_in_physical_pixels: u32,
}

/// A window event the pump forwards to the processor that owns that window.
///
/// Deliberately narrow: the pump translates the winit events a window owner
/// must act on and drops the rest, so no winit vocabulary reaches processors
/// through this seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEventForOwningProcessor {
    /// The window's drawable area changed. Never zero in either dimension —
    /// the pump drops the minimise-to-nothing events winit also reports.
    ResizedToPhysicalPixels { width: u32, height: u32 },
    /// The user asked to close this window.
    CloseRequestedByUser,
}

/// A window minted by the pump on behalf of one processor.
///
/// Dropping it closes the window and deregisters it: the pump holds no
/// reference of its own, so this is the only thing keeping the window alive.
pub struct WindowRegisteredWithEventPump {
    window_id: WindowId,
    events_from_event_pump: Receiver<WindowEventForOwningProcessor>,
    control_messages_to_event_pump: EventLoopProxy<WindowEventPumpControlMessage>,
    window: Arc<Window>,
}

impl WindowRegisteredWithEventPump {
    /// The window itself, for the raw-window-handle seam that mints a present
    /// target. Cloning the `Arc` keeps the window alive past this
    /// registration, which is what the present target's handle requires.
    pub fn window_shared_with_event_pump(&self) -> &Arc<Window> {
        &self.window
    }

    /// Every event the pump has routed to this window since the last drain,
    /// reduced to what an owner acts on. Never blocks.
    ///
    /// Resizes coalesce to the last: a drag emits one event per motion step and
    /// only the final extent is worth a swapchain recreate.
    pub fn drain_window_events_from_event_pump(&self) -> CoalescedWindowEventsFromEventPump {
        let mut coalesced = CoalescedWindowEventsFromEventPump::default();
        for event in self.events_from_event_pump.try_iter() {
            match event {
                WindowEventForOwningProcessor::ResizedToPhysicalPixels { width, height } => {
                    coalesced.resized_to_physical_pixels = Some((width, height));
                }
                WindowEventForOwningProcessor::CloseRequestedByUser => {
                    coalesced.close_requested_by_user = true;
                }
            }
        }
        coalesced
    }

    /// The window's current drawable size in physical pixels, clamped away
    /// from zero so it is always a legal swapchain extent.
    pub fn current_physical_size(&self) -> (u32, u32) {
        let size = self.window.inner_size();
        (size.width.max(1), size.height.max(1))
    }
}

impl Drop for WindowRegisteredWithEventPump {
    fn drop(&mut self) {
        let _ = self.control_messages_to_event_pump.send_event(
            WindowEventPumpControlMessage::ForgetWindowOfOwningProcessor {
                window_id: self.window_id,
            },
        );
    }
}

/// What one drain of a window's event stream amounts to, for the processor
/// that owns the window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CoalescedWindowEventsFromEventPump {
    /// The window's final extent this drain, if it was resized at all.
    pub resized_to_physical_pixels: Option<(u32, u32)>,
    /// Whether the user asked to close the window during this drain.
    pub close_requested_by_user: bool,
}

/// The process-wide pump. Reached through [`process_wide_window_event_pump`];
/// never constructed by callers.
pub struct ProcessWideWindowEventPump {
    control_messages_to_event_pump: EventLoopProxy<WindowEventPumpControlMessage>,
}

impl ProcessWideWindowEventPump {
    /// Ask the pump for a window. The window is created on the pump's thread
    /// and handed back; every policy decision about it stays with the caller.
    pub fn request_window_for_owning_processor(
        &self,
        request: WindowRegistrationRequestFromOwningProcessor,
    ) -> Result<WindowRegisteredWithEventPump> {
        let (reply_to_requesting_processor, reply_from_event_pump) = sync_channel(1);
        self.control_messages_to_event_pump
            .send_event(
                WindowEventPumpControlMessage::CreateWindowForOwningProcessor {
                    request,
                    reply_to_requesting_processor,
                },
            )
            .map_err(|_| {
                Error::DisplaySurfaceUnavailable(
                    "window event pump is no longer running; no window can be created".into(),
                )
            })?;

        // The two failures are told apart: a dead pump is immediate and
        // permanent, a timeout means the compositor is still thinking. Reporting
        // the first as the second sends a reader hunting a slow compositor that
        // was never involved.
        match reply_from_event_pump.recv_timeout(WINDOW_EVENT_PUMP_REPLY_TIMEOUT) {
            Ok(registration) => registration,
            Err(RecvTimeoutError::Disconnected) => Err(Error::DisplaySurfaceUnavailable(
                "window event pump stopped before it answered a window request".into(),
            )),
            Err(RecvTimeoutError::Timeout) => Err(Error::DisplaySurfaceUnavailable(format!(
                "window event pump did not answer a window request within \
                 {WINDOW_EVENT_PUMP_REPLY_TIMEOUT:?}"
            ))),
        }
    }
}

/// The one pump for this process, started on first use.
///
/// The outcome is cached either way: a process that cannot build an event loop
/// — no display server, or a non-winit consumer already took the one slot —
/// answers with the same error forever rather than retrying per caller and
/// burning the slot on a race.
pub fn process_wide_window_event_pump() -> Result<&'static ProcessWideWindowEventPump> {
    static PROCESS_WIDE_WINDOW_EVENT_PUMP: OnceLock<
        std::result::Result<ProcessWideWindowEventPump, String>,
    > = OnceLock::new();

    PROCESS_WIDE_WINDOW_EVENT_PUMP
        .get_or_init(start_window_event_pump_thread)
        .as_ref()
        .map_err(|reason| Error::DisplaySurfaceUnavailable(reason.clone()))
}

/// Messages the pump's own thread acts on. Carried over the winit proxy, which
/// is the only way to reach an `ActiveEventLoop` from another thread.
enum WindowEventPumpControlMessage {
    CreateWindowForOwningProcessor {
        request: WindowRegistrationRequestFromOwningProcessor,
        reply_to_requesting_processor: SyncSender<Result<WindowRegisteredWithEventPump>>,
    },
    ForgetWindowOfOwningProcessor {
        window_id: WindowId,
    },
}

fn start_window_event_pump_thread() -> std::result::Result<ProcessWideWindowEventPump, String> {
    let (pump_startup_outcome_sender, pump_startup_outcome) = sync_channel(1);

    std::thread::Builder::new()
        .name("streamlib-window-event-pump".to_string())
        .spawn(move || {
            let event_loop = match build_the_processes_one_event_loop() {
                Ok(event_loop) => event_loop,
                Err(reason) => {
                    let _ = pump_startup_outcome_sender.send(Err(reason));
                    return;
                }
            };
            // `ActiveEventLoop` has no `create_proxy`, so the handler must
            // carry its own clone to hand out with each registration.
            let control_messages_to_event_pump = event_loop.create_proxy();
            let mut handler = WindowEventPumpApplicationHandler {
                startup_reply: Some((
                    control_messages_to_event_pump.clone(),
                    pump_startup_outcome_sender,
                )),
                control_messages_to_event_pump,
                registered_windows: RegisteredWindowsByWindowId::default(),
            };
            if let Err(e) = event_loop.run_app(&mut handler) {
                tracing::error!(error = %e, "window event pump: event loop exited with an error");
            }
            // Reaching here means the one loop this process may build is spent
            // and no further window can ever be created; say so once rather
            // than letting later requests time out silently.
            tracing::error!(
                "window event pump: the event loop stopped — no further windows can be created \
                 in this process"
            );
        })
        .map_err(|e| format!("failed to spawn the window event pump thread: {e}"))?;

    match pump_startup_outcome.recv_timeout(WINDOW_EVENT_PUMP_REPLY_TIMEOUT) {
        Ok(Ok(control_messages_to_event_pump)) => Ok(ProcessWideWindowEventPump {
            control_messages_to_event_pump,
        }),
        Ok(Err(reason)) => Err(reason),
        Err(_) => Err(format!(
            "the window event pump thread did not start within {WINDOW_EVENT_PUMP_REPLY_TIMEOUT:?}"
        )),
    }
}

fn build_the_processes_one_event_loop()
-> std::result::Result<EventLoop<WindowEventPumpControlMessage>, String> {
    // The pump runs on its own thread, not the process main thread; both Linux
    // backends need their own any-thread opt-in (each trait method flags only
    // its own backend).
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    use winit::platform::x11::EventLoopBuilderExtX11;

    let mut builder = EventLoop::<WindowEventPumpControlMessage>::with_user_event();
    EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
    EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
    builder
        .build()
        .map_err(|e| format!("failed to build the window event loop: {e}"))
}

/// The pump thread's book of live windows. Holds the delivery end only — the
/// window itself belongs to the processor that asked for it, so a registration
/// dropped by its owner closes the window without the pump's involvement.
#[derive(Default)]
struct RegisteredWindowsByWindowId {
    events_to_owning_processors: HashMap<WindowId, Sender<WindowEventForOwningProcessor>>,
}

impl RegisteredWindowsByWindowId {
    fn register(
        &mut self,
        window_id: WindowId,
        events_to_owning_processor: Sender<WindowEventForOwningProcessor>,
    ) {
        self.events_to_owning_processors
            .insert(window_id, events_to_owning_processor);
    }

    fn forget(&mut self, window_id: WindowId) {
        self.events_to_owning_processors.remove(&window_id);
    }

    /// Route one event to the window's own owner. An event for a window that
    /// is not registered is dropped, and an owner that has gone away is
    /// forgotten here so the book does not grow for the process's lifetime.
    fn deliver(&mut self, window_id: WindowId, event: WindowEventForOwningProcessor) {
        let Some(events_to_owning_processor) = self.events_to_owning_processors.get(&window_id)
        else {
            return;
        };
        if events_to_owning_processor.send(event).is_err() {
            self.forget(window_id);
        }
    }

    fn registered_window_count(&self) -> usize {
        self.events_to_owning_processors.len()
    }
}

struct WindowEventPumpApplicationHandler {
    /// Taken on the first `resumed`: a window cannot be created before then,
    /// so callers are not handed a pump they cannot yet use.
    startup_reply: Option<(
        EventLoopProxy<WindowEventPumpControlMessage>,
        SyncSender<std::result::Result<EventLoopProxy<WindowEventPumpControlMessage>, String>>,
    )>,
    control_messages_to_event_pump: EventLoopProxy<WindowEventPumpControlMessage>,
    registered_windows: RegisteredWindowsByWindowId,
}

impl ApplicationHandler<WindowEventPumpControlMessage> for WindowEventPumpApplicationHandler {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some((control_messages_to_event_pump, startup_outcome_sender)) =
            self.startup_reply.take()
        {
            let _ = startup_outcome_sender.send(Ok(control_messages_to_event_pump));
            tracing::info!("window event pump: ready");
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, message: WindowEventPumpControlMessage) {
        match message {
            WindowEventPumpControlMessage::CreateWindowForOwningProcessor {
                request,
                reply_to_requesting_processor,
            } => {
                let reply = self.create_window_for_owning_processor(event_loop, request);
                // A requester that timed out and dropped its receiver leaves the
                // window here; letting the registration drop closes it and
                // deregisters it, rather than stranding a routing entry no
                // later event will ever sweep.
                if let Err(std::sync::mpsc::SendError(unclaimed)) =
                    reply_to_requesting_processor.send(reply)
                {
                    drop(unclaimed);
                }
            }
            WindowEventPumpControlMessage::ForgetWindowOfOwningProcessor { window_id } => {
                self.registered_windows.forget(window_id);
                tracing::debug!(
                    registered_window_count = self.registered_windows.registered_window_count(),
                    "window event pump: window deregistered"
                );
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.registered_windows.deliver(
                window_id,
                WindowEventForOwningProcessor::CloseRequestedByUser,
            ),
            WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    return;
                }
                self.registered_windows.deliver(
                    window_id,
                    WindowEventForOwningProcessor::ResizedToPhysicalPixels {
                        width: size.width,
                        height: size.height,
                    },
                );
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Owners render on their own threads, so the pump has no cadence of
        // its own: it sleeps until a window event or a control message
        // arrives and never spins.
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

impl WindowEventPumpApplicationHandler {
    fn create_window_for_owning_processor(
        &mut self,
        event_loop: &ActiveEventLoop,
        request: WindowRegistrationRequestFromOwningProcessor,
    ) -> Result<WindowRegisteredWithEventPump> {
        let window = event_loop
            .create_window(window_attributes_for_request(&request))
            .map_err(|e| {
                Error::DisplaySurfaceUnavailable(format!(
                    "window event pump: creating window '{}' failed: {e}",
                    request.window_title
                ))
            })?;
        let window = Arc::new(window);
        let (events_to_owning_processor, events_from_event_pump) = std::sync::mpsc::channel();
        self.registered_windows
            .register(window.id(), events_to_owning_processor);
        tracing::info!(
            window_title = %request.window_title,
            registered_window_count = self.registered_windows.registered_window_count(),
            "window event pump: window registered"
        );
        Ok(WindowRegisteredWithEventPump {
            window_id: window.id(),
            events_from_event_pump,
            control_messages_to_event_pump: self.control_messages_to_event_pump.clone(),
            window,
        })
    }
}

fn window_attributes_for_request(
    request: &WindowRegistrationRequestFromOwningProcessor,
) -> WindowAttributes {
    WindowAttributes::default()
        .with_title(request.window_title.clone())
        .with_inner_size(PhysicalSize::new(
            request.initial_width_in_physical_pixels.max(1),
            request.initial_height_in_physical_pixels.max(1),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_for(
        title: &str,
        width: u32,
        height: u32,
    ) -> WindowRegistrationRequestFromOwningProcessor {
        WindowRegistrationRequestFromOwningProcessor {
            window_title: title.to_string(),
            initial_width_in_physical_pixels: width,
            initial_height_in_physical_pixels: height,
        }
    }

    #[test]
    fn an_event_reaches_only_the_owner_of_its_own_window() {
        let mut registered_windows = RegisteredWindowsByWindowId::default();
        let (first_sender, first_owner) = std::sync::mpsc::channel();
        let (second_sender, second_owner) = std::sync::mpsc::channel();
        registered_windows.register(WindowId::from(1_u64), first_sender);
        registered_windows.register(WindowId::from(2_u64), second_sender);

        registered_windows.deliver(
            WindowId::from(2_u64),
            WindowEventForOwningProcessor::CloseRequestedByUser,
        );

        assert_eq!(
            second_owner.try_recv().ok(),
            Some(WindowEventForOwningProcessor::CloseRequestedByUser),
            "the addressed window's owner receives the event"
        );
        assert!(
            first_owner.try_recv().is_err(),
            "a second window's owner never sees another window's events"
        );
    }

    #[test]
    fn an_event_for_an_unregistered_window_is_dropped() {
        let mut registered_windows = RegisteredWindowsByWindowId::default();
        let (sender, owner) = std::sync::mpsc::channel();
        registered_windows.register(WindowId::from(1_u64), sender);

        registered_windows.deliver(
            WindowId::from(7_u64),
            WindowEventForOwningProcessor::CloseRequestedByUser,
        );

        assert!(owner.try_recv().is_err());
        assert_eq!(registered_windows.registered_window_count(), 1);
    }

    #[test]
    fn a_window_whose_owner_went_away_is_forgotten_on_the_next_event() {
        let mut registered_windows = RegisteredWindowsByWindowId::default();
        let (sender, owner) = std::sync::mpsc::channel();
        registered_windows.register(WindowId::from(1_u64), sender);
        drop(owner);

        registered_windows.deliver(
            WindowId::from(1_u64),
            WindowEventForOwningProcessor::CloseRequestedByUser,
        );

        assert_eq!(
            registered_windows.registered_window_count(),
            0,
            "the book does not keep a record whose owner is gone"
        );
    }

    #[test]
    fn deregistering_one_window_leaves_the_others_registered() {
        let mut registered_windows = RegisteredWindowsByWindowId::default();
        let (first_sender, _first_owner) = std::sync::mpsc::channel();
        let (second_sender, second_owner) = std::sync::mpsc::channel();
        registered_windows.register(WindowId::from(1_u64), first_sender);
        registered_windows.register(WindowId::from(2_u64), second_sender);

        registered_windows.forget(WindowId::from(1_u64));

        assert_eq!(registered_windows.registered_window_count(), 1);
        registered_windows.deliver(
            WindowId::from(2_u64),
            WindowEventForOwningProcessor::CloseRequestedByUser,
        );
        assert!(
            second_owner.try_recv().is_ok(),
            "the surviving window still receives its own events"
        );
    }

    #[test]
    fn a_request_carries_its_title_and_size_to_the_window_attributes() {
        let attributes = window_attributes_for_request(&request_for("Debug view", 640, 480));
        assert_eq!(attributes.title, "Debug view");
        assert_eq!(
            attributes.inner_size,
            Some(PhysicalSize::new(640_u32, 480_u32).into())
        );
    }

    #[test]
    fn a_zero_sized_request_is_clamped_to_a_legal_extent() {
        let attributes = window_attributes_for_request(&request_for("Zero", 0, 0));
        assert_eq!(
            attributes.inner_size,
            Some(PhysicalSize::new(1_u32, 1_u32).into()),
            "a zero extent is never handed to winit or to a swapchain"
        );
    }
}

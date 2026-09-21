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
//! events. The pump never mints a surface and never draws; it hands the owner
//! what a present target is minted from.
//!
//! In `core/` rather than `linux/` because it is one seam with a per-platform
//! loop under it. On Linux the loop runs on a thread of its own. On Apple it
//! must live on the process's first thread, so it is built there when the
//! runtime starts and driven there while `rt.run()` blocks; AppKit touches a
//! view only from that thread, so the pump also attaches each window's Metal
//! layer as it mints the window. Neither changes what a window owner asks for.

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopClosed, EventLoopProxy};
use winit::window::{Window, WindowAttributes, WindowId};

#[cfg(target_os = "macos")]
use crate::apple::metal_layer_added_as_sublayer_of_window_content_view::MetalLayerAddedAsSublayerOfWindowContentView;
use crate::core::error::{Error, Result};
use crate::vulkan::rhi::PresentSurfaceSource;

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
    /// Requested initial width in the desktop's logical pixels — physical
    /// pixels divided by the display's scale factor, so a window is the same
    /// size on a 1x and a 2x display.
    pub initial_width_in_logical_pixels: u32,
    /// Requested initial height in the desktop's logical pixels.
    pub initial_height_in_logical_pixels: u32,
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
/// Dropping it deregisters the window and hands it back to the pump, which
/// closes it: the pump holds no reference of its own while it is registered,
/// so this is the only thing keeping the window alive.
pub struct WindowRegisteredWithEventPump {
    window_id: WindowId,
    events_from_event_pump: Receiver<WindowEventForOwningProcessor>,
    control_messages_to_event_pump: EventLoopProxy<WindowEventPumpControlMessage>,
    /// Taken in `Drop` and sent to the pump, so the window is released on the
    /// pump's thread. AppKit closes a window only on the first thread and
    /// winit blocks until it has, so an owner releasing it during teardown —
    /// when nothing drives the loop — would otherwise hang there.
    window_handed_back_to_the_event_pump_on_drop: ManuallyDrop<WindowMintedByTheEventPump>,
    physical_size_when_minted: (u32, u32),
}

/// A window and what its present target is minted from, released together.
struct WindowMintedByTheEventPump {
    window: Window,
    #[cfg(target_os = "macos")]
    metal_layer_added_as_sublayer_of_window_content_view:
        MetalLayerAddedAsSublayerOfWindowContentView,
}

impl WindowRegisteredWithEventPump {
    /// The pump's id for this window.
    pub fn window_id(&self) -> WindowId {
        self.window_id
    }

    /// What this window's present target is minted from. Borrowed, so the
    /// window outlives the target only while this registration does.
    pub fn present_surface_source(&self) -> PresentSurfaceSource<'_> {
        let window_minted_by_the_event_pump = &*self.window_handed_back_to_the_event_pump_on_drop;
        #[cfg(target_os = "linux")]
        {
            PresentSurfaceSource::NativeWindow {
                window_handle: &window_minted_by_the_event_pump.window,
                display_handle: &window_minted_by_the_event_pump.window,
            }
        }
        #[cfg(target_os = "macos")]
        {
            PresentSurfaceSource::MetalLayerAddedAsSublayerOfWindowContentView(
                &window_minted_by_the_event_pump
                    .metal_layer_added_as_sublayer_of_window_content_view,
            )
        }
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

    /// The window's drawable size in physical pixels when the pump minted
    /// it, clamped to a legal swapchain extent. Read on the pump's thread, so
    /// asking never waits on it.
    pub fn physical_size_when_minted(&self) -> (u32, u32) {
        self.physical_size_when_minted
    }

    /// The window's current drawable size in physical pixels, clamped to a
    /// legal swapchain extent. On Apple this waits on the process's first
    /// thread, so it does not answer while nothing drives the loop.
    pub fn current_physical_size(&self) -> (u32, u32) {
        legal_swapchain_extent_of(
            self.window_handed_back_to_the_event_pump_on_drop
                .window
                .inner_size(),
        )
    }
}

/// A window size clamped away from zero, so it is always a legal swapchain
/// extent.
fn legal_swapchain_extent_of(size: PhysicalSize<u32>) -> (u32, u32) {
    (size.width.max(1), size.height.max(1))
}

impl Drop for WindowRegisteredWithEventPump {
    fn drop(&mut self) {
        // SAFETY: `drop` runs once, and nothing reads the field after this.
        let window_minted_by_the_event_pump =
            unsafe { ManuallyDrop::take(&mut self.window_handed_back_to_the_event_pump_on_drop) };
        let sent_to_the_event_pump = self.control_messages_to_event_pump.send_event(
            WindowEventPumpControlMessage::ForgetAndCloseWindowOfOwningProcessor {
                window_id: self.window_id,
                window_minted_by_the_event_pump,
            },
        );
        if let Err(EventLoopClosed(message_the_stopped_event_pump_handed_back)) =
            sent_to_the_event_pump
        {
            // A stopped pump never runs again. On Apple only it may close a
            // window, and closing one here would wait on it forever, so the
            // window is left for the process's exit; elsewhere it closes here.
            #[cfg(target_os = "macos")]
            std::mem::forget(message_the_stopped_event_pump_handed_back);
            #[cfg(not(target_os = "macos"))]
            drop(message_the_stopped_event_pump_handed_back);
        }
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
    registered_window_count: Arc<AtomicUsize>,
}

impl ProcessWideWindowEventPump {
    /// How many windows the pump is currently routing events to.
    ///
    /// Lags a registration's drop by the round trip its deregistration takes
    /// through the event loop, so a caller watching for a drop polls rather
    /// than reads once.
    pub fn registered_window_count(&self) -> usize {
        self.registered_window_count.load(Ordering::Acquire)
    }

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
///
/// On Apple the pump can only be built on the process's first thread, so a
/// first call from any other thread settles the answer as a refusal.
pub fn process_wide_window_event_pump() -> Result<&'static ProcessWideWindowEventPump> {
    static PROCESS_WIDE_WINDOW_EVENT_PUMP: OnceLock<
        std::result::Result<ProcessWideWindowEventPump, String>,
    > = OnceLock::new();

    #[cfg(target_os = "linux")]
    let start_the_process_wide_window_event_pump = start_window_event_pump_thread;
    #[cfg(target_os = "macos")]
    let start_the_process_wide_window_event_pump = build_the_window_event_pump_on_the_first_thread;

    PROCESS_WIDE_WINDOW_EVENT_PUMP
        .get_or_init(start_the_process_wide_window_event_pump)
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
    ForgetAndCloseWindowOfOwningProcessor {
        window_id: WindowId,
        window_minted_by_the_event_pump: WindowMintedByTheEventPump,
    },
}

#[cfg(target_os = "linux")]
fn start_window_event_pump_thread() -> std::result::Result<ProcessWideWindowEventPump, String> {
    let (pump_startup_outcome_sender, pump_startup_outcome) = sync_channel(1);
    let registered_window_count = Arc::new(AtomicUsize::new(0));
    let registered_window_count_for_pump_thread = Arc::clone(&registered_window_count);

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
            let mut handler = WindowEventPumpApplicationHandler::new(
                control_messages_to_event_pump.clone(),
                registered_window_count_for_pump_thread,
                Some((control_messages_to_event_pump, pump_startup_outcome_sender)),
            );
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
            registered_window_count,
        }),
        Ok(Err(reason)) => Err(reason),
        Err(RecvTimeoutError::Disconnected) => Err(
            "the window event pump thread stopped before it reported that it was ready".to_string(),
        ),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "the window event pump thread did not start within {WINDOW_EVENT_PUMP_REPLY_TIMEOUT:?}"
        )),
    }
}

fn build_the_processes_one_event_loop()
-> std::result::Result<EventLoop<WindowEventPumpControlMessage>, String> {
    let mut builder = EventLoop::<WindowEventPumpControlMessage>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::EventLoopBuilderExtWayland;
        use winit::platform::x11::EventLoopBuilderExtX11;

        // Both Linux backends need their own any-thread opt-in (each trait
        // method flags only its own backend).
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;

        // The engine installs its own menu, whose Quit requests a runtime
        // shutdown instead of terminating the process under the run loop.
        builder.with_default_menu(false);
    }
    builder
        .build()
        .map_err(|e| format!("failed to build the window event loop: {e}"))
}

/// The process's one event loop and the pump it serves, kept on the first
/// thread — the only thread AppKit lets drive it. Taken out for the length of
/// a drive, so a drive nested further up the same stack finds it absent.
#[cfg(target_os = "macos")]
struct EventLoopWithItsPumpOnTheFirstThread {
    event_loop: EventLoop<WindowEventPumpControlMessage>,
    window_event_pump: WindowEventPumpApplicationHandler,
    /// Whether any drive has launched the application yet. A process that
    /// never drove the loop has nothing on screen to close.
    has_been_driven: bool,
}

#[cfg(target_os = "macos")]
thread_local! {
    static EVENT_LOOP_WITH_ITS_PUMP_ON_THE_FIRST_THREAD: std::cell::RefCell<
        Option<EventLoopWithItsPumpOnTheFirstThread>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "macos")]
fn build_the_window_event_pump_on_the_first_thread()
-> std::result::Result<ProcessWideWindowEventPump, String> {
    let Some(first_thread) = objc2::MainThreadMarker::new() else {
        return Err(concat!(
            "the window event pump is built on the process's first thread, and was first ",
            "asked for on another — the runtime was started off the first thread, so no ",
            "window can be created in this process",
        )
        .to_string());
    };
    if objc2_app_kit::NSApplication::sharedApplication(first_thread).isRunning() {
        return Err(concat!(
            "another NSApplication run loop already drives the process's first thread, so ",
            "the window event pump cannot own it",
        )
        .to_string());
    }

    let event_loop = build_the_processes_one_event_loop()?;
    let control_messages_to_event_pump = event_loop.create_proxy();
    let registered_window_count = Arc::new(AtomicUsize::new(0));
    let window_event_pump = WindowEventPumpApplicationHandler::new(
        control_messages_to_event_pump.clone(),
        Arc::clone(&registered_window_count),
        None,
        first_thread,
    );
    EVENT_LOOP_WITH_ITS_PUMP_ON_THE_FIRST_THREAD.with(|event_loop_slot_on_the_first_thread| {
        *event_loop_slot_on_the_first_thread.borrow_mut() =
            Some(EventLoopWithItsPumpOnTheFirstThread {
                event_loop,
                window_event_pump,
                has_been_driven: false,
            });
    });
    tracing::info!("window event pump: ready on the process's first thread");
    Ok(ProcessWideWindowEventPump {
        control_messages_to_event_pump,
        registered_window_count,
    })
}

/// How a drive of the event loop on the process's first thread ended.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEventPumpDriveOnTheFirstThreadOutcome {
    /// The loop ran on this thread until the observation broke.
    DrivenUntilTheObservationBroke,
    /// The loop did not run to the observation's end — this thread does not
    /// hold it, it is already being driven further up this thread's stack, or
    /// it stopped first. The caller keeps observing without it.
    NotDrivenToTheObservationsEnd,
}

/// Drive the process's one event loop on the calling thread, calling
/// `observe_between_events` about every `observation_interval` until it breaks.
///
/// Ending a drive closes every window in the process — winit closes them all
/// when its loop exits — so a caller drives once per run, not in slices.
/// `tear_down_before_the_application_terminates` runs only if AppKit
/// terminates the process under the loop — the Dock's Quit, a logout — which
/// exits the process as soon as the loop reports it.
#[cfg(target_os = "macos")]
pub fn drive_the_window_event_pump_on_the_first_thread_until(
    observation_interval: Duration,
    observe_between_events: impl FnMut() -> std::ops::ControlFlow<()>,
    tear_down_before_the_application_terminates: impl FnOnce(),
) -> WindowEventPumpDriveOnTheFirstThreadOutcome {
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand;

    let Some(mut event_loop_with_its_pump) =
        EVENT_LOOP_WITH_ITS_PUMP_ON_THE_FIRST_THREAD.with(|event_loop_slot_on_the_first_thread| {
            event_loop_slot_on_the_first_thread.borrow_mut().take()
        })
    else {
        return WindowEventPumpDriveOnTheFirstThreadOutcome::NotDrivenToTheObservationsEnd;
    };
    event_loop_with_its_pump.has_been_driven = true;

    let mut window_event_pump_driven_until_the_observation_breaks =
        WindowEventPumpDrivenUntilTheObservationBreaks {
            window_event_pump: &mut event_loop_with_its_pump.window_event_pump,
            observe_between_events,
            observation_interval,
            next_observation_at: std::time::Instant::now(),
            observation_broke: false,
            tear_down_before_the_application_terminates: Some(
                tear_down_before_the_application_terminates,
            ),
        };
    let run_outcome = event_loop_with_its_pump
        .event_loop
        .run_app_on_demand(&mut window_event_pump_driven_until_the_observation_breaks);
    let observation_broke = window_event_pump_driven_until_the_observation_breaks.observation_broke;

    EVENT_LOOP_WITH_ITS_PUMP_ON_THE_FIRST_THREAD.with(|event_loop_slot_on_the_first_thread| {
        *event_loop_slot_on_the_first_thread.borrow_mut() = Some(event_loop_with_its_pump);
    });
    if let Err(e) = run_outcome {
        tracing::error!(error = %e, "window event pump: the event loop stopped with an error");
    }
    if observation_broke {
        WindowEventPumpDriveOnTheFirstThreadOutcome::DrivenUntilTheObservationBroke
    } else {
        WindowEventPumpDriveOnTheFirstThreadOutcome::NotDrivenToTheObservationsEnd
    }
}

/// Deregister and release, on the first thread, the windows their owners
/// handed back while nothing drove the loop — a teardown's windows are
/// otherwise held until the next drive. Does nothing off the first thread, or
/// where the loop was never driven.
#[cfg(target_os = "macos")]
pub fn release_the_windows_handed_back_while_the_event_pump_was_not_driven() {
    let has_been_driven =
        EVENT_LOOP_WITH_ITS_PUMP_ON_THE_FIRST_THREAD.with(|event_loop_slot_on_the_first_thread| {
            event_loop_slot_on_the_first_thread
                .borrow()
                .as_ref()
                .is_some_and(|event_loop_with_its_pump| event_loop_with_its_pump.has_been_driven)
        });
    if has_been_driven {
        // One turn: the loop hands its queued messages over before it first
        // asks whether to wait, and the observation ends it there.
        drive_the_window_event_pump_on_the_first_thread_until(
            Duration::ZERO,
            || std::ops::ControlFlow::Break(()),
            || {},
        );
    }
}

/// The pump, for the length of one drive, with the observation that ends it.
#[cfg(target_os = "macos")]
struct WindowEventPumpDrivenUntilTheObservationBreaks<'pump, ObserveBetweenEvents, TearDown> {
    window_event_pump: &'pump mut WindowEventPumpApplicationHandler,
    observe_between_events: ObserveBetweenEvents,
    observation_interval: Duration,
    next_observation_at: std::time::Instant,
    observation_broke: bool,
    tear_down_before_the_application_terminates: Option<TearDown>,
}

#[cfg(target_os = "macos")]
impl<ObserveBetweenEvents, TearDown> ApplicationHandler<WindowEventPumpControlMessage>
    for WindowEventPumpDrivenUntilTheObservationBreaks<'_, ObserveBetweenEvents, TearDown>
where
    ObserveBetweenEvents: FnMut() -> std::ops::ControlFlow<()>,
    TearDown: FnOnce(),
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        crate::apple::application_menu::install_the_application_menu_whose_quit_requests_a_runtime_shutdown(
            self.window_event_pump.first_thread,
        );
        self.window_event_pump.resumed(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, message: WindowEventPumpControlMessage) {
        self.window_event_pump.user_event(event_loop, message);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.window_event_pump
            .window_event(event_loop, window_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = std::time::Instant::now();
        if now >= self.next_observation_at {
            if (self.observe_between_events)().is_break() {
                self.observation_broke = true;
                event_loop.exit();
                return;
            }
            self.next_observation_at = now + self.observation_interval;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_observation_at));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.observation_broke {
            return;
        }
        tracing::warn!(
            "window event pump: the application is terminating under the event loop; tearing \
             the runtime down before the process exits"
        );
        if let Some(tear_down_before_the_application_terminates) =
            self.tear_down_before_the_application_terminates.take()
        {
            tear_down_before_the_application_terminates();
        }
    }
}

/// The pump thread's book of live windows. Holds the delivery end only — the
/// window itself belongs to the processor that asked for it until its
/// registration hands it back.
struct RegisteredWindowsByWindowId {
    events_to_owning_processors: HashMap<WindowId, Sender<WindowEventForOwningProcessor>>,
    /// Published for [`ProcessWideWindowEventPump::registered_window_count`],
    /// which is read from other threads and so cannot reach the map itself.
    published_registered_window_count: Arc<AtomicUsize>,
}

impl RegisteredWindowsByWindowId {
    fn new(published_registered_window_count: Arc<AtomicUsize>) -> Self {
        Self {
            events_to_owning_processors: HashMap::new(),
            published_registered_window_count,
        }
    }

    fn publish_registered_window_count(&self) {
        self.published_registered_window_count
            .store(self.events_to_owning_processors.len(), Ordering::Release);
    }

    fn register(
        &mut self,
        window_id: WindowId,
        events_to_owning_processor: Sender<WindowEventForOwningProcessor>,
    ) {
        self.events_to_owning_processors
            .insert(window_id, events_to_owning_processor);
        self.publish_registered_window_count();
    }

    fn forget(&mut self, window_id: WindowId) {
        self.events_to_owning_processors.remove(&window_id);
        self.publish_registered_window_count();
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

/// The proxy a pump hands its callers once it can mint windows, and where it
/// hands it.
type WindowEventPumpStartupReply = (
    EventLoopProxy<WindowEventPumpControlMessage>,
    SyncSender<std::result::Result<EventLoopProxy<WindowEventPumpControlMessage>, String>>,
);

struct WindowEventPumpApplicationHandler {
    /// Taken on the first `resumed`: a window cannot be created before then,
    /// so callers are not handed a pump they cannot yet use.
    startup_reply: Option<WindowEventPumpStartupReply>,
    control_messages_to_event_pump: EventLoopProxy<WindowEventPumpControlMessage>,
    registered_windows: RegisteredWindowsByWindowId,
    /// On Apple the handler lives and runs only on the first thread.
    #[cfg(target_os = "macos")]
    first_thread: objc2::MainThreadMarker,
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
            WindowEventPumpControlMessage::ForgetAndCloseWindowOfOwningProcessor {
                window_id,
                window_minted_by_the_event_pump,
            } => {
                self.registered_windows.forget(window_id);
                drop(window_minted_by_the_event_pump);
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
    fn new(
        control_messages_to_event_pump: EventLoopProxy<WindowEventPumpControlMessage>,
        registered_window_count: Arc<AtomicUsize>,
        startup_reply: Option<WindowEventPumpStartupReply>,
        #[cfg(target_os = "macos")] first_thread: objc2::MainThreadMarker,
    ) -> Self {
        Self {
            startup_reply,
            control_messages_to_event_pump,
            registered_windows: RegisteredWindowsByWindowId::new(registered_window_count),
            #[cfg(target_os = "macos")]
            first_thread,
        }
    }

    /// Pair a window the pump just created with what its present target is
    /// minted from.
    fn pair_the_window_with_its_present_surface_source(
        &self,
        window: Window,
    ) -> Result<WindowMintedByTheEventPump> {
        #[cfg(target_os = "macos")]
        crate::apple::appkit_content_view_of_winit_window::close_without_animating(
            &window,
            self.first_thread,
        )?;
        Ok(WindowMintedByTheEventPump {
            #[cfg(target_os = "macos")]
            metal_layer_added_as_sublayer_of_window_content_view:
                MetalLayerAddedAsSublayerOfWindowContentView::add_as_a_sublayer_of_the_content_view_of(
                    &window,
                    self.first_thread,
                )?,
            window,
        })
    }

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
        let physical_size_when_minted = legal_swapchain_extent_of(window.inner_size());
        let window_minted_by_the_event_pump =
            self.pair_the_window_with_its_present_surface_source(window)?;
        let window_id = window_minted_by_the_event_pump.window.id();
        let (events_to_owning_processor, events_from_event_pump) = std::sync::mpsc::channel();
        self.registered_windows
            .register(window_id, events_to_owning_processor);
        tracing::info!(
            window_title = %request.window_title,
            registered_window_count = self.registered_windows.registered_window_count(),
            "window event pump: window registered"
        );
        Ok(WindowRegisteredWithEventPump {
            window_id,
            events_from_event_pump,
            control_messages_to_event_pump: self.control_messages_to_event_pump.clone(),
            window_handed_back_to_the_event_pump_on_drop: ManuallyDrop::new(
                window_minted_by_the_event_pump,
            ),
            physical_size_when_minted,
        })
    }
}

fn window_attributes_for_request(
    request: &WindowRegistrationRequestFromOwningProcessor,
) -> WindowAttributes {
    WindowAttributes::default()
        .with_title(request.window_title.clone())
        .with_inner_size(LogicalSize::new(
            request.initial_width_in_logical_pixels.max(1),
            request.initial_height_in_logical_pixels.max(1),
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
            initial_width_in_logical_pixels: width,
            initial_height_in_logical_pixels: height,
        }
    }

    #[test]
    fn an_event_reaches_only_the_owner_of_its_own_window() {
        let mut registered_windows =
            RegisteredWindowsByWindowId::new(Arc::new(AtomicUsize::new(0)));
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
        let mut registered_windows =
            RegisteredWindowsByWindowId::new(Arc::new(AtomicUsize::new(0)));
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
        let mut registered_windows =
            RegisteredWindowsByWindowId::new(Arc::new(AtomicUsize::new(0)));
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
        let mut registered_windows =
            RegisteredWindowsByWindowId::new(Arc::new(AtomicUsize::new(0)));
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
            Some(LogicalSize::new(640_u32, 480_u32).into())
        );
    }

    #[test]
    fn a_zero_sized_request_is_clamped_to_a_legal_extent() {
        let attributes = window_attributes_for_request(&request_for("Zero", 0, 0));
        assert_eq!(
            attributes.inner_size,
            Some(LogicalSize::new(1_u32, 1_u32).into()),
            "a zero extent is never handed to winit or to a swapchain"
        );
    }
}
